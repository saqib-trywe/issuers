// SPDX-License-Identifier: GPL-3.0-only

//! The arguments each tool takes, and the schemas an agent reads to learn them.
//!
//! Arguments are an MCP concern with no counterpart in the HTTP API, so they
//! are defined here rather than in the library. What is *not* redefined here
//! is the domain: [`StatusArg`] and [`PriorityArg`] build their schemas from
//! `Status::ALL` and `Priority::ALL`, so the values an agent is offered cannot
//! drift from the values the tracker accepts. A hardcoded list had already
//! drifted once, in the CLI's help text.

use std::str::FromStr;

use issuers::domain::{IssueId, Priority, Size, Status};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Deserialize;
use serde::de::{Error as _, Unexpected};

fn list(labels: &[&str]) -> String {
    labels.join(", ")
}

/// Builds a string schema whose `enum` is the domain's own list of labels.
fn labelled(labels: &[&str], description: &str) -> Schema {
    let json = serde_json::json!({
        "type": "string",
        "enum": labels,
        "description": description,
    });
    Schema::from(
        json.as_object()
            .expect("a JSON object was just built")
            .clone(),
    )
}

/// A Status, parsed the way every other entry point parses one.
#[derive(Debug, Clone, Copy)]
pub struct StatusArg(pub Status);

impl<'de> Deserialize<'de> for StatusArg {
    fn deserialize<D: serde::Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(input)?;
        Status::from_str(&raw).map(StatusArg).map_err(|_| {
            let expected: &str = &format!("one of {}", list(&Status::ALL.map(Status::label)));
            D::Error::invalid_value(Unexpected::Str(&raw), &expected)
        })
    }
}

impl JsonSchema for StatusArg {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Status".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        let labels: Vec<&str> = Status::ALL.iter().map(|status| status.label()).collect();
        labelled(&labels, "Where an issue sits in its lifecycle.")
    }
}

/// A Priority, likewise.
#[derive(Debug, Clone, Copy)]
pub struct PriorityArg(pub Priority);

impl<'de> Deserialize<'de> for PriorityArg {
    fn deserialize<D: serde::Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(input)?;
        Priority::from_str(&raw).map(PriorityArg).map_err(|_| {
            let expected: &str = &format!("one of {}", list(&Priority::ALL.map(Priority::label)));
            D::Error::invalid_value(Unexpected::Str(&raw), &expected)
        })
    }
}

impl JsonSchema for PriorityArg {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Priority".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        let labels: Vec<&str> = Priority::ALL
            .iter()
            .map(|priority| priority.label())
            .collect();
        labelled(&labels, "How much an issue matters relative to others.")
    }
}

/// Tells an absent key apart from an explicit null, so an agent can unsize an
/// issue as well as size it. See `api::wire` for the same helper on the wire.
fn present_but_maybe_null<'de, D, T>(input: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(input).map(Some)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListIssues {
    /// Only issues with this status.
    pub status: Option<StatusArg>,
    /// Only issues carrying this tag. Tag names ignore case.
    pub tag: Option<String>,
    /// Either "none", for issues that are not sub-issues of anything, or the
    /// id of an issue, for that issue's sub-issues.
    pub parent: Option<String>,
    /// Only issues whose title contains this text. Titles only — bodies and
    /// tags are not searched.
    pub search: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetIssue {
    /// The id of the issue to read.
    pub id: IssueId,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateIssue {
    /// A one-line summary of the work. Required.
    pub title: String,
    /// Free text. Notes, reproduction steps, whatever is worth keeping.
    pub body: Option<String>,
    /// Defaults to Todo.
    pub status: Option<StatusArg>,
    /// Defaults to None.
    pub priority: Option<PriorityArg>,
    /// Tags to attach. Accepted here because at creation there is nothing to
    /// clobber; afterwards use add_tag and remove_tag.
    pub tags: Option<Vec<String>>,
    /// How big this work is, relative to other issues: a whole number from 0
    /// to 255. 0 means no work; leaving it out means not decided yet. Work
    /// that will not fit in 255 should be several issues instead.
    pub size: Option<Size>,
    /// File this as a sub-issue of an existing issue. Accepted here because
    /// at creation "absent" unambiguously means "no parent"; afterwards use
    /// add_sub_issue.
    pub parent_id: Option<IssueId>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateIssue {
    /// The id of the issue to change.
    pub id: IssueId,
    /// A field left out is left alone.
    pub title: Option<String>,
    pub body: Option<String>,
    pub status: Option<StatusArg>,
    pub priority: Option<PriorityArg>,
    /// A whole number from 0 to 255 to size it, or null to make it unsized
    /// again. Leaving the field out entirely leaves the size alone — null and
    /// absent mean different things here.
    #[serde(default, deserialize_with = "present_but_maybe_null")]
    pub size: Option<Option<Size>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeTag {
    /// The issue to tag or untag.
    pub id: IssueId,
    /// The tag name. Case-insensitive, and may contain spaces or slashes —
    /// "ui/theme" is one flat name, not a tag inside another.
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeSubIssue {
    /// The issue that holds the sub-issue.
    pub parent_id: IssueId,
    /// The issue that is part of it.
    pub sub_issue_id: IssueId,
}

/// `list_tags` takes nothing, but MCP still wants a schema for it.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArguments {}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::schema_for;
    use serde_json::{from_value, json};

    #[test]
    fn a_status_is_read_the_way_every_other_entry_point_reads_one() {
        let arg: StatusArg = from_value(json!("doing")).expect("case folds");
        assert_eq!(arg.0, Status::Doing);
        assert_eq!(
            from_value::<StatusArg>(json!("CANCELLED")).unwrap().0,
            Status::Cancelled
        );
    }

    #[test]
    fn an_unknown_status_names_the_ones_that_exist() {
        let err = from_value::<StatusArg>(json!("Wontfix")).expect_err("not a status");
        let message = err.to_string();
        assert!(message.contains("Wontfix"), "{message}");
        for status in Status::ALL {
            assert!(message.contains(status.label()), "{message}");
        }
    }

    #[test]
    fn an_unknown_priority_does_the_same() {
        let err = from_value::<PriorityArg>(json!("Critical")).expect_err("not a priority");
        let message = err.to_string();
        assert!(message.contains("Critical"), "{message}");
        assert!(message.contains("Urgent"), "{message}");
    }

    /// The schema is built from `Status::ALL`, so what an agent is offered
    /// cannot drift from what the tracker accepts.
    #[test]
    fn the_schema_offers_the_domains_own_list() {
        let schema = serde_json::to_value(schema_for!(StatusArg)).unwrap();
        let offered: Vec<String> = from_value(schema["enum"].clone()).expect("an enum");
        let domain: Vec<String> = Status::ALL
            .iter()
            .map(|status| status.label().to_string())
            .collect();
        assert_eq!(offered, domain);

        let schema = serde_json::to_value(schema_for!(PriorityArg)).unwrap();
        let offered: Vec<String> = from_value(schema["enum"].clone()).expect("an enum");
        let domain: Vec<String> = Priority::ALL
            .iter()
            .map(|priority| priority.label().to_string())
            .collect();
        assert_eq!(offered, domain);
    }

    /// Without `deny_unknown_fields`, `update_issue` with a `tags` field would
    /// report success and change nothing — the trap `pico-args` sets for the
    /// command line, in a different costume.
    #[test]
    fn an_argument_we_were_never_asked_for_is_refused() {
        let err = from_value::<UpdateIssue>(json!({ "id": 1, "tags": ["ui"] }))
            .expect_err("tags is not patchable");
        assert!(err.to_string().contains("tags"), "{err}");

        let err =
            from_value::<UpdateIssue>(json!({ "id": 1, "statuss": "Done" })).expect_err("a typo");
        assert!(err.to_string().contains("statuss"), "{err}");

        let err = from_value::<ListIssues>(json!({ "limit": 10 })).expect_err("no limit exists");
        assert!(err.to_string().contains("limit"), "{err}");
    }

    #[test]
    fn every_listing_argument_is_optional() {
        let empty: ListIssues = from_value(json!({})).expect("all optional");
        assert!(empty.status.is_none());
        assert!(empty.tag.is_none());
        assert!(empty.parent.is_none());
        assert!(empty.search.is_none());

        // `list_tags` takes nothing at all, and must accept being sent nothing.
        from_value::<NoArguments>(json!({})).expect("no arguments");
    }

    #[test]
    fn creating_needs_only_a_title() {
        let minimal: CreateIssue = from_value(json!({ "title": "Ship it" })).expect("just a title");
        assert_eq!(minimal.title, "Ship it");
        assert!(minimal.parent_id.is_none());
        assert!(minimal.tags.is_none());

        from_value::<CreateIssue>(json!({})).expect_err("a title is required");
    }
}
