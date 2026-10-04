// SPDX-License-Identifier: GPL-3.0-only

//! Turning API responses into text.
//!
//! Pure with respect to the network: everything here takes already-fetched
//! values, which is what makes it testable without a socket.

use chrono::{DateTime, Local, Utc};
use termcolor::{Color, ColorSpec, WriteColor};

use crate::api::wire::{IssueJson, TagJson};
use crate::domain::{Priority, Status, display_title, wall_clock};

/// How much of a title survives. Fixed rather than measured: fitting the
/// terminal would mean a `TIOCGWINSZ` ioctl, and an `unsafe` block for column
/// alignment is a poor trade.
const TITLE_WIDTH: usize = 52;

const STATUS_WIDTH: usize = 9;
const PRIORITY_WIDTH: usize = 8;

/// Wide enough for a four-digit total, which is more than a one-level tree of
/// `u8` Sizes reaches in practice.
const SIZE_WIDTH: usize = 5;

pub fn list(out: &mut dyn WriteColor, issues: &[IssueJson]) -> std::io::Result<()> {
    if issues.is_empty() {
        return writeln!(out, "No issues.");
    }

    let id_width = issues
        .iter()
        .map(|issue| issue.id.to_string().len())
        .max()
        .unwrap_or(2)
        .max(2);

    dim(out, |out| {
        writeln!(
            out,
            "{:>id_width$}  {:<STATUS_WIDTH$}  {:<PRIORITY_WIDTH$}  {:>SIZE_WIDTH$}  \
             {:<TITLE_WIDTH$}  TAGS",
            "ID", "STATUS", "PRIORITY", "SIZE", "TITLE"
        )
    })?;

    for issue in issues {
        write!(out, "{:>id_width$}  ", issue.id)?;
        coloured(out, status_colour(&issue.status), |out| {
            write!(out, "{:<STATUS_WIDTH$}  ", issue.status)
        })?;
        coloured(out, priority_colour(&issue.priority), |out| {
            write!(out, "{:<PRIORITY_WIDTH$}  ", issue.priority)
        })?;
        // The total, not the Issue's own Size: a Parent's row should say what
        // the whole family comes to.
        let size = issue
            .total_size
            .map(|total| total.to_string())
            .unwrap_or_default();
        write!(out, "{size:>SIZE_WIDTH$}  ")?;
        write!(out, "{:<TITLE_WIDTH$}  ", title_cell(issue))?;
        dim(out, |out| writeln!(out, "{}", issue.tags.join(", ")))?;
    }
    Ok(())
}

pub fn show(
    out: &mut dyn WriteColor,
    issue: &IssueJson,
    parent: Option<&IssueJson>,
    children: &[IssueJson],
) -> std::io::Result<()> {
    writeln!(out, "#{} {}", issue.id, display_title(&issue.title))?;

    write!(out, "  Status    ")?;
    coloured(out, status_colour(&issue.status), |out| {
        write!(out, "{}", issue.status)
    })?;
    let outstanding = issue.sub_issue_ids.len() - issue.settled_sub_issues;
    if outstanding > 0 {
        dim(out, |out| {
            write!(out, "  ({outstanding} sub-issue(s) outstanding)")
        })?;
    }
    writeln!(out)?;

    write!(out, "  Priority  ")?;
    coloured(out, priority_colour(&issue.priority), |out| {
        writeln!(out, "{}", issue.priority)
    })?;

    // Absent when nothing in the family carries a Size: a tracker that never
    // sizes anything should not grow a row saying so.
    if let Some(total) = issue.total_size {
        write!(out, "  Size      ")?;
        // An en dash for a Parent that carries no Size of its own: its parts
        // account for all of it.
        match issue.size {
            Some(own) => write!(out, "{own}")?,
            None => write!(out, "\u{2013}")?,
        }
        if !issue.sub_issue_ids.is_empty() {
            let sized = issue.sub_issue_ids.len() - issue.unsized_sub_issues;
            write!(out, "  ")?;
            dim(out, |out| {
                write!(
                    out,
                    "(total {total}, {sized} of {} part(s) sized)",
                    issue.sub_issue_ids.len()
                )
            })?;
        }
        writeln!(out)?;
    }

    if !issue.tags.is_empty() {
        writeln!(out, "  Tags      {}", issue.tags.join(", "))?;
    }

    // Titles rather than bare ids: an id you then have to look up is not an
    // answer, and both are one request away.
    if let Some(parent) = parent {
        writeln!(
            out,
            "  Part of   #{} {}",
            parent.id,
            display_title(&parent.title)
        )?;
    }
    if !children.is_empty() {
        let done = issue.settled_sub_issues;
        writeln!(out, "  Sub-issues ({done}/{} done)", children.len())?;
        for child in children {
            write!(out, "    #{:<5} ", child.id)?;
            coloured(out, status_colour(&child.status), |out| {
                write!(out, "{:<STATUS_WIDTH$}", child.status)
            })?;
            writeln!(out, "  {}", display_title(&child.title))?;
        }
    }

    dim(out, |out| {
        writeln!(
            out,
            "  Created   {}\n  Updated   {}",
            timestamp(&issue.created_at),
            timestamp(&issue.updated_at)
        )
    })?;

    if !issue.body.trim().is_empty() {
        writeln!(out, "\n{}", issue.body.trim_end())?;
    }
    Ok(())
}

pub fn tags(out: &mut dyn WriteColor, tags: &[TagJson]) -> std::io::Result<()> {
    if tags.is_empty() {
        return writeln!(out, "No tags.");
    }
    let width = tags.iter().map(|tag| tag.name.len()).max().unwrap_or(0);
    for tag in tags {
        write!(out, "{:<width$}  ", tag.name)?;
        dim(out, |out| writeln!(out, "{}", tag.count))?;
    }
    Ok(())
}

// ---- the sub-issue markers --------------------------------------------------

///
/// Built from whichever Issues were fetched. `list` completes it with a second
/// request when a filter left some children out, rather than printing a
/// fraction that quietly excludes them.
fn title_cell(issue: &IssueJson) -> String {
    let prefix = match issue.parent_id {
        Some(parent) => format!("↳ #{parent} "),
        None => String::new(),
    };
    // The API sends the fraction, so a filter that hides a parent's children
    // can no longer make it quietly wrong.
    let suffix = match issue.sub_issue_ids.len() {
        0 => String::new(),
        total => format!(" [{}/{total}]", issue.settled_sub_issues),
    };

    let budget = TITLE_WIDTH.saturating_sub(width(&prefix) + width(&suffix));
    format!(
        "{prefix}{}{suffix}",
        truncate(display_title(&issue.title), budget)
    )
}

fn truncate(text: &str, budget: usize) -> String {
    if width(text) <= budget {
        return text.to_string();
    }
    // One column is spent on the ellipsis that says something was cut.
    let keep = budget.saturating_sub(1);
    text.chars().take(keep).collect::<String>() + "…"
}

/// Character count, not byte length — `format!`'s width counts characters too,
/// so alignment holds for a title containing anything non-ASCII.
fn width(text: &str) -> usize {
    text.chars().count()
}

/// RFC3339 with microseconds is right on the wire and noise on a screen.
fn timestamp(raw: &str) -> String {
    match DateTime::parse_from_rfc3339(raw) {
        Ok(at) => wall_clock(at.with_timezone(&Utc), &Local),
        Err(_) => raw.to_string(),
    }
}

// ---- colour -----------------------------------------------------------------

/// Parsed rather than string-matched, so that adding a sixth Status is:
/// a sixth Status is then a non-exhaustive match here — a compile error —
/// rather than a column that silently loses its colour.
fn status_colour(status: &str) -> Option<Color> {
    match status.parse::<Status>().ok()? {
        Status::Todo => None,
        Status::Doing => Some(Color::Blue),
        Status::Blocked => Some(Color::Red),
        Status::Done => Some(Color::Green),
        Status::Cancelled => Some(Color::Magenta),
    }
}

fn priority_colour(priority: &str) -> Option<Color> {
    match priority.parse::<Priority>().ok()? {
        Priority::Urgent => Some(Color::Red),
        Priority::High => Some(Color::Yellow),
        Priority::None | Priority::Low | Priority::Medium => None,
    }
}

fn coloured(
    out: &mut dyn WriteColor,
    colour: Option<Color>,
    body: impl FnOnce(&mut dyn WriteColor) -> std::io::Result<()>,
) -> std::io::Result<()> {
    match colour {
        Some(colour) => {
            out.set_color(ColorSpec::new().set_fg(Some(colour)))?;
            let result = body(out);
            out.reset()?;
            result
        }
        None => body(out),
    }
}

fn dim(
    out: &mut dyn WriteColor,
    body: impl FnOnce(&mut dyn WriteColor) -> std::io::Result<()>,
) -> std::io::Result<()> {
    out.set_color(ColorSpec::new().set_dimmed(true))?;
    let result = body(out);
    out.reset()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::IssueId;
    use termcolor::Buffer;

    fn issue(id: IssueId, title: &str, status: &str) -> IssueJson {
        IssueJson {
            id,
            title: title.to_string(),
            body: String::new(),
            status: status.to_string(),
            priority: "Medium".to_string(),
            tags: Vec::new(),
            parent_id: None,
            sub_issue_ids: Vec::new(),
            settled_sub_issues: 0,
            size: None,
            total_size: None,
            unsized_sub_issues: 0,
            created_at: "2026-09-05T14:23:11.482913Z".to_string(),
            updated_at: "2026-09-05T14:23:11.482913Z".to_string(),
        }
    }

    fn text(render: impl FnOnce(&mut Buffer) -> std::io::Result<()>) -> String {
        let mut buffer = Buffer::no_color();
        render(&mut buffer).unwrap();
        String::from_utf8(buffer.into_inner()).unwrap()
    }

    #[test]
    fn a_blank_title_reads_the_same_here_as_it_does_in_the_window() {
        // The window shows "Untitled" for a title the user has not typed yet.
        // Printing the raw title instead gave one Issue two names depending
        // on where you looked at it.
        let listed = text(|buffer| list(buffer, &[issue(1, "   ", "Todo")]));
        assert!(listed.contains("Untitled"), "{listed}");

        let mut blank = issue(1, "   ", "Todo");
        blank.parent_id = Some(2);
        let parent = issue(2, "", "Doing");

        let shown = text(|buffer| show(buffer, &blank, Some(&parent), &[]));
        assert!(shown.contains("#1 Untitled"), "{shown}");
        assert!(shown.contains("Part of   #2 Untitled"), "{shown}");
    }

    #[test]
    fn every_status_and_priority_label_is_understood_by_the_colours() {
        // Todo and the low priorities are deliberately uncoloured; everything
        // else must be recognised. A label the colour function did not know
        // used to fall through to "no colour", which looks like a style
        // choice rather than the bug it is.
        for status in Status::ALL {
            assert_eq!(
                status_colour(status.label()).is_some(),
                status != Status::Todo,
                "{}",
                status.label()
            );
        }
        for priority in Priority::ALL {
            let loud = matches!(priority, Priority::Urgent | Priority::High);
            assert_eq!(
                priority_colour(priority.label()).is_some(),
                loud,
                "{}",
                priority.label()
            );
        }
    }

    #[test]
    fn an_empty_list_says_so_rather_than_printing_a_header() {
        let out = text(|buffer| list(buffer, &[]));
        assert_eq!(out, "No issues.\n");
    }

    #[test]
    fn a_parent_shows_progress_and_a_sub_issue_names_its_parent() {
        let mut parent = issue(1, "Ship the CLI", "Doing");
        parent.sub_issue_ids = vec![2, 3];
        parent.settled_sub_issues = 1;
        let mut done = issue(2, "Parse arguments", "Done");
        done.parent_id = Some(1);
        let mut todo = issue(3, "Write the client", "Todo");
        todo.parent_id = Some(1);

        let out = text(|buffer| list(buffer, &[parent, done, todo]));

        assert!(out.contains("Ship the CLI [1/2]"), "{out}");
        assert!(out.contains("↳ #1 Parse arguments"), "{out}");
    }

    #[test]
    fn a_progress_fraction_survives_a_filter_that_hides_the_children() {
        // The whole point of sending it: the parent alone is enough, where
        // the CLI used to need a second request to get this right.
        let mut parent = issue(1, "p", "Todo");
        parent.sub_issue_ids = vec![2, 3];
        parent.settled_sub_issues = 1;

        let out = text(|buffer| list(buffer, &[parent]));
        assert!(out.contains("[1/2]"), "{out}");
    }

    #[test]
    fn an_unsized_tracker_grows_no_size_row() {
        let out = text(|buffer| show(buffer, &issue(1, "unsized", "Todo"), None, &[]));
        assert!(!out.contains("Size"), "{out}");

        let listed = text(|buffer| list(buffer, &[issue(1, "unsized", "Todo")]));
        // The column is always in the header; the cell is blank.
        assert!(listed.contains("SIZE"), "{listed}");
        assert!(listed.contains("unsized"), "{listed}");
    }

    #[test]
    fn a_leaf_shows_its_own_size_and_nothing_about_parts() {
        let mut leaf = issue(1, "leaf", "Todo");
        leaf.size = Some(5);
        leaf.total_size = Some(5);

        let out = text(|buffer| show(buffer, &leaf, None, &[]));
        assert!(out.contains("Size      5"), "{out}");
        assert!(!out.contains("part(s) sized"), "{out}");
    }

    #[test]
    fn a_parent_shows_its_own_size_beside_the_total() {
        // The Parent's own 3 is the work its parts do not cover, so the total
        // is 3 + 5 + 8 rather than 5 + 8.
        let mut parent = issue(1, "parent", "Todo");
        parent.size = Some(3);
        parent.sub_issue_ids = vec![2, 3, 4];
        parent.unsized_sub_issues = 1;
        parent.total_size = Some(16);

        let out = text(|buffer| show(buffer, &parent, None, &[]));
        assert!(out.contains("Size      3"), "its own: {out}");
        assert!(
            out.contains("(total 16, 2 of 3 part(s) sized)"),
            "and the whole: {out}"
        );
    }

    #[test]
    fn a_parent_carrying_no_size_of_its_own_still_totals_its_parts() {
        let mut parent = issue(1, "parent", "Todo");
        parent.sub_issue_ids = vec![2];
        parent.total_size = Some(5);

        let out = text(|buffer| show(buffer, &parent, None, &[]));
        assert!(out.contains("Size      \u{2013}"), "an en dash: {out}");
        assert!(out.contains("total 5"), "{out}");
    }

    #[test]
    fn the_list_column_carries_the_total_not_the_own_size() {
        // A Parent's row should say what the whole family comes to.
        let mut parent = issue(1, "parent", "Todo");
        parent.size = Some(3);
        parent.sub_issue_ids = vec![2];
        parent.total_size = Some(11);

        let listed = text(|buffer| list(buffer, &[parent]));
        assert!(listed.contains("11"), "{listed}");
        assert!(!listed.contains(" 3  "), "not the own size: {listed}");
    }

    #[test]
    fn an_issue_with_no_sub_issues_shows_no_fraction() {
        let out = text(|buffer| list(buffer, &[issue(1, "alone", "Todo")]));
        assert!(!out.contains("["), "{out}");
    }

    #[test]
    fn a_long_title_is_cut_without_losing_the_markers() {
        let mut long = issue(1, &"x".repeat(200), "Todo");
        long.parent_id = Some(9);
        long.sub_issue_ids = vec![2];

        let cell = title_cell(&long);
        assert_eq!(width(&cell), TITLE_WIDTH);
        assert!(cell.starts_with("↳ #9 "), "{cell}");
        assert!(cell.ends_with("[0/1]"), "{cell}");
        assert!(cell.contains('…'), "{cell}");
    }

    #[test]
    fn a_short_title_is_left_alone() {
        assert_eq!(truncate("brief", 20), "brief");
        assert_eq!(truncate("exactly-ten", 11), "exactly-ten");
    }

    #[test]
    fn show_names_the_related_issues_rather_than_their_ids_alone() {
        let mut child = issue(5, "the child", "Todo");
        child.parent_id = Some(1);
        child.body = "some prose".to_string();
        let parent = issue(1, "the parent", "Doing");

        let out = text(|buffer| show(buffer, &child, Some(&parent), &[]));
        assert!(out.contains("#5 the child"), "{out}");
        assert!(out.contains("Part of   #1 the parent"), "{out}");
        assert!(out.contains("some prose"), "{out}");
    }

    #[test]
    fn show_counts_outstanding_work_under_a_parent() {
        // The count comes from the parent, as the API sent it — not from
        // counting the children that happen to have been fetched.
        let mut parent = issue(1, "the parent", "Todo");
        parent.sub_issue_ids = vec![2, 3];
        parent.settled_sub_issues = 1;
        let children = vec![issue(2, "done one", "Done"), issue(3, "not yet", "Todo")];

        let out = text(|buffer| show(buffer, &parent, None, &children));
        assert!(out.contains("1 sub-issue(s) outstanding"), "{out}");
        assert!(out.contains("Sub-issues (1/2 done)"), "{out}");
    }

    #[test]
    fn timestamps_lose_their_microseconds() {
        let rendered = timestamp("2026-09-05T14:23:11.482913Z");
        assert!(!rendered.contains('.'), "{rendered}");
        assert!(rendered.starts_with("2026-09-05") || rendered.starts_with("2026-09-0"));
        // Anything unparseable is passed through rather than hidden.
        assert_eq!(timestamp("not a date"), "not a date");
    }

    #[test]
    fn tags_are_listed_with_their_counts() {
        let out = text(|buffer| {
            tags(
                buffer,
                &[
                    TagJson {
                        name: "bug".into(),
                        count: 3,
                    },
                    TagJson {
                        name: "ui".into(),
                        count: 1,
                    },
                ],
            )
        });
        assert_eq!(out, "bug  3\nui   1\n");
    }
}
