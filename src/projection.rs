// SPDX-License-Identifier: GPL-3.0-only

//! App-level ownership of the [`Store`] and the in-memory Issue list.
//!
//! This is the single writer. Both the UI and the HTTP API mutate through it,
//! which is what makes their capabilities identical by construction rather
//! than by discipline — there is no second path to the database.
//!
//! Deliberately free of `gpui`: the UI wraps this in an `Entity` to get
//! change notification, but nothing here knows that. That keeps every
//! interesting mutation testable without a window. See `docs/adr/0005`.

use std::collections::BTreeSet;
use std::fmt;

use anyhow::Result;

use crate::domain::{
    Issue, IssueId, Priority, Size, Status, Tag, normalise_tags, sort_for_display,
};
use crate::store::Store;

/// A write the data will not allow.
///
/// Distinct from a storage failure: nothing is broken and retrying will not
/// help — the request conflicts with the current shape of things. The API
/// turns these into `409 Conflict`, and the UI prevents most of them from
/// being expressible in the first place. See `docs/adr/0007`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Hierarchy is one level deep, and the proposed parent is itself a
    /// sub-issue.
    ParentIsSubIssue(IssueId),
    /// Hierarchy is one level deep, and the Issue being attached has
    /// sub-issues of its own.
    ChildHasSubIssues(IssueId),
    SelfParent,
    /// A Done parent may not take on work that is still outstanding.
    ParentAlreadyDone(IssueId),
    /// A parent is Done only once nothing beneath it is outstanding.
    SubIssuesOutstanding(usize),
    /// Reopening this would leave a Done parent sitting above open work.
    ParentIsDone(IssueId),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::ParentIsSubIssue(id) => write!(
                f,
                "#{id} is itself a sub-issue, and sub-issues cannot have sub-issues"
            ),
            Refused::ChildHasSubIssues(id) => write!(
                f,
                "#{id} has sub-issues of its own, and sub-issues cannot have sub-issues"
            ),
            Refused::SelfParent => f.write_str("an issue cannot be its own parent"),
            Refused::ParentAlreadyDone(id) => {
                write!(f, "#{id} is Done and cannot take on outstanding work")
            }
            Refused::SubIssuesOutstanding(count) => {
                write!(f, "{count} sub-issue(s) are still outstanding",)
            }
            Refused::ParentIsDone(id) => {
                write!(f, "its parent #{id} is Done; reopen that first")
            }
        }
    }
}

/// Why a mutation did not happen.
#[derive(Debug)]
pub enum WriteError {
    /// No such Issue. The API answers 404.
    NotFound(IssueId),
    /// The caller asked for something the data forbids. The API answers 409.
    Refused(Refused),
    /// The database failed. The API answers 500.
    Store(anyhow::Error),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::NotFound(id) => write!(f, "no issue #{id}"),
            WriteError::Refused(refusal) => refusal.fmt(f),
            WriteError::Store(err) => write!(f, "{err:#}"),
        }
    }
}

/// Lets the existing `?` on store calls keep working unchanged.
impl From<anyhow::Error> for WriteError {
    fn from(err: anyhow::Error) -> Self {
        WriteError::Store(err)
    }
}

impl From<Refused> for WriteError {
    fn from(refusal: Refused) -> Self {
        WriteError::Refused(refusal)
    }
}

/// The result of any mutation.
pub type Written<T> = std::result::Result<T, WriteError>;

/// A change to some of an Issue's fields.
///
/// Every field is optional and only those supplied are written. Two callers
/// touching different fields therefore cannot clobber one another — the API
/// setting a Status never rewrites a title the UI is in the middle of editing.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IssuePatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub status: Option<Status>,
    pub priority: Option<Priority>,
    /// Replaces the whole Tag set. Use [`Projection::add_tag`] to change one
    /// Tag without a read-modify-write.
    pub tags: Option<Vec<Tag>>,
    /// Three states, which is why it is nested: absent leaves the Size alone,
    /// `Some(None)` clears it, `Some(Some(n))` sets it. `0` cannot stand in
    /// for "unsized" — it is a real Size meaning no work. Build it with
    /// [`IssuePatch::size`] and [`IssuePatch::clear_size`] rather than by
    /// hand, because `size(None)` would read like "leave it alone".
    pub size: Option<Option<Size>>,
}

impl IssuePatch {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn status(mut self, status: Status) -> Self {
        self.status = Some(status);
        self
    }

    pub fn priority(mut self, priority: Priority) -> Self {
        self.priority = Some(priority);
        self
    }

    pub fn tags(mut self, tags: Vec<Tag>) -> Self {
        self.tags = Some(tags);
        self
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = Some(Some(size));
        self
    }

    /// Makes the Issue unsized again, which is not the same as sizing it zero.
    pub fn clear_size(mut self) -> Self {
        self.size = Some(None);
        self
    }
}

/// Writes a patch's fields onto an Issue, touching only those it names.
///
/// Shared by creating and patching, so a field means the same thing whichever
/// of the two it arrives through. `in_use` is the corpus's Tag vocabulary,
/// which new Tags are folded against.
fn apply(issue: &mut Issue, patch: IssuePatch, in_use: &BTreeSet<Tag>) {
    if let Some(title) = patch.title {
        issue.title = title;
    }
    if let Some(body) = patch.body {
        issue.body = body;
    }
    if let Some(status) = patch.status {
        issue.status = status;
    }
    if let Some(priority) = patch.priority {
        issue.priority = priority;
    }
    if let Some(tags) = patch.tags {
        issue.tags = normalise_tags(tags, in_use);
    }
    // The outer `Some` says the patch mentioned Size at all; the inner one is
    // the value, where `None` means unsize it.
    if let Some(size) = patch.size {
        issue.size = size;
    }
}

/// Settled-over-total for a set of parts, or `None` when there are none.
///
/// Free-standing so that [`Projection::settled_progress`] and
/// [`SubIssues::settled_progress`] are the same answer reached two ways
/// rather than two answers — the number beside a row and the number behind a
/// disabled Done button have to agree.
fn settled_progress(sub_issues: &[&Issue]) -> Option<(usize, usize)> {
    if sub_issues.is_empty() {
        return None;
    }
    let settled = sub_issues
        .iter()
        .filter(|issue| issue.status.is_settled())
        .count();
    Some((settled, sub_issues.len()))
}

/// Which Issues are part of which, worked out once.
///
/// An Issue does not know its own parts — that is a fact about the whole
/// corpus, which is why [`Projection::sub_issues`] scans for them. Asking it
/// per row makes a render quadratic in the number of Issues shown; this
/// answers the same question for everything at once, in a single pass, and
/// hands back borrows of the very same Issues.
///
/// Derived on demand and never stored, like [`crate::domain::SizeRollup`]: it
/// borrows the corpus, so it cannot outlive a mutation and go stale.
pub struct SubIssues<'a> {
    by_parent: std::collections::HashMap<IssueId, Vec<&'a Issue>>,
}

impl<'a> SubIssues<'a> {
    fn of(issues: &'a [Issue]) -> Self {
        let mut by_parent: std::collections::HashMap<IssueId, Vec<&'a Issue>> =
            std::collections::HashMap::new();
        // One pass in the order given, which is display order, so each list
        // comes out ordered exactly as `sub_issues` would have returned it.
        for issue in issues {
            if let Some(parent) = issue.parent_id {
                by_parent.entry(parent).or_default().push(issue);
            }
        }
        Self { by_parent }
    }

    /// One Issue's parts, in display order. Empty for an Issue that holds
    /// nothing — the same answer [`Projection::sub_issues`] gives.
    pub fn of_issue(&self, id: IssueId) -> &[&'a Issue] {
        self.by_parent.get(&id).map_or(&[], Vec::as_slice)
    }

    /// Settled-over-total for one Issue's parts, or `None` when it has none.
    pub fn settled_progress(&self, id: IssueId) -> Option<(usize, usize)> {
        settled_progress(self.of_issue(id))
    }
}

/// Every Issue, display-sorted, plus the database they came from.
pub struct Projection {
    store: Store,
    issues: Vec<Issue>,
}

impl Projection {
    pub fn load(store: Store) -> Result<Self> {
        let mut issues = store.load_all()?;
        sort_for_display(&mut issues);
        Ok(Self { store, issues })
    }

    // ---- reads --------------------------------------------------------------

    /// Every Issue in display order. Readers render straight from this.
    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }

    pub fn get(&self, id: IssueId) -> Option<&Issue> {
        self.issues.iter().find(|issue| issue.id == id)
    }

    /// Every Tag some Issue carries. Derived: there is no Tag registry.
    pub fn tags_in_use(&self) -> BTreeSet<Tag> {
        self.issues
            .iter()
            .flat_map(|issue| issue.tags.iter().cloned())
            .collect()
    }

    pub fn count_with_tag(&self, tag: &Tag) -> usize {
        self.issues
            .iter()
            .filter(|issue| issue.tags.contains(tag))
            .count()
    }

    // ---- hierarchy ----------------------------------------------------------

    /// The Issues that are part of `id`, in display order.
    pub fn sub_issues(&self, id: IssueId) -> Vec<&Issue> {
        self.issues
            .iter()
            .filter(|issue| issue.parent_id == Some(id))
            .collect()
    }

    /// Every Issue's parts, in one pass.
    ///
    /// [`Self::sub_issues`] scans the corpus, which is the right answer for
    /// one Issue and the wrong one for a list of them — a row that shows its
    /// parts' progress and their Size turns a render into two scans per row,
    /// so a window showing everything costs the square of what it shows.
    /// This is that scan done once. See [`SubIssues`].
    pub fn sub_issue_index(&self) -> SubIssues<'_> {
        SubIssues::of(&self.issues)
    }

    /// How many sub-issues are settled, and how many there are — the `2/3` on
    /// a parent's row. `None` when it has no sub-issues at all.
    ///
    /// Deliberately the same predicate the Done rule uses, so the number on
    /// screen explains the disabled button exactly.
    pub fn settled_progress(&self, id: IssueId) -> Option<(usize, usize)> {
        settled_progress(&self.sub_issues(id))
    }

    fn outstanding_count(&self, id: IssueId) -> usize {
        self.sub_issues(id)
            .iter()
            .filter(|issue| !issue.status.is_settled())
            .count()
    }

    /// Whether `id` may be marked Done right now.
    fn can_be_done(&self, id: IssueId) -> bool {
        self.outstanding_count(id) == 0
    }

    /// Whether `parent` may take on a part, given whether that part is
    /// settled — the half of the attachment rules that is about the parent.
    ///
    /// Split out because it is everything answerable *before the child
    /// exists*, which is what `POST /issues` with a `parent_id` needs. Asking
    /// afterwards means creating an Issue in order to discover the request was
    /// refused, and answering 404 to a caller who now owns something they were
    /// never told about.
    fn may_hold(
        &self,
        parent: IssueId,
        child_is_settled: bool,
    ) -> std::result::Result<(), WriteError> {
        let Some(target) = self.get(parent) else {
            return Err(WriteError::NotFound(parent));
        };

        // One level deep, this end of it.
        if target.parent_id.is_some() {
            return Err(Refused::ParentIsSubIssue(parent).into());
        }

        // A Done parent may not take on work that is still outstanding.
        if target.status == Status::Done && !child_is_settled {
            return Err(Refused::ParentAlreadyDone(parent).into());
        }

        Ok(())
    }

    /// Whether `child` may be attached under `parent`, and why not.
    ///
    /// The single statement of the attachment rules. [`Self::set_parent`]
    /// enforces them and the two `eligible_*` methods offer them as a list:
    /// stated once, in refusal form, with the predicate form falling out of it
    /// rather than restating it. Written twice they had already diverged —
    /// `eligible_sub_issues` never checked whether the proposed parent was
    /// itself a sub-issue, and was correct only because the detail pane
    /// declines to show that picker.
    fn may_attach(&self, child: IssueId, parent: IssueId) -> std::result::Result<(), WriteError> {
        let Some(issue) = self.get(child) else {
            return Err(WriteError::NotFound(child));
        };
        if parent == child {
            return Err(Refused::SelfParent.into());
        }

        // The parent's half first, so a parent that is not there is reported
        // as such rather than behind something about the child.
        self.may_hold(parent, issue.status.is_settled())?;

        // One level deep, the other end of it.
        if !self.sub_issues(child).is_empty() {
            return Err(Refused::ChildHasSubIssues(child).into());
        }

        Ok(())
    }

    /// Issues that could become sub-issues of `parent`.
    ///
    /// Includes Issues that already belong to somebody else — attaching one
    /// moves it — so callers must show the current parent rather than let a
    /// pick quietly empty another Issue.
    pub fn eligible_sub_issues(&self, parent: IssueId) -> Vec<&Issue> {
        self.issues
            .iter()
            // Already there: offering it again would be a move to where it is.
            .filter(|issue| issue.parent_id != Some(parent))
            .filter(|issue| self.may_attach(issue.id, parent).is_ok())
            .collect()
    }

    /// Issues that `child` could be made part of.
    pub fn eligible_parents(&self, child: IssueId) -> Vec<&Issue> {
        self.issues
            .iter()
            .filter(|issue| self.may_attach(child, issue.id).is_ok())
            .collect()
    }

    // ---- writes -------------------------------------------------------------

    /// Files a new Issue with any other supplied fields, under `parent` if
    /// one is named.
    ///
    /// All of it is one write, so a failure part-way leaves no Issue behind
    /// for a retry to duplicate. The fields go through [`apply`] exactly as a
    /// later patch's would, so a Tag arriving at creation time is folded
    /// against the vocabulary the same way; and the parent is asked
    /// [`Self::may_hold`] — everything an attach checks that is answerable
    /// before the child exists, which for a child with no parts of its own is
    /// everything.
    pub fn create(
        &mut self,
        title: &str,
        rest: IssuePatch,
        parent: Option<IssueId>,
    ) -> Written<Issue> {
        let mut draft = Issue::draft(title);
        apply(&mut draft, rest, &self.tags_in_use());
        if let Some(parent) = parent {
            self.may_hold(parent, draft.status.is_settled())?;
            draft.parent_id = Some(parent);
        }

        let issue = self.store.insert(&draft)?;
        self.issues.push(issue.clone());
        sort_for_display(&mut self.issues);
        Ok(issue)
    }

    /// Applies a patch, returning the Issue as it now stands, or `None` when
    /// no such Issue exists.
    ///
    /// A patch that changes nothing writes nothing, so `updated_at` does not
    /// move for a no-op — a keystroke that restores the previous text should
    /// not reorder the list.
    pub fn patch(&mut self, id: IssueId, patch: IssuePatch) -> Written<Issue> {
        let Some(current) = self.get(id) else {
            return Err(WriteError::NotFound(id));
        };
        if let Some(status) = patch.status {
            self.check_status_change(id, status)?;
        }

        let mut next = current.clone();
        // Deliberately from the whole corpus: a Tag's established spelling is
        // a global fact.
        apply(&mut next, patch, &self.tags_in_use());
        self.commit(next)
    }

    /// Guards the two directions of the completion rule.
    ///
    /// A parent may only be marked Done once nothing beneath it is
    /// outstanding; and a sub-issue may not be reopened while its parent is
    /// Done, which would produce that same forbidden state from below. Making
    /// both unreachable is what keeps this an invariant rather than a nudge.
    fn check_status_change(&self, id: IssueId, next: Status) -> std::result::Result<(), Refused> {
        if next == Status::Done && !self.can_be_done(id) {
            return Err(Refused::SubIssuesOutstanding(self.outstanding_count(id)));
        }

        if !next.is_settled()
            && let Some(parent) = self.get(id).and_then(|issue| issue.parent_id)
            && self.get(parent).is_some_and(|p| p.status == Status::Done)
        {
            return Err(Refused::ParentIsDone(parent));
        }

        Ok(())
    }

    /// Files `child` under `parent`, or removes it from whatever holds it.
    ///
    /// Attaching an Issue that already belongs to another parent *moves* it —
    /// there is no separate operation for that, because `set_parent` already
    /// says everything a move needs to say.
    pub fn set_parent(&mut self, child: IssueId, parent: Option<IssueId>) -> Written<Issue> {
        if self.get(child).is_none() {
            return Err(WriteError::NotFound(child));
        }

        if let Some(parent) = parent {
            self.may_attach(child, parent)?;
        }

        let mut next = self.get(child).cloned().expect("checked above");
        next.parent_id = parent;
        self.commit(next)
    }

    /// Makes `next` the stored version of its Issue: SQLite first, and the
    /// Vec only once that has succeeded.
    ///
    /// The single way an existing Issue changes. Editing the Vec in place and
    /// writing afterwards is the ADR-0002 bug run backwards — a write the
    /// database refused stays on screen until a restart quietly takes it away.
    /// Returns the current version unwritten when `next` is no different, so
    /// a no-op does not move `updated_at`.
    fn commit(&mut self, mut next: Issue) -> Written<Issue> {
        let Some(index) = self.issues.iter().position(|issue| issue.id == next.id) else {
            return Err(WriteError::NotFound(next.id));
        };
        if self.issues[index] == next {
            return Ok(next);
        }

        next.updated_at = self.store.update(&next)?;
        self.issues[index] = next.clone();
        sort_for_display(&mut self.issues);
        Ok(next)
    }

    /// Adds one Tag. Race-free where a read-modify-write from a caller is not:
    /// the read and the write happen together, under the single writer.
    pub fn add_tag(&mut self, id: IssueId, tag: Tag) -> Written<Issue> {
        let Some(issue) = self.get(id) else {
            return Err(WriteError::NotFound(id));
        };
        let mut tags = issue.tags.clone();
        if !tags.contains(&tag) {
            tags.push(tag);
        }
        self.patch(id, IssuePatch::default().tags(tags))
    }

    pub fn remove_tag(&mut self, id: IssueId, tag: &Tag) -> Written<Issue> {
        let Some(issue) = self.get(id) else {
            return Err(WriteError::NotFound(id));
        };
        let tags = issue
            .tags
            .iter()
            .filter(|candidate| *candidate != tag)
            .cloned()
            .collect();
        self.patch(id, IssuePatch::default().tags(tags))
    }

    /// Erases an Issue.
    ///
    /// Its sub-issues survive as ordinary top-level Issues: the foreign key is
    /// `ON DELETE SET NULL`, so orphaning is the schema's doing rather than
    /// something this code has to remember. Deleting a parent is not a licence
    /// to erase work nobody asked to erase.
    pub fn delete(&mut self, id: IssueId) -> Written<()> {
        if self.get(id).is_none() {
            return Err(WriteError::NotFound(id));
        }
        self.store.delete(id)?;
        self.issues.retain(|issue| issue.id != id);
        for issue in self.issues.iter_mut() {
            if issue.parent_id == Some(id) {
                issue.parent_id = None;
            }
        }
        Ok(())
    }

    // ---- settings -----------------------------------------------------------

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.store.get_setting(key)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.store.set_setting(key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection() -> Projection {
        Projection::load(Store::open_in_memory().expect("in-memory store")).expect("projection")
    }

    fn tag(name: &str) -> Tag {
        name.parse().expect("valid tag")
    }

    fn filed(p: &mut Projection, title: &str) -> IssueId {
        p.create(title, IssuePatch::default(), None).unwrap().id
    }

    /// Every eligibility answer must match the refusal that would follow.
    ///
    /// This is the property the two forms of the rule exist to share: a
    /// picker that offers something the writer would refuse is a control that
    /// fails on click, and one that hides something the writer would accept
    /// is work you cannot do.
    fn offers_agree_with_refusals(p: &Projection) {
        let ids: Vec<IssueId> = p.issues().iter().map(|issue| issue.id).collect();
        for &parent in &ids {
            let offered: Vec<IssueId> = p
                .eligible_sub_issues(parent)
                .iter()
                .map(|issue| issue.id)
                .collect();
            for &child in &ids {
                let allowed = p.may_attach(child, parent).is_ok();
                // Something already under this parent is withheld from the
                // list without being refused: attaching it would be a move to
                // where it already is.
                let already_there = p.get(child).and_then(|issue| issue.parent_id) == Some(parent);
                assert_eq!(
                    offered.contains(&child),
                    allowed && !already_there,
                    "sub-issue offer for #{child} under #{parent}"
                );
            }
        }
        for &child in &ids {
            let offered: Vec<IssueId> = p
                .eligible_parents(child)
                .iter()
                .map(|issue| issue.id)
                .collect();
            for &parent in &ids {
                assert_eq!(
                    offered.contains(&parent),
                    p.may_attach(child, parent).is_ok(),
                    "parent offer of #{parent} for #{child}"
                );
            }
        }
    }

    /// The index exists only to answer the scan's question faster. If the two
    /// ever disagree, a row shows a different `2/3` and a different total from
    /// the detail pane beside it — so they are checked against each other
    /// rather than each against a hand-written expectation.
    #[test]
    fn the_index_answers_exactly_what_scanning_would() {
        let mut p = projection();
        let top = filed(&mut p, "top");
        let other = filed(&mut p, "other");
        let childless = filed(&mut p, "childless");
        for title in ["a", "b", "c"] {
            let child = filed(&mut p, title);
            p.set_parent(child, Some(top)).unwrap();
        }
        let settled = filed(&mut p, "settled");
        p.set_parent(settled, Some(other)).unwrap();
        p.patch(settled, IssuePatch::default().status(Status::Done))
            .unwrap();

        let index = p.sub_issue_index();
        for issue in p.issues() {
            let scanned = p.sub_issues(issue.id);
            assert_eq!(
                index.of_issue(issue.id),
                scanned.as_slice(),
                "parts of #{}",
                issue.id
            );
            assert_eq!(
                index.settled_progress(issue.id),
                p.settled_progress(issue.id),
                "progress of #{}",
                issue.id
            );
        }
        assert!(index.of_issue(childless).is_empty());
        assert_eq!(index.settled_progress(childless), None);
        assert_eq!(index.settled_progress(top), Some((0, 3)));
        assert_eq!(index.settled_progress(other), Some((1, 1)));
    }

    #[test]
    fn a_sub_issue_is_never_offered_anything_to_hold() {
        // The gap: this used to return a full list for a parent that was
        // itself a sub-issue, so every candidate would be refused on click.
        // It was only ever right because the detail pane hides that picker.
        let mut p = projection();
        let top = filed(&mut p, "top");
        let middle = filed(&mut p, "middle");
        filed(&mut p, "loner");
        p.set_parent(middle, Some(top)).unwrap();

        assert!(
            p.eligible_sub_issues(middle).is_empty(),
            "a sub-issue cannot take sub-issues"
        );
        offers_agree_with_refusals(&p);
    }

    #[test]
    fn what_is_offered_is_exactly_what_would_be_accepted() {
        let mut p = projection();
        let parent = filed(&mut p, "parent");
        let child = filed(&mut p, "child");
        let done = filed(&mut p, "done");
        let settled = filed(&mut p, "settled");
        let loner = filed(&mut p, "loner");

        p.set_parent(child, Some(parent)).unwrap();
        p.patch(done, IssuePatch::default().status(Status::Done))
            .unwrap();
        p.patch(settled, IssuePatch::default().status(Status::Cancelled))
            .unwrap();

        // Every combination, in a corpus holding a parent, a sub-issue, a
        // Done issue, a Cancelled one and an unattached one.
        offers_agree_with_refusals(&p);

        // And the individual rules, so a failure above is readable.
        assert!(!p.eligible_parents(child).iter().any(|i| i.id == child));
        assert!(
            !p.eligible_parents(loner).iter().any(|i| i.id == done),
            "a Done issue cannot take on outstanding work"
        );
        assert!(
            p.eligible_parents(settled).iter().any(|i| i.id == done),
            "but it can take on work that is already settled"
        );
        assert!(
            !p.eligible_parents(loner).iter().any(|i| i.id == child),
            "a sub-issue cannot be a parent"
        );
    }

    #[test]
    fn a_new_projection_is_empty() {
        assert!(projection().issues().is_empty());
    }

    #[test]
    fn create_with_no_extra_fields_matches_the_stores_defaults() {
        let mut p = projection();
        let issue = p.create("plain", IssuePatch::default(), None).unwrap();

        assert_eq!(issue.title, "plain");
        assert_eq!(issue.status, Status::Todo);
        assert_eq!(issue.priority, Priority::None);
        assert!(issue.tags.is_empty());
    }

    #[test]
    fn create_applies_the_rest_of_the_fields_in_one_call() {
        let mut p = projection();
        let issue = p
            .create(
                "full",
                IssuePatch::default()
                    .body("described")
                    .status(Status::Doing)
                    .priority(Priority::Urgent)
                    .tags(vec![tag("Bug")]),
                None,
            )
            .unwrap();

        assert_eq!(issue.body, "described");
        assert_eq!(issue.status, Status::Doing);
        assert_eq!(issue.priority, Priority::Urgent);
        assert_eq!(issue.tags, vec![tag("Bug")]);
    }

    #[test]
    fn a_patch_touches_only_the_fields_it_names() {
        // The property the whole API design rests on: a caller setting Status
        // cannot clobber a title another caller is editing.
        let mut p = projection();
        let created = p.create("original", IssuePatch::default(), None).unwrap();

        p.patch(created.id, IssuePatch::default().status(Status::Done))
            .unwrap();

        let after = p.get(created.id).unwrap();
        assert_eq!(after.status, Status::Done);
        assert_eq!(
            after.title, "original",
            "title was not named, so not written"
        );
    }

    #[test]
    fn patching_an_unknown_issue_reports_rather_than_panics() {
        let mut p = projection();
        let err = p
            .patch(404, IssuePatch::default().title("ghost"))
            .unwrap_err();
        assert!(matches!(err, WriteError::NotFound(404)));
    }

    #[test]
    fn a_patch_that_changes_nothing_does_not_move_updated_at() {
        let mut p = projection();
        let created = p.create("stable", IssuePatch::default(), None).unwrap();

        p.patch(created.id, IssuePatch::default().title("stable"))
            .unwrap();

        assert_eq!(p.get(created.id).unwrap().updated_at, created.updated_at);
    }

    #[test]
    fn a_real_patch_does_move_updated_at() {
        let mut p = projection();
        let created = p.create("moving", IssuePatch::default(), None).unwrap();

        p.patch(created.id, IssuePatch::default().title("moved"))
            .unwrap();

        assert!(p.get(created.id).unwrap().updated_at >= created.updated_at);
    }

    #[test]
    fn tags_arriving_by_patch_fold_against_the_established_spelling() {
        let mut p = projection();
        let first = p
            .create("first", IssuePatch::default().tags(vec![tag("Bug")]), None)
            .unwrap();
        let second = p.create("second", IssuePatch::default(), None).unwrap();

        // Lowercase on the way in; the corpus already says "Bug".
        p.patch(second.id, IssuePatch::default().tags(vec![tag("bug")]))
            .unwrap();

        assert_eq!(p.get(second.id).unwrap().tags[0].as_str(), "Bug");
        assert_eq!(p.tags_in_use().len(), 1);
        assert_eq!(p.count_with_tag(&tag("BUG")), 2);
        assert_eq!(first.tags[0].as_str(), "Bug");
    }

    #[test]
    fn adding_a_tag_leaves_the_others_alone() {
        let mut p = projection();
        let issue = p
            .create("tagged", IssuePatch::default().tags(vec![tag("ui")]), None)
            .unwrap();

        p.add_tag(issue.id, tag("Bug")).unwrap();

        assert_eq!(p.get(issue.id).unwrap().tags, vec![tag("Bug"), tag("ui")]);
    }

    #[test]
    fn adding_a_tag_twice_is_idempotent() {
        let mut p = projection();
        let issue = p.create("tagged", IssuePatch::default(), None).unwrap();

        p.add_tag(issue.id, tag("Bug")).unwrap();
        p.add_tag(issue.id, tag("bug")).unwrap();

        assert_eq!(p.get(issue.id).unwrap().tags, vec![tag("Bug")]);
    }

    #[test]
    fn removing_a_tag_is_case_insensitive_and_idempotent() {
        let mut p = projection();
        let issue = p
            .create("tagged", IssuePatch::default().tags(vec![tag("Bug")]), None)
            .unwrap();

        p.remove_tag(issue.id, &tag("BUG")).unwrap();
        p.remove_tag(issue.id, &tag("BUG")).unwrap();

        assert!(p.get(issue.id).unwrap().tags.is_empty());
        assert!(p.tags_in_use().is_empty(), "the Tag went with its last use");
    }

    #[test]
    fn deleting_reports_whether_there_was_anything_to_delete() {
        let mut p = projection();
        let issue = p.create("doomed", IssuePatch::default(), None).unwrap();

        p.delete(issue.id).unwrap();
        assert!(
            matches!(p.delete(issue.id).unwrap_err(), WriteError::NotFound(_)),
            "already gone"
        );
        assert!(p.issues().is_empty());
    }

    // ---- sub-issues ---------------------------------------------------------

    fn refusal(err: WriteError) -> Refused {
        match err {
            WriteError::Refused(refusal) => refusal,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// A parent with `count` sub-issues, all Todo.
    fn family(p: &mut Projection, count: usize) -> (IssueId, Vec<IssueId>) {
        let parent = p.create("parent", IssuePatch::default(), None).unwrap().id;
        let children = (0..count)
            .map(|n| {
                let child = p
                    .create(&format!("child {n}"), IssuePatch::default(), None)
                    .unwrap();
                p.set_parent(child.id, Some(parent)).unwrap();
                child.id
            })
            .collect();
        (parent, children)
    }

    #[test]
    fn attaching_and_detaching_a_sub_issue() {
        let mut p = projection();
        let (parent, children) = family(&mut p, 2);

        assert_eq!(p.sub_issues(parent).len(), 2);
        assert_eq!(p.get(children[0]).unwrap().parent_id, Some(parent));

        p.set_parent(children[0], None).unwrap();
        assert_eq!(p.sub_issues(parent).len(), 1);
        assert_eq!(p.get(children[0]).unwrap().parent_id, None);
    }

    #[test]
    fn attaching_an_issue_that_already_has_a_parent_moves_it() {
        // There is no separate move operation: setting the parent is one.
        let mut p = projection();
        let (first, children) = family(&mut p, 1);
        let second = p
            .create("second parent", IssuePatch::default(), None)
            .unwrap()
            .id;

        p.set_parent(children[0], Some(second)).unwrap();

        assert!(p.sub_issues(first).is_empty(), "moved out of the first");
        assert_eq!(p.sub_issues(second).len(), 1);
    }

    #[test]
    fn hierarchy_is_one_level_deep_from_both_ends() {
        let mut p = projection();
        let (parent, children) = family(&mut p, 1);
        let outsider = p
            .create("outsider", IssuePatch::default(), None)
            .unwrap()
            .id;

        // A sub-issue cannot become a parent.
        assert_eq!(
            refusal(p.set_parent(outsider, Some(children[0])).unwrap_err()),
            Refused::ParentIsSubIssue(children[0])
        );
        // Something that already has parts cannot become a part.
        assert_eq!(
            refusal(p.set_parent(parent, Some(outsider)).unwrap_err()),
            Refused::ChildHasSubIssues(parent)
        );
    }

    #[test]
    fn an_issue_cannot_be_its_own_parent() {
        let mut p = projection();
        let issue = p.create("lonely", IssuePatch::default(), None).unwrap().id;
        assert_eq!(
            refusal(p.set_parent(issue, Some(issue)).unwrap_err()),
            Refused::SelfParent
        );
    }

    #[test]
    fn a_parent_is_not_done_until_nothing_under_it_is_outstanding() {
        let mut p = projection();
        let (parent, children) = family(&mut p, 2);

        assert!(!p.can_be_done(parent));
        assert_eq!(
            refusal(
                p.patch(parent, IssuePatch::default().status(Status::Done))
                    .unwrap_err()
            ),
            Refused::SubIssuesOutstanding(2)
        );

        p.patch(children[0], IssuePatch::default().status(Status::Done))
            .unwrap();
        assert!(!p.can_be_done(parent), "one still open");

        p.patch(children[1], IssuePatch::default().status(Status::Done))
            .unwrap();
        assert!(p.can_be_done(parent));
        p.patch(parent, IssuePatch::default().status(Status::Done))
            .unwrap();
        assert_eq!(p.get(parent).unwrap().status, Status::Done);
    }

    #[test]
    fn a_cancelled_sub_issue_counts_as_settled() {
        // Blocking on it would push you toward deleting a record the glossary
        // says is worth keeping.
        let mut p = projection();
        let (parent, children) = family(&mut p, 1);

        p.patch(children[0], IssuePatch::default().status(Status::Cancelled))
            .unwrap();

        assert!(p.can_be_done(parent));
        assert!(
            p.patch(parent, IssuePatch::default().status(Status::Done))
                .is_ok()
        );
    }

    #[test]
    fn a_parent_may_be_cancelled_with_work_still_open_beneath_it() {
        let mut p = projection();
        let (parent, _) = family(&mut p, 2);
        assert!(
            p.patch(parent, IssuePatch::default().status(Status::Cancelled))
                .is_ok(),
            "cancelling propagates to nothing and is never blocked"
        );
    }

    #[test]
    fn a_sub_issue_cannot_be_reopened_under_a_done_parent() {
        // The invariant has to be unreachable from below too, or it is only
        // true at the instant you press the button.
        let mut p = projection();
        let (parent, children) = family(&mut p, 1);
        p.patch(children[0], IssuePatch::default().status(Status::Done))
            .unwrap();
        p.patch(parent, IssuePatch::default().status(Status::Done))
            .unwrap();

        assert_eq!(
            refusal(
                p.patch(children[0], IssuePatch::default().status(Status::Todo))
                    .unwrap_err()
            ),
            Refused::ParentIsDone(parent)
        );
        // Cancelling it is still settled, so still allowed.
        assert!(
            p.patch(children[0], IssuePatch::default().status(Status::Cancelled))
                .is_ok()
        );
    }

    /// Only Done constrains a parent. Cancelling is a decision about the
    /// parent's own work, not a claim that everything beneath it is finished,
    /// so it neither blocks nor is blocked — which is what the glossary means
    /// by cancelling a Parent never being blocked. The spec said "Done or
    /// Cancelled" in two places while the code said Done; this pins which.
    #[test]
    fn a_cancelled_parent_takes_work_as_any_other_issue_does() {
        let mut p = projection();
        let parent = p
            .create(
                "abandoned",
                IssuePatch::default().status(Status::Cancelled),
                None,
            )
            .unwrap()
            .id;
        let open = p
            .create("still wanted", IssuePatch::default(), None)
            .unwrap()
            .id;

        assert!(p.set_parent(open, Some(parent)).is_ok());
        assert!(
            p.patch(open, IssuePatch::default().status(Status::Doing))
                .is_ok(),
            "and it can still be moved along underneath"
        );
    }

    #[test]
    fn a_done_parent_will_not_take_on_outstanding_work() {
        let mut p = projection();
        let parent = p
            .create("done", IssuePatch::default().status(Status::Done), None)
            .unwrap()
            .id;
        let open = p.create("open", IssuePatch::default(), None).unwrap().id;
        let closed = p
            .create("closed", IssuePatch::default().status(Status::Done), None)
            .unwrap()
            .id;

        assert_eq!(
            refusal(p.set_parent(open, Some(parent)).unwrap_err()),
            Refused::ParentAlreadyDone(parent)
        );
        assert!(
            p.set_parent(closed, Some(parent)).is_ok(),
            "already settled"
        );
    }

    #[test]
    fn detaching_is_always_allowed_even_from_a_done_parent() {
        let mut p = projection();
        let (parent, children) = family(&mut p, 1);
        p.patch(children[0], IssuePatch::default().status(Status::Done))
            .unwrap();
        p.patch(parent, IssuePatch::default().status(Status::Done))
            .unwrap();

        assert!(p.set_parent(children[0], None).is_ok());
    }

    #[test]
    fn deleting_a_parent_orphans_its_children_rather_than_erasing_them() {
        let mut p = projection();
        let (parent, children) = family(&mut p, 2);

        p.delete(parent).unwrap();

        assert_eq!(p.issues().len(), 2, "the work survives");
        for child in children {
            assert_eq!(p.get(child).unwrap().parent_id, None);
        }
    }

    #[test]
    fn orphaning_survives_a_reload() {
        // The foreign key does this, not us — guard that the schema agrees.
        let mut p = projection();
        let (parent, children) = family(&mut p, 1);
        p.delete(parent).unwrap();

        let reloaded = p.store.load_all().unwrap();
        let child = reloaded.iter().find(|i| i.id == children[0]).unwrap();
        assert_eq!(child.parent_id, None);
    }

    #[test]
    fn settled_progress_matches_the_completion_rule() {
        let mut p = projection();
        let (parent, children) = family(&mut p, 3);
        assert_eq!(p.settled_progress(parent), Some((0, 3)));

        p.patch(children[0], IssuePatch::default().status(Status::Done))
            .unwrap();
        p.patch(children[1], IssuePatch::default().status(Status::Cancelled))
            .unwrap();
        assert_eq!(p.settled_progress(parent), Some((2, 3)));
        assert!(!p.can_be_done(parent));

        assert_eq!(
            p.settled_progress(children[0]),
            None,
            "no children of its own"
        );
    }

    #[test]
    fn candidate_lists_exclude_what_would_be_refused() {
        let mut p = projection();
        let (parent, children) = family(&mut p, 1);
        let loose = p.create("loose", IssuePatch::default(), None).unwrap().id;

        let candidates: Vec<IssueId> = p
            .eligible_sub_issues(parent)
            .iter()
            .map(|issue| issue.id)
            .collect();
        assert!(candidates.contains(&loose));
        assert!(!candidates.contains(&parent), "not itself");
        assert!(!candidates.contains(&children[0]), "already there");

        // A parent cannot itself be filed under anything.
        assert!(p.eligible_parents(parent).is_empty());
        let parents: Vec<IssueId> = p
            .eligible_parents(loose)
            .iter()
            .map(|issue| issue.id)
            .collect();
        assert!(parents.contains(&parent));
        assert!(!parents.contains(&children[0]), "already a sub-issue");
    }

    #[test]
    fn issues_are_kept_in_display_order() {
        let mut p = projection();
        p.create("low", IssuePatch::default().priority(Priority::Low), None)
            .unwrap();
        let urgent = p
            .create(
                "urgent",
                IssuePatch::default().priority(Priority::Urgent),
                None,
            )
            .unwrap();

        assert_eq!(p.issues()[0].id, urgent.id);
    }

    /// The Issues as SQLite has them, which is what a restart would show.
    fn on_disk(p: &Projection) -> Vec<Issue> {
        let mut issues = p.store.load_all().unwrap();
        sort_for_display(&mut issues);
        issues
    }

    #[test]
    fn a_patch_the_store_refuses_leaves_memory_untouched() {
        // ADR-0002 from the other side: a change the database never took must
        // not be shown either, or it is on screen until a restart erases it.
        let mut p = projection();
        let id = filed(&mut p, "before");
        p.store.sabotage("PRAGMA query_only = ON;");

        let err = p
            .patch(
                id,
                IssuePatch::default().title("after").tags(vec![tag("ui")]),
            )
            .unwrap_err();

        assert!(matches!(err, WriteError::Store(_)));
        assert_eq!(p.get(id).unwrap().title, "before");
        assert!(p.get(id).unwrap().tags.is_empty());
        assert_eq!(p.issues(), on_disk(&p).as_slice());
    }

    #[test]
    fn an_attach_the_store_refuses_leaves_memory_untouched() {
        let mut p = projection();
        let parent = filed(&mut p, "parent");
        let child = filed(&mut p, "child");
        p.store.sabotage("PRAGMA query_only = ON;");

        let err = p.set_parent(child, Some(parent)).unwrap_err();

        assert!(matches!(err, WriteError::Store(_)));
        assert_eq!(p.get(child).unwrap().parent_id, None);
        assert!(p.sub_issues(parent).is_empty());
        assert_eq!(p.issues(), on_disk(&p).as_slice());
    }

    #[test]
    fn a_create_that_fails_part_way_files_nothing() {
        // The row is written before its Tags, so failing on the Tags is the
        // half-written case: nothing at all may survive it.
        let mut p = projection();
        p.store.sabotage(
            "CREATE TEMP TRIGGER no_tags BEFORE INSERT ON issue_tag
             BEGIN SELECT RAISE(ABORT, 'sabotaged'); END;",
        );

        let err = p
            .create("tagged", IssuePatch::default().tags(vec![tag("ui")]), None)
            .unwrap_err();

        assert!(matches!(err, WriteError::Store(_)));
        assert!(p.issues().is_empty());
        assert!(on_disk(&p).is_empty());
    }

    #[test]
    fn a_create_under_a_parent_is_filed_there_in_one_step() {
        let mut p = projection();
        let parent = filed(&mut p, "parent");

        let child = p
            .create("child", IssuePatch::default(), Some(parent))
            .unwrap();

        assert_eq!(child.parent_id, Some(parent));
        assert_eq!(child.created_at, child.updated_at, "one write, not two");
        assert_eq!(p.issues(), on_disk(&p).as_slice());
    }

    #[test]
    fn a_create_under_a_refusing_parent_files_nothing() {
        let mut p = projection();
        let parent = p
            .create("done", IssuePatch::default().status(Status::Done), None)
            .unwrap()
            .id;

        let err = p
            .create("open work", IssuePatch::default(), Some(parent))
            .unwrap_err();

        assert!(matches!(
            err,
            WriteError::Refused(Refused::ParentAlreadyDone(id)) if id == parent
        ));
        assert_eq!(p.issues().len(), 1);
        assert!(matches!(
            p.create("orphan", IssuePatch::default(), Some(404)),
            Err(WriteError::NotFound(404))
        ));
        assert_eq!(on_disk(&p).len(), 1);
    }

    #[test]
    fn writes_survive_a_reload_from_the_same_database() {
        // Guards the ADR-0002 failure mode: an in-memory change that never
        // reached SQLite looks identical until you restart.
        let mut p = projection();
        let issue = p
            .create(
                "persisted",
                IssuePatch::default()
                    .status(Status::Blocked)
                    .tags(vec![tag("ui")]),
                None,
            )
            .unwrap();

        let reloaded = p.store.load_all().unwrap();
        let found = reloaded.iter().find(|i| i.id == issue.id).unwrap();
        assert_eq!(found.status, Status::Blocked);
        assert_eq!(found.tags, vec![tag("ui")]);
    }
}
