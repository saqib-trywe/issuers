// SPDX-License-Identifier: GPL-3.0-only

//! Right column: inline editing of the selected Issue.
//!
//! Edits auto-save on a debounce; there is no save button. Status and
//! Priority are segmented buttons rather than dropdowns — with five fixed
//! values each, a menu costs a click and buys nothing.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::combobox::Combobox;
use gpui_component::input::{Input, Textarea};
use gpui_component::tag::Tag as TagChip;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt};

use super::tag_colour::colour_for;
use super::tracker::IssueTracker;
use issuers::domain::{Issue, IssueId, Priority, Status, Tag};

impl IssueTracker {
    pub(super) fn render_issue_detail(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(issue) = self.selected_issue(cx) else {
            return div()
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Select an issue, or press c to create one.")
                .into_any_element();
        };

        let id = issue.id;
        let title = issue.display_title().to_string();
        let status = issue.status;
        let priority = issue.priority;
        let tags = issue.tags.clone();
        let created = issue.created_at.format("%Y-%m-%d %H:%M").to_string();
        let updated = issue.updated_at.format("%Y-%m-%d %H:%M").to_string();
        let progress = self.settled_progress(id, cx);
        let sub_issues = self.selected_sub_issues(cx);
        let rollup = issuers::domain::SizeRollup::of(issue, &sub_issues.iter().collect::<Vec<_>>());
        let parent = self.selected_parent(cx);
        let sub_issue_count = sub_issues.len();

        div()
            .id("detail-scroll")
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("#{id}")),
                    )
                    .child(
                        Button::new("delete-issue")
                            .danger()
                            .small()
                            .label("Delete")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_delete(id, title.clone(), sub_issue_count, window, cx);
                            })),
                    ),
            )
            .child(Input::new(&self.title_input))
            .child(self.render_status_row(status, progress, cx))
            .child(self.render_priority_row(priority, cx))
            .child(self.render_size_row(rollup, sub_issues.len(), cx))
            .child(self.render_tag_editor(tags, cx))
            .child(self.render_relationships(sub_issues, parent, cx))
            .child(Textarea::new(&self.body_input))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Created {created} · Updated {updated}")),
            )
            .into_any_element()
    }

    /// Status buttons, with Done disabled while sub-issues are outstanding.
    ///
    /// The tooltip is not decoration: a control that refuses to work without
    /// saying why is worse than one that rejects you afterwards. It does show
    /// on a disabled button — `Button` attaches its tooltip outside the
    /// disabled guard, and only `on_click` short-circuits.
    fn render_status_row(
        &self,
        active: Status,
        progress: Option<(usize, usize)>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let outstanding = progress.map_or(0, |(settled, total)| total - settled);

        div()
            .flex()
            .flex_row()
            .gap_1()
            .children(Status::ALL.into_iter().map(move |status| {
                let blocked = status == Status::Done && outstanding > 0;
                Button::new(SharedString::from(format!("status-{}", status.label())))
                    .small()
                    .label(status.label())
                    .when(status == active, |button| button.primary())
                    .when(status != active, |button| button.outline())
                    .when(blocked, |button| {
                        button
                            .disabled(true)
                            .tooltip(format!("{outstanding} sub-issue(s) still outstanding"))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_status(status, window, cx);
                    }))
            }))
    }

    /// The Issue's own Size, and what the family comes to.
    ///
    /// A number rather than a row of buttons: a Size has 256 values, and it is
    /// a judgement you type. The total is shown only when there are parts to
    /// add up, and the sized-count beside it stops the figure reading as
    /// complete when it is a lower bound.
    fn render_size_row(
        &self,
        rollup: issuers::domain::SizeRollup,
        parts: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let summary = match (parts, rollup.total) {
            (0, _) => None,
            (parts, Some(total)) => Some(format!(
                "total {total} · {} of {parts} part(s) sized",
                parts - rollup.unsized_sub_issues
            )),
            (parts, None) => Some(format!("no part of {parts} is sized")),
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Size"),
            )
            .child(
                div()
                    .w(px(72.0))
                    .child(Input::new(&self.size_input).small()),
            )
            .children(summary.map(|summary| {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary)
            }))
    }

    /// Sub-issues and parent.
    ///
    /// Hierarchy is one level deep, so an Issue is either a parent or a part,
    /// never both — which is why only one of the two controls is offered once
    /// the Issue has committed to a direction.
    fn render_relationships(
        &self,
        sub_issues: Vec<Issue>,
        parent: Option<Issue>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let is_part = parent.is_some();
        let has_parts = !sub_issues.is_empty();

        div()
            .flex()
            .flex_col()
            .gap_2()
            // Offered while this Issue is nobody's part.
            .when(!is_part, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Sub-issues"),
                        )
                        .children(
                            sub_issues
                                .into_iter()
                                .map(|child| Self::render_sub_issue_row(child, cx)),
                        )
                        .child(
                            Combobox::new(&self.sub_issue_select)
                                .small()
                                .placeholder("Add a sub-issue…")
                                .search_placeholder("Find an issue…")
                                .menu_width(px(320.0)),
                        ),
                )
            })
            // Offered while this Issue has no parts of its own.
            .when(!has_parts, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Part of"),
                        )
                        // Picking a different Issue moves it; clearing detaches.
                        .child(
                            Combobox::new(&self.parent_select)
                                .small()
                                .cleanable(true)
                                .placeholder("Not part of another issue")
                                .search_placeholder("Find an issue…")
                                .menu_width(px(320.0)),
                        ),
                )
            })
    }

    fn render_sub_issue_row(issue: Issue, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = issue.id;
        let title = issue.display_title().to_string();
        let status = issue.status;

        div()
            .id(SharedString::from(format!("sub-issue-{id}")))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().muted))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_issue(id, window, cx);
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("#{id}")),
            )
            .child(div().flex_1().text_sm().child(title))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(status.label()),
            )
            .child(
                Button::new(SharedString::from(format!("detach-{id}")))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Close).xsmall())
                    .tooltip("Remove from this issue")
                    .tab_stop(false)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.detach_sub_issue(id, window, cx);
                    })),
            )
    }

    fn render_priority_row(&self, active: Priority, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .gap_1()
            .children(Priority::ALL.into_iter().map(|priority| {
                Button::new(SharedString::from(format!("priority-{}", priority.label())))
                    .small()
                    .label(priority.label())
                    .when(priority == active, |button| button.primary())
                    .when(priority != active, |button| button.outline())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_priority(priority, window, cx);
                    }))
            }))
    }

    /// Tags: the chips are the display and the way to remove one, the
    /// Combobox is the way to add one.
    ///
    /// Removal deliberately does not go through the Combobox's own selection
    /// API, which mutates its state without emitting a change event — the
    /// Issue stays the single source of truth and the picker is re-pointed at
    /// it afterwards.
    fn render_tag_editor(
        &self,
        tags: Vec<Tag>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let this = cx.entity();

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .children(tags.into_iter().map(|tag| {
                        let colour = colour_for(&tag);
                        let key = tag.key();
                        let name = tag.as_str().to_owned();
                        TagChip::color(colour).small().child(name).child(
                            Button::new(SharedString::from(format!("remove-tag-{key}")))
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::Close).xsmall())
                                .tab_stop(false)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.remove_tag(tag.clone(), window, cx);
                                })),
                        )
                    })),
            )
            .child(
                Combobox::new(&self.tag_select)
                    .small()
                    .placeholder("Add a tag…")
                    .search_placeholder("Find or create a tag…")
                    .menu_width(px(220.0))
                    // The library's create recipe only adds the typed name to
                    // the dropdown; this puts it straight on the Issue.
                    //
                    // The label cannot quote what you typed: this closure runs
                    // inside `ComboboxState::render`, so reading that entity
                    // for its query would be a double lease, and the callback
                    // is handed no context to reach it another way. Clicking
                    // with an empty search box is a no-op.
                    .footer(move |_, cx| {
                        let this = this.clone();
                        Button::new("create-tag")
                            .ghost()
                            .w_full()
                            .justify_start()
                            .text_color(cx.theme().foreground)
                            .icon(Icon::new(IconName::Plus))
                            .label("Create tag from search")
                            .on_click(move |_, window, cx| {
                                this.update(cx, |this, cx| this.create_tag_from_query(window, cx));
                            })
                    }),
            )
    }

    /// Deleting erases an Issue outright, so it is always confirmed. To
    /// abandon work while keeping the record, set the Status to Cancelled.
    pub(super) fn confirm_delete(
        &mut self,
        id: IssueId,
        title: String,
        sub_issue_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity();
        // Says what happens to the parts, because erasing work nobody asked to
        // erase is the one unrecoverable mistake here — and it does not happen.
        let released = match sub_issue_count {
            0 => String::new(),
            1 => " Its 1 sub-issue will be kept, no longer part of anything.".to_string(),
            many => format!(" Its {many} sub-issues will be kept, no longer part of anything."),
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title("Delete this issue?")
                .description(format!(
                    "#{id} “{title}” will be erased.{released} To abandon it but keep \
                     the record, set its status to Cancelled instead."
                ))
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| this.delete_issue(id, window, cx));
                    true
                })
        });
    }
}
