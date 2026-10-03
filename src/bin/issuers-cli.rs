// SPDX-License-Identifier: GPL-3.0-only

//! The `issuers-cli` command. Everything is in `issuers::cli`, so the same
//! code can be driven from an integration test without spawning a process.

use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(issuers::cli::run(std::env::args_os().skip(1).collect()))
}
