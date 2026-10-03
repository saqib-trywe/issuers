// SPDX-License-Identifier: GPL-3.0-only

//! Left column: the View list.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::select::{Select, SelectState};
use gpui_component::tag::Tag as TagChip;
use gpui_component::{ActiveTheme, Sizable};

use super::tag_colour::colour_for;
use super::theme_catalogue::ThemeListDelegate;
use super::tracker::IssueTracker;
use issuers::domain::{Narrowing, Tag, View};

impl IssueTracker {
    pub(super) fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let active = self.working.view();
        // Counted straight off the shared corpus. This used to clone every
        // Issue, bodies included, to work out six integers — on every frame.
        let counts: Vec<(View, usize)> = {
            let issues = self.projection(cx).issues();
            View::ALL
                .into_iter()
                .map(|view| (view, Narrowing::for_view(view).count(issues)))
                .collect()
        };

        div()
            .w(px(232.0))
            .h_full()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Views"),
            )
            .children({
                let mut rows = Vec::with_capacity(counts.len());
                for (view, count) in counts {
                    rows.push(Self::render_view_row(view, count, view == active, cx));
                }
                rows
            })
            // `Option` is an iterator of zero or one, so an untagged database
            // drops this out of the tree rather than showing a heading over
            // nothing.
            .children(self.render_tags(cx))
            .child(self.render_appearance(cx))
    }

    /// The Tag list: every Tag some Issue carries, alphabetically.
    ///
    /// Alphabetical rather than by count so a Tag keeps its place — a list
    /// that reshuffles as counts drift is one you can never learn. This is the
    /// only unbounded part of the sidebar, so it is the part that scrolls;
    /// Views stay at the top and Appearance stays pinned to the bottom.
    fn render_tags(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let tags = self.tags_in_use(cx);
        if tags.is_empty() {
            return None;
        }

        let active = self.working.tag().cloned();
        let mut rows = Vec::with_capacity(tags.len());
        for tag in &tags {
            let count = self.count_for_tag(tag, cx);
            let is_active = active.as_ref() == Some(tag);
            rows.push(Self::render_tag_row(tag.clone(), count, is_active, cx));
        }

        Some(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .pt_2()
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Tags"),
                )
                .child(
                    div()
                        .id("tag-scroll")
                        .flex_1()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(rows),
                ),
        )
    }

    fn render_tag_row(
        tag: Tag,
        count: usize,
        is_active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let colour = colour_for(&tag);
        let name = tag.as_str().to_owned();

        div()
            // Keyed on the folded name, which is the Tag's actual identity.
            .id(SharedString::from(format!("tag-{}", tag.key())))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .when(is_active, |this| this.bg(cx.theme().accent))
            .when(!is_active, |this| {
                this.hover(|style| style.bg(cx.theme().muted))
            })
            // Picking the active Tag again clears the filter, so a Tag row is
            // its own way back out.
            .on_click(cx.listener(move |this, _, window, cx| {
                this.toggle_tag_filter(tag.clone(), window, cx);
            }))
            .child(TagChip::color(colour).small().child(name))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(count.to_string()),
            )
    }

    /// Theme pickers, pinned to the foot of the sidebar.
    ///
    /// Two selectors and no mode switch: which theme applies is the user's
    /// choice, but *when* dark applies stays with the OS.
    fn render_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .mt_auto()
            .pt_2()
            .flex()
            .flex_col()
            .gap_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Appearance"),
            )
            .child(Self::render_theme_row("Light", &self.light_select, cx))
            .child(Self::render_theme_row("Dark", &self.dark_select, cx))
    }

    fn render_theme_row(
        label: &'static str,
        state: &Entity<SelectState<ThemeListDelegate>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .px_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            // With nothing stored the app uses gpui-component's built-in
            // theme, which is not one of the embedded files and so has no row
            // to select. "Default" is the honest label for that state.
            .child(
                Select::new(state)
                    .small()
                    .placeholder("Default")
                    .menu_width(px(200.0)),
            )
    }

    fn render_view_row(
        view: View,
        count: usize,
        is_active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id(SharedString::from(format!("view-{}", view.label())))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .when(is_active, |this| this.bg(cx.theme().accent))
            .when(!is_active, |this| {
                this.hover(|style| style.bg(cx.theme().muted))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_view(view, window, cx);
            }))
            .child(
                div()
                    .text_sm()
                    .when(is_active, |this| {
                        this.text_color(cx.theme().accent_foreground)
                    })
                    .child(view.label()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(count.to_string()),
            )
    }
}
