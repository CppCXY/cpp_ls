//! **The first completion on a real project, and where its time went.**
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_ls --test real_project -- --ignored --nocapture
//! ```
//!
//! # Why this exists, and why it is `#[ignore]`d
//!
//! Every end-to-end latency figure this repository has comes from `latency.rs`, whose fixture is one file and a
//! standard-library closure the index finishes in **1.7 s**. The project the user actually opens takes **70 s** to
//! index, and **no completion has ever been timed on one** — so "why is the first completion slow there" has had no
//! answer but an argument. This is the missing measurement, and it is `#[ignore]`d because it spawns a server over a
//! project of a hundred thousand lines and eats a core for a minute.
//!
//! # What it prints, and why those five numbers
//!
//! The server already separates the two halves of a completion, at `debug`:
//!
//! ```text
//!   completion: prepare … ms, catch up … ms, modules … ms, … before the query    ← the write lock + one parse
//!   completion: view … ms, completions … ms, … item(s)                           ← the query itself
//! ```
//!
//! `prepare` takes the session's **write** lock, and the pump holds that lock in slices this repository has measured
//! at **84–2430 ms** against a 50 ms budget (`analysis_state.rs`) while it indexes. So if `prepare` dominates, the
//! answer is the lock and the fix is in the pump's slice budget; if `completions` dominates, the answer is the query
//! degrading on a cold index. The two have nothing in common but the millisecond, which is why this prints both
//! rather than a total.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The project to open, and the file to open in it. Defaulted to the one this was written for, so that the command
/// above works with no environment at all; `CPPLS_REAL_PROJECT` overrides the directory.
fn project() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    )
}

/// The file whose completion is timed — the one a person would have open, and the one the project's own source
/// listing puts first.
fn entry(project: &std::path::Path) -> std::path::PathBuf {
    std::env::var("CPPLS_REAL_ENTRY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| project.join("CodeFormatServer").join("src").join("main.cpp"))
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    log_dir: std::path::PathBuf,
}

impl Server {
    fn start(project: &std::path::Path) -> Self {
        let log_dir = std::env::temp_dir().join("cppls-real-project");
        let _ = std::fs::remove_dir_all(&log_dir);
        std::fs::create_dir_all(&log_dir).expect("a log directory");

        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg(&log_dir)
            // The five numbers this probe exists for are `debug`, and the default level is `info`.
            .arg("--log-level")
            .arg("debug")
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the server starts");

        // Taken rather than cloned: a child's pipes are not files and have no `try_clone`.
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Server {
            child,
            stdin,
            stdout,
            log_dir,
        }
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_string(&message).expect("the message serializes");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("written");
        self.stdin.flush().expect("flushed");
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let Some(message) = self.read() else {
                panic!("the server closed before answering {method}");
            };
            if message["id"] == json!(id) {
                return message;
            }
        }
    }

    fn read(&mut self) -> Option<Value> {
        let mut length = 0usize;
        loop {
            let mut header = String::new();
            if self.stdout.read_line(&mut header).ok()? == 0 {
                return None;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some(rest) = header.strip_prefix("Content-Length: ") {
                length = rest.trim().parse().ok()?;
            }
        }
        let mut body = vec![0u8; length];
        self.stdout.read_exact(&mut body).ok()?;
        serde_json::from_slice(&body).ok()
    }

    /// Everything the server has written about completions so far.
    fn completion_lines(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.log_dir) else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for line in text.lines() {
                if line.contains("completion") {
                    lines.push(line.to_string());
                }
            }
        }
        lines
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "spawns a server over a real project; run with --ignored"]
fn the_first_completion_on_a_real_project_says_where_its_time_went() {
    let project = project();
    let entry = entry(&project);
    let text = std::fs::read_to_string(&entry).expect("the entry file is readable");
    let uri = format!("file:///{}", entry.display().to_string().replace('\\', "/"));

    // **A cursor right after a `.`,** which is the completion a person actually waits for: `full.` needs the type of
    // `full` resolved, and that type is declared in another file whose summary may not be read yet. A cursor on a
    // `}` (the first version of this probe) asks an easy question — names in scope — and measured 377 ms first and
    // 9 ms after, which is not the number anyone is waiting for.
    //
    // The first `.` that is not inside a comment or a `#include` is used, and the cursor is one past it. A file with
    // no such line falls back to the last non-empty line, which at least keeps the probe honest about which it
    // measured by printing the line it chose.
    let mut line = 0;
    let mut character = 4;
    let mut on_a_member = false;
    for (index, text_line) in text.lines().enumerate() {
        let trimmed = text_line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with('*') {
            continue;
        }
        if let Some(dot) = text_line.find('.') {
            line = index;
            character = dot + 1;
            on_a_member = true;
            break;
        }
    }
    if !on_a_member {
        for (index, text_line) in text.lines().enumerate() {
            if !text_line.trim().is_empty() {
                line = index;
            }
        }
        character = 4;
    }
    println!(
        "cursor: line {line}, character {character}, on_a_member {on_a_member} — {:?}",
        text.lines().nth(line).unwrap_or_default()
    );

    let mut server = Server::start(&project);

    let started = Instant::now();
    let answer = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": format!("file:///{}", project.display().to_string().replace('\\', "/")),
            "capabilities": { "textDocument": { "completion": { "completionItem": { "snippetSupport": true } } } },
        }),
    );
    println!("initialize answered in {:?}", started.elapsed());
    assert!(answer["result"].is_object(), "initialize failed: {answer}");

    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": text } }),
    );

    // **Asked immediately, with no waiting at all** — the question is what a person who opens a project and types
    // gets, not what a patient client gets.
    let started = Instant::now();
    let answer = server.request(
        2,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        }),
    );
    let wall = started.elapsed();
    let items = answer["result"]
        .as_array()
        .map(|items| items.len())
        .or_else(|| answer["result"]["items"].as_array().map(|items| items.len()))
        .unwrap_or(0);
    println!("\nthe first completion: {wall:?}, {items} item(s)");

    // The same question again, once the first one has warmed whatever it warms.
    let started = Instant::now();
    let answer = server.request(
        3,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        }),
    );
    let items = answer["result"]
        .as_array()
        .map(|items| items.len())
        .or_else(|| answer["result"]["items"].as_array().map(|items| items.len()))
        .unwrap_or(0);
    println!("the second completion: {:?}, {items} item(s)", started.elapsed());

    // The pump is still running; let it write what it has before the log is read.
    std::thread::sleep(Duration::from_millis(1500));
    println!("\n--- what the server said about it ---");
    for line in server.completion_lines() {
        println!("{line}");
    }
}
