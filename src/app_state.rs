// SPDX-License-Identifier: GPL-3.0-only

//! The shared [`Projection`], held for the life of the process.
//!
//! It lives here rather than on the window because the HTTP API has to answer
//! whether or not a window is open, and because two windows must not each own
//! a private copy of the same issues. See `docs/adr/0005`.

use gpui::{App, Entity, Global};

use issuers::projection::Projection;

struct GlobalProjection(Entity<Projection>);

impl Global for GlobalProjection {}

/// Installs the shared Projection. Called once, before the first window.
pub fn set_projection(projection: Entity<Projection>, cx: &mut App) {
    cx.set_global(GlobalProjection(projection));
}

/// A handle to the shared Projection.
///
/// Panics if the Projection was never installed, which is a startup-ordering
/// mistake rather than a condition to handle: nothing in this app can do
/// anything useful without it.
pub fn projection(cx: &App) -> Entity<Projection> {
    cx.global::<GlobalProjection>().0.clone()
}
