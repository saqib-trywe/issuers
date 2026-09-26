// SPDX-License-Identifier: GPL-3.0-only

//! `argv` in, a [`Command`] out. No I/O, so every rule here is a unit test.
//!
//! Enum values are parsed here rather than passed through as text: the CLI and
//! the API share one `FromStr`, so validating early cannot disagree with the
//! server, and a typo becomes a usage error instead of a round-trip.

use std::ffi::OsString;

use crate::domain::{IssueId, ParentFilter, Priority, Size, Status, Tag};

use super::Failure;

/// When to colour output. `auto` means "only when stdout is a terminal".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    Auto,
    Always,
    Never,
}

/// Where a body comes from. Reading is deferred so parsing stays pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Text(String),
    /// `--body -`, the form that lets the CLI compose with other commands.
    Stdin,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Filters {
    pub status: Option<Status>,
    pub tag: Option<Tag>,
    pub parent: Option<ParentFilter>,
    pub search: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct NewIssue {
    pub title: String,
    pub body: Option<Body>,
    pub status: Option<Status>,
    pub priority: Option<Priority>,
    /// Repeatable. Safe at creation only, where there is nothing to clobber —
    /// see the note on [`Changes`].
    pub tags: Vec<Tag>,
    pub parent: Option<IssueId>,
    /// No `none` here: at creation, absent already means unsized.
    pub size: Option<Size>,
}

/// The fields `issue set` may change.
///
/// Deliberately without Tags. `PATCH {tags:[…]}` replaces the whole set, so
/// changing one Tag through it means read-modify-write; the API grew
/// `PUT`/`DELETE /issues/{id}/tags/{name}` precisely to avoid that, and
/// `issue tag add|rm` is the only way in.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub title: Option<String>,
    pub body: Option<Body>,
    pub status: Option<Status>,
    pub priority: Option<Priority>,
    /// Three states, as on the wire: absent leaves the Size alone,
    /// `--size none` clears it, `--size 5` sets it.
    pub size: Option<Option<Size>>,
}

impl Changes {
    /// Every field, so that adding one and forgetting this cannot turn a real
    /// change into "nothing to set" — which is what happened when `--size`
    /// arrived: `--size 0` and `--size none` were both refused as empty.
    pub fn is_empty(&self) -> bool {
        let Changes {
            title,
            body,
            status,
            priority,
            size,
        } = self;
        title.is_none()
            && body.is_none()
            && status.is_none()
            && priority.is_none()
            && size.is_none()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    List(Filters),
    Show(IssueId),
    New(Box<NewIssue>),
    Set(IssueId, Box<Changes>),
    Remove {
        id: IssueId,
        force: bool,
    },
    Tags,
    Tag {
        id: IssueId,
        names: Vec<Tag>,
        add: bool,
    },
    Sub {
        id: IssueId,
        children: Vec<IssueId>,
        add: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Invocation {
    pub json: bool,
    pub colour: Colour,
    pub command: Command,
}

/// Reads `--color` alone, without validating anything else.
///
/// [`run`](super::run) needs the colour choice before parsing can fail, so
/// that a usage error is written to a stream configured the way it was asked
/// for. An unusable value falls back to `auto` and is reported by `parse`.
pub fn colour_of(argv: &[OsString]) -> Colour {
    let mut words = argv.iter().map(|word| word.to_string_lossy());
    while let Some(word) = words.next() {
        let value = match word.strip_prefix("--color=") {
            Some(value) => value.to_string(),
            None if word == "--color" => match words.next() {
                Some(value) => value.into_owned(),
                None => return Colour::Auto,
            },
            None => continue,
        };
        return match value.as_str() {
            "always" => Colour::Always,
            "never" => Colour::Never,
            _ => Colour::Auto,
        };
    }
    Colour::Auto
}

pub fn parse(argv: Vec<OsString>) -> Result<Invocation, Failure> {
    let mut args = pico_args::Arguments::from_vec(argv);

    // Read anywhere in the line, so both `issue --json list` and
    // `issue list --json` work.
    let json = args.contains("--json");
    let colour = match value(&mut args, "--color")? {
        None => Colour::Auto,
        Some(word) => match word.as_str() {
            "auto" => Colour::Auto,
            "always" => Colour::Always,
            "never" => Colour::Never,
            other => {
                return Err(Failure::usage(format!(
                    "--color takes auto, always or never: {other}"
                )));
            }
        },
    };

    if args.contains(["-h", "--help"]) {
        return Ok(Invocation {
            json,
            colour,
            command: Command::Help,
        });
    }
    if args.contains(["-V", "--version"]) {
        return Ok(Invocation {
            json,
            colour,
            command: Command::Version,
        });
    }

    let verb = args
        .subcommand()
        .map_err(|err| Failure::usage(err.to_string()))?;
    let Some(verb) = verb else {
        return Ok(Invocation {
            json,
            colour,
            command: Command::Help,
        });
    };

    let command = match verb.as_str() {
        "list" => list(args)?,
        "show" => Command::Show(one_id(args, "show")?),
        "new" => new(args)?,
        "set" => set(args)?,
        "rm" => remove(args)?,
        "tags" => {
            rest(args, 0, "tags")?;
            Command::Tags
        }
        "tag" => tag(args)?,
        "sub" => sub(args)?,
        "help" => Command::Help,
        other => {
            return Err(Failure::usage(format!(
                "no such command: {other}. Try `issue --help`"
            )));
        }
    };

    Ok(Invocation {
        json,
        colour,
        command,
    })
}

// ---- per-command parsing ----------------------------------------------------

fn list(mut args: pico_args::Arguments) -> Result<Command, Failure> {
    let status = value(&mut args, "--status")?
        .map(|raw| parse_as::<Status>(&raw))
        .transpose()?;
    let tag = value(&mut args, "--tag")?
        .map(|raw| parse_as::<Tag>(&raw))
        .transpose()?;
    let search = value(&mut args, "--search")?;
    let parent = value(&mut args, "--parent")?
        .map(|raw| parse_as::<ParentFilter>(&raw))
        .transpose()?;
    rest(args, 0, "list")?;

    Ok(Command::List(Filters {
        status,
        tag,
        parent,
        search,
    }))
}

fn new(mut args: pico_args::Arguments) -> Result<Command, Failure> {
    let body = body(&mut args)?;
    let status = value(&mut args, "--status")?
        .map(|raw| parse_as::<Status>(&raw))
        .transpose()?;
    let priority = value(&mut args, "--priority")?
        .map(|raw| parse_as::<Priority>(&raw))
        .transpose()?;
    let tags = args
        .values_from_str::<_, String>("--tag")
        .map_err(|err| Failure::usage(err.to_string()))?
        .iter()
        .map(|raw| parse_as::<Tag>(raw))
        .collect::<Result<Vec<_>, _>>()?;
    let parent = value(&mut args, "--parent")?
        .map(|raw| id_from(&raw))
        .transpose()?;
    let size = value(&mut args, "--size")?
        .map(|raw| size_from(&raw))
        .transpose()?;

    let mut free = rest(args, 1, "new")?;
    let title = free.remove(0);
    if title.trim().is_empty() {
        return Err(Failure::usage("a title is required: issue new \"Title\""));
    }

    Ok(Command::New(Box::new(NewIssue {
        title,
        body,
        status,
        priority,
        tags,
        parent,
        size,
    })))
}

fn set(mut args: pico_args::Arguments) -> Result<Command, Failure> {
    let title = value(&mut args, "--title")?;
    // The API would refuse it too; saying so here makes it a bad command line
    // (exit 2), as it is for `issue new`, rather than a failed request.
    if title
        .as_deref()
        .is_some_and(|title| title.trim().is_empty())
    {
        return Err(Failure::usage("--title must not be blank"));
    }
    let body = body(&mut args)?;
    let status = value(&mut args, "--status")?
        .map(|raw| parse_as::<Status>(&raw))
        .transpose()?;
    let priority = value(&mut args, "--priority")?
        .map(|raw| parse_as::<Priority>(&raw))
        .transpose()?;
    // `none` unsizes, matching `--parent none`. Absent leaves it alone.
    let size = match value(&mut args, "--size")? {
        None => None,
        Some(raw) if raw == "none" => Some(None),
        Some(raw) => Some(Some(size_from(&raw)?)),
    };
    let id = one_id(args, "set")?;

    let changes = Changes {
        title,
        body,
        status,
        priority,
        size,
    };
    // An empty PATCH is a valid no-op to the API, so it would exit 0 having
    // done nothing that was asked for. That is worth refusing.
    if changes.is_empty() {
        return Err(Failure::usage(
            "nothing to set. Give at least one of --title, --body, --status, \
             --priority, --size",
        ));
    }

    Ok(Command::Set(id, Box::new(changes)))
}

fn remove(mut args: pico_args::Arguments) -> Result<Command, Failure> {
    let force = args.contains(["-f", "--force"]);
    Ok(Command::Remove {
        id: one_id(args, "rm")?,
        force,
    })
}

fn tag(mut args: pico_args::Arguments) -> Result<Command, Failure> {
    let add = add_or_remove(&mut args, "tag")?;
    let mut free = rest(args, 2, "tag")?;
    let id = id_from(&free.remove(0))?;
    let names = free
        .iter()
        .map(|raw| parse_as::<Tag>(raw))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Command::Tag { id, names, add })
}

fn sub(mut args: pico_args::Arguments) -> Result<Command, Failure> {
    let add = add_or_remove(&mut args, "sub")?;
    let mut free = rest(args, 2, "sub")?;
    let id = id_from(&free.remove(0))?;
    let children = free
        .iter()
        .map(|raw| id_from(raw))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Command::Sub { id, children, add })
}

fn add_or_remove(args: &mut pico_args::Arguments, verb: &str) -> Result<bool, Failure> {
    match args
        .subcommand()
        .map_err(|err| Failure::usage(err.to_string()))?
    {
        Some(word) if word == "add" => Ok(true),
        Some(word) if word == "rm" => Ok(false),
        Some(other) => Err(Failure::usage(format!(
            "{verb} takes add or rm, not {other}"
        ))),
        None => Err(Failure::usage(format!("{verb} takes add or rm"))),
    }
}

// ---- shared pieces ----------------------------------------------------------

fn value(args: &mut pico_args::Arguments, key: &'static str) -> Result<Option<String>, Failure> {
    args.opt_value_from_str::<_, String>(key)
        .map_err(|err| Failure::usage(err.to_string()))
}

fn body(args: &mut pico_args::Arguments) -> Result<Option<Body>, Failure> {
    Ok(value(args, "--body")?.map(|raw| match raw.as_str() {
        "-" => Body::Stdin,
        _ => Body::Text(raw),
    }))
}

fn parse_as<T: std::str::FromStr>(raw: &str) -> Result<T, Failure>
where
    T::Err: std::fmt::Display,
{
    raw.parse().map_err(|err| Failure::usage(format!("{err}")))
}

/// A Size, refused rather than clamped when it will not fit.
///
/// The ceiling is the point: work that does not fit in 255 is not one large
/// Issue, it is a tree of Issues nobody has written down yet.
fn size_from(raw: &str) -> Result<Size, Failure> {
    raw.parse().map_err(|_| {
        Failure::usage(format!(
            "a size is a whole number from 0 to {}: {raw}",
            Size::MAX
        ))
    })
}

fn id_from(raw: &str) -> Result<IssueId, Failure> {
    raw.parse()
        .map_err(|_| Failure::usage(format!("not an issue id: {raw}")))
}

fn one_id(args: pico_args::Arguments, verb: &str) -> Result<IssueId, Failure> {
    let free = rest(args, 1, verb)?;
    id_from(&free[0])
}

/// Collects the positional arguments, insisting on exactly `wanted`.
///
/// This is also where unrecognised flags are caught. `pico-args` leaves
/// anything it was never asked for in the remainder and is otherwise happy, so
/// without this `issue set 7 --statuss Done` would report success and change
/// nothing.
fn rest(args: pico_args::Arguments, wanted: usize, verb: &str) -> Result<Vec<String>, Failure> {
    let mut free = Vec::new();
    for raw in args.finish() {
        let word = raw.to_string_lossy().into_owned();
        if word.starts_with('-') && word != "-" {
            return Err(Failure::usage(format!("unknown option for {verb}: {word}")));
        }
        free.push(word);
    }

    // Variadic commands take a minimum; the rest take an exact count.
    let variadic = matches!(verb, "tag" | "sub");
    let short = free.len() < wanted;
    let long = !variadic && free.len() > wanted;
    if short || long {
        return Err(Failure::usage(format!(
            "{verb} wants {wanted} argument(s), got {}. Try `issue --help`",
            free.len()
        )));
    }
    Ok(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_words(line: &str) -> Result<Invocation, Failure> {
        parse(line.split_whitespace().map(OsString::from).collect())
    }

    fn command(line: &str) -> Command {
        parse_words(line).expect(line).command
    }

    fn usage(line: &str) -> String {
        match parse_words(line) {
            Err(Failure::Usage(message)) => message,
            other => panic!("{line} should have been a usage error, got {other:?}"),
        }
    }

    #[test]
    fn no_arguments_shows_the_help() {
        assert_eq!(command(""), Command::Help);
        assert_eq!(command("--help"), Command::Help);
        assert_eq!(command("help"), Command::Help);
    }

    #[test]
    fn global_flags_are_read_on_either_side_of_the_command() {
        assert!(parse_words("--json list").unwrap().json);
        assert!(parse_words("list --json").unwrap().json);
        assert_eq!(
            parse_words("list --color never").unwrap().colour,
            Colour::Never
        );
        assert_eq!(parse_words("list").unwrap().colour, Colour::Auto);
        assert!(usage("list --color mauve").contains("mauve"));
    }

    #[test]
    fn list_filters_are_parsed_into_domain_values() {
        let Command::List(filters) = command("list --status doing --tag Bug --search theme") else {
            panic!("expected a list");
        };
        // Case-folded on the way in, so the request carries the canonical name.
        assert_eq!(filters.status, Some(Status::Doing));
        assert_eq!(filters.tag, Some("bug".parse().unwrap()));
        assert_eq!(filters.search.as_deref(), Some("theme"));
        assert_eq!(filters.parent, None);
    }

    #[test]
    fn parent_takes_an_id_or_the_word_none() {
        let Command::List(filters) = command("list --parent 7") else {
            panic!()
        };
        assert_eq!(filters.parent, Some(ParentFilter::Under(7)));

        let Command::List(filters) = command("list --parent none") else {
            panic!()
        };
        assert_eq!(filters.parent, Some(ParentFilter::Unparented));

        assert!(usage("list --parent all").contains("all"));
    }

    #[test]
    fn an_unknown_option_is_refused_rather_than_ignored() {
        // The whole reason `rest` exists: pico-args is content to leave an
        // option it was never asked about lying in the remainder.
        assert!(usage("set 7 --statuss Done").contains("--statuss"));
        assert!(usage("list --bogus").contains("--bogus"));
        assert!(usage("show 7 --force").contains("--force"));
    }

    #[test]
    fn an_unknown_status_or_priority_is_a_usage_error_not_a_round_trip() {
        assert!(usage("list --status Closed").contains("Closed"));
        assert!(usage("new t --priority Critical").contains("Critical"));
    }

    #[test]
    fn setting_nothing_is_refused() {
        // The API would accept the empty patch and report success.
        assert!(usage("set 7").contains("nothing to set"));
    }

    #[test]
    fn set_changes_only_the_fields_it_names() {
        let Command::Set(id, changes) = command("set 7 --status done") else {
            panic!()
        };
        assert_eq!(id, 7);
        assert_eq!(changes.status, Some(Status::Done));
        assert_eq!(changes.title, None);
        assert_eq!(changes.body, None);
    }

    #[test]
    fn a_body_may_come_from_stdin() {
        // A change that only touches the Size is still a change. `--size 0`
        // sets it to zero, which is a real Size, and `--size none` unsizes it.
        let Command::Set(_, changes) = command("set 7 --size 0") else {
            panic!("expected a set")
        };
        assert_eq!(changes.size, Some(Some(0)));

        let Command::Set(_, changes) = command("set 7 --size none") else {
            panic!("expected a set")
        };
        assert_eq!(changes.size, Some(None));

        let Command::Set(_, changes) = command("set 7 --status done") else {
            panic!("expected a set")
        };
        assert_eq!(changes.size, None, "absent leaves the size alone");

        assert!(usage("set 7 --size 256").contains("255"));
        assert!(usage("set 7 --size half").contains("half"));

        let Command::New(new) = command("new title --size 4") else {
            panic!("expected a new")
        };
        assert_eq!(new.size, Some(4));
        // At creation there is no `none`: absent already means unsized.
        assert!(usage("new title --size none").contains("none"));

        let Command::New(new) = command("new title --body -") else {
            panic!()
        };
        assert_eq!(new.body, Some(Body::Stdin));

        let Command::New(new) = parse(
            ["new", "title", "--body", "some prose"]
                .iter()
                .map(OsString::from)
                .collect(),
        )
        .unwrap()
        .command
        else {
            panic!()
        };
        assert_eq!(new.body, Some(Body::Text("some prose".into())));
    }

    #[test]
    fn new_takes_repeatable_tags_but_set_takes_none() {
        let Command::New(new) = command("new title --tag bug --tag ui") else {
            panic!()
        };
        assert_eq!(new.tags.len(), 2);
        // `set --tag` would mean read-modify-write; `issue tag` exists instead.
        assert!(usage("set 7 --tag bug").contains("--tag"));
    }

    #[test]
    fn a_blank_title_is_refused_before_the_request() {
        assert!(usage("new").contains("wants 1"));
        assert!(parse(["new", "   "].iter().map(OsString::from).collect()).is_err());
        let blank = parse(
            ["set", "7", "--title", "   "]
                .iter()
                .map(OsString::from)
                .collect(),
        );
        assert!(matches!(blank, Err(Failure::Usage(message)) if message.contains("blank")));
    }

    #[test]
    fn rm_needs_force_only_as_a_flag_here() {
        assert_eq!(
            command("rm 7"),
            Command::Remove {
                id: 7,
                force: false
            }
        );
        assert_eq!(
            command("rm 7 --force"),
            Command::Remove { id: 7, force: true }
        );
        assert_eq!(command("rm 7 -f"), Command::Remove { id: 7, force: true });
    }

    #[test]
    fn tag_and_sub_take_several_at_once() {
        assert_eq!(
            command("tag add 7 bug ui"),
            Command::Tag {
                id: 7,
                names: vec!["bug".parse().unwrap(), "ui".parse().unwrap()],
                add: true
            }
        );
        assert_eq!(
            command("sub rm 7 12 13"),
            Command::Sub {
                id: 7,
                children: vec![12, 13],
                add: false
            }
        );
        assert!(usage("tag 7 bug").contains("add or rm"));
        assert!(usage("tag toggle 7 bug").contains("toggle"));
    }

    #[test]
    fn a_non_numeric_id_is_caught_here() {
        assert!(usage("show seven").contains("seven"));
        assert!(usage("sub add 7 twelve").contains("twelve"));
    }

    #[test]
    fn an_unknown_command_names_itself() {
        // `close` in particular: the glossary rules the word out, so the
        // suggestion has to be the help rather than a silent alias.
        assert!(usage("close 7").contains("close"));
    }

    #[test]
    fn too_many_positionals_are_refused() {
        assert!(usage("show 7 8").contains("wants 1"));
        assert!(usage("tags extra").contains("wants 0"));
    }
}
