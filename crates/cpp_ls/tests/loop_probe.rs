//! **A probe, not a test**: drive the real server over a real project the way an editor does, and report what its
//! log says about repeated work.
//!
//! Run it by hand:
//!
//! ```text
//! CPPLS_LOOP_ROOT=E:\EmmyLuaCodeStyle CPPLS_LOOP_FILE=CodeFormatCLib/src/CodeFormat.cpp \
//!   cargo test -p cpp_ls --release --test loop_probe -- --ignored --nocapture
//! ```
//!
//! It opens the file, edits it once, and pulls diagnostics ten times with a pause between — the rhythm of a person
//! waiting for squiggles — then counts the log's repeated lines. The question it answers is the one a log cannot:
//! *which* of the repeated messages is the loop.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[test]
#[ignore = "a probe: needs CPPLS_LOOP_ROOT and CPPLS_LOOP_FILE, and prints instead of asserting"]
fn what_repeats() {
    let root = PathBuf::from(std::env::var("CPPLS_LOOP_ROOT").expect("CPPLS_LOOP_ROOT"));
    let file = root.join(std::env::var("CPPLS_LOOP_FILE").expect("CPPLS_LOOP_FILE"));
    let text = std::fs::read_to_string(&file).expect("the file is readable");

    let log_dir = std::env::temp_dir().join("cpp_ls-loop-probe");
    let _ = std::fs::remove_dir_all(&log_dir);
    std::fs::create_dir_all(&log_dir).expect("a log directory");

    let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
        .arg("--log-path")
        .arg(&log_dir)
        .arg("--log-level")
        .arg("info")
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("CPPLS_TRACE_COOKING", "1")
        .spawn()
        .expect("the server starts");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, messages): (Sender<Value>, Receiver<Value>) = std::sync::mpsc::channel();
    std::thread::spawn(move || read_messages(stdout, tx));
    let mut server = Server {
        child,
        stdin,
        messages,
    };

    let uri = uri_of(&file);
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": {
                "window": { "workDoneProgress": true },
                "textDocument": { "diagnostic": { "dynamicRegistration": true, "refreshSupport": true } },
                // **What VSCode really sends**, and the capability the removed refresh used to read:
                "workspace": { "diagnostics": { "refreshSupport": true } },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": text } }),
    );

    // **Wait for the workspace to settle** — the server indexes on its own, and the loop this probe is about is the one
    // *after* the index: the reader's log showed `republishing` alternating with `index slice: 4 to cook, 9 left`
    // while nothing was being typed.
    let settled_at = Instant::now();
    std::thread::sleep(Duration::from_secs(25));
    println!("waited {:?} for the index", settled_at.elapsed());

    // Everything the log said **from here on** is the answer: the segment after the workspace settled.
    let logged_from = log_lines(&log_dir).len();

    println!("--- settled, now one edit and ten pulls");
    let edited = format!("{text}\n// one line\n");
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": edited }],
        }),
    );
    let started = Instant::now();
    for id in 200..210 {
        std::thread::sleep(Duration::from_millis(250));
        let _ = server.request(
            id,
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri } }),
        );
    }
    println!("ten pulls over {:?}", started.elapsed());

    let _ = server.request(99, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    drop(server);

    // **What the log says after the workspace settled**, counted by kind — the numbers a reader of the log cannot
    // count by eye, and the segment that answers "does it repeat while nothing is being typed".
    let entries: Vec<String> = log_lines(&log_dir).into_iter().skip(logged_from).collect();
    let count = |needle: &str| entries.iter().filter(|line| line.contains(needle)).count();
    println!("--- the log, by kind");
    for needle in [
        "republishing open file diagnostics",
        "want_the_closure_cooked(",
        "index slice:",
        "cook slice:",
        "cook slice committed:",
        "want_cooked_reading(",
        "index drained",
        "the pump made no progress",
    ] {
        println!("{:>5}  {needle}", count(needle));
    }
    println!("--- the first twenty lines that repeat");
    for line in entries
        .iter()
        .filter(|line| {
            line.contains("republishing")
                || line.contains("want_the_closure_cooked(")
                || line.contains("index slice:")
                || line.contains("cook slice")
        })
        .take(20)
    {
        println!("{line}");
    }
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
}

impl Server {
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

fn uri_of(path: &std::path::Path) -> String {
    let mut text = path.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        text.insert(0, '/');
    }
    format!("file://{text}")
}
/// Every line of every log file in `dir`, in file order.
fn log_lines(dir: &std::path::Path) -> Vec<String> {
    let mut lines = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return lines;
    };
    for entry in entries.flatten() {
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        lines.extend(content.lines().map(str::to_string));
    }
    lines
}