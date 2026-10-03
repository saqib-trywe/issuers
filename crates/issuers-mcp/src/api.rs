// SPDX-License-Identifier: GPL-3.0-only

//! The bridge to the running app.
//!
//! Everything an agent asks for arrives here as one [`Call`] — built by
//! `issuers::operations`, the same module the `issuers-cli` command builds its
//! requests with — and leaves as either the JSON the API sent or a sentence
//! explaining why it did not. The tracker is never opened directly: this
//! process is a client of the running app. See `docs/adr/0009`.

use issuers::client::{self, ClientError};
use issuers::operations::Call;
use serde_json::Value;

/// Sends one request, off the async runtime.
///
/// The client is deliberately blocking and will wait up to ten seconds — and
/// the thread that answers it is the app's *main* thread, which is also the
/// one painting the window. Holding a runtime worker for that long would stop
/// this server answering anything at all, including cancellation.
pub async fn send(call: Call) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || blocking(&call))
        .await
        .unwrap_or_else(|err| Err(format!("the request could not be run: {err}")))
}

fn blocking(call: &Call) -> Result<Value, String> {
    // Connecting per call rather than once at startup is load-bearing. The
    // app regenerates its bearer token on every launch, so an address read
    // when this server started goes stale the moment you quit and reopen
    // Issues — and the failure would look like a bug in authentication
    // rather than a cache that needed dropping.
    let client = client::connect().map_err(describe)?;
    let reply = client.send(call).map_err(describe)?;

    // The API's own sentence, unedited. A 409 here is not a malfunction —
    // "2 sub-issue(s) are still outstanding" is something an agent should
    // read and act on, so it must survive all the way to the tool result.
    if reply.status >= 400 {
        return Err(reply.error_message());
    }

    if reply.body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&reply.body)
        .map_err(|err| format!("the API sent JSON that could not be read: {err}"))
}

fn describe(err: ClientError) -> String {
    err.message().to_string()
}
