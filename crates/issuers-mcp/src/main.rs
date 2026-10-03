// SPDX-License-Identifier: GPL-3.0-only

//! `issuers-mcp` — the Model Context Protocol surface of the Issuers tracker.
//!
//! A separate process that speaks MCP on stdio and reaches the running app
//! through its local HTTP API. It is a client, never a second writer: the app
//! remains the only thing that opens the database. See `docs/adr/0009`.

mod api;
mod args;
mod tools;

use rmcp::ServiceExt;
use rmcp::transport::stdio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Stdout *is* the protocol channel: one stray line of prose written there
    // lands in the middle of a JSON-RPC message and the client's parser fails
    // on it. Diagnostics go to stderr, and stay silent unless RUST_LOG asks.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Serving starts whether or not the app is running. An MCP client launches
    // this once and keeps it for the whole session, so exiting because the
    // tracker happens to be closed would take the tools away for good; each
    // call reports it instead.
    let service = tools::Issues.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
