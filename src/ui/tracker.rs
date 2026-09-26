// SPDX-License-Identifier: GPL-3.0-only

//! Root view: selection, filters and editing state, rendered from the shared
//! Projection. The three columns are rendered by sibling modules.
//!
//! This view deliberately does *not* own the issues. They live in an
//! app-level `Entity<Projection>` that outlives the window and that the HTTP
//! API writes to as well; the view observes it and re-settles itself on every
//! change, whoever made it. See `docs/adr/0005`.

use std::collections::BTreeSet;
use std::time::Duration;

use gpui::*;
use gpui_component::input::{InputEvent, InputState, TextareaState};

use gpui_component::ThemeMode;
use gpui_component::combobox::{ComboboxEvent, ComboboxState};
use gpui_component::searchable_list::SearchableVec;
use gpui_component::select::{SelectEvent, SelectState};

use super::preferences::Preferences;
use super::theme_catalogue::{ThemeCatalogue, ThemeListDelegate};
use super::working_state::WorkingState;
use issue_tracker::domain::{Issue, IssueId, Priority, Status, Tag, View};
use issue_tracker::projection::{IssuePatch, Projection, Written};

/// The Tag editor in the detail pane. Tags are plain names, so the delegate is
/// the library's own `SearchableVec` and no custom one is needed.
pub(super) type TagCombobox = ComboboxState<SearchableVec<String>>;

/// A picker over Issues. Entries read `#12 Title`, optionally annotated with
/// the parent they would be taken from.
pub(super) type IssueCombobox = ComboboxState<SearchableVec<String>>;

/// How an Issue is spelled inside a picker.
fn issue_label(issue: &Issue) -> String {
    format!("#{} {}", issue.id, issue.display_title())
}

/// The same, annotated when the Issue already belongs to someone.
///
/// Attaching it would move it, so the row has to say what it would be taken
/// from — without that this would be a silent theft from another Issue.
fn issue_label_annotated(issue: &Issue, projection: &Projection) -> String {
    match issue.parent_id.and_then(|id| projection.get(id)) {
        Some(parent) => format!("{} · under #{}", issue_label(issue), parent.id),
        None => issue_label(issue),
    }
}

/// Recovers the id from a picker entry.
fn id_from_label(label: &str) -> Option<IssueId> {
    label.strip_prefix('#')?.split(' ').next()?.parse().ok()
}

fn issue_items(labels: Vec<String>) -> SearchableVec<String> {
    SearchableVec::new(labels)
}

fn tag_items(in_use: &BTreeSet<Tag>) -> SearchableVec<String> {
    SearchableVec::new(
        in_use
            .iter()
            .map(|tag| tag.as_str().to_owned())
            .collect::<Vec<_>>(),
    )
}

/// How long editing pauses before an auto-save fires.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

/// Key context for the issue list. Navigation bindings are scoped to it so
/// that typing `j` into a text input types a `j` rather than moving the
/// selection.
pub(super) const LIST_CONTEXT: &str = "IssueList";

actions!(
    issue_tracker,
    [
        SelectNext,
        SelectPrev,
        CreateIssue,
        EditIssue,
        DeleteIssue,
        FocusFilter,
        FocusTags,
        FocusSize,
        FocusSubIssues,
        CancelEditing,
        ToggleSidebar,
        Quit,
        Hide,
        HideOthers,
        Minimize,
        Zoom,
        CloseWindow,
    ]
);

/// Switches to a View. Carries its payload so one action serves all six menu
/// items; `no_json` keeps `schemars` out of the dependency tree.
#[derive(Clone, PartialEq, Default, Debug, gpui::Action)]
#[action(namespace = issue_tracker, no_json)]
pub struct ShowView {
    pub view: View,
}

/// Reads a preference, treating a read failure as "unset" — a broken settings
/// row should cost you a theme, not the app.
fn read_setting(projection: &Projection, key: &str) -> Option<String> {
    match projection.setting(key) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("failed to read setting {key}: {err:#}");
            None
        }
    }
}

/// Every open window's view, so a pending edit can be flushed before the API
/// writes. Weak, because a window closing must not be prevented by this list.
#[derive(Default)]
struct OpenTrackers(Vec<WeakEntity<IssueTracker>>);

impl Global for OpenTrackers {}

/// Lands every window's debounced edit immediately.
///
/// Called before the API applies a write. A pending edit belongs to an earlier
/// moment than the request now arriving, so it goes first and the API's
/// change — being later — wins. Without this the debounce would fire *after*
/// the API write and silently undo it.
pub fn flush_pending_edits(cx: &mut App) {
    let open = cx.default_global::<OpenTrackers>().0.clone();
    let mut live = Vec::with_capacity(open.len());
    for weak in open {
        if let Some(tracker) = weak.upgrade() {
            tracker.update(cx, |this, cx| this.flush_pending_save(cx));
            live.push(weak);
        }
    }
    // Closed windows are dropped here rather than accumulating forever.
    cx.default_global::<OpenTrackers>().0 = live;
}

/// Registers key bindings. Called once at startup.
pub fn init(cx: &mut App) {
    cx.default_global::<OpenTrackers>();
    cx.bind_keys([
        KeyBinding::new("j", SelectNext, Some(LIST_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(LIST_CONTEXT)),
        KeyBinding::new("k", SelectPrev, Some(LIST_CONTEXT)),
        KeyBinding::new("up", SelectPrev, Some(LIST_CONTEXT)),
        KeyBinding::new("c", CreateIssue, Some(LIST_CONTEXT)),
        KeyBinding::new("e", EditIssue, Some(LIST_CONTEXT)),
        KeyBinding::new("x", DeleteIssue, Some(LIST_CONTEXT)),
        KeyBinding::new("/", FocusFilter, Some(LIST_CONTEXT)),
        KeyBinding::new("t", FocusTags, Some(LIST_CONTEXT)),
        KeyBinding::new("s", FocusSubIssues, Some(LIST_CONTEXT)),
        // `s` is taken by sub-issues and `e` by the title, so Size gets `z`.
        // A duplicate binding is resolved silently in favour of one of them,
        // so a clash here is invisible rather than an error.
        KeyBinding::new("z", FocusSize, Some(LIST_CONTEXT)),
        KeyBinding::new("escape", CancelEditing, None),
        // Deliberately unscoped: unlike j/k/c these have to work while a text
        // input has focus, and cmd chords cannot collide with typing.
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-n", CreateIssue, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-h", Hide, None),
    ]);
}

pub struct IssueTracker {
    /// The shared issue store. Not owned — see the module docs.
    projection: Entity<Projection>,
    /// What this window is looking at. Free of gpui, and tested there.
    pub(super) working: WorkingState,
    /// What it was told to look like. Likewise.
    preferences: Preferences,

    pub(super) title_input: Entity<InputState>,
    pub(super) body_input: Entity<TextareaState>,
    /// A number, or empty for unsized. Saved on the same debounce as the
    /// title and body, because it is edited the same way.
    pub(super) size_input: Entity<InputState>,
    pub(super) filter_input: Entity<InputState>,
    pub(super) new_issue_input: Entity<InputState>,
    /// Whether the inline "new issue" row is showing.
    pub(super) creating: bool,

    pub(super) list_focus: FocusHandle,
    /// Replaced on every keystroke; dropping the previous task cancels it,
    /// which is what makes the save debounced.
    save_task: Option<Task<()>>,
    /// Same debounce trick for View/selection/filter, which change far too
    /// often to write through on every keypress.
    ui_state_task: Option<Task<()>>,

    /// Every embedded theme, split by mode. Held so a selection can be
    /// resolved back to the config it applies.
    catalogue: ThemeCatalogue,
    pub(super) tag_select: Entity<TagCombobox>,
    /// Adds a sub-issue to the selected Issue.
    pub(super) sub_issue_select: Entity<IssueCombobox>,
    /// Chooses the selected Issue's parent. Clearing it detaches; choosing a
    /// different one moves it.
    pub(super) parent_select: Entity<IssueCombobox>,
    pub(super) light_select: Entity<SelectState<ThemeListDelegate>>,
    pub(super) dark_select: Entity<SelectState<ThemeListDelegate>>,

    /// Subscriptions die with the view if not held here.
    _subscriptions: Vec<Subscription>,
}

impl IssueTracker {
    pub fn new(
        projection: Entity<Projection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Settings and Tag vocabulary are read up front, in a single borrow,
        // so it has ended before any entity is constructed below.
        let (preferences, in_use) = {
            let p = projection.read(cx);
            (
                Preferences::restore(|key| read_setting(p, key)),
                p.tags_in_use(),
            )
        };

        // Working state settles itself: an unparseable value reads as absent,
        // a Tag nothing carries any more is dropped, and a selection that no
        // longer resolves falls back to the first visible Issue.
        let working = {
            let p = projection.read(cx);
            WorkingState::restore(p.issues(), |key| read_setting(p, key))
        };

        let title_input = cx.new(|cx| InputState::new(window, cx).placeholder("Issue title"));
        let size_input = cx.new(|cx| InputState::new(window, cx).placeholder("—"));
        let body_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(8, 40)
                .placeholder("Describe the work…")
        });

        // Themes: resolve the persisted choices, falling back to the built-in
        // defaults so a first run looks exactly as it did before this feature.
        let catalogue = ThemeCatalogue::load();
        let light_config = preferences
            .theme(ThemeMode::Light)
            .and_then(|name| catalogue.find(ThemeMode::Light, name))
            .map(|choice| choice.config.clone());
        let dark_config = preferences
            .theme(ThemeMode::Dark)
            .and_then(|name| catalogue.find(ThemeMode::Dark, name))
            .map(|choice| choice.config.clone());

        if light_config.is_some() || dark_config.is_some() {
            super::theme::apply_themes(light_config, dark_config, window, cx);
        }

        let light_delegate = ThemeListDelegate::new(catalogue.for_mode(ThemeMode::Light).to_vec());
        let dark_delegate = ThemeListDelegate::new(catalogue.for_mode(ThemeMode::Dark).to_vec());
        let light_index = preferences
            .theme(ThemeMode::Light)
            .and_then(|name| light_delegate.index_of(name));
        let dark_index = preferences
            .theme(ThemeMode::Dark)
            .and_then(|name| dark_delegate.index_of(name));

        let light_select = cx.new(|cx| SelectState::new(light_delegate, light_index, window, cx));
        let dark_select = cx.new(|cx| SelectState::new(dark_delegate, dark_index, window, cx));

        let tag_select = cx.new(|cx| {
            ComboboxState::new(tag_items(&in_use), Vec::new(), window, cx)
                .multiple(true)
                .searchable(true)
        });

        let sub_issue_select = cx.new(|cx| {
            ComboboxState::new(issue_items(Vec::new()), Vec::new(), window, cx).searchable(true)
        });
        let parent_select = cx.new(|cx| {
            ComboboxState::new(issue_items(Vec::new()), Vec::new(), window, cx).searchable(true)
        });

        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter titles…"));
        let new_issue_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("New issue title…"));

        let subscriptions = vec![
            // Every change to the shared store lands here, whether this window
            // made it or the HTTP API did.
            cx.observe_in(&projection, window, |this, _, window, cx| {
                this.on_projection_changed(window, cx);
            }),
            cx.subscribe_in(
                &title_input,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.schedule_save(cx);
                    }
                },
            ),
            cx.subscribe_in(&body_input, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.schedule_save(cx);
                }
            }),
            cx.subscribe_in(&size_input, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.schedule_save(cx);
                }
            }),
            cx.subscribe_in(
                &filter_input,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        let text = input.read(cx).value().to_string();
                        this.settling(cx, |working, issues| working.set_filter(text, issues));
                        this.after_settling(true, window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &new_issue_input,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        let title = input.read(cx).value().to_string();
                        this.commit_new_issue(title, window, cx);
                    }
                },
            ),
            // Fires on every toggle in the dropdown, add or remove.
            cx.subscribe_in(
                &tag_select,
                window,
                |this, _, event: &ComboboxEvent<SearchableVec<String>>, _, cx| {
                    let ComboboxEvent::Change(values) = event else {
                        return;
                    };
                    this.apply_tag_selection(values.clone(), cx);
                },
            ),
            cx.subscribe_in(
                &sub_issue_select,
                window,
                |this, _, event: &ComboboxEvent<SearchableVec<String>>, window, cx| {
                    let ComboboxEvent::Change(values) = event else {
                        return;
                    };
                    if let Some(child) = values.first().and_then(|label| id_from_label(label)) {
                        this.attach_sub_issue(child, window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &parent_select,
                window,
                |this, _, event: &ComboboxEvent<SearchableVec<String>>, window, cx| {
                    let ComboboxEvent::Change(values) = event else {
                        return;
                    };
                    // An empty selection is the clear affordance: detach.
                    let parent = values.first().and_then(|label| id_from_label(label));
                    this.set_parent(parent, window, cx);
                },
            ),
            cx.subscribe_in(
                &light_select,
                window,
                |this, _, event: &SelectEvent<ThemeListDelegate>, window, cx| {
                    let SelectEvent::Confirm(Some(name)) = event else {
                        return;
                    };
                    this.choose_theme(ThemeMode::Light, &name.clone(), window, cx);
                },
            ),
            cx.subscribe_in(
                &dark_select,
                window,
                |this, _, event: &SelectEvent<ThemeListDelegate>, window, cx| {
                    let SelectEvent::Confirm(Some(name)) = event else {
                        return;
                    };
                    this.choose_theme(ThemeMode::Dark, &name.clone(), window, cx);
                },
            ),
            // Chrome rather than issues, but this is the only object that
            // lives as long as the window, so it holds the subscription.
            super::theme::observe_system_appearance(window),
            // Closing the window drops this entity along with any pending
            // debounced write, so flush on the way out.
            cx.on_release(|this, cx| {
                if this.save_task.take().is_some()
                    && let Some(id) = this.working.selected()
                {
                    this.write_edits(id, cx);
                }
                this.write_ui_state(cx);
            }),
        ];

        let mut this = Self {
            projection,
            working,
            preferences,
            title_input,
            body_input,
            size_input,
            filter_input,
            new_issue_input,
            creating: false,
            list_focus: cx.focus_handle(),
            save_task: None,
            ui_state_task: None,
            catalogue,
            tag_select,
            sub_issue_select,
            parent_select,
            light_select,
            dark_select,
            _subscriptions: subscriptions,
        };
        // The filter came back as a plain String; the input showing it has to
        // be told separately. `set_value` suppresses change events, so this
        // does not re-trigger the filter subscription.
        if !this.working.filter().is_empty() {
            let filter = this.working.filter().to_string();
            this.filter_input
                .update(cx, |input, cx| input.set_value(filter, window, cx));
        }

        this.refresh_inputs(true, window, cx);
        // Without this the window opens with nothing focused, so the list
        // bindings (j/k/c/e/x) are dead until something is clicked.
        this.list_focus.focus(window, cx);

        let weak = cx.weak_entity();
        cx.default_global::<OpenTrackers>().0.push(weak);
        this
    }

    // ---- projection queries -------------------------------------------------

    /// The shared Projection, borrowed for as long as `cx` is.
    ///
    /// The render paths read the corpus through this and ask it everything
    /// they need from one borrow. Anything that also has to take `&mut self`
    /// — every working-state change — goes through [`Self::settling`]
    /// instead, because this borrow would block it.
    pub(super) fn projection<'a>(&self, cx: &'a App) -> &'a Projection {
        self.projection.read(cx)
    }

    /// Runs a working-state change against the shared corpus.
    ///
    /// The *handle* is cloned — a pointer, not the Issues — so the borrow of
    /// the corpus is of `cx` alone and leaves `self.working` free to be taken
    /// mutably beside it. This used to clone the Issues themselves to dodge
    /// that borrow, which copied every title and body on every keystroke, on
    /// every `j`/`k`, and on every write the API made.
    ///
    /// The narrowing itself lives in [`WorkingState`]; this only supplies the
    /// corpus, which is the one thing that module deliberately does not hold.
    fn settling<R>(
        &mut self,
        cx: &App,
        change: impl FnOnce(&mut WorkingState, &[Issue]) -> R,
    ) -> R {
        let projection = self.projection.clone();
        change(&mut self.working, projection.read(cx).issues())
    }

    /// Every Tag carried by some Issue, sorted, first-spelling-wins.
    pub(super) fn tags_in_use(&self, cx: &App) -> BTreeSet<Tag> {
        self.projection.read(cx).tags_in_use()
    }

    /// How many Issues carry a Tag, ignoring the View and the filter — the
    /// Tag equivalent of [`Self::count_for`].
    pub(super) fn count_for_tag(&self, tag: &Tag, cx: &App) -> usize {
        self.projection.read(cx).count_with_tag(tag)
    }

    pub(super) fn selected_issue<'a>(&self, cx: &'a App) -> Option<&'a Issue> {
        self.projection.read(cx).get(self.working.selected()?)
    }

    /// The selected Issue's parts, in display order.
    pub(super) fn selected_sub_issues(&self, cx: &App) -> Vec<Issue> {
        let Some(id) = self.working.selected() else {
            return Vec::new();
        };
        self.projection
            .read(cx)
            .sub_issues(id)
            .into_iter()
            .cloned()
            .collect()
    }

    /// The Issue the selected one is part of, if any.
    pub(super) fn selected_parent(&self, cx: &App) -> Option<Issue> {
        let parent = self.selected_issue(cx)?.parent_id?;
        self.projection.read(cx).get(parent).cloned()
    }

    /// Settled-over-total for an Issue's parts, or `None` when it has none.
    ///
    /// One Issue's worth. The list asks the same question of every row at
    /// once and goes through [`Projection::sub_issue_index`] instead.
    pub(super) fn settled_progress(&self, id: IssueId, cx: &App) -> Option<(usize, usize)> {
        self.projection.read(cx).settled_progress(id)
    }

    // ---- reacting to the shared store ---------------------------------------

    /// Re-settles this window after *any* change to the shared Projection.
    ///
    /// UI-initiated and API-initiated writes both arrive here, which is what
    /// keeps them consistent: there is no separate "and the API also needs
    /// to…" path to forget about.
    fn on_projection_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settled = self.settling(cx, |working, issues| working.settle(issues));
        self.refresh_inputs(settled.selection_moved, window, cx);
        cx.notify();
    }

    /// The view's half of a working-state change: sync the inputs, persist,
    /// redraw. Every mutator ends here so none of them can forget a step.
    ///
    /// `force` is [`Self::refresh_inputs`]'s: it says the inputs no longer
    /// describe what they are showing. Moving the selection forces it; so
    /// does changing the View or a filter, because the pickers then describe
    /// a different Issue even when the selection itself survived.
    fn after_settling(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_inputs(force, window, cx);
        self.schedule_ui_state_save(cx);
        cx.notify();
    }

    /// Mirrors the selected Issue into the inputs. `set_value` deliberately
    /// suppresses change events, so this never triggers an auto-save.
    ///
    /// A *focused* input is left alone unless `force` — meaning the selection
    /// itself moved. An external write landing mid-sentence must not eat the
    /// characters under the cursor; an unfocused input, by contrast, is always
    /// refreshed, because stale text there would be written straight back over
    /// that same change on the next keystroke.
    fn refresh_inputs(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (title, body, size) = match self.selected_issue(cx) {
            Some(issue) => (
                issue.title.clone(),
                issue.body.clone(),
                issue.size.map(|size| size.to_string()).unwrap_or_default(),
            ),
            None => (String::new(), String::new(), String::new()),
        };

        let title_focused = self
            .title_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let body_focused = self.body_input.read(cx).focus_handle(cx).is_focused(window);

        if force || !title_focused {
            self.title_input
                .update(cx, |input, cx| input.set_value(title, window, cx));
        }
        if force || !body_focused {
            self.body_input
                .update(cx, |input, cx| input.set_value(body, window, cx));
        }
        let size_focused = self.size_input.read(cx).focus_handle(cx).is_focused(window);
        if force || !size_focused {
            self.size_input
                .update(cx, |input, cx| input.set_value(size, window, cx));
        }

        // The pickers are part of "what the selected Issue looks like", so
        // they are re-pointed here rather than by each caller. Leaving this to
        // callers is exactly how the tag box and the parent box came to show
        // the previously selected Issue's answers.
        self.sync_tag_select(window, cx);
        self.sync_relation_selects(window, cx);
    }

    /// Applies a change to the shared Projection and notifies its observers.
    ///
    /// That notification is what drives [`Self::on_projection_changed`], so a
    /// write made here re-settles the view by exactly the path an API write
    /// takes.
    fn write<T>(
        &self,
        cx: &mut App,
        change: impl FnOnce(&mut Projection) -> Written<T>,
    ) -> Option<T> {
        self.projection
            .update(cx, |projection, cx| match change(projection) {
                Ok(value) => {
                    cx.notify();
                    Some(value)
                }
                Err(err) => {
                    // Refusals are mostly unreachable from the UI, which
                    // disables or filters out the controls that would cause
                    // them; anything arriving here is worth seeing.
                    eprintln!("failed to write to the issue store: {err}");
                    None
                }
            })
    }

    // ---- selection ----------------------------------------------------------

    pub(super) fn select_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        self.settling(cx, |working, issues| working.select_view(view, issues));
        self.after_settling(true, window, cx);
        // The View menu carries a checkmark, so the tree has to be rebuilt
        // for it to follow the active View.
        super::menus::rebuild(view, cx);
    }

    pub(super) fn select_issue(
        &mut self,
        id: IssueId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.working.selected() == Some(id) {
            return;
        }
        // Any pending edit belongs to the issue we're leaving, so flush it
        // rather than letting it land on the newly selected one.
        self.flush_pending_save(cx);
        let settled = self.working.select_issue(id);
        self.after_settling(settled.selection_moved, window, cx);
    }

    /// Selects a Tag filter, or clears it when the active Tag is picked again.
    pub(super) fn toggle_tag_filter(
        &mut self,
        tag: Tag,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settling(cx, |working, issues| working.toggle_tag(tag, issues));
        self.after_settling(true, window, cx);
    }

    fn move_selection(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        // A pending edit belongs to the Issue being left, and
        // `flush_pending_save` reads the selection to know what it is saving —
        // so it has to run before the selection moves, not after.
        self.flush_pending_save(cx);
        let settled = self.settling(cx, |working, issues| working.move_selection(delta, issues));
        self.after_settling(settled.selection_moved, window, cx);
    }

    // ---- editing ------------------------------------------------------------

    /// Restarts the debounce window. The previous task is dropped, cancelling
    /// the save it was waiting to perform.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.working.selected() else {
            return;
        };
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            this.update(cx, |this, cx| this.write_edits(id, cx)).ok();
        }));
    }

    /// Saves immediately, abandoning any pending debounce.
    ///
    /// Also runs before the API applies a write, so a half-typed title reaches
    /// the store before anything else touches that Issue.
    pub(super) fn flush_pending_save(&mut self, cx: &mut Context<Self>) {
        if self.save_task.take().is_some()
            && let Some(id) = self.working.selected()
        {
            self.write_edits(id, cx);
            cx.notify();
        }
    }

    /// Writes the title, body and size inputs through to the shared store.
    ///
    /// Takes `&mut App` rather than `Context<Self>` so it can also run from
    /// `on_release`, where the entity is being torn down and no `Context`
    /// exists. Only these three fields are named, so this can never clobber a
    /// Status or a Tag that something else set in the meantime.
    fn write_edits(&mut self, id: IssueId, cx: &mut App) {
        self.save_task = None;
        let title = self.title_input.read(cx).value().to_string();
        let body = self.body_input.read(cx).value().to_string();
        let typed = self.size_input.read(cx).value().to_string();

        let patch = IssuePatch::default().title(title).body(body);
        let patch = match typed.trim() {
            "" => patch.clear_size(),
            // A half-typed or over-large number leaves the stored Size alone
            // rather than clobbering a good value; the input is put back the
            // next time the selection moves.
            // Qualified: `gpui::Size` is in scope from the glob import, and
            // is a width-and-height, not this.
            text => match text.parse::<issue_tracker::domain::Size>() {
                Ok(size) => patch.size(size),
                Err(_) => patch,
            },
        };

        self.write(cx, |projection| projection.patch(id, patch));
    }

    pub(super) fn set_status(
        &mut self,
        status: Status,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.patch_selected(IssuePatch::default().status(status), cx);
    }

    pub(super) fn set_priority(
        &mut self,
        priority: Priority,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.patch_selected(IssuePatch::default().priority(priority), cx);
    }

    /// Applies a patch to the selected Issue.
    ///
    /// Tagging goes through here like everything else, so it stamps
    /// `updated_at` and re-sorts exactly as a Status change does: tagging is
    /// an edit, not a free annotation.
    fn patch_selected(&mut self, patch: IssuePatch, cx: &mut Context<Self>) {
        let Some(id) = self.working.selected() else {
            return;
        };
        self.write(cx, |projection| projection.patch(id, patch));
    }

    /// Applies whatever the Combobox now reports as selected.
    fn apply_tag_selection(&mut self, values: Vec<String>, cx: &mut Context<Self>) {
        let tags = values
            .iter()
            .filter_map(|value| value.parse().ok())
            .collect();
        self.patch_selected(IssuePatch::default().tags(tags), cx);
    }

    /// Adds the Tag currently typed into the Combobox's search box.
    ///
    /// The library's own recipe for this only appends the typed name to the
    /// dropdown list, leaving you to click the thing you just typed. Putting
    /// it straight on the Issue saves that second click.
    pub(super) fn create_tag_from_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.tag_select.read(cx).query(cx).to_string();
        let Ok(tag) = query.parse::<Tag>() else {
            return;
        };
        let Some(id) = self.working.selected() else {
            return;
        };
        self.write(cx, |projection| projection.add_tag(id, tag));
        self.sync_tag_select(window, cx);
    }

    /// Takes one Tag off the selected Issue, from its chip's remove button.
    pub(super) fn remove_tag(&mut self, tag: Tag, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.working.selected() else {
            return;
        };
        self.write(cx, |projection| projection.remove_tag(id, &tag));
        self.sync_tag_select(window, cx);
    }

    /// Files an existing Issue under the selected one.
    pub(super) fn attach_sub_issue(
        &mut self,
        child: IssueId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(parent) = self.working.selected() else {
            return;
        };
        self.write(cx, |projection| projection.set_parent(child, Some(parent)));
        self.sync_relation_selects(window, cx);
    }

    /// Removes one of the selected Issue's parts, which becomes top-level.
    pub(super) fn detach_sub_issue(
        &mut self,
        child: IssueId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.write(cx, |projection| projection.set_parent(child, None));
        self.sync_relation_selects(window, cx);
    }

    /// Files the selected Issue under `parent`, or removes it from whatever
    /// holds it. Choosing a different parent moves it; there is no separate
    /// move operation.
    pub(super) fn set_parent(
        &mut self,
        parent: Option<IssueId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.working.selected() else {
            return;
        };
        if self
            .selected_issue(cx)
            .is_some_and(|issue| issue.parent_id == parent)
        {
            return;
        }
        self.write(cx, |projection| projection.set_parent(id, parent));
        self.sync_relation_selects(window, cx);
    }

    /// Points both relationship pickers at what is currently possible.
    ///
    /// Candidates are filtered to what `Projection` would actually accept, so
    /// the refusals exist for the API's benefit rather than being reachable by
    /// clicking.
    fn sync_relation_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.working.selected() else {
            return;
        };

        let (children, parents, current_parent) = {
            let projection = self.projection.read(cx);
            (
                projection
                    .eligible_sub_issues(id)
                    .iter()
                    .map(|issue| issue_label_annotated(issue, projection))
                    .collect::<Vec<_>>(),
                projection
                    .eligible_parents(id)
                    .iter()
                    .map(|issue| issue_label(issue))
                    .collect::<Vec<_>>(),
                projection
                    .get(id)
                    .and_then(|issue| issue.parent_id)
                    .and_then(|parent| projection.get(parent))
                    .map(issue_label),
            )
        };

        self.sub_issue_select.update(cx, |state, cx| {
            state.set_items(issue_items(children), window, cx);
            // Nothing stays selected here: this control is an action, and the
            // Issue it just attached is no longer a candidate.
            state.set_selected_values(&[], window, cx);
        });
        self.parent_select.update(cx, |state, cx| {
            state.set_items(issue_items(parents), window, cx);
            let selected: Vec<String> = current_parent.into_iter().collect();
            state.set_selected_values(&selected, window, cx);
        });
    }

    /// Points the Combobox at the current Tag vocabulary and the selected
    /// Issue's Tags.
    ///
    /// `set_selected_values` clears the search query as a side effect, so this
    /// runs on settle rather than on every keystroke.
    fn sync_tag_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = tag_items(&self.tags_in_use(cx));
        let selected: Vec<String> = self
            .selected_issue(cx)
            .map(|issue| {
                issue
                    .tags
                    .iter()
                    .map(|tag| tag.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        self.tag_select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            state.set_selected_values(&selected, window, cx);
        });
    }

    // ---- create -------------------------------------------------------------

    pub(super) fn start_creating(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.creating = true;
        self.new_issue_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn commit_new_issue(&mut self, title: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = title.trim().to_string();
        if title.is_empty() {
            self.cancel_creating(window, cx);
            return;
        }
        if let Some(issue) = self.write(cx, |projection| {
            projection.create(&title, IssuePatch::default(), None)
        }) {
            self.working.select_issue(issue.id);
            // Stay in create mode so several issues can be typed in a row.
            self.new_issue_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.refresh_inputs(true, window, cx);
        }
        cx.notify();
    }

    pub(super) fn cancel_creating(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.creating = false;
        self.new_issue_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    // ---- working state ------------------------------------------------------

    /// Queues a write of View, selection and filter.
    ///
    /// Debounced because these change on every `j`/`k` and every character
    /// typed into the filter, unlike the preferences which change rarely.
    fn schedule_ui_state_save(&mut self, cx: &mut Context<Self>) {
        self.ui_state_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            this.update(cx, |this, cx| this.write_ui_state(cx)).ok();
        }));
    }

    /// Writes working state immediately. Failures are logged, never fatal —
    /// losing your place is not worth interrupting anyone over.
    fn write_ui_state(&mut self, cx: &App) {
        self.ui_state_task = None;
        self.persist(self.working.settings(), cx);
    }

    /// The only place a preference or a piece of working state is written.
    ///
    /// Failures are logged and never fatal: losing your place, or which theme
    /// you picked, is not worth interrupting anyone over — and a settings
    /// table you cannot write to should not take a working window with it.
    fn persist(&self, pairs: Vec<(&'static str, String)>, cx: &App) {
        for (key, value) in pairs {
            if let Err(err) = self.projection.read(cx).set_setting(key, &value) {
                eprintln!("failed to save {key}: {err:#}");
            }
        }
    }

    /// Flushes everything still sitting behind a debounce.
    ///
    /// Without this, quitting or closing the window within the debounce
    /// window silently discards the last edit. That path only became
    /// reachable once `cmd-q` started working.
    pub(super) fn flush_pending_writes(&mut self, cx: &mut Context<Self>) {
        self.flush_pending_save(cx);
        if self.ui_state_task.is_some() {
            self.write_ui_state(cx);
        }
    }

    // ---- sidebar ------------------------------------------------------------

    /// Shows or hides the sidebar.
    ///
    /// Unlike [`Self::choose_theme`], a failed write does not abort the
    /// change: this is view state, and the user's immediate intent should not
    /// be held hostage by a settings write. The worst case is that the next
    /// launch disagrees, which is recoverable with another `cmd-b`.
    pub(super) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.preferences.toggle_sidebar();
        cx.notify();
        self.persist(self.preferences.settings(), cx);
    }

    pub(super) fn sidebar_hidden(&self) -> bool {
        self.preferences.sidebar_hidden()
    }

    fn on_toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_sidebar(cx);
    }

    // ---- application ---------------------------------------------------------

    fn on_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        // Flush before asking the app to go away, or the last 400ms of
        // editing is lost.
        self.flush_pending_writes(cx);
        cx.quit();
    }

    fn on_hide(&mut self, _: &Hide, _: &mut Window, cx: &mut Context<Self>) {
        cx.hide();
    }

    fn on_hide_others(&mut self, _: &HideOthers, _: &mut Window, cx: &mut Context<Self>) {
        cx.hide_other_apps();
    }

    fn on_minimize(&mut self, _: &Minimize, window: &mut Window, _: &mut Context<Self>) {
        window.minimize_window();
    }

    fn on_zoom(&mut self, _: &Zoom, window: &mut Window, _: &mut Context<Self>) {
        window.zoom_window();
    }

    fn on_close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        // The app stays alive with no window, so this has to flush too.
        self.flush_pending_writes(cx);
        window.remove_window();
    }

    fn on_show_view(&mut self, action: &ShowView, window: &mut Window, cx: &mut Context<Self>) {
        self.select_view(action.view, window, cx);
    }

    // ---- appearance ---------------------------------------------------------

    /// Persists a theme choice and applies it.
    ///
    /// Writes through to the store *before* touching the live theme, mirroring
    /// the ordering discipline in ADR-0002 — if the write fails the app keeps
    /// the appearance the database can actually reproduce on next launch.
    pub(super) fn choose_theme(
        &mut self,
        mode: ThemeMode,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(choice) = self.catalogue.find(mode, name) else {
            eprintln!("theme {name:?} is not in the catalogue");
            return;
        };
        let config = choice.config.clone();

        // Applied first, then remembered. A settings table that cannot be
        // written should cost you the memory of the choice, not the choice —
        // returning early here made the picker look broken instead.
        match mode {
            ThemeMode::Light => super::theme::apply_themes(Some(config), None, window, cx),
            ThemeMode::Dark => super::theme::apply_themes(None, Some(config), window, cx),
        }
        cx.notify();

        self.preferences.choose_theme(mode, name);
        self.persist(self.preferences.settings(), cx);
    }

    // ---- delete -------------------------------------------------------------

    /// Erases an Issue. Guarded by a confirmation dialog at the call site —
    /// cancelling an Issue is a Status change, this is for mistakes.
    ///
    /// Selection and Tag filter are re-settled by
    /// [`Self::on_projection_changed`], exactly as they would be had the API
    /// done the deleting.
    pub(super) fn delete_issue(
        &mut self,
        id: IssueId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Don't let a queued auto-save resurrect what we're about to erase.
        self.save_task = None;
        self.write(cx, |projection| projection.delete(id));
    }

    // ---- actions ------------------------------------------------------------

    fn on_select_next(&mut self, _: &SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, window, cx);
    }

    fn on_select_prev(&mut self, _: &SelectPrev, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, window, cx);
    }

    fn on_create_issue(&mut self, _: &CreateIssue, window: &mut Window, cx: &mut Context<Self>) {
        self.start_creating(window, cx);
    }

    fn on_edit_issue(&mut self, _: &EditIssue, window: &mut Window, cx: &mut Context<Self>) {
        if self.working.selected().is_some() {
            self.title_input
                .update(cx, |input, cx| input.focus(window, cx));
        }
    }

    fn on_delete_issue(&mut self, _: &DeleteIssue, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, title)) = self
            .selected_issue(cx)
            .map(|issue| (issue.id, issue.display_title().to_string()))
        else {
            return;
        };
        let sub_issue_count = self.selected_sub_issues(cx).len();
        self.confirm_delete(id, title, sub_issue_count, window, cx);
    }

    fn on_focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_input
            .update(cx, |input, cx| input.focus(window, cx));
    }

    fn on_focus_tags(&mut self, _: &FocusTags, window: &mut Window, cx: &mut Context<Self>) {
        if self.working.selected().is_some() {
            self.tag_select
                .update(cx, |state, cx| state.focus(window, cx));
        }
    }

    /// The size input is not reachable by tabbing — focus goes through the
    /// Status buttons first — so it gets a binding of its own, as the tag
    /// editor does.
    fn on_focus_size(&mut self, _: &FocusSize, window: &mut Window, cx: &mut Context<Self>) {
        if self.working.selected().is_some() {
            self.size_input
                .update(cx, |input, cx| input.focus(window, cx));
        }
    }

    fn on_focus_sub_issues(
        &mut self,
        _: &FocusSubIssues,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.working.selected().is_some() {
            self.sub_issue_select
                .update(cx, |state, cx| state.focus(window, cx));
        }
    }

    /// Escape always returns to the list, from wherever focus currently is.
    fn on_cancel_editing(
        &mut self,
        _: &CancelEditing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.creating {
            self.cancel_creating(window, cx);
            return;
        }
        self.flush_pending_save(cx);
        self.list_focus.focus(window, cx);
        cx.notify();
    }
}

impl Render for IssueTracker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // `Option` is an iterator of zero or one, so this drops the sidebar
        // out of the tree entirely rather than rendering it at zero width.
        let sidebar = if self.sidebar_hidden() {
            None
        } else {
            Some(self.render_sidebar(cx))
        };

        div()
            .key_context("IssueTracker")
            .on_action(cx.listener(Self::on_select_next))
            .on_action(cx.listener(Self::on_select_prev))
            .on_action(cx.listener(Self::on_create_issue))
            .on_action(cx.listener(Self::on_edit_issue))
            .on_action(cx.listener(Self::on_delete_issue))
            .on_action(cx.listener(Self::on_focus_filter))
            .on_action(cx.listener(Self::on_focus_tags))
            .on_action(cx.listener(Self::on_focus_size))
            .on_action(cx.listener(Self::on_focus_sub_issues))
            .on_action(cx.listener(Self::on_cancel_editing))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_hide))
            .on_action(cx.listener(Self::on_hide_others))
            .on_action(cx.listener(Self::on_minimize))
            .on_action(cx.listener(Self::on_zoom))
            .on_action(cx.listener(Self::on_close_window))
            .on_action(cx.listener(Self::on_show_view))
            .size_full()
            .flex()
            .flex_row()
            .children(sidebar)
            .child(self.render_issue_list(window, cx))
            .child(self.render_issue_detail(window, cx))
    }
}
