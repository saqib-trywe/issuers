// SPDX-License-Identifier: GPL-3.0-only

//! `issuers-cli` — the command-line surface.
//!
//! A *client* of the local HTTP API, never a second writer: everything goes
//! through the running app, so the window's projection and the database can
//! never disagree about what happened. The cost is that these commands need
//! Issues to be running, which is reported as its own exit code rather than
//! folded in with genuine failures. See `docs/adr/0008`.

pub mod args;
mod render;

use std::ffi::OsString;
use std::io::{IsTerminal, Read, Write};

use termcolor::{ColorChoice, StandardStream, WriteColor};

use crate::api::wire::{IssueJson, NewIssue as NewIssueBody, PatchIssue, TagJson};
use crate::domain::{IssueId, Narrowing, ParentFilter, Priority, Status, View};

use crate::client::{self, Client, ClientError, Reply};
use crate::operations::{self, Call};
use args::{Body, Changes, Colour, Command, Filters, NewIssue};

/// Built from the domain rather than written out, so the list of statuses can
/// never drift from the ones `FromStr` accepts.
fn help() -> String {
    let statuses: Vec<&str> = Status::ALL.iter().map(|status| status.label()).collect();
    let priorities: Vec<&str> = Priority::ALL
        .iter()
        .map(|priority| priority.label())
        .collect();

    format!(
        "\
issuers-cli — the command line for the Issuers tracker

USAGE
  issuers-cli <command> [options]

COMMANDS
  list                        List issues, most pressing first
  show <id>                   Show one issue in full
  new \"<title>\"               File a new issue
  set <id>                    Change an issue's fields
  rm <id>                     Delete an issue permanently
  tag add|rm <id> <name>...   Add or remove tags
  sub add|rm <id> <child>...  Attach or detach sub-issues
  tags                        List the tags in use, with counts

LIST OPTIONS
  --status <status>           {statuses}
  --tag <name>                Only issues carrying this tag
  --parent <id|none>          Sub-issues of an issue, or only unparented ones
  --search <text>             Only issues whose title contains this

NEW AND SET OPTIONS
  --title <text>              set only
  --body <text|->             \"-\" reads the body from standard input
  --status <status>
  --priority <priority>       {priorities}
  --size <n|none>             Relative size, 0-255. `none` unsizes; `new`
                              takes a number only
  --tag <name>                new only, repeatable. Use `issuers-cli tag` to change
                              the tags of an issue that already exists
  --parent <id>               new only

GLOBAL OPTIONS
  --json                      Print the API's JSON instead of a table
  --color <auto|always|never>
  -h, --help                  Show this
  -V, --version               Show the version

Status and priority names are case-insensitive. Deleting is permanent; to
record that you decided against an issue instead, use --status Cancelled.

Issuers must be running: `issuers-cli` talks to it over a local HTTP API rather than
opening the database itself.
",
        statuses = statuses.join(", "),
        priorities = priorities.join(", "),
    )
}

/// Why a command did not finish, and what the shell should be told.
///
/// The three kinds are separated because a caller's next move differs: fix the
/// command, fix the request, or start the app.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// The command line was wrong. Exit 2.
    Usage(String),
    /// The request was made and refused, or something else broke. Exit 1.
    Failed(String),
    /// There is no API to talk to. Exit 3, so a wrapper can tell this apart
    /// from a genuine failure and offer to start the app.
    NotRunning(String),
}

impl Failure {
    pub fn usage(message: impl Into<String>) -> Self {
        Failure::Usage(message.into())
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Failure::Failed(message.into())
    }

    pub fn not_running(message: impl Into<String>) -> Self {
        Failure::NotRunning(message.into())
    }

    pub fn code(&self) -> u8 {
        match self {
            Failure::Failed(_) => 1,
            Failure::Usage(_) => 2,
            Failure::NotRunning(_) => 3,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Failure::Usage(message) | Failure::Failed(message) | Failure::NotRunning(message) => {
                message
            }
        }
    }
}

/// The client knows whether there was an app to talk to; only the CLI knows
/// that this is worth a distinct exit code.
impl From<ClientError> for Failure {
    fn from(err: ClientError) -> Self {
        match err {
            ClientError::NotRunning(message) => Failure::NotRunning(message),
            ClientError::Failed(message) => Failure::Failed(message),
        }
    }
}

/// Runs one invocation and returns the process exit code.
///
/// Errors go to stderr as prose whatever `--json` says, so stdout is always
/// either valid JSON or empty and `| jq` never chokes on an explanation.
pub fn run(argv: Vec<OsString>) -> u8 {
    // Resolved before parsing so a failure is reported to a stream that has
    // already decided about colour; `--color` is read twice, harmlessly.
    let colour = args::colour_of(&argv);
    let mut out = stream(colour);

    match run_into(argv, &mut out) {
        Ok(()) => 0,
        Err(failure) => {
            eprintln!("issuers-cli: {}", failure.message());
            failure.code()
        }
    }
}

/// The whole command, writing to a caller-supplied stream.
///
/// Separate from [`run`] so an integration test can drive a real request over
/// a real socket and read what came out, without spawning a process.
pub fn run_into(argv: Vec<OsString>, out: &mut dyn WriteColor) -> Result<(), Failure> {
    let invocation = args::parse(argv)?;

    match invocation.command {
        Command::Help => write!(out, "{}", help()).map_err(broken_pipe),
        Command::Version => {
            writeln!(out, "issue {}", env!("CARGO_PKG_VERSION")).map_err(broken_pipe)
        }
        command => {
            let client = client::connect()?;
            dispatch(&client, out, invocation.json, command)
        }
    }
}

fn dispatch(
    client: &Client,
    out: &mut dyn WriteColor,
    json_only: bool,
    command: Command,
) -> Result<(), Failure> {
    match command {
        Command::Help | Command::Version => unreachable!("handled before connecting"),

        Command::List(filters) => {
            let reply = client.send(&operations::list(&narrowing(filters)))?;
            let issues: Vec<IssueJson> = decode(&ok(reply)?)?;
            if json_only {
                return emit(out, &issues);
            }

            render::list(out, &issues).map_err(broken_pipe)
        }

        Command::Show(id) => {
            let reply = ok(client.send(&operations::get(id))?)?;
            if json_only {
                return raw(out, &reply);
            }
            let issue: IssueJson = decode(&reply)?;
            present(client, out, &issue)
        }

        Command::New(new) => {
            let reply = ok(client.send(&operations::create(&new_issue(*new)?))?)?;
            if json_only {
                return raw(out, &reply);
            }
            let issue: IssueJson = decode(&reply)?;
            present(client, out, &issue)
        }

        Command::Set(id, changes) => {
            let reply = ok(client.send(&operations::update(id, &patch(*changes)?))?)?;
            if json_only {
                return raw(out, &reply);
            }
            let issue: IssueJson = decode(&reply)?;
            present(client, out, &issue)
        }

        Command::Remove { id, force } => remove(client, out, id, force),

        Command::Tags => {
            let reply = ok(client.send(&operations::tags())?)?;
            if json_only {
                return raw(out, &reply);
            }
            let tags: Vec<TagJson> = decode(&reply)?;
            render::tags(out, &tags).map_err(broken_pipe)
        }

        Command::Tag { id, names, add } => {
            let calls: Vec<(String, Call)> = names
                .iter()
                .map(|tag| {
                    let call = match add {
                        true => operations::add_tag(id, tag),
                        false => operations::remove_tag(id, tag),
                    };
                    (tag.as_str().to_string(), call)
                })
                .collect();
            each(client, out, json_only, add, calls)
        }

        Command::Sub { id, children, add } => {
            let calls: Vec<(String, Call)> = children
                .iter()
                .map(|child| {
                    let call = match add {
                        true => operations::add_sub_issue(id, *child),
                        false => operations::remove_sub_issue(id, *child),
                    };
                    (format!("#{child}"), call)
                })
                .collect();
            each(client, out, json_only, add, calls)
        }
    }
}

/// Applies one request per named thing, in order.
///
/// A refusal partway through leaves the earlier ones applied, so the error
/// says which landed — otherwise `issuers-cli tag add 7 a b c` failing on `b` looks
/// like it did nothing.
fn each(
    client: &Client,
    out: &mut dyn WriteColor,
    json_only: bool,
    add: bool,
    calls: Vec<(String, Call)>,
) -> Result<(), Failure> {
    if calls.is_empty() {
        return Err(Failure::usage("nothing to add or remove"));
    }

    let mut applied: Vec<String> = Vec::new();
    let mut last = None;

    for (name, call) in calls {
        let reply = client.send(&call)?;
        match ok(reply) {
            Ok(reply) => {
                applied.push(name);
                last = Some(reply);
            }
            Err(failure) => {
                let verb = if add { "applied" } else { "removed" };
                let already = if applied.is_empty() {
                    String::new()
                } else {
                    format!(" (already {verb}: {})", applied.join(", "))
                };
                return Err(Failure::failed(format!(
                    "{name}: {}{already}",
                    failure.message()
                )));
            }
        }
    }

    // The final reply is the Issue's finished state, which is what was asked
    // about however many requests it took to get there.
    let reply = last.expect("a non-empty list applied at least one");
    if json_only {
        return raw(out, &reply);
    }
    let issue: IssueJson = decode(&reply)?;
    present(client, out, &issue)
}

fn remove(
    client: &Client,
    out: &mut dyn WriteColor,
    id: IssueId,
    force: bool,
) -> Result<(), Failure> {
    // Fetched first so the prompt can name what is about to go, and say what
    // happens to its sub-issues — the one thing about deleting that surprises
    // people. `--force` skips both the prompt and the request.
    if !force && std::io::stdin().is_terminal() {
        let issue: IssueJson = decode(&ok(client.send(&operations::get(id))?)?)?;
        let released = match issue.sub_issue_ids.len() {
            0 => String::new(),
            1 => " Its 1 sub-issue will be kept, no longer part of anything.".to_string(),
            many => format!(" Its {many} sub-issues will be kept, no longer part of anything."),
        };
        eprintln!(
            "#{} \u{201c}{}\u{201d} will be erased.{released}\n\
             To abandon it but keep the record, use `issuers-cli set {} --status Cancelled`.",
            issue.id, issue.title, issue.id
        );
        eprint!("Delete it? [y/N] ");
        std::io::stderr().flush().ok();

        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|err| Failure::failed(format!("reading your answer: {err}")))?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
            return Err(Failure::failed("cancelled"));
        }
    } else if !force {
        return Err(Failure::usage(
            "deleting is permanent and this is not a terminal: pass --force to mean it",
        ));
    }

    ok(client.send(&operations::delete(id))?)?;
    writeln!(out, "Deleted #{id}.").map_err(broken_pipe)
}

/// Renders one Issue with its neighbours named rather than numbered.
///
/// At most three requests: the Issue itself, its parent, and — thanks to
/// `?parent=`, added for exactly this — all of its children in one go, rather
/// than one request per child.
fn present(client: &Client, out: &mut dyn WriteColor, issue: &IssueJson) -> Result<(), Failure> {
    let parent = match issue.parent_id {
        Some(parent) => Some(decode::<IssueJson>(&ok(
            client.send(&operations::get(parent))?
        )?)?),
        None => None,
    };
    let children: Vec<IssueJson> = if issue.sub_issue_ids.is_empty() {
        Vec::new()
    } else {
        let children = Narrowing {
            parent: Some(ParentFilter::Under(issue.id)),
            ..Default::default()
        };
        decode(&ok(client.send(&operations::list(&children))?)?)?
    };

    render::show(out, issue, parent.as_ref(), &children).map_err(broken_pipe)
}

// ---- request and response plumbing ------------------------------------------

/// The command line's filters, as the narrowing every surface shares.
///
/// `--status` absent is not a filter to remember later: it is the View that
/// admits everything.
fn narrowing(filters: Filters) -> Narrowing {
    Narrowing {
        view: match filters.status {
            Some(status) => View::WithStatus(status),
            None => View::All,
        },
        tag: filters.tag,
        title: filters.search,
        parent: filters.parent,
    }
}

/// Stdin is resolved here rather than inside `operations`, so that building a
/// request stays a pure function.
fn new_issue(new: NewIssue) -> Result<NewIssueBody, Failure> {
    Ok(NewIssueBody {
        title: new.title,
        parent_id: new.parent,
        rest: PatchIssue {
            body: new.body.map(read_body).transpose()?,
            status: new.status.map(|status| status.label().to_string()),
            priority: new.priority.map(|priority| priority.label().to_string()),
            // An empty `--tag` list means "say nothing about tags", not
            // "clear them" — clearing is what `tag rm` is for.
            tags: match new.tags.is_empty() {
                true => None,
                false => Some(new.tags.iter().map(|tag| tag.as_str().to_owned()).collect()),
            },
            size: new.size.map(Some),
            ..Default::default()
        },
        ..Default::default()
    })
}

fn patch(changes: Changes) -> Result<PatchIssue, Failure> {
    Ok(PatchIssue {
        title: changes.title,
        body: changes.body.map(read_body).transpose()?,
        status: changes.status.map(|status| status.label().to_string()),
        priority: changes
            .priority
            .map(|priority| priority.label().to_string()),
        size: changes.size,
        ..Default::default()
    })
}

fn read_body(source: Body) -> Result<String, Failure> {
    match source {
        Body::Text(text) => Ok(text),
        Body::Stdin => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|err| Failure::failed(format!("reading the body from stdin: {err}")))?;
            Ok(text)
        }
    }
}

/// Turns a non-2xx reply into a failure carrying the API's own sentence,
/// including the 409 refusals the domain rules produce.
fn ok(reply: Reply) -> Result<Reply, Failure> {
    if (200..300).contains(&reply.status) {
        Ok(reply)
    } else {
        Err(Failure::failed(reply.error_message()))
    }
}

fn decode<T: serde::de::DeserializeOwned>(reply: &Reply) -> Result<T, Failure> {
    serde_json::from_slice(&reply.body)
        .map_err(|err| Failure::failed(format!("could not read the API's response: {err}")))
}

fn emit<T: serde::Serialize>(out: &mut dyn WriteColor, value: &T) -> Result<(), Failure> {
    let text = serde_json::to_string(value)
        .map_err(|err| Failure::failed(format!("could not write JSON: {err}")))?;
    writeln!(out, "{text}").map_err(broken_pipe)
}

/// Passes the API's bytes through untouched, so `--json` is exactly what the
/// API said rather than a re-serialisation of it.
fn raw(out: &mut dyn WriteColor, reply: &Reply) -> Result<(), Failure> {
    out.write_all(&reply.body).map_err(broken_pipe)?;
    if !reply.body.ends_with(b"\n") {
        writeln!(out).map_err(broken_pipe)?;
    }
    Ok(())
}

/// `issuers-cli list | head` closes the pipe early. That is the reader's decision,
/// not an error worth reporting.
fn broken_pipe(err: std::io::Error) -> Failure {
    if err.kind() == std::io::ErrorKind::BrokenPipe {
        std::process::exit(0);
    }
    Failure::failed(format!("writing output: {err}"))
}

/// termcolor's own `Auto` inspects `TERM` but not whether stdout is a
/// terminal, so the decision is made here.
fn stream(colour: Colour) -> StandardStream {
    let choice = match colour {
        Colour::Always => ColorChoice::Always,
        Colour::Never => ColorChoice::Never,
        Colour::Auto => {
            if std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
                ColorChoice::Auto
            } else {
                ColorChoice::Never
            }
        }
    };
    StandardStream::stdout(choice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Tag, View};

    fn tag(name: &str) -> Tag {
        name.parse().expect("a usable tag")
    }

    /// The help text is built from `Status::ALL`/`Priority::ALL` because a
    /// hardcoded list had already drifted from the domain once — it offered a
    /// priority called "Normal", which has never existed.
    #[test]
    fn the_help_text_offers_exactly_what_the_domain_accepts() {
        let help = help();
        for status in Status::ALL {
            assert!(help.contains(status.label()), "{}", status.label());
        }
        for priority in Priority::ALL {
            assert!(help.contains(priority.label()), "{}", priority.label());
        }
        assert!(!help.contains("Normal"), "the label that never existed");
    }

    #[test]
    fn an_absent_status_filter_is_the_view_that_admits_everything() {
        let narrowed = narrowing(Filters::default());
        assert_eq!(narrowed.view, View::All);
        assert_eq!(narrowed, Narrowing::default());
    }

    #[test]
    fn each_filter_reaches_the_narrowing() {
        let narrowed = narrowing(Filters {
            status: Some(Status::Doing),
            tag: Some(tag("ui")),
            search: Some("flash".into()),
            parent: Some(ParentFilter::Unparented),
        });

        assert_eq!(narrowed.view, View::WithStatus(Status::Doing));
        assert_eq!(narrowed.tag, Some(tag("ui")));
        assert_eq!(narrowed.title.as_deref(), Some("flash"));
        assert_eq!(narrowed.parent, Some(ParentFilter::Unparented));
    }

    #[test]
    fn a_new_issue_carries_only_what_was_asked_for() {
        let body = new_issue(NewIssue {
            title: "Ship it".into(),
            body: Some(Body::Text("notes".into())),
            status: Some(Status::Doing),
            priority: Some(Priority::High),
            tags: vec![tag("ui")],
            parent: Some(3),
            size: Some(5),
        })
        .expect("no stdin needed");

        assert_eq!(body.title, "Ship it");
        assert_eq!(body.parent_id, Some(3));
        assert_eq!(body.rest.body.as_deref(), Some("notes"));
        assert_eq!(body.rest.status.as_deref(), Some("Doing"));
        assert_eq!(body.rest.priority.as_deref(), Some("High"));
        assert_eq!(body.rest.tags, Some(vec!["ui".to_string()]));
        assert_eq!(body.rest.size, Some(Some(5)));
    }

    #[test]
    fn no_tags_says_nothing_about_tags_rather_than_clearing_them() {
        // `PATCH {tags: []}` clears the set, so an absent `--tag` must not
        // serialise as an empty list.
        let body = new_issue(NewIssue {
            title: "Bare".into(),
            body: None,
            status: None,
            priority: None,
            tags: Vec::new(),
            parent: None,
            size: None,
        })
        .expect("no stdin needed");

        assert_eq!(body.rest.tags, None);
        assert_eq!(body.rest.status, None);
        assert_eq!(body.rest.size, None, "absent, not sized zero");
        assert_eq!(body.parent_id, None);
    }

    #[test]
    fn a_patch_names_only_the_fields_that_were_given() {
        let changed = patch(Changes {
            title: None,
            body: None,
            status: Some(Status::Cancelled),
            priority: None,
            size: None,
        })
        .expect("no stdin needed");

        assert_eq!(changed.status.as_deref(), Some("Cancelled"));
        assert_eq!(changed.title, None);
        assert_eq!(changed.body, None);
        assert_eq!(changed.priority, None);
        // The field that exists only to be rejected must never be sent.
        assert!(changed.parent_id.is_none());
    }

    #[test]
    fn setting_a_size_and_clearing_one_are_different_patches() {
        let sized = patch(Changes {
            size: Some(Some(8)),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(serde_json::to_string(&sized).unwrap(), r#"{"size":8}"#);

        let cleared = patch(Changes {
            size: Some(None),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            serde_json::to_string(&cleared).unwrap(),
            r#"{"size":null}"#,
            "`--size none` has to reach the wire as an explicit null"
        );
    }

    #[test]
    fn an_empty_change_set_produces_an_empty_patch() {
        let changed = patch(Changes::default()).expect("no stdin needed");
        assert_eq!(serde_json::to_string(&changed).unwrap(), "{}");
    }

    #[test]
    fn a_literal_body_needs_no_stdin() {
        assert_eq!(
            read_body(Body::Text("written out".into())).unwrap(),
            "written out"
        );
    }

    #[test]
    fn the_exit_codes_are_the_ones_a_wrapper_branches_on() {
        assert_eq!(Failure::usage("x").code(), 2);
        assert_eq!(Failure::failed("x").code(), 1);
        assert_eq!(Failure::not_running("x").code(), 3);
        assert_eq!(Failure::failed("why").message(), "why");

        // The client knows only whether there was an app to talk to; the exit
        // code is the CLI's business.
        assert_eq!(
            Failure::from(ClientError::NotRunning("gone".into())),
            Failure::NotRunning("gone".into())
        );
        assert_eq!(
            Failure::from(ClientError::Failed("broke".into())),
            Failure::Failed("broke".into())
        );
    }
}
