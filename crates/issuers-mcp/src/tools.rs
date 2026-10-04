// SPDX-License-Identifier: GPL-3.0-only

//! The nine tools an agent sees.
//!
//! One tool per domain operation, named as the domain names things rather
//! than as HTTP spells them. Results are the API's own JSON, passed through
//! untouched — the shape is already specified in `docs/openapi.yaml`, and
//! passing it through means the fields an agent reads are by construction the
//! fields the server sends.
//!
//! There is deliberately no `delete_issue`. See `docs/adr/0009`.

use issuers::api::wire::{NewIssue, PatchIssue};
use issuers::domain::{Narrowing, ParentFilter, Tag, View};
use issuers::operations;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities, ServerConfig,
};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use serde_json::{Value, json};

use crate::api;
use crate::args::*;

/// Sent to the agent once, at initialize, before any tool is called.
///
/// The rules below cannot be inferred from the schemas, and an agent that has
/// not been told them learns each one by being refused. Every sentence here
/// is the glossary's — if `CONTEXT.md` changes, this changes with it.
const INSTRUCTIONS: &str = "\
These tools read and change the issues in a single-user desktop tracker. You are
acting as the person who owns them, not as a separate participant: there are no
assignees, no reporters and no comments, because there is only ever one user.

Four rules are enforced by the tracker and will refuse your request if broken:

- Cancelling is not deleting. Cancelled records that work was deliberately
  abandoned, and is the right way to close something that will not be done.
  There is no tool for deleting an issue: deleting erases a mistake — a typo, a
  stray keystroke — and that judgement belongs to the person, not to you.
- Sub-issues go exactly one level deep. A sub-issue cannot have sub-issues of
  its own, and an issue that already holds sub-issues cannot become one.
- An issue cannot be marked Done while any of its sub-issues is outstanding,
  and a sub-issue cannot be reopened under a parent that is already Done. Done
  and Cancelled both count as settled. Cancelling a parent is always allowed.
- Tags exist only while an issue carries them, ignore case (Bug and bug are one
  tag), and are flat: \"ui/theme\" is a single name that happens to contain a
  slash, not a tag inside another.

Sizes are optional and relative: a whole number from 0 to 255 whose unit is
whatever you have been using. Nothing enforces the following, so it is on you to
get right — an issue holding sub-issues is sized by adding their sizes to its
own, so its own size means the work its parts do not cover, not the whole job.
Putting the whole job on the parent double-counts it. A size of 0 means no work;
leaving it unset means undecided, and setting it to null makes it undecided
again.

The tracker must be running for any of this to work. If it is not, every tool
will say so, and the remedy is for the person to open it.";

#[derive(Clone)]
pub struct Issues;

#[tool_router]
impl Issues {
    /// List issues, most pressing first. Bodies are omitted — use get_issue to
    /// read one. Every filter is optional and they combine.
    #[tool(annotations(read_only_hint = true))]
    async fn list_issues(&self, Parameters(args): Parameters<ListIssues>) -> CallToolResult {
        // `parent` arrives as a string because "none" is one of its values;
        // parsing it here means a typo is a tool error rather than a 400.
        let parent = match args.parent.as_deref().map(str::parse::<ParentFilter>) {
            Some(Ok(parent)) => Some(parent),
            Some(Err(err)) => return failed(format!("{err} — use an issue id or \"none\"")),
            None => None,
        };
        let narrowing = Narrowing {
            view: match args.status {
                Some(status) => View::WithStatus(status.0),
                None => View::All,
            },
            tag: args
                .tag
                .as_deref()
                .map(str::parse)
                .transpose()
                .ok()
                .flatten(),
            title: args.search,
            parent,
        };

        match api::send(operations::list(&narrowing)).await {
            // Listing is for finding; reading is for reading. A tracker's
            // bodies are the bulk of it, and an agent that wants one knows
            // the id by the time it does.
            Ok(issues) => ok(json!({ "issues": without_bodies(issues) })),
            Err(message) => failed(message),
        }
    }

    /// Read one issue in full, including its body, its tags, its parent and
    /// the ids of its sub-issues.
    #[tool(annotations(read_only_hint = true))]
    async fn get_issue(&self, Parameters(args): Parameters<GetIssue>) -> CallToolResult {
        respond(operations::get(args.id)).await
    }

    /// File a new issue. Only the title is required.
    #[tool]
    async fn create_issue(&self, Parameters(args): Parameters<CreateIssue>) -> CallToolResult {
        let new = NewIssue {
            title: args.title,
            parent_id: args.parent_id,
            rest: PatchIssue {
                body: args.body,
                status: args.status.map(|status| status.0.label().to_string()),
                priority: args.priority.map(|priority| priority.0.label().to_string()),
                tags: args.tags,
                size: args.size.map(Some),
                ..Default::default()
            },
            ..Default::default()
        };
        respond(operations::create(&new)).await
    }

    /// Change an issue's title, body, status or priority. A field left out is
    /// left alone. Tags and parentage are not changed here — a patch would
    /// replace the whole tag set, so add_tag, remove_tag, add_sub_issue and
    /// remove_sub_issue exist to change one thing at a time.
    #[tool]
    async fn update_issue(&self, Parameters(args): Parameters<UpdateIssue>) -> CallToolResult {
        let patch = PatchIssue {
            title: args.title,
            body: args.body,
            status: args.status.map(|status| status.0.label().to_string()),
            priority: args.priority.map(|priority| priority.0.label().to_string()),
            size: args.size,
            ..Default::default()
        };
        respond(operations::update(args.id, &patch)).await
    }

    /// Attach a tag to an issue. Adding a tag it already carries is not an
    /// error. If the name matches an existing tag in any case, the existing
    /// spelling is kept.
    #[tool(annotations(idempotent_hint = true))]
    async fn add_tag(&self, Parameters(args): Parameters<ChangeTag>) -> CallToolResult {
        match args.name.parse::<Tag>() {
            Ok(tag) => respond(operations::add_tag(args.id, &tag)).await,
            Err(err) => failed(err.to_string()),
        }
    }

    /// Remove a tag from an issue. Removing one it does not carry is not an
    /// error. A tag that no issue carries ceases to exist.
    #[tool(annotations(idempotent_hint = true))]
    async fn remove_tag(&self, Parameters(args): Parameters<ChangeTag>) -> CallToolResult {
        match args.name.parse::<Tag>() {
            Ok(tag) => respond(operations::remove_tag(args.id, &tag)).await,
            Err(err) => failed(err.to_string()),
        }
    }

    /// Make one issue a sub-issue of another. An issue that is already a
    /// sub-issue of something else is moved rather than copied.
    #[tool(annotations(idempotent_hint = true))]
    async fn add_sub_issue(&self, Parameters(args): Parameters<ChangeSubIssue>) -> CallToolResult {
        respond(operations::add_sub_issue(args.parent_id, args.sub_issue_id)).await
    }

    /// Detach a sub-issue from its parent. It survives as an ordinary issue.
    #[tool(annotations(idempotent_hint = true))]
    async fn remove_sub_issue(
        &self,
        Parameters(args): Parameters<ChangeSubIssue>,
    ) -> CallToolResult {
        respond(operations::remove_sub_issue(
            args.parent_id,
            args.sub_issue_id,
        ))
        .await
    }

    /// List every tag in use, with the number of issues carrying it.
    #[tool(annotations(read_only_hint = true))]
    async fn list_tags(&self, Parameters(_): Parameters<NoArguments>) -> CallToolResult {
        match api::send(operations::tags()).await {
            Ok(tags) => ok(json!({ "tags": tags })),
            Err(message) => failed(message),
        }
    }
}

#[tool_handler]
impl ServerHandler for Issues {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
        info.protocol_version = ProtocolVersion::LATEST;
        // `Implementation::from_build_env` would report the SDK's own name,
        // since its `env!` is expanded where the SDK was compiled.
        info.server_info = Implementation::new("issuers", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(INSTRUCTIONS.to_string());
        info
    }
}

// ---- helpers ----------------------------------------------------------------

async fn respond(call: operations::Call) -> CallToolResult {
    match api::send(call).await {
        Ok(value) => ok(value),
        Err(message) => failed(message),
    }
}

/// A successful result: structured content, with a text copy beside it for
/// clients that predate structured content.
fn ok(value: Value) -> CallToolResult {
    CallToolResult::structured(value)
}

/// A failure the agent is meant to read and act on, rather than a malfunction
/// — so a tool error, not a protocol error.
fn failed(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// Drops `body` from every issue in a list.
///
/// The one place the API's JSON is not passed through unchanged. A missing
/// field is visible to the agent and documented in the tool description,
/// whereas a truncated list would silently change the answer to "what is
/// blocked?" in a way it could not detect.
fn without_bodies(issues: Value) -> Value {
    match issues {
        Value::Array(issues) => Value::Array(
            issues
                .into_iter()
                .map(|mut issue| {
                    if let Value::Object(fields) = &mut issue {
                        fields.remove("body");
                    }
                    issue
                })
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_drops_bodies_and_nothing_else() {
        let issues = json!([{ "id": 1, "title": "t", "body": "long", "tags": [] }]);
        let stripped = without_bodies(issues);
        let issue = &stripped[0];
        assert!(issue.get("body").is_none());
        assert_eq!(issue["title"], "t");
        assert_eq!(issue["id"], 1);
    }
}
