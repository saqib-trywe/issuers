// SPDX-License-Identifier: GPL-3.0-only

//! Pure domain types.
//!
//! This module must not depend on `gpui` or `rusqlite`. Keeping it free of
//! both is what lets it move into its own crate later without a rewrite.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use chrono::{DateTime, Utc};

/// Issues are identified by a sequential integer, shown to the user as `#42`.
pub type IssueId = i64;

/// How big a piece of work is, relative to the others.
///
/// Unitless on purpose — points, hours or afternoons, whichever you meant.
/// `0` is a real answer meaning "no work", distinct from an absent Size, which
/// means "not decided yet".
///
/// `u8` is the rule rather than a check beside it: an Issue that will not fit
/// in 255 is not a large Issue, it is a tree of Issues that has not been
/// written down yet.
pub type Size = u8;

/// Returned when a `TEXT` column holds a value outside the known set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub kind: &'static str,
    pub value: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unrecognised {}: {:?}", self.kind, self.value)
    }
}

impl std::error::Error for ParseError {}

/// Where an Issue sits in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Status {
    #[default]
    Todo,
    Doing,
    Blocked,
    Done,
    Cancelled,
}

impl Status {
    pub const ALL: [Status; 5] = [
        Status::Todo,
        Status::Doing,
        Status::Blocked,
        Status::Done,
        Status::Cancelled,
    ];

    /// Whether this Status represents work that is no longer outstanding.
    ///
    /// Done and Cancelled both settle an Issue: one finished, the other was
    /// deliberately abandoned. Neither is waiting on anybody. This is the
    /// predicate a parent is measured against before it may be marked Done —
    /// blocking a parent on a Cancelled child would push you toward deleting
    /// records the glossary says are worth keeping.
    pub fn is_settled(self) -> bool {
        matches!(self, Status::Done | Status::Cancelled)
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Todo => "Todo",
            Status::Doing => "Doing",
            Status::Blocked => "Blocked",
            Status::Done => "Done",
            Status::Cancelled => "Cancelled",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Case-insensitive, so `done` and `Done` are the same Status.
///
/// Tag identity already folds case deliberately; Status matching exactly was
/// the inconsistency. The database only ever holds canonical labels, so this
/// widens what is accepted without changing what is stored.
impl FromStr for Status {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Status::ALL
            .into_iter()
            .find(|status| status.label().eq_ignore_ascii_case(s))
            .ok_or_else(|| ParseError {
                kind: "status",
                value: s.to_owned(),
            })
    }
}

/// How much an Issue matters relative to others.
///
/// Variants are declared lowest-first so the derived `Ord` sorts naturally;
/// the list view then sorts descending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Priority {
    #[default]
    None,
    Low,
    Medium,
    High,
    Urgent,
}

impl Priority {
    pub const ALL: [Priority; 5] = [
        Priority::None,
        Priority::Low,
        Priority::Medium,
        Priority::High,
        Priority::Urgent,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Priority::None => "None",
            Priority::Low => "Low",
            Priority::Medium => "Medium",
            Priority::High => "High",
            Priority::Urgent => "Urgent",
        }
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Case-insensitive, for the reason given on [`Status`]'s implementation.
impl FromStr for Priority {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Priority::ALL
            .into_iter()
            .find(|priority| priority.label().eq_ignore_ascii_case(s))
            .ok_or_else(|| ParseError {
                kind: "priority",
                value: s.to_owned(),
            })
    }
}

/// The longest a Tag may be. Long enough for a short phrase, short enough
/// that a chip never crowds out the title it sits under.
const MAX_TAG_LEN: usize = 50;

/// A user-invented name attached to an Issue.
///
/// Tags are derived rather than declared: one exists exactly as long as some
/// Issue carries it, and vanishes when the last one lets go. See
/// `docs/adr/0004`.
///
/// Identity is case-insensitive — `Bug` and `bug` are one Tag — so the
/// comparison traits are written over [`Tag::key`] rather than derived. That
/// makes it structurally impossible for a case variant to slip past a
/// `contains` check, which no database constraint could catch: without a `tag`
/// table there is nothing for a unique index to be unique *across*.
#[derive(Debug, Clone)]
pub struct Tag(String);

impl Tag {
    /// The name as the user first spelled it.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The comparison identity: case-folded.
    pub fn key(&self) -> String {
        self.0.to_lowercase()
    }

    /// Returns the spelling already established for this name, if there is
    /// one.
    ///
    /// Case-insensitive equality keeps *comparisons* honest, but the junction
    /// table stores whatever was typed. Folding on the way in is what stops
    /// `Bug` and `bug` showing up as two rows in the sidebar.
    pub fn canonicalise(self, in_use: &BTreeSet<Tag>) -> Tag {
        in_use.get(&self).cloned().unwrap_or(self)
    }
}

impl PartialEq for Tag {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for Tag {}

impl PartialOrd for Tag {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Tag {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}

impl Hash for Tag {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Tag {
    type Err = ParseError;

    /// Trims, then rejects anything that would make a Tag unusable: an empty
    /// name, a control character (which is also what keeps names free of
    /// newlines), or a name too long to render.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty()
            || trimmed.chars().any(char::is_control)
            || trimmed.chars().count() > MAX_TAG_LEN
        {
            return Err(ParseError {
                kind: "tag",
                value: s.to_owned(),
            });
        }
        Ok(Tag(trimmed.to_owned()))
    }
}

/// Prepares Tags for storage: folded against the spellings already in use,
/// deduplicated, and sorted so display order never depends on entry order.
pub fn normalise_tags(
    candidates: impl IntoIterator<Item = Tag>,
    in_use: &BTreeSet<Tag>,
) -> Vec<Tag> {
    candidates
        .into_iter()
        .map(|tag| tag.canonicalise(in_use))
        // A `BTreeSet` keeps the first of any pair it considers equal and
        // iterates in order, so this dedupes and sorts in one pass.
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// A single unit of tracked work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub id: IssueId,
    pub title: String,
    pub body: String,
    pub status: Status,
    pub priority: Priority,
    /// How big this piece of work is on its own.
    ///
    /// For a Parent this is the work its parts do *not* cover — integration,
    /// review, the bits that never became their own Issue — which is what
    /// makes [`SizeRollup`] adding rather than double-counting.
    pub size: Option<Size>,
    /// Sorted, and free of case-variant duplicates. Maintained through
    /// [`normalise_tags`].
    pub tags: Vec<Tag>,
    /// The Issue this one is part of, if any.
    ///
    /// Hierarchy is exactly one level deep: an Issue with a parent has no
    /// sub-issues of its own. Nothing in this type enforces that — it is a
    /// condition across rows, so it lives in `Projection`. See
    /// `docs/adr/0007`.
    pub parent_id: Option<IssueId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The title as displayed when the user hasn't typed one yet.
///
/// Free-standing because not every surface holds an `Issue`: the CLI and the
/// MCP server see a title that arrived as JSON. A blank title has to read the
/// same in a terminal as it does in the window, and it will not if each
/// surface decides for itself.
pub fn display_title(title: &str) -> &str {
    if title.trim().is_empty() {
        "Untitled"
    } else {
        title
    }
}

impl Issue {
    /// An Issue not yet filed: defaults everywhere but the title.
    ///
    /// Its `id` and timestamps are placeholders — they are the store's to
    /// assign, and `Store::insert` ignores whatever a draft carries there.
    pub fn draft(title: impl Into<String>) -> Self {
        Self {
            id: 0,
            title: title.into(),
            body: String::new(),
            status: Status::default(),
            priority: Priority::default(),
            size: None,
            tags: Vec::new(),
            parent_id: None,
            created_at: DateTime::UNIX_EPOCH,
            updated_at: DateTime::UNIX_EPOCH,
        }
    }

    /// The title as displayed when the user hasn't typed one yet.
    pub fn display_title(&self) -> &str {
        display_title(&self.title)
    }
}

/// A named, predefined slice of all Issues, shown in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum View {
    #[default]
    All,
    WithStatus(Status),
}

impl View {
    pub const ALL: [View; 6] = [
        View::All,
        View::WithStatus(Status::Todo),
        View::WithStatus(Status::Doing),
        View::WithStatus(Status::Blocked),
        View::WithStatus(Status::Done),
        View::WithStatus(Status::Cancelled),
    ];

    pub fn label(self) -> &'static str {
        match self {
            View::All => "All Issues",
            View::WithStatus(status) => status.label(),
        }
    }

    pub fn contains(self, issue: &Issue) -> bool {
        match self {
            View::All => true,
            View::WithStatus(status) => issue.status == status,
        }
    }
}

/// The persisted form of a View.
///
/// Deliberately distinct from [`View::label`], which is human-facing text —
/// `All` persists as `"All"` but displays as `"All Issues"`. Keeping them
/// separate means the display text can be reworded without invalidating
/// anything already stored.
impl fmt::Display for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            View::All => f.write_str("All"),
            View::WithStatus(status) => f.write_str(status.label()),
        }
    }
}

impl FromStr for View {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "All" {
            return Ok(View::All);
        }
        s.parse::<Status>()
            .map(View::WithStatus)
            .map_err(|_| ParseError {
                kind: "view",
                value: s.to_owned(),
            })
    }
}

/// Which Issues a Parent narrowing admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentFilter {
    /// Only Issues that are not part of anything.
    Unparented,
    /// The Sub-issues of one Issue.
    Under(IssueId),
}

impl ParentFilter {
    pub fn matches(self, issue: &Issue) -> bool {
        match self {
            ParentFilter::Unparented => issue.parent_id.is_none(),
            ParentFilter::Under(parent) => issue.parent_id == Some(parent),
        }
    }
}

/// `none` is a literal rather than an empty value, because an empty one is
/// what a caller sends by accident when a variable was never set.
impl FromStr for ParentFilter {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "none" {
            return Ok(ParentFilter::Unparented);
        }
        s.parse::<IssueId>()
            .map(ParentFilter::Under)
            .map_err(|_| ParseError {
                kind: "parent",
                value: s.to_owned(),
            })
    }
}

/// One narrowing of the Issues.
///
/// Composition is all it does: a View, optionally within a Tag, optionally
/// matching a title, optionally under a Parent. There is deliberately no query
/// language — no `status:done tag:ui`, no saved searches — because a View is
/// predefined and everything else here narrows within one.
///
/// It lives in `domain` because every surface asks the same question: the
/// window narrows by what you clicked, the API by its query string. Written
/// twice, the two drift, and they had already started to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Narrowing {
    pub view: View,
    pub tag: Option<Tag>,
    /// Matched against the title, ignoring case and surrounding space.
    /// Titles only — never bodies, never Tag names.
    pub title: Option<String>,
    pub parent: Option<ParentFilter>,
}

impl Narrowing {
    /// A whole View, unnarrowed.
    pub fn for_view(view: View) -> Self {
        Narrowing {
            view,
            ..Default::default()
        }
    }

    /// The matching Issues, in the order they were given — which is display
    /// order, so every surface agrees on what "first" means.
    pub fn select<'a>(&self, issues: &'a [Issue]) -> Vec<&'a Issue> {
        let needle = self.needle();
        issues
            .iter()
            .filter(|issue| self.admits(issue, needle.as_deref()))
            .collect()
    }

    /// How many match, without materialising them — a sidebar badge wants the
    /// number and nothing else, once per View per render.
    pub fn count(&self, issues: &[Issue]) -> usize {
        let needle = self.needle();
        issues
            .iter()
            .filter(|issue| self.admits(issue, needle.as_deref()))
            .count()
    }

    /// Folded once per call rather than once per Issue. A blank title match
    /// is no match at all, so a filter box the user has emptied narrows
    /// nothing.
    fn needle(&self) -> Option<String> {
        self.title
            .as_ref()
            .map(|title| title.trim().to_lowercase())
            .filter(|title| !title.is_empty())
    }

    fn admits(&self, issue: &Issue, needle: Option<&str>) -> bool {
        self.view.contains(issue)
            && self.tag.as_ref().is_none_or(|tag| issue.tags.contains(tag))
            && self.parent.is_none_or(|parent| parent.matches(issue))
            && needle.is_none_or(|needle| issue.title.to_lowercase().contains(needle))
    }
}

/// An Issue's Size together with its parts'.
///
/// Derived on every read and never stored: a stored total is a second copy of
/// something the Issues already say, and it goes wrong the moment a part is
/// moved, deleted, or resized. Summing a few thousand Issues in memory is free.
///
/// Settled parts still count. A Size is a fact about the work, not about how
/// much of it is left, so a Parent does not shrink as you finish it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeRollup {
    /// The Issue's own Size plus its parts'. `None` when nothing in the family
    /// carries one — distinct from `Some(0)`, which is a family that adds up
    /// to no work.
    pub total: Option<u32>,
    /// How many parts have no Size. A total that silently omitted them would
    /// be a lower bound presented as a figure.
    pub unsized_sub_issues: usize,
}

impl SizeRollup {
    /// Takes the parts themselves rather than a count, so the two numbers are
    /// derived from one slice and cannot contradict each other.
    pub fn of(issue: &Issue, sub_issues: &[&Issue]) -> Self {
        let sizes = std::iter::once(issue.size).chain(sub_issues.iter().map(|part| part.size));

        // Stays `None` until something is actually sized, which is what keeps
        // "nothing is sized" distinct from "adds up to zero".
        let mut total = None;
        for size in sizes.flatten() {
            total = Some(total.unwrap_or(0) + u32::from(size));
        }

        SizeRollup {
            total,
            unsized_sub_issues: sub_issues.iter().filter(|part| part.size.is_none()).count(),
        }
    }
}

/// Orders Issues for display: highest priority first, then most recently
/// updated. Ties break on `id` so the order is never ambiguous.
pub fn sort_for_display(issues: &mut [Issue]) {
    issues.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(b.updated_at.cmp(&a.updated_at))
            .then(b.id.cmp(&a.id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A corpus for narrowing: distinct Statuses, Tags and parentage.
    fn corpus() -> Vec<Issue> {
        let mut issues = vec![
            narrowable(1, "Fix the flash", Status::Doing, &["ui"]),
            narrowable(2, "Write the ADR", Status::Todo, &["docs"]),
            narrowable(3, "Ship the CLI", Status::Done, &["ui", "cli"]),
            narrowable(4, "Chase the bug", Status::Todo, &[]),
        ];
        issues[3].parent_id = Some(1);
        issues
    }

    fn narrowable(id: IssueId, title: &str, status: Status, tags: &[&str]) -> Issue {
        Issue {
            id,
            title: title.to_string(),
            body: String::new(),
            status,
            priority: Priority::None,
            tags: tags.iter().map(|tag| tag.parse().unwrap()).collect(),
            parent_id: None,
            size: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn ids(issues: Vec<&Issue>) -> Vec<IssueId> {
        issues.into_iter().map(|issue| issue.id).collect()
    }

    fn sized(id: IssueId, size: Option<Size>) -> Issue {
        let mut issue = narrowable(id, "sized", Status::Todo, &[]);
        issue.size = size;
        issue
    }

    #[test]
    fn a_family_with_no_sizes_at_all_has_no_total() {
        // Distinct from a total of zero, which is a family that adds up to no
        // work. Collapsing them would lose the only thing `Option` is for.
        let parent = sized(1, None);
        let parts = [sized(2, None), sized(3, None)];
        let rollup = SizeRollup::of(&parent, &parts.iter().collect::<Vec<_>>());

        assert_eq!(rollup.total, None);
        assert_eq!(rollup.unsized_sub_issues, 2);
    }

    #[test]
    fn a_size_of_zero_is_a_real_answer() {
        let rollup = SizeRollup::of(&sized(1, Some(0)), &[]);
        assert_eq!(rollup.total, Some(0), "no work is not the same as unknown");
    }

    #[test]
    fn a_parents_own_size_adds_to_its_parts() {
        // Its own Size is the work the parts do not cover, so it adds rather
        // than replacing. See docs/adr/0011.
        let parent = sized(1, Some(3));
        let parts = [sized(2, Some(5)), sized(3, Some(8))];
        let rollup = SizeRollup::of(&parent, &parts.iter().collect::<Vec<_>>());

        assert_eq!(rollup.total, Some(16));
        assert_eq!(rollup.unsized_sub_issues, 0);
    }

    #[test]
    fn an_unsized_part_is_counted_rather_than_treated_as_zero() {
        // A total that silently omitted it would be a lower bound presented
        // as a figure.
        let parent = sized(1, None);
        let parts = [sized(2, Some(5)), sized(3, None), sized(4, Some(2))];
        let rollup = SizeRollup::of(&parent, &parts.iter().collect::<Vec<_>>());

        assert_eq!(rollup.total, Some(7));
        assert_eq!(rollup.unsized_sub_issues, 1);
    }

    #[test]
    fn a_settled_part_still_counts() {
        // A Size is a fact about the work, not about how much is left: a
        // Parent that shrank as you finished it would read as a bad estimate.
        let parent = sized(1, None);
        let mut done = sized(2, Some(5));
        done.status = Status::Done;
        let mut cancelled = sized(3, Some(2));
        cancelled.status = Status::Cancelled;

        let rollup = SizeRollup::of(&parent, &[&done, &cancelled]);
        assert_eq!(rollup.total, Some(7));
    }

    #[test]
    fn a_total_outgrows_the_type_a_size_is_held_in() {
        // 255 is the ceiling for what a person types, not for what the
        // arithmetic produces — which is why a total is `u32`.
        let parent = sized(1, Some(u8::MAX));
        let parts: Vec<Issue> = (2..12).map(|id| sized(id, Some(u8::MAX))).collect();
        let rollup = SizeRollup::of(&parent, &parts.iter().collect::<Vec<_>>());

        assert_eq!(rollup.total, Some(255 * 11));
    }

    #[test]
    fn an_issue_with_no_parts_totals_to_its_own_size() {
        assert_eq!(SizeRollup::of(&sized(1, Some(4)), &[]).total, Some(4));
        assert_eq!(SizeRollup::of(&sized(1, None), &[]).total, None);
        assert_eq!(SizeRollup::of(&sized(1, None), &[]).unsized_sub_issues, 0);
    }

    #[test]
    fn an_empty_narrowing_admits_everything() {
        let issues = corpus();
        assert_eq!(Narrowing::default().select(&issues).len(), 4);
        assert_eq!(Narrowing::default().count(&issues), 4);
    }

    #[test]
    fn a_narrowing_keeps_the_order_it_was_given() {
        // Display order is decided once, by `sort_for_display`; narrowing
        // must not quietly reorder, or "the first one" would mean different
        // things in the window and over HTTP.
        let issues = corpus();
        assert_eq!(ids(Narrowing::default().select(&issues)), vec![1, 2, 3, 4]);
    }

    #[test]
    fn the_narrowings_compose() {
        let issues = corpus();
        let ui = Narrowing {
            tag: Some("ui".parse().unwrap()),
            ..Default::default()
        };
        assert_eq!(ids(ui.select(&issues)), vec![1, 3]);

        // A Tag narrows within a View, and a title within both.
        let within = Narrowing {
            view: View::WithStatus(Status::Done),
            title: Some("ship".into()),
            ..ui.clone()
        };
        assert_eq!(ids(within.select(&issues)), vec![3]);

        let contradictory = Narrowing {
            view: View::WithStatus(Status::Todo),
            ..within
        };
        assert!(contradictory.select(&issues).is_empty());
    }

    #[test]
    fn a_title_match_ignores_case_and_surrounding_space() {
        let issues = corpus();
        let narrowing = Narrowing {
            title: Some("  FLASH  ".into()),
            ..Default::default()
        };
        assert_eq!(ids(narrowing.select(&issues)), vec![1]);
    }

    #[test]
    fn a_blank_title_match_narrows_nothing() {
        // An emptied filter box must show everything, not nothing.
        let issues = corpus();
        for blank in ["", "   "] {
            let narrowing = Narrowing {
                title: Some(blank.into()),
                ..Default::default()
            };
            assert_eq!(narrowing.count(&issues), 4, "{blank:?}");
        }
    }

    #[test]
    fn a_title_match_reads_titles_only() {
        let mut issues = corpus();
        issues[0].body = "mentions the ADR".to_string();
        let narrowing = Narrowing {
            title: Some("ADR".into()),
            ..Default::default()
        };
        assert_eq!(ids(narrowing.select(&issues)), vec![2], "not the body");
    }

    #[test]
    fn a_tag_match_folds_case_because_tag_identity_does() {
        let issues = corpus();
        let narrowing = Narrowing {
            tag: Some("UI".parse().unwrap()),
            ..Default::default()
        };
        assert_eq!(ids(narrowing.select(&issues)), vec![1, 3]);
    }

    #[test]
    fn a_parent_narrowing_asks_both_halves_of_the_question() {
        let issues = corpus();

        let under = Narrowing {
            parent: Some(ParentFilter::Under(1)),
            ..Default::default()
        };
        assert_eq!(ids(under.select(&issues)), vec![4]);

        let top = Narrowing {
            parent: Some(ParentFilter::Unparented),
            ..Default::default()
        };
        assert_eq!(ids(top.select(&issues)), vec![1, 2, 3]);
    }

    #[test]
    fn a_parent_filter_reads_an_id_or_the_literal_none() {
        assert_eq!(
            "none".parse::<ParentFilter>().unwrap(),
            ParentFilter::Unparented
        );
        assert_eq!("7".parse::<ParentFilter>().unwrap(), ParentFilter::Under(7));

        // An empty value is what a caller sends when a variable was never
        // set, so it must not quietly mean "none".
        for bad in ["", "all", "none ", "None", "seven"] {
            let err = bad.parse::<ParentFilter>().unwrap_err();
            assert_eq!(err.kind, "parent", "{bad:?}");
            assert!(err.to_string().contains(bad), "{bad:?}");
        }
    }

    #[test]
    fn counting_a_view_ignores_everything_narrowing_it() {
        let issues = corpus();
        assert_eq!(Narrowing::for_view(View::All).count(&issues), 4);
        assert_eq!(
            Narrowing::for_view(View::WithStatus(Status::Todo)).count(&issues),
            2
        );
        assert_eq!(
            Narrowing::for_view(View::WithStatus(Status::Cancelled)).count(&issues),
            0
        );
    }

    fn issue(id: IssueId, priority: Priority, updated_at: &str) -> Issue {
        let at = DateTime::parse_from_rfc3339(updated_at)
            .unwrap()
            .with_timezone(&Utc);
        Issue {
            id,
            title: format!("issue {id}"),
            body: String::new(),
            status: Status::Todo,
            priority,
            tags: Vec::new(),
            parent_id: None,
            size: None,
            created_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn status_round_trips_through_text() {
        for status in Status::ALL {
            assert_eq!(status.to_string().parse::<Status>().unwrap(), status);
        }
    }

    #[test]
    fn priority_round_trips_through_text() {
        for priority in Priority::ALL {
            assert_eq!(priority.to_string().parse::<Priority>().unwrap(), priority);
        }
    }

    #[test]
    fn status_and_priority_fold_case() {
        // What a terminal user types, and what the API therefore accepts.
        assert_eq!("done".parse::<Status>().unwrap(), Status::Done);
        assert_eq!("CANCELLED".parse::<Status>().unwrap(), Status::Cancelled);
        assert_eq!("urgent".parse::<Priority>().unwrap(), Priority::Urgent);
        // Folding must not make two variants collide.
        for status in Status::ALL {
            assert_eq!(
                status.label().to_lowercase().parse::<Status>().unwrap(),
                status
            );
        }
        for priority in Priority::ALL {
            assert_eq!(
                priority.label().to_lowercase().parse::<Priority>().unwrap(),
                priority
            );
        }
    }

    #[test]
    fn unknown_text_is_rejected_rather_than_defaulted() {
        assert!("Wontfix".parse::<Status>().is_err());
        assert!("Critical".parse::<Priority>().is_err());
    }

    #[test]
    fn done_and_cancelled_are_settled_the_rest_are_not() {
        assert!(Status::Done.is_settled());
        assert!(Status::Cancelled.is_settled(), "abandoned is still decided");
        assert!(!Status::Todo.is_settled());
        assert!(!Status::Doing.is_settled());
        assert!(!Status::Blocked.is_settled());
    }

    #[test]
    fn priority_orders_none_lowest_and_urgent_highest() {
        assert!(Priority::None < Priority::Low);
        assert!(Priority::Urgent > Priority::High);
    }

    #[test]
    fn all_view_contains_every_status() {
        for status in Status::ALL {
            let mut candidate = issue(1, Priority::None, "2026-01-01T00:00:00Z");
            candidate.status = status;
            assert!(View::All.contains(&candidate));
        }
    }

    #[test]
    fn status_view_contains_only_its_own_status() {
        let view = View::WithStatus(Status::Blocked);
        let mut blocked = issue(1, Priority::None, "2026-01-01T00:00:00Z");
        blocked.status = Status::Blocked;
        let todo = issue(2, Priority::None, "2026-01-01T00:00:00Z");

        assert!(view.contains(&blocked));
        assert!(!view.contains(&todo));
    }

    #[test]
    fn view_round_trips_through_text() {
        for view in View::ALL {
            assert_eq!(view.to_string().parse::<View>().unwrap(), view);
        }
    }

    #[test]
    fn view_persistence_is_independent_of_display_text() {
        // "All Issues" is what the user sees; "All" is what gets stored.
        // Rewording the label must not invalidate stored values.
        assert_eq!(View::All.to_string(), "All");
        assert_eq!(View::All.label(), "All Issues");
    }

    #[test]
    fn unknown_view_is_rejected() {
        assert!("Archived".parse::<View>().is_err());
        assert!("All Issues".parse::<View>().is_err());
    }

    #[test]
    fn display_orders_by_priority_then_recency() {
        let mut issues = vec![
            issue(1, Priority::Low, "2026-01-03T00:00:00Z"),
            issue(2, Priority::Urgent, "2026-01-01T00:00:00Z"),
            issue(3, Priority::Low, "2026-01-05T00:00:00Z"),
        ];

        sort_for_display(&mut issues);

        // Urgent first despite being the oldest, then Low by recency.
        assert_eq!(
            issues.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    fn tag(name: &str) -> Tag {
        name.parse().expect("valid tag")
    }

    #[test]
    fn tags_trim_and_preserve_their_spelling() {
        assert_eq!(tag("  Needs design  ").as_str(), "Needs design");
    }

    #[test]
    fn unusable_tag_names_are_rejected() {
        assert!("".parse::<Tag>().is_err());
        assert!("   ".parse::<Tag>().is_err());
        // Control characters are barred, which is also what guarantees a name
        // can never contain a newline.
        assert!("two\nlines".parse::<Tag>().is_err());
        assert!("x".repeat(MAX_TAG_LEN + 1).parse::<Tag>().is_err());
        assert!("x".repeat(MAX_TAG_LEN).parse::<Tag>().is_ok());
    }

    #[test]
    fn slashes_and_spaces_are_ordinary_characters() {
        // Deliberately flat: `ui/theme` is one name, not a child of `ui`.
        assert_eq!(tag("ui/theme").as_str(), "ui/theme");
        assert_ne!(tag("ui/theme"), tag("ui"));
    }

    #[test]
    fn tags_compare_without_regard_to_case() {
        assert_eq!(tag("Bug"), tag("bug"));
        assert_eq!(tag("Bug").cmp(&tag("bug")), Ordering::Equal);

        // The property that motivates the hand-written impls: a case variant
        // must never slip past a membership test.
        assert!([tag("Bug")].contains(&tag("bug")));
    }

    #[test]
    fn a_set_keeps_the_first_spelling_it_saw() {
        let mut in_use = BTreeSet::new();
        in_use.insert(tag("Bug"));
        in_use.insert(tag("bug"));

        assert_eq!(in_use.len(), 1);
        assert_eq!(in_use.iter().next().unwrap().as_str(), "Bug");
    }

    #[test]
    fn canonicalising_adopts_the_established_spelling() {
        let in_use = BTreeSet::from([tag("Bug")]);

        assert_eq!(tag("bug").canonicalise(&in_use).as_str(), "Bug");
        // An unrelated name keeps the spelling it arrived with.
        assert_eq!(tag("UI").canonicalise(&in_use).as_str(), "UI");
    }

    #[test]
    fn normalising_folds_dedupes_and_sorts() {
        let in_use = BTreeSet::from([tag("Bug")]);
        let tags = normalise_tags([tag("ui"), tag("bug"), tag("BUG")], &in_use);

        assert_eq!(
            tags.iter().map(Tag::as_str).collect::<Vec<_>>(),
            vec!["Bug", "ui"]
        );
    }

    #[test]
    fn blank_titles_display_as_untitled() {
        let mut candidate = issue(1, Priority::None, "2026-01-01T00:00:00Z");
        candidate.title = "   ".into();
        assert_eq!(candidate.display_title(), "Untitled");
    }
}
