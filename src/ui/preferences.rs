// SPDX-License-Identifier: GPL-3.0-only

//! What the window was told to look like: the chosen light and dark themes,
//! and whether the sidebar is showing.
//!
//! Free of `gpui`, like [`super::working_state::WorkingState`], and shaped the
//! same way — restored from a lookup, handed back as key/value pairs, with the
//! writing left to the caller. The two are separate because the `setting`
//! table's own comment separates them: these are preferences, the `ui.*` keys
//! are where you happened to be.
//!
//! It holds theme *names*, not resolved configs. What themes exist is
//! `ThemeCatalogue`'s subject; what was chosen is this one's.

use gpui_component::ThemeMode;
use issuers::store::settings_keys;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Preferences {
    light: Option<String>,
    dark: Option<String>,
    sidebar_hidden: bool,
}

impl Preferences {
    pub fn restore(read: impl Fn(&str) -> Option<String>) -> Self {
        Preferences {
            light: read(settings_keys::THEME_LIGHT),
            dark: read(settings_keys::THEME_DARK),
            // Anything other than "true" — absent or malformed included —
            // means visible, which is the safer state to land on: a window
            // whose sidebar will not come back is much worse than one that
            // shows it when you asked for it to be hidden.
            sidebar_hidden: read(settings_keys::SIDEBAR_HIDDEN)
                .is_some_and(|value| value == "true"),
        }
    }

    /// The keys and values to persist. No I/O: the caller owns the writing,
    /// and this owns the only list of what there is to write.
    pub fn settings(&self) -> Vec<(&'static str, String)> {
        let mut pairs = vec![(
            settings_keys::SIDEBAR_HIDDEN,
            self.sidebar_hidden.to_string(),
        )];
        // An unchosen theme is left absent rather than written as empty, so
        // that "never picked one" and "picked one called nothing" stay
        // distinguishable in the table.
        if let Some(light) = &self.light {
            pairs.push((settings_keys::THEME_LIGHT, light.clone()));
        }
        if let Some(dark) = &self.dark {
            pairs.push((settings_keys::THEME_DARK, dark.clone()));
        }
        pairs
    }

    pub fn theme(&self, mode: ThemeMode) -> Option<&str> {
        match mode {
            ThemeMode::Light => self.light.as_deref(),
            ThemeMode::Dark => self.dark.as_deref(),
        }
    }

    pub fn choose_theme(&mut self, mode: ThemeMode, name: impl Into<String>) {
        match mode {
            ThemeMode::Light => self.light = Some(name.into()),
            ThemeMode::Dark => self.dark = Some(name.into()),
        }
    }

    pub fn sidebar_hidden(&self) -> bool {
        self.sidebar_hidden
    }

    /// Returns the new state, so the caller does not have to ask again.
    pub fn toggle_sidebar(&mut self) -> bool {
        self.sidebar_hidden = !self.sidebar_hidden;
        self.sidebar_hidden
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn stored(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    fn reread(preferences: &Preferences) -> Preferences {
        let saved = preferences.settings();
        Preferences::restore(move |key| {
            saved
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
        })
    }

    #[test]
    fn a_first_run_has_no_themes_and_a_visible_sidebar() {
        let preferences = Preferences::restore(stored(&[]));
        assert_eq!(preferences.theme(ThemeMode::Light), None);
        assert_eq!(preferences.theme(ThemeMode::Dark), None);
        assert!(!preferences.sidebar_hidden());
    }

    #[test]
    fn only_the_word_true_hides_the_sidebar() {
        // A window whose sidebar will not come back is worse than one that
        // shows it unasked, so anything unreadable means visible.
        for value in ["false", "", "yes", "TRUE", "1"] {
            let preferences =
                Preferences::restore(stored(&[(settings_keys::SIDEBAR_HIDDEN, value)]));
            assert!(!preferences.sidebar_hidden(), "{value:?}");
        }
        let hidden = Preferences::restore(stored(&[(settings_keys::SIDEBAR_HIDDEN, "true")]));
        assert!(hidden.sidebar_hidden());
    }

    #[test]
    fn the_two_modes_are_chosen_independently() {
        // Over half the vendored theme families ship only one mode, which is
        // why these are not one choice.
        let mut preferences = Preferences::restore(stored(&[]));
        preferences.choose_theme(ThemeMode::Dark, "Ayu Dark");

        assert_eq!(preferences.theme(ThemeMode::Dark), Some("Ayu Dark"));
        assert_eq!(preferences.theme(ThemeMode::Light), None);
    }

    #[test]
    fn toggling_reports_where_it_landed() {
        let mut preferences = Preferences::restore(stored(&[]));
        assert!(preferences.toggle_sidebar());
        assert!(preferences.sidebar_hidden());
        assert!(!preferences.toggle_sidebar());
        assert!(!preferences.sidebar_hidden());
    }

    #[test]
    fn what_is_written_is_what_comes_back() {
        let mut preferences = Preferences::restore(stored(&[]));
        preferences.choose_theme(ThemeMode::Light, "Rosé Pine Dawn");
        preferences.choose_theme(ThemeMode::Dark, "Ayu Dark");
        preferences.toggle_sidebar();

        assert_eq!(reread(&preferences), preferences);
    }

    #[test]
    fn an_untouched_preference_round_trips_too() {
        let preferences = Preferences::restore(stored(&[]));
        assert_eq!(reread(&preferences), preferences);
    }

    #[test]
    fn a_theme_never_chosen_is_not_written_as_empty() {
        // "never picked one" and "picked one called nothing" must stay
        // distinguishable, or a blank row would resolve to no theme forever.
        let preferences = Preferences::restore(stored(&[]));
        let keys: Vec<&str> = preferences
            .settings()
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(keys, vec![settings_keys::SIDEBAR_HIDDEN]);
    }
}
