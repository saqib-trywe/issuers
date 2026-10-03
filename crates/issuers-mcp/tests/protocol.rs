// SPDX-License-Identifier: GPL-3.0-only

//! End to end over stdio and a real socket: the actual binary, speaking real
//! JSON-RPC, against a stub that answers like the API.
//!
//! This is the same shape as `tests/cli.rs` in the parent crate, and for the
//! same reason — it exercises what a mock could not: the handshake, the bytes
//! the client puts on the wire, the address file it reads to find them, and
//! the difference between a tool error and a protocol error.
//!
//! It is one `#[test]` per scenario, each with its own directory, because
//! `ISSUERS_DB` is per-process and the process here is a child.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use serde_json::{Value, json};

const ISSUE: &str = r#"{"id":7,"title":"Fix the flash","body":"a long body","status":"Doing",
    "priority":"Urgent","tags":["ui"],"parent_id":null,"sub_issue_ids":[],"settled_sub_issues":0,
    "size":null,"total_size":null,"unsized_sub_issues":0,
    "created_at":"2026-09-05T14:23:11.482913Z","updated_at":"2026-09-05T14:23:11.482913Z"}"#;

/// A stub API: answers each connection with the next canned reply, and
/// records the request it was sent.
fn start_stub(replies: Vec<(u16, String)>) -> (mpsc::Receiver<String>, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let (sender, seen) = mpsc::channel();

    thread::spawn(move || {
        for (status, body) in replies {
            let Ok(mut stream) = listener.accept().map(|(stream, _)| stream) else {
                return;
            };
            let mut raw = Vec::new();
            let _ = stream.read_to_end(&mut raw);
            let _ = sender.send(String::from_utf8_lossy(&raw).into_owned());

            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    (seen, port)
}

/// A live `issuers-mcp`, already through the handshake.
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl Session {
    fn start(database: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_issuers-mcp"))
            .env("ISSUERS_DB", database)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("launching issuers-mcp");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut session = Session {
            child,
            stdin,
            stdout,
            next_id: 0,
        };

        session.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }),
        );
        session.notify("notifications/initialized");
        session
    }

    fn notify(&mut self, method: &str) {
        let line = json!({ "jsonrpc": "2.0", "method": method, "params": {} });
        writeln!(self.stdin, "{line}").expect("writing a notification");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{line}").expect("writing a request");

        // Anything that is not the answer to this request is a notification.
        loop {
            let mut raw = String::new();
            let read = self.stdout.read_line(&mut raw).expect("reading a reply");
            assert!(read > 0, "the server closed the stream");
            let message: Value = serde_json::from_str(&raw).expect("a JSON-RPC message");
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                return message;
            }
        }
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn publish(directory: &std::path::Path, port: u16) {
    std::fs::write(
        directory.join("api.json"),
        format!(r#"{{"port":{port},"token":"deadbeef"}}"#),
    )
    .expect("publishing the address");
}

fn scratch(name: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!("issuers-mcp-{name}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a scratch directory");
    directory
}

/// The result of a tool call, as an agent would read it.
fn tool_result(reply: &Value) -> &Value {
    reply.get("result").unwrap_or_else(|| {
        panic!(
            "expected a tool result, got a protocol error: {}",
            reply["error"]["message"]
        )
    })
}

fn is_error(reply: &Value) -> bool {
    tool_result(reply)["isError"] == json!(true)
}

fn error_text(reply: &Value) -> String {
    tool_result(reply)["content"][0]["text"]
        .as_str()
        .expect("error text")
        .to_string()
}

#[test]
fn the_handshake_advertises_nine_tools_and_the_domain_s_rules() {
    let directory = scratch("handshake");
    let mut session = Session::start(&directory.join("issues.db"));

    let listed = session.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().expect("tools");
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();

    assert_eq!(names.len(), 9, "nine tools: {names:?}");
    // The one operation deliberately withheld from an agent: deleting erases
    // a mistake, which is the person's judgement to make. See docs/adr/0009.
    assert!(!names.contains(&"delete_issue"));
    for expected in [
        "list_issues",
        "get_issue",
        "create_issue",
        "update_issue",
        "add_tag",
        "remove_tag",
        "add_sub_issue",
        "remove_sub_issue",
        "list_tags",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }

    let reads = tools
        .iter()
        .find(|tool| tool["name"] == "list_issues")
        .unwrap();
    assert_eq!(reads["annotations"]["readOnlyHint"], json!(true));

    // Statuses come from the domain, so this list cannot drift from the one
    // the tracker accepts.
    let status = &tools
        .iter()
        .find(|tool| tool["name"] == "create_issue")
        .unwrap()["inputSchema"]["$defs"]["Status"]["enum"];
    assert_eq!(
        status,
        &json!(["Todo", "Doing", "Blocked", "Done", "Cancelled"])
    );
}

#[test]
fn a_missing_app_is_a_tool_error_not_a_dead_server() {
    let directory = scratch("no-app");
    // No api.json: the app has never been opened.
    let mut session = Session::start(&directory.join("issues.db"));

    let reply = session.call("list_issues", json!({}));
    assert!(is_error(&reply));
    assert!(
        error_text(&reply).contains("not running"),
        "{}",
        error_text(&reply)
    );

    // The server must still be there afterwards. An MCP client launches it
    // once for a whole session, so exiting would take the tools away for good.
    let listed = session.request("tools/list", json!({}));
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 9);
}

#[test]
fn a_read_reaches_the_api_and_comes_back_structured() {
    let directory = scratch("read");
    let (seen, port) = start_stub(vec![(200, format!("[{ISSUE}]"))]);
    publish(&directory, port);

    let mut session = Session::start(&directory.join("issues.db"));
    let reply = session.call(
        "list_issues",
        json!({ "status": "doing", "tag": "ui/theme", "search": "flash" }),
    );

    let request = seen
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    // Status is folded to the domain's spelling; a tag's slash is escaped in
    // a query value, where it is not part of the path.
    assert!(
        request.starts_with("GET /issues?status=Doing&tag=ui%2Ftheme&q=flash HTTP/1.1"),
        "{request}"
    );
    assert!(request.contains("Authorization: Bearer deadbeef"));
    assert!(request.contains("Host: 127.0.0.1:"));
    // The API refuses anything that looks like a browser.
    assert!(!request.to_lowercase().contains("origin:"));

    let issues = &tool_result(&reply)["structuredContent"]["issues"];
    assert_eq!(issues[0]["id"], json!(7));
    assert_eq!(issues[0]["title"], json!("Fix the flash"));
    // Listing is for finding: bodies are dropped, and only bodies.
    assert!(issues[0].get("body").is_none());
    assert_eq!(issues[0]["tags"], json!(["ui"]));
    // A text copy travels beside the structured content.
    assert!(
        tool_result(&reply)["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Fix the flash")
    );
}

#[test]
fn a_refusal_arrives_as_the_api_s_own_sentence() {
    let directory = scratch("refusal");
    let (_seen, port) = start_stub(vec![(
        409,
        r#"{"error":"2 sub-issue(s) are still outstanding"}"#.to_string(),
    )]);
    publish(&directory, port);

    let mut session = Session::start(&directory.join("issues.db"));
    let reply = session.call("update_issue", json!({ "id": 1, "status": "Done" }));

    // A 409 is information an agent should act on, not a malfunction — so a
    // tool error carrying the sentence, never a protocol error.
    assert!(is_error(&reply));
    assert_eq!(error_text(&reply), "2 sub-issue(s) are still outstanding");
}

#[test]
fn an_argument_we_were_never_asked_for_is_refused() {
    let directory = scratch("unknown-argument");
    // No reply is queued: nothing should reach the socket at all.
    let (seen, port) = start_stub(Vec::new());
    publish(&directory, port);

    let mut session = Session::start(&directory.join("issues.db"));

    // `PATCH {tags:[…]}` replaces the whole set, so update_issue does not
    // take tags. Ignoring the field would report success and change nothing —
    // the same trap `pico-args` sets for the CLI.
    let reply = session.call("update_issue", json!({ "id": 1, "tags": ["ui"] }));
    assert!(is_error(&reply));
    assert!(
        error_text(&reply).contains("tags"),
        "{}",
        error_text(&reply)
    );

    let reply = session.call("update_issue", json!({ "id": 1, "statuss": "Done" }));
    assert!(error_text(&reply).contains("statuss"));

    // An unknown status names the ones that exist, from Status::ALL.
    let reply = session.call("update_issue", json!({ "id": 1, "status": "Wontfix" }));
    assert!(error_text(&reply).contains("Wontfix"));
    assert!(error_text(&reply).contains("Cancelled"));

    assert!(
        seen.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "a rejected argument must not reach the API"
    );
}

#[test]
fn a_tag_name_travels_whole() {
    let directory = scratch("tags");
    let (seen, port) = start_stub(vec![(200, ISSUE.to_string())]);
    publish(&directory, port);

    let mut session = Session::start(&directory.join("issues.db"));
    session.call("add_tag", json!({ "id": 7, "name": "needs design/ui" }));

    let request = seen
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    // The API reads the whole path remainder as the name, so the slash stays
    // and the space does not.
    assert!(
        request.starts_with("PUT /issues/7/tags/needs%20design/ui HTTP/1.1"),
        "{request}"
    );
}
