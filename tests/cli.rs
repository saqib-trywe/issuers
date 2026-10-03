// SPDX-License-Identifier: GPL-3.0-only

//! End to end over a real socket, against a stub that speaks the API's
//! responses.
//!
//! This exercises the parts unit tests cannot reach: the request bytes the
//! client puts on the wire, the address file it reads to find them, and the
//! exit codes it maps failures onto. There is deliberately no test here that
//! needs the application window — a suite that needs a GUI stops being run.
//!
//! It is one `#[test]` on purpose. `ISSUERS_DB` is process-wide state,
//! and cargo runs the tests in a file on parallel threads.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

use issuers::cli::{self, Failure};
use termcolor::Buffer;

/// One canned exchange: what the stub should reply with next.
struct Stub {
    /// The request lines the client sent, in order.
    seen: mpsc::Receiver<String>,
}

fn start(replies: Vec<(u16, String)>) -> (Stub, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let (sender, seen) = mpsc::channel();

    thread::spawn(move || {
        for (status, body) in replies {
            let Ok(mut stream) = listener.accept().map(|(stream, _)| stream) else {
                return;
            };
            let request = read_request(&mut stream);
            let _ = sender.send(request);

            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    (Stub { seen }, port)
}

/// The client shuts down its write half after sending, so read-to-end here
/// terminates without needing to understand Content-Length.
fn read_request(stream: &mut TcpStream) -> String {
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    String::from_utf8_lossy(&raw).into_owned()
}

fn publish(dir: &std::path::Path, port: u16) {
    std::fs::write(
        dir.join("api.json"),
        format!(r#"{{"port":{port},"token":"deadbeef"}}"#),
    )
    .expect("publishing the address");
}

fn run(line: &str) -> (Result<(), Failure>, String) {
    let mut buffer = Buffer::no_color();
    let outcome = cli::run_into(
        line.split_whitespace().map(Into::into).collect(),
        &mut buffer,
    );
    (outcome, String::from_utf8(buffer.into_inner()).unwrap())
}

const ISSUE: &str = r#"{"id":7,"title":"Fix the flash","body":"","status":"Doing",
    "priority":"Urgent","tags":["ui"],"parent_id":null,"sub_issue_ids":[],"settled_sub_issues":0,
    "size":null,"total_size":null,"unsized_sub_issues":0,
    "created_at":"2026-09-05T14:23:11.482913Z","updated_at":"2026-09-05T14:23:11.482913Z"}"#;

#[test]
fn the_cli_end_to_end() {
    let dir = std::env::temp_dir().join(format!("issue-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    let db = dir.join("issues.db");
    // Safety: this test file is a process of its own and runs one test.
    unsafe { std::env::set_var("ISSUERS_DB", &db) };

    // ---- the app is not running ---------------------------------------------
    let _ = std::fs::remove_file(dir.join("api.json"));
    let (outcome, _) = run("list");
    let failure = outcome.expect_err("no address file means no API");
    assert_eq!(
        failure.code(),
        3,
        "a wrapper must be able to tell this apart"
    );
    assert!(failure.message().contains("not running"), "{failure:?}");

    // ---- a published address nothing answers --------------------------------
    // A port nothing is listening on: the app crashed and left the file.
    let dead = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    publish(&dir, dead_port);
    let failure = run("list").0.expect_err("nothing is listening");
    assert_eq!(failure.code(), 3);
    assert!(failure.message().contains("stale"), "{failure:?}");

    // ---- an address file that cannot be read --------------------------------
    // Present, so "not running" would be the wrong answer, but unusable — so
    // this is a failure rather than an absence, and exits 1 rather than 3.
    std::fs::write(dir.join("api.json"), "not json").expect("an unreadable address");
    let failure = run("list").0.expect_err("the address cannot be parsed");
    assert_eq!(failure.code(), 1, "unreadable is not the same as absent");
    assert!(failure.message().contains("not readable"), "{failure:?}");

    // ---- a listing, and what went on the wire -------------------------------
    let (stub, port) = start(vec![(200, format!("[{ISSUE}]"))]);
    publish(&dir, port);

    let (outcome, printed) = run("list --status doing --tag ui --search flash");
    outcome.expect("a 200 listing");
    let sent = stub.seen.recv().expect("the stub saw a request");

    assert!(sent.starts_with("GET /issues?"), "{sent}");
    assert!(
        sent.contains("status=Doing"),
        "case-folded on the way in: {sent}"
    );
    assert!(
        sent.contains("tag=ui") && sent.contains("q=flash"),
        "{sent}"
    );
    assert!(sent.contains("Authorization: Bearer deadbeef"), "{sent}");
    // The server refuses anything carrying an Origin, and insists on a
    // loopback Host. Both are the client's job to get right.
    assert!(sent.contains("Host: 127.0.0.1:"), "{sent}");
    assert!(!sent.to_lowercase().contains("origin:"), "{sent}");
    assert!(printed.contains("Fix the flash"), "{printed}");
    assert!(printed.contains("Doing"), "{printed}");

    // ---- a refusal keeps the API's own sentence -----------------------------
    let (_stub, port) = start(vec![(
        409,
        r#"{"error":"2 sub-issue(s) are still outstanding"}"#.into(),
    )]);
    publish(&dir, port);

    let failure = run("set 7 --status done").0.expect_err("a 409");
    assert_eq!(failure.code(), 1, "refused is not the same as unreachable");
    assert_eq!(failure.message(), "2 sub-issue(s) are still outstanding");

    // ---- --json passes the API's bytes through unchanged --------------------
    let (_stub, port) = start(vec![(200, ISSUE.to_string())]);
    publish(&dir, port);

    let (outcome, printed) = run("show 7 --json");
    outcome.expect("a 200");
    assert_eq!(
        printed.trim(),
        ISSUE,
        "re-serialised rather than passed through"
    );

    // ---- a spelling mistake never reaches the socket ------------------------
    let failure = run("set 7 --statuss done").0.expect_err("unknown option");
    assert_eq!(failure.code(), 2);
    assert!(failure.message().contains("--statuss"), "{failure:?}");

    // ---- several tags, refused partway --------------------------------------
    let (_stub, port) = start(vec![
        (200, ISSUE.to_string()),
        (400, r#"{"error":"unusable tag name"}"#.into()),
    ]);
    publish(&dir, port);

    let failure = run("tag add 7 ui bad")
        .0
        .expect_err("the second is refused");
    assert_eq!(failure.code(), 1);
    // Saying which ones landed is the whole point: the first one did.
    assert!(
        failure.message().contains("already applied: ui"),
        "{failure:?}"
    );

    // ---- filing, and the body that goes with it -----------------------------
    let (stub, port) = start(vec![(201, ISSUE.to_string()), (200, format!("[{ISSUE}]"))]);
    publish(&dir, port);

    let (outcome, printed) = run("new Ship-it --status doing --priority urgent --tag ui");
    outcome.expect("a 201");
    let sent = stub.seen.recv().expect("a request");
    assert!(sent.starts_with("POST /issues "), "{sent}");
    let body = sent.rsplit("\r\n\r\n").next().unwrap();
    assert!(body.contains(r#""title":"Ship-it""#), "{body}");
    assert!(body.contains(r#""status":"Doing""#), "case-folded: {body}");
    assert!(body.contains(r#""priority":"Urgent""#), "{body}");
    assert!(body.contains(r#""tags":["ui"]"#), "{body}");
    // A patch names only what it changes, and creation is no different.
    assert!(
        !body.contains("null"),
        "absent fields must be absent: {body}"
    );
    assert!(printed.contains("Fix the flash"), "{printed}");

    // ---- showing an Issue names its neighbours ------------------------------
    // At most three requests: the Issue, its parent, its children in one go.
    let parent = ISSUE.replace(r#""id":7"#, r#""id":1"#);
    let child = ISSUE
        .replace(r#""id":7"#, r#""id":9"#)
        .replace(r#""parent_id":null"#, r#""parent_id":7"#);
    let family = ISSUE
        .replace(r#""parent_id":null"#, r#""parent_id":1"#)
        .replace(r#""sub_issue_ids":[]"#, r#""sub_issue_ids":[9]"#);
    let (stub, port) = start(vec![
        (200, family),
        (200, parent),
        (200, format!("[{child}]")),
    ]);
    publish(&dir, port);

    let (outcome, printed) = run("show 7");
    outcome.expect("a 200");
    assert!(
        stub.seen.recv().unwrap().starts_with("GET /issues/7 "),
        "the Issue"
    );
    assert!(
        stub.seen.recv().unwrap().starts_with("GET /issues/1 "),
        "its parent"
    );
    let children = stub.seen.recv().unwrap();
    assert!(
        children.starts_with("GET /issues?parent=7 "),
        "every child in one request: {children}"
    );
    assert!(printed.contains("Part of"), "{printed}");

    // ---- the tag list ------------------------------------------------------
    let (_stub, port) = start(vec![(200, r#"[{"name":"ui","count":3}]"#.into())]);
    publish(&dir, port);
    let (outcome, printed) = run("tags");
    outcome.expect("a 200");
    assert!(printed.contains("ui") && printed.contains("3"), "{printed}");

    // ---- attaching sub-issues ----------------------------------------------
    let (stub, port) = start(vec![(200, ISSUE.to_string()), (200, ISSUE.to_string())]);
    publish(&dir, port);
    run("sub add 7 9").0.expect("a 200");
    let sent = stub.seen.recv().unwrap();
    assert!(sent.starts_with("PUT /issues/7/sub-issues/9 "), "{sent}");

    // ---- deleting is refused without a terminal, unless forced --------------
    let failure = run("rm 7").0.expect_err("not a terminal");
    assert_eq!(failure.code(), 2, "a bad invocation, not a failed request");
    assert!(failure.message().contains("--force"), "{failure:?}");

    let (stub, port) = start(vec![(204, String::new())]);
    publish(&dir, port);
    let (outcome, printed) = run("rm 7 --force");
    outcome.expect("a 204");
    assert!(stub.seen.recv().unwrap().starts_with("DELETE /issues/7 "));
    assert!(printed.contains("Deleted #7"), "{printed}");

    std::fs::remove_dir_all(&dir).ok();
}
