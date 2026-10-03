// SPDX-License-Identifier: GPL-3.0-only

//! Middle column: the filterable, keyboard-navigable Issue list.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::Input;
use gpui_component::sidebar::SidebarToggleButton;
use gpui_component::tag::Tag as TagChip;
use gpui_component::{ActiveTheme, Side, Sizable};

use super::tag_colour::colour_for;
use super::tracker::{IssueTracker, LIST_CONTEXT};
use issuers::domain::{IssueId, Priority, SizeRollup, Status, Tag};

impl IssueTracker {
    pub(super) fn render_issue_list(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.working.selected();
        // Collected into owned rows first: the Issues are borrowed from the
        // shared Projection through `cx`, and building elements needs `cx`
        // mutably. Only what a row displays is cloned — never the body.
        //
        // Both of the per-row hierarchy answers come from one index, built
        // once here. Asking the Projection per row meant two scans of the
        // whole corpus for every row painted.
        let projection = self.projection(cx);
        let sub_issues = projection.sub_issue_index();
        let visible: Vec<Row> = self
            .working
            .visible(projection.issues())
            .into_iter()
            .map(|issue| Row {
                id: issue.id,
                title: issue.display_title().to_string(),
                status: issue.status,
                priority: issue.priority,
                tags: issue.tags.clone(),
                parent_id: issue.parent_id,
                progress: sub_issues.settled_progress(issue.id),
                total_size: SizeRollup::of(issue, sub_issues.of_issue(issue.id)).total,
            })
            .collect();
        let is_empty = visible.is_empty();
        let mut rows = Vec::with_capacity(visible.len());
        for row in visible {
            let is_selected = selected == Some(row.id);
            rows.push(Self::render_row(row, is_selected, cx));
        }

        div()
            // Focusable, so navigation bindings apply only when the list has
            // focus and never while typing into an input.
            .track_focus(&self.list_focus)
            .key_context(LIST_CONTEXT)
            .w(px(380.0))
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.render_header(cx))
            .when(self.creating, |this| this.child(self.render_new_row(cx)))
            .child(
                div()
                    .id("issue-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .when(is_empty && !self.creating, |this| {
                        this.child(
                            div()
                                .p_4()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No issues here. Press c to create one."),
                        )
                    })
                    .children(rows),
            )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            // Lives here rather than in the sidebar so it stays reachable —
            // and in the same place — whether the sidebar is shown or hidden.
            .child(
                SidebarToggleButton::new()
                    .side(Side::Left)
                    .collapsed(self.sidebar_hidden())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
            )
            // With the sidebar hidden this is the only thing naming the
            // active View.
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.working.view().label()),
            )
            .child(div().flex_1().child(Input::new(&self.filter_input).small()))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("c new"),
            )
    }

    fn render_new_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .child(Input::new(&self.new_issue_input).small())
    }

    fn render_row(row: Row, is_selected: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let Row {
            id,
            title,
            status,
            priority,
            tags,
            parent_id,
            progress,
            total_size,
        } = row;

        div()
            .id(SharedString::from(format!("issue-{id}")))
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .cursor_pointer()
            .when(is_selected, |this| this.bg(cx.theme().accent))
            .when(!is_selected, |this| {
                this.hover(|style| style.bg(cx.theme().muted))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_issue(id, window, cx);
                this.list_focus.focus(window, cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("#{id}")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .when(is_selected, |this| {
                                this.text_color(cx.theme().accent_foreground)
                            })
                            .child(title),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(status.label())
                    .when(priority != Priority::None, |this| {
                        this.child(priority.label())
                    })
                    // Why the Done button is greyed out, visible without
                    // opening the Issue.
                    .when_some(progress, |this, (settled, total)| {
                        this.child(format!("{settled}/{total}"))
                    })
                    // The total, not this Issue's own Size: a row should say
                    // what the whole family comes to.
                    .when_some(total_size, |this, total| {
                        this.child(format!("\u{2211}{total}"))
                    })
                    .when_some(parent_id, |this, parent| this.child(format!("↳ #{parent}")))
                    // Colour is most of why a Tag is legible here at all —
                    // without it these are more grey text competing with the
                    // Status beside them.
                    .children(tags.into_iter().map(|tag| {
                        TagChip::color(colour_for(&tag))
                            .xsmall()
                            .child(tag.as_str().to_owned())
                    })),
            )
    }
}

/// One list row's worth of an Issue.
///
/// Exists so the borrow of the shared Projection can end before the element
/// tree is built, and so a row never clones an Issue body it does not show.
struct Row {
    id: IssueId,
    title: String,
    status: Status,
    priority: Priority,
    tags: Vec<Tag>,
    /// Set when this Issue is part of another one.
    parent_id: Option<IssueId>,
    /// Settled-over-total for this Issue's own parts, when it has any.
    progress: Option<(usize, usize)>,
    /// What the whole family comes to, when anything in it is sized.
    total_size: Option<u32>,
}
