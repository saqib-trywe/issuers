// SPDX-License-Identifier: GPL-3.0-only

//! The listening socket for the local HTTP API, and the bridge onto the main
//! thread.
//!
//! Deliberately blocking: a `std::net::TcpListener` on its own thread, one
//! thread per connection. GPUI's executor has no I/O reactor, and the API is a
//! handful of endpoints on loopback — this is the same shape Zed uses for its
//! own in-process HTTP server. Nothing here decides anything; parsing,
//! authenticating and routing all live in `issue_tracker::api`, which is testable
//! without a socket. See `docs/adr/0006`.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gpui::*;

use crate::app_state;
use issue_tracker::api::{Api, Parsed, Request, Response, parse};
use issue_tracker::store;

/// Tried first, so `curl localhost:8787/issues` works without reading a file.
/// An ephemeral port is used when something else already has it.
const DEFAULT_PORT: u16 = 8787;

/// Thread-per-connection with no ceiling is an unbounded thread spawn on a
/// socket every local process can reach.
const MAX_CONNECTIONS: usize = 16;

/// A client that opens a connection and then says nothing must not hold a
/// thread forever.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Headers plus body. Bodies are already capped by the parser; this bounds
/// what a client can make us buffer before we have even parsed the headers.
const MAX_REQUEST: usize = (1 << 20) + (64 * 1024);

/// How long a connection thread waits for the main thread to *start* on its
/// request. Once started, the answer is waited for however long it takes,
/// because by then giving up could only misreport what happened.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// One request, and somewhere to put the answer.
struct Job {
    request: Request,
    reply: SyncSender<Response>,
    claim: Arc<Claim>,
}

/// Who gets to decide a request's fate once it has been queued: the main
/// thread by answering it, or the connection thread by giving up on it.
///
/// Exactly one of them wins. Without this a connection that timed out told
/// its caller the request had failed while the job stayed queued, so a write
/// could land *after* the caller had been told it did not — and a retry would
/// then do it twice.
#[derive(Default)]
struct Claim(AtomicU8);

impl Claim {
    const PENDING: u8 = 0;
    const TAKEN: u8 = 1;
    const ABANDONED: u8 = 2;

    /// The main thread's half: `true` means handle the job, `false` means the
    /// caller has already been told nothing happened, so nothing may.
    fn take(&self) -> bool {
        self.settle(Self::TAKEN)
    }

    /// The connection thread's half: `true` means the job will never run,
    /// `false` means it is already running and its answer is on the way.
    fn abandon(&self) -> bool {
        self.settle(Self::ABANDONED)
    }

    fn settle(&self, to: u8) -> bool {
        self.0
            .compare_exchange(Self::PENDING, to, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// Binds, publishes, and starts serving. Failure is reported and survivable:
/// the API is a convenience, and losing it must never cost you the app.
pub fn start(cx: &mut App) {
    let (listener, port) = match bind() {
        Ok(bound) => bound,
        Err(err) => {
            eprintln!("the local API is unavailable: {err:#}");
            return;
        }
    };

    let token = generate_token();
    // Published only once the listener is genuinely bound, so the file never
    // advertises an address that will refuse connections.
    if let Err(err) = publish(port, &token) {
        eprintln!("the local API is unavailable: could not publish its address: {err:#}");
        return;
    }

    let (sender, receiver) = async_channel::unbounded::<Job>();
    thread::spawn(move || accept_loop(listener, sender));
    serve(Api::new(token), receiver, cx);

    cx.on_app_quit(|_| {
        // The file's presence means "the app is up", so it has to go.
        if let Err(err) = unpublish() {
            eprintln!("failed to remove the API address file: {err:#}");
        }
        async {}
    })
    .detach();

    eprintln!("issue API listening on http://127.0.0.1:{port}");
}

fn bind() -> Result<(TcpListener, u16)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, DEFAULT_PORT))
        .or_else(|_| TcpListener::bind((Ipv4Addr::LOCALHOST, 0)))
        .context("binding to localhost")?;
    let port = listener
        .local_addr()
        .context("reading the bound port")?
        .port();
    Ok((listener, port))
}

/// A fresh token every launch. Nothing long-lived is left on disk, and it
/// makes reading `api.json` the only way to talk to the API — so no client can
/// quietly hardcode a secret and drift.
fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system random source");
    bytes.iter().fold(String::new(), |mut token, byte| {
        use std::fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
        token
    })
}

fn publish(port: u16, token: &str) -> Result<()> {
    let path = store::api_file_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("creating the data directory")?;
    }
    // Removed first: `mode` only applies when the file is created, so reusing
    // an existing one could leave looser permissions in place.
    let _ = std::fs::remove_file(&path);

    let body = serde_json::json!({ "port": port, "token": token }).to_string();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(body.as_bytes())
        .context("writing the API address file")?;
    Ok(())
}

fn unpublish() -> Result<()> {
    let path = store::api_file_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("removing {}", path.display())),
    }
}

// ---- the socket threads -----------------------------------------------------

fn accept_loop(listener: TcpListener, sender: async_channel::Sender<Job>) {
    let live = Arc::new(AtomicUsize::new(0));

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if sender.is_closed() {
            return;
        }

        if live.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            // Shed rather than queue. A local client holding seventeen sockets
            // open is misbehaving, and making it wait only moves the problem.
            let _ = (&stream)
                .write_all(&Response::error(503, "too many concurrent connections").to_bytes());
            continue;
        }

        live.fetch_add(1, Ordering::Relaxed);
        let sender = sender.clone();
        let live = Arc::clone(&live);
        thread::spawn(move || {
            serve_connection(stream, &sender);
            live.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

fn serve_connection(mut stream: TcpStream, sender: &async_channel::Sender<Job>) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));

    let response = match read_request(&mut stream) {
        Ok(request) => {
            let (reply, answer) = sync_channel(1);
            let claim = Arc::new(Claim::default());
            let job = Job {
                request,
                reply,
                claim: Arc::clone(&claim),
            };
            match sender.send_blocking(job) {
                Ok(()) => match answer.recv_timeout(ANSWER_TIMEOUT) {
                    Ok(response) => response,
                    Err(_) if claim.abandon() => Response::error(
                        503,
                        "the application was too busy to answer; nothing was changed",
                    ),
                    // Taken just as we gave up: it is running, so its answer
                    // is the truth and is worth the wait.
                    Err(_) => answer.recv().unwrap_or_else(|_| {
                        Response::error(500, "the application stopped mid-request")
                    }),
                },
                Err(_) => Response::error(503, "the application is shutting down"),
            }
        }
        Err(refusal) => refusal,
    };

    let _ = stream.write_all(&response.to_bytes());
    let _ = stream.flush();
}

fn read_request(stream: &mut TcpStream) -> Result<Request, Response> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];

    loop {
        match parse(&buffer) {
            Parsed::Complete(request) => return Ok(request),
            Parsed::Malformed(why) => return Err(Response::error(400, why)),
            Parsed::Incomplete => {}
        }
        if buffer.len() > MAX_REQUEST {
            return Err(Response::error(413, "request is too large"));
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(Response::error(400, "the connection closed mid-request")),
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(_) => return Err(Response::error(400, "timed out waiting for the request")),
        }
    }
}

// ---- the main thread --------------------------------------------------------

/// Answers jobs on the main thread, one at a time.
///
/// Serialising through here is what makes the API and the UI a single writer:
/// no locks, and no interleaving with a half-applied edit.
fn serve(api: Api, jobs: async_channel::Receiver<Job>, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Ok(job) = jobs.recv().await {
            if !job.claim.take() {
                // Its caller was already told nothing changed.
                continue;
            }
            let mutating = job.request.method != "GET";

            let response = cx.update(|cx| {
                if mutating {
                    super::tracker::flush_pending_edits(cx);
                }
                let projection = app_state::projection(cx);
                projection.update(cx, |projection, cx| {
                    let response = api.handle(&job.request, projection);
                    // Only a write that landed is worth waking every window
                    // for; a read or a rejection changed nothing.
                    if mutating && response.status < 400 {
                        cx.notify();
                    }
                    response
                })
            });

            // The client may have hung up; that is its business.
            let _ = job.reply.send(response);
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings in `gpui::test`, which shadows `#[test]`.
    use super::Claim;

    #[test]
    fn a_job_is_either_answered_or_abandoned_never_both() {
        let taken = Claim::default();
        assert!(taken.take());
        assert!(!taken.abandon(), "a running job cannot be given up on");

        let abandoned = Claim::default();
        assert!(abandoned.abandon());
        assert!(!abandoned.take(), "an abandoned job must not run");
    }
}
