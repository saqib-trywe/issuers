// SPDX-License-Identifier: GPL-3.0-only

//! The macOS application menu.
//!
//! Menu items dispatch actions, so anything reachable from a menu also needs
//! a binding or handler elsewhere. The View menu carries a checkmark on the
//! active View, which means the whole tree has to be rebuilt whenever the
//! View changes — see [`rebuild`].

use gpui::{App, Menu, MenuItem, OsAction, SystemMenuType};
use gpui_component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};

use super::tracker::{
    CloseWindow, CreateIssue, DeleteIssue, Hide, HideOthers, Minimize, Quit, ShowView,
    ToggleSidebar, Zoom,
};
use issuers::domain::View;

/// Installs the menu bar for the given active View.
///
/// Call again whenever the active View changes so the checkmark follows it;
/// `set_menus` replaces the whole tree.
pub fn rebuild(active: View, cx: &mut App) {
    cx.set_menus([
        // On macOS the first menu is the application menu, and its name is
        // what appears in bold next to the Apple logo. Ours reads "Issuers"
        // rather than the binary name because of this.
        Menu::new("Issuers").items([
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Issuers", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::separator(),
            MenuItem::action("Quit Issues", Quit),
        ]),
        // These shortcuts already work — gpui-component's Input binds them in
        // the Input context. The menu exists for discoverability and so macOS
        // can route the commands itself.
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::separator(),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("Issue").items([
            MenuItem::action("New Issue", CreateIssue),
            MenuItem::separator(),
            MenuItem::action("Delete Issue", DeleteIssue),
        ]),
        // Listing the Views here is what keeps navigation reachable when the
        // sidebar is hidden.
        Menu::new("View").items(
            [
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::separator(),
            ]
            .into_iter()
            .chain(View::ALL.into_iter().map(|view| MenuItem::Action {
                name: view.label().into(),
                action: Box::new(ShowView { view }),
                os_action: None,
                checked: view == active,
                disabled: false,
            })),
        ),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
    ]);
}
