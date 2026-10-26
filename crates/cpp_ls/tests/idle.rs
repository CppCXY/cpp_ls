//! **A session that has nothing to do must tell the client nothing.**
//!
//! The report that produced this file: "the server keeps showing diagnostics and taking them away again, and it
//! sits on one core, just because I opened a file". Its log showed `index slice: prepared 0, committed 0 … 4 to
//! cook, 2 left` every ~250 ms for minutes after the index had drained — a cooking queue that never emptied (see
//! `docs/cook-loop.md` for the two causes and their fix).
//!
//! A unit test cannot see that: it is a property of the **flow** between the pump, the session and the client. This
//! one can: it starts the real server over stdio, opens a file, waits for the workspace to settle, and then watches
//! the wire. What it asserts is the thing a user notices —
//!
//! ```text
//! · while idle, the server sends no `workspace/*/refresh` and no `publishDiagnostics` at all
//! · one edit does not send a **diagnostic** refresh either — that message is gone from the server altogether
//! ```
//!
//! # Why the client advertises refresh support
//!
//! Because it is what VSCode sends for a pull-diagnostics setup, and a test that hid the capability would prove
//! nothing about a server that wanted to use it. The inlay-hint and semantic-token refreshes are still real, and are
//! what the edit budget below is about; the **diagnostic** refresh is asserted to be absent, because asking a pull
//! client to ask again is what made it ask in a loop (see `docs/cook-loop.md`).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long the server is watched after the workspace has settled.
///
/// Five seconds, because the loop this guards was *slow*: the pump's own no-progress guard fired every eight
/// slices, and a drain sent its refreshes then — a shorter window could see nothing and pass while the defect was
/// live. It is the whole cost of the test, and it buys the one property that cannot be asserted from inside.
const IDLE_WINDOW: Duration = Duration::from_secs(5);

/// How many server-initiated messages one edit may cause.
///
/// The honest set after an edit is: at most one *answer* refresh for the hints and one for the tokens — and **no
/// diagnostic refresh at all**, which is asserted separately below. Three leaves room for a progress notification
/// and nothing else; the loop this guards produced one every few slices.
const EDIT_BUDGET: usize = 3;

const MAIN_CPP: &str = r#"#include "a.h"

int main() {
    return a_value();
}
"#;

const A_H: &str = "#pragma once\ninline int a_value() { return 1; }\n";

#[test]
fn an_idle_session_sends_the_client_nothing() {
    let project = Project::new("idle");
    project.write("main.cpp", MAIN_CPP);
    project.write("a.h", A_H);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            // **Only the capabilities whose absence would make the test vacuous**: without refresh support for
            // diagnostics and hints the server would not want to send anything, and silence would prove nothing.
            // `semanticTokens` is left out on purpose: its capability object has required fields (`requests`,
            // `tokenTypes`, …) that a hand-written fixture should not have to spell, and the token refresh shares
            // the code path with the hint refresh.
            "capabilities": {
                "window": { "workDoneProgress": true },
                "textDocument": {
                    "diagnostic": { "dynamicRegistration": true, "refreshSupport": true },
                    "inlayHint": { "dynamicRegistration": true, "refreshSupport": true },
                },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    // **Settled means "the diagnostics request is answered"** — the pull path prepares the file before it answers,
    // so an answer is the server saying "the work for this file is done".
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let response = server.request(
            10,
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": main_uri } }),
        );
        assert!(
            response.get("result").is_some(),
            "the diagnostic request was answered: {response}"
        );
        if server.quiet() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // **The observation.** Everything the server says while nothing happens.
    let idle = server.listen(IDLE_WINDOW);
    let noise: Vec<String> = idle
        .iter()
        .filter_map(|message| message.get("method").and_then(Value::as_str))
        .map(str::to_string)
        .filter(|method| {
            method.contains("refresh")
                || method.ends_with("publishDiagnostics")
                || method.ends_with("progress")
        })
        .collect();
    assert!(
        noise.is_empty(),
        "an idle session sent {} message(s) in {IDLE_WINDOW:?}: {noise:?}",
        noise.len()
    );

    // **One edit, and a bounded answer.** The client edits a file, so the server owes it an updated reading — but
    // a bounded one: the refreshes are what the client cannot infer.
    let edited = MAIN_CPP.replace("return a_value();", "return a_value() + 1;");
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": main_uri, "version": 2 },
            "contentChanges": [{ "text": edited }],
        }),
    );
    let after_edit = server.listen(Duration::from_secs(2));
    let methods: Vec<&str> = after_edit
        .iter()
        .filter_map(|message| message.get("method").and_then(Value::as_str))
        .collect();
    let refreshes = methods
        .iter()
        .filter(|method| method.contains("refresh"))
        .count();
    assert!(
        refreshes <= EDIT_BUDGET,
        "one edit caused {refreshes} refresh request(s) (budget {EDIT_BUDGET}): {methods:?}"
    );

    // **And never this one.** `workspace/diagnostic/refresh` is not sent by this server at all: a pull client asks on
    // its own rhythm, and the request made it ask in a loop — the reason this test exists.
    assert!(
        !methods.contains(&"workspace/diagnostic/refresh"),
        "the server asked for a diagnostic refresh, which it must never do: {methods:?}"
    );
}

/// A temporary project on disk.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir().join("cpp_ls-idle").join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a project directory");
        Project { root }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn write(&self, name: &str, text: &str) {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a directory");
        }
        std::fs::write(path, text).expect("a file");
    }
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
}

impl Server {
    fn start(root: &Path) -> Server {
        let log_dir = root.join("log");
        let _ = std::fs::create_dir_all(&log_dir);
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg(&log_dir)
            .arg("--log-level")
            .arg("info")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the server starts");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, messages): (Sender<Value>, Receiver<Value>) = std::sync::mpsc::channel();
        std::thread::spawn(move || read_messages(stdout, tx));
        Server {
            child,
            stdin,
            messages,
        }
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_string(&message).expect("a message serializes");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len())
            .expect("the message is written");
        self.stdin.flush().expect("the message is flushed");
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.messages.recv_timeout(left) {
                Ok(message) if message.get("id") == Some(&json!(id)) => return message,
                Ok(_) => {}
                Err(error) => panic!("the server did not answer: {error}"),
            }
        }
    }

    /// Is nothing waiting? A settled workspace is quiet, so this is the signal that the index has drained.
    fn quiet(&mut self) -> bool {
        std::thread::sleep(Duration::from_millis(300));
        self.messages.try_recv().is_err()
    }

    /// Every message the server sent in `window`, responses to something the test asked included.
    fn listen(&mut self, window: Duration) -> Vec<Value> {
        let deadline = Instant::now() + window;
        let mut heard = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return heard;
            }
            match self.messages.recv_timeout(left) {
                Ok(message) => heard.push(message),
                Err(_) => return heard,
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_messages(stdout: ChildStdout, tx: Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut length = None;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                return;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some(rest) = header.strip_prefix("Content-Length:") {
                length = rest.trim().parse::<usize>().ok();
            }
        }
        let Some(length) = length else {
            return;
        };
        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&body) else {
            continue;
        };
        if tx.send(message).is_err() {
            return;
        }
    }
}

fn uri_of(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        text.insert(0, '/');
    }
    format!("file://{text}")
}
