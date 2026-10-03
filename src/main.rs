// SPDX-License-Identifier: GPL-3.0-only

mod app_state;
mod ui;

use gpui::*;
use gpui_component::Root;

use issuers::domain;
use issuers::projection::Projection;
use issuers::store::Store;
use ui::IssueTracker;

/// Opens the main window.
///
/// Extracted so the Dock-icon reopen handler can call it too: on macOS the
/// app stays alive after its last window closes, and clicking the icon has to
/// be able to bring one back.
fn open_main_window(cx: &mut App) {
    // The window borrows the shared Projection rather than opening its own
    // database: the HTTP API writes to the same one, and a second copy would
    // go stale the moment either wrote.
    let projection = app_state::projection(cx);

    let opened = cx.open_window(
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Issuers".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.0), px(0.0)),
                size: size(px(1200.0), px(720.0)),
            })),
            ..Default::default()
        },
        |window, cx| {
            // Before the first paint: gpui_component::init pins the theme
            // to Light, so a dark-mode machine would otherwise flash white.
            ui::apply_system_appearance(window, cx);

            let view = cx.new(|cx| IssueTracker::new(projection, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        },
    );

    if let Err(err) = opened {
        eprintln!("failed to open window: {err:#}");
    }
}

fn main() {
    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);

    // The app outlives its window, per macOS convention, so the Dock icon
    // needs to be able to bring one back. Registered on the builder rather
    // than inside `run`, which is where this hook lives.
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            open_main_window(cx);
        }
    });

    app.run(move |cx| {
        // Registers gpui-component's global state, theme, and key bindings.
        // Must run before any window is opened.
        gpui_component::init(cx);
        ui::init(cx);
        ui::menus::rebuild(domain::View::default(), cx);

        // The database *is* the application: with no issues there is nothing
        // to show and nothing to serve. Failing loudly beats the alternative,
        // which on macOS is a running process with no window and no
        // explanation.
        let projection = match Store::open().and_then(Projection::load) {
            Ok(projection) => cx.new(|_| projection),
            Err(err) => {
                eprintln!("failed to open the issue database: {err:#}");
                std::process::exit(1);
            }
        };
        app_state::set_projection(projection, cx);

        // Serves whether or not a window is open, which is the point of it.
        ui::api_server::start(cx);

        open_main_window(cx);
    });
}
