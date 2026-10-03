// SPDX-License-Identifier: GPL-3.0-only

//! The parts of the issue tracker that do not draw anything.
//!
//! `domain` holds pure types, `store` persists them, `projection` is the
//! single writer over both, `api` exposes them over HTTP and `cli` consumes
//! that API from a terminal. None of them may depend on `gpui`.
//!
//! The view lives in the `Issuers` binary rather than here, which is what makes
//! that rule structural: nothing in this library can name `ui` or
//! `app_state`. See `docs/adr/0008`.

pub mod api;
pub mod cli;
pub mod client;
pub mod domain;
pub mod operations;
pub mod projection;
pub mod store;
