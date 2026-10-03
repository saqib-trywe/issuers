// SPDX-License-Identifier: GPL-3.0-only

//! A blocking HTTP client for one server, on one host, over one socket.
//!
//! Hand-rolled for the same reason the server was: everything here is
//! loopback, so there is no TLS to negotiate, no redirect to follow, no proxy
//! to honour and no chunked encoding to reassemble. The server writes
//! `Connection: close` and serves one request per connection, so end-of-stream
//! delimits the body and there is no keep-alive bookkeeping either.
//!
//! Shared by every client of the API — the `issuers-cli` command and the MCP server
//! both reach the app through here, and neither reaches the database. It knows
//! only what the transport can tell it apart: whether there was an app to talk
//! to. What a caller *does* about that — an exit code, a tool error — is the
//! caller's business, which is why this does not know about either.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::Duration;

use serde::Deserialize;

use crate::operations::Call;
use crate::store;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Why a request did not produce a reply.
///
/// Only the distinction the transport can actually make. An API that answered
/// with a refusal is a [`Reply`], not an error — reading a 409 is the caller's
/// job, and the sentence it carries is usually worth showing.
#[derive(Debug, PartialEq, Eq)]
pub enum ClientError {
    /// There is no app to talk to: no address file, or nothing behind it.
    NotRunning(String),
    /// The request was attempted and something broke.
    Failed(String),
}

impl ClientError {
    fn not_running(message: impl Into<String>) -> Self {
        ClientError::NotRunning(message.into())
    }

    fn failed(message: impl Into<String>) -> Self {
        ClientError::Failed(message.into())
    }

    pub fn message(&self) -> &str {
        match self {
            ClientError::NotRunning(message) | ClientError::Failed(message) => message,
        }
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(self.message())
    }
}

impl std::error::Error for ClientError {}

/// What the app publishes beside the database once its listener is bound.
#[derive(Deserialize)]
struct Address {
    port: u16,
    token: String,
}

pub struct Client {
    address: SocketAddr,
    token: String,
}

pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Reply {
    /// The one-sentence message every API failure carries.
    pub fn error_message(&self) -> String {
        #[derive(Deserialize)]
        struct ErrorBody {
            error: String,
        }
        match serde_json::from_slice::<ErrorBody>(&self.body) {
            Ok(body) => body.error,
            Err(_) => format!("the API returned {} with no explanation", self.status),
        }
    }
}

/// Reads the published address. Its absence *is* the signal that the app is
/// not running, so that case is reported as such rather than as a failure.
pub fn connect() -> Result<Client, ClientError> {
    let path = store::api_file_path().map_err(|err| ClientError::failed(format!("{err:#}")))?;

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ClientError::not_running(
                "Issuers is not running. Open it and try again.".to_string(),
            ));
        }
        Err(err) => {
            return Err(ClientError::failed(format!(
                "reading {}: {err}",
                path.display()
            )));
        }
    };

    let address: Address = serde_json::from_str(&raw)
        .map_err(|err| ClientError::failed(format!("{} is not readable: {err}", path.display())))?;

    Ok(Client {
        address: SocketAddr::from(([127, 0, 0, 1], address.port)),
        token: address.token,
    })
}

impl Client {
    /// Performs one [`Call`]. The only way to reach the API — which is what
    /// makes `operations` the only place that decides what a request looks
    /// like, rather than merely the tidiest.
    pub fn send(&self, call: &Call) -> Result<Reply, ClientError> {
        let (method, path, body) = (call.method.name(), call.path.as_str(), call.method.body());
        let mut stream = TcpStream::connect_timeout(&self.address, TIMEOUT).map_err(|err| {
            // A published address that nothing answers means the app died
            // without cleaning up. Same remedy, so same exit code — but the
            // wording has to say the file is stale, or the next person will
            // go looking for a bug in the client.
            if matches!(
                err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut
            ) {
                ClientError::not_running(
                    "Issuers is not running — it left a stale address file behind. \
                     Open it and try again."
                        .to_string(),
                )
            } else {
                ClientError::failed(format!("connecting to the API: {err}"))
            }
        })?;
        stream.set_read_timeout(Some(TIMEOUT)).ok();
        stream.set_write_timeout(Some(TIMEOUT)).ok();

        stream
            .write_all(&self.request_bytes(method, path, body))
            .and_then(|()| stream.flush())
            .map_err(|err| ClientError::failed(format!("sending the request: {err}")))?;
        // Tells the server the request is complete without waiting on it.
        stream.shutdown(Shutdown::Write).ok();

        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .map_err(|err| ClientError::failed(format!("reading the response: {err}")))?;

        parse_reply(&raw)
    }

    fn request_bytes(&self, method: &str, path: &str, body: Option<&str>) -> Vec<u8> {
        // `Host` must be a loopback spelling and there must be no `Origin`:
        // both are checked by the server's guard against browser-driven
        // requests, and both are satisfied by simply being honest here.
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\n\
             Host: 127.0.0.1:{}\r\n\
             Authorization: Bearer {}\r\n\
             Connection: close\r\n",
            self.address.port(),
            self.token,
        );
        match body {
            Some(body) => {
                head.push_str("Content-Type: application/json\r\n");
                head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
                head.push_str(body);
            }
            None => head.push_str("Content-Length: 0\r\n\r\n"),
        }
        head.into_bytes()
    }
}

fn parse_reply(raw: &[u8]) -> Result<Reply, ClientError> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| ClientError::failed("the API sent a truncated response"))?;

    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| ClientError::failed("the API sent an unreadable status line"))?;

    Ok(Reply {
        status,
        body: raw[split + 4..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_says_only_what_the_transport_can_tell_apart() {
        // Whether there was an app to talk to. What a caller does about it —
        // an exit code, a tool error — is the caller's business.
        let gone = ClientError::not_running("Issuers is not running.");
        assert_eq!(gone.message(), "Issuers is not running.");
        assert_eq!(gone.to_string(), "Issuers is not running.");
        assert_eq!(
            ClientError::failed("the socket closed").message(),
            "the socket closed"
        );
        assert_ne!(gone, ClientError::failed("Issuers is not running."));
    }

    #[test]
    fn a_reply_is_split_at_the_blank_line() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]";
        let reply = parse_reply(raw).unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, b"[]");
    }

    #[test]
    fn a_body_containing_a_blank_line_survives() {
        let raw = b"HTTP/1.1 200 OK\r\n\r\n{\"body\":\"one\\r\\n\\r\\ntwo\"}";
        let reply = parse_reply(raw).unwrap();
        assert_eq!(reply.body, b"{\"body\":\"one\\r\\n\\r\\ntwo\"}");
    }

    #[test]
    fn an_empty_body_is_fine() {
        let raw = b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n";
        let reply = parse_reply(raw).unwrap();
        assert_eq!(reply.status, 204);
        assert!(reply.body.is_empty());
    }

    #[test]
    fn a_truncated_response_is_an_error_not_a_panic() {
        assert!(parse_reply(b"HTTP/1.1 200 OK\r\n").is_err());
        assert!(parse_reply(b"").is_err());
        assert!(parse_reply(b"garbage\r\n\r\n").is_err());
    }

    #[test]
    fn an_error_body_yields_the_api_s_own_sentence() {
        let reply = Reply {
            status: 409,
            body: br#"{"error":"3 sub-issue(s) are still outstanding"}"#.to_vec(),
        };
        assert_eq!(
            reply.error_message(),
            "3 sub-issue(s) are still outstanding"
        );

        let opaque = Reply {
            status: 500,
            body: b"not json".to_vec(),
        };
        assert!(opaque.error_message().contains("500"));
    }
}
