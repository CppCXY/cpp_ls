//! **How long a hover takes on a real project, position by position** — the probe behind "hover sometimes spins for a
//! long time".
//!
//! ```text
//! $env:CPPLS_HOVER_ROOT = "E:\EmmyLuaCodeStyle"
//! $env:CPPLS_HOVER_FILE = "CodeFormat\src\CodeFormat.cpp"
//! cargo test --release -p cpp_ls --test hover_latency -- --ignored --nocapture
//! ```
//!
//! # What it prints, and why each number is needed
//!
//! ```text
//!   round 1   471.1 ms  0:26  Inc  (null)     the first hover of a position: the render may not exist yet
//!   round 2     9.3 ms  0:26  Inc  (null)     the same position after it
//! ```
//!
//! and then the server's own `a query took … ms — permit … gate … session … ran …` lines, because that sentence is the
//! difference between *waiting* and *working*:
//!
//! ```text
//!   permit      every core is already running a query — the analysis pool is saturated
//!   gate        an update is in flight: the index pump holds the session for writing
//!   session     a writer holds the session, or one is waiting for it
//!   ran         the query's own work — a render, a definition search, an inference
//! ```
//!
//! A hover that spins with `gate`/`session` high is a scheduling problem (the pump), and one with `ran` high is work
//! the hover asked for (a render of the file, a search of the index). They need different fixes, which is why the
//! probe prints both rather than a total.
//!
//! # Why this is a test and not a script
//!
//! Because it drives the **real binary** (`CARGO_BIN_EXE_cpp_ls`, so `--release` is the product) over the real
//! protocol, and it must be re-runnable on any project: `tests/latency.rs` does the same for completions, and the
//! two share the shape.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// **The project to hover in**, and the file to open.
fn fixture() -> Option<(PathBuf, PathBuf)> {
    let root = std::env::var_os("CPPLS_HOVER_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    let file = std::env::var_os("CPPLS_HOVER_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"CodeFormat\src\CodeFormat.cpp"));
    let path = root.join(&file);
    path.is_file().then_some((root, path))
}

/// **The positions to hover**: every `std::name` (the expensive shape — it asks for a type from the standard library)
/// and every `name(` / `name.` (the ordinary one), in the order the file writes them.
fn positions(text: &str, at_most: usize) -> Vec<(u32, u32, String)> {
    let mut found = Vec::new();
    for (number, line) in text.lines().enumerate() {
        // `std::name` first: the column points at the name, which is what a reader's cursor is on.
        if let Some(at) = line.find("std::") {
            let rest = &line[at + 5..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                found.push((number as u32, (at + 5) as u32, format!("std::{name}")));
            }
            continue;
        }
        if let Some(at) = line.find(['(', '.']) {
            let start = line[..at]
                .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
                .map_or(0, |index| index + 1);
            let name: String = line[start..at].to_string();
            if !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()) {
                found.push((number as u32, start as u32, name));
            }
        }
        if found.len() >= at_most {
            break;
        }
    }
    found
}

#[test]
#[ignore = "drives the real server on a real project; run with --ignored"]
fn a_hover_does_not_spin() {
    let Some((root, file)) = fixture() else {
        eprintln!("skipped: no fixture (set CPPLS_HOVER_ROOT and CPPLS_HOVER_FILE)");
        return;
    };
    let text = std::fs::read_to_string(&file).expect("the fixture reads");
    let at_most: usize = std::env::var("CPPLS_HOVER_COUNT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12);
    let picked = positions(&text, at_most);
    assert!(
        !picked.is_empty(),
        "no position matched in {}",
        file.display()
    );

    let log = std::env::temp_dir().join(format!("cppls-hover-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&log);
    let mut server = Server::start(&root, &log);

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": { "textDocument": { "hover": { "contentFormat": ["markdown", "plaintext"] } } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri_of(&file), "languageId": "cpp", "version": 1, "text": text }
        }),
    );

    let mut numbers: Vec<(String, Duration)> = Vec::new();
    for round in 1..=2u32 {
        for (line, character, what) in &picked {
            let id = i64::from(round) * 1000 + i64::from(*line);
            let started = Instant::now();
            let response = server.request(
                id,
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": uri_of(&file) },
                    "position": { "line": line, "character": character },
                }),
            );
            let took = started.elapsed();
            let summary = response["result"]["contents"]["value"]
                .as_str()
                .map(|value| value.lines().next().unwrap_or_default().to_string())
                .unwrap_or_else(|| "(null)".to_string());
            eprintln!(
                "  round {round}  {:>7.1} ms  {line}:{character:<4} {what:<22} {summary}",
                took.as_secs_f64() * 1000.0
            );
            numbers.push((format!("round {round} {line}:{character} {what}"), took));
        }
    }

    server.request(9999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    drop(server);

    let mut sorted = numbers.clone();
    sorted.sort_by_key(|(_, took)| std::cmp::Reverse(*took));
    eprintln!("\n  the slowest ten:");
    for (what, took) in sorted.iter().take(10) {
        eprintln!("    {:>8.1} ms  {what}", took.as_secs_f64() * 1000.0);
    }
    let total: Duration = numbers.iter().map(|(_, took)| *took).sum();
    eprintln!(
        "\n  {} hover(s), {:.1} ms in total, {:.1} ms average",
        numbers.len(),
        total.as_secs_f64() * 1000.0,
        total.as_secs_f64() * 1000.0 / numbers.len() as f64
    );

    // **The server's own sentence**, which is the point of the probe: the four numbers say which of the four queues a
    // hover waited on.
    eprintln!("\n  the server's slow-query lines:");
    let mut lines = 0;
    if let Ok(entries) = std::fs::read_dir(&log) {
        for entry in entries.flatten() {
            let Ok(body) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for line in body.lines() {
                if line.contains("a query took")
                    || line.contains("held the analysis")
                    || line.contains("render")
                {
                    eprintln!("    {line}");
                    lines += 1;
                    if lines > 60 {
                        return;
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&log);
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
}

impl Server {
    fn start(root: &Path, log: &Path) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg(log)
            .arg("--log-level")
            .arg("debug")
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
                Err(error) => panic!("the server did not answer {method}: {error}"),
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

/// **A hover in each of the project's own sources** — where the slow ones are, and what shape they have.
///
/// The single-file probe answers "how long does a hover take here"; this one answers "which file, and is it the first
/// hover of that file". It opens each source, sends one hover at the first `std::`/call on a line with code, and
/// prints the number beside the line. The server's own `a query took … ran …` lines follow, so a slow file can be
/// explained rather than blamed.
#[test]
#[ignore = "drives the real server on a real project; run with --ignored"]
fn where_a_hover_is_slow() {
    let Some((root, _)) = fixture() else {
        eprintln!("skipped: no fixture (set CPPLS_HOVER_ROOT)");
        return;
    };
    let at_most: usize = std::env::var("CPPLS_HOVER_FILES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20);

    // **The project's own sources**, from the compile database when there is one: those are the files a build
    // actually compiles, and therefore the ones whose includes resolve.
    let mut sources: Vec<PathBuf> = Vec::new();
    let database = root.join("build-cppls").join("compile_commands.json");
    if let Ok(body) = std::fs::read_to_string(&database) {
        for entry in body.split("\"file\"").skip(1) {
            if let Some(start) = entry.find('"')
                && let Some(end) = entry[start + 1..].find('"')
            {
                let named = &entry[start + 1..start + 1 + end];
                let path = root.join(named.replace('/', "\\"));
                if path.is_file() {
                    sources.push(path);
                }
            }
        }
    }
    if sources.is_empty() {
        for entry in walk(&root) {
            if matches!(
                entry.extension().and_then(|ext| ext.to_str()),
                Some("cpp" | "cc" | "cxx")
            ) {
                sources.push(entry);
            }
            if sources.len() >= at_most {
                break;
            }
        }
    }
    sources.sort();
    sources.dedup();
    sources.truncate(at_most);
    if sources.is_empty() {
        eprintln!("skipped: no source file under {}", root.display());
        return;
    }

    let log = std::env::temp_dir().join(format!("cppls-hover-many-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&log);
    let mut server = Server::start(&root, &log);
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": { "textDocument": { "hover": { "contentFormat": ["markdown", "plaintext"] } } },
        }),
    );
    server.notify("initialized", json!({}));

    let mut numbers: Vec<(String, Duration)> = Vec::new();
    for (index, path) in sources.iter().enumerate() {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some((line, character, what)) = positions(&text, 1).into_iter().next() else {
            continue;
        };
        server.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": { "uri": uri_of(path), "languageId": "cpp", "version": 1, "text": text }
            }),
        );
        let started = Instant::now();
        let _ = server.request(
            100 + index as i64,
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri_of(path) },
                "position": { "line": line, "character": character },
            }),
        );
        let took = started.elapsed();
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        eprintln!(
            "  {:>8.1} ms  {:>4}:{:<4} {:<20} {name}",
            took.as_secs_f64() * 1000.0,
            line,
            character,
            what
        );
        numbers.push((name, took));
    }

    server.request(9999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    drop(server);

    numbers.sort_by_key(|(_, took)| std::cmp::Reverse(*took));
    eprintln!("\n  the slowest five:");
    for (name, took) in numbers.iter().take(5) {
        eprintln!("    {:>8.1} ms  {name}", took.as_secs_f64() * 1000.0);
    }
    let total: Duration = numbers.iter().map(|(_, took)| *took).sum();
    eprintln!(
        "\n  {} file(s), {:.1} ms in total, {:.1} ms average",
        numbers.len(),
        total.as_secs_f64() * 1000.0,
        total.as_secs_f64() * 1000.0 / numbers.len().max(1) as f64
    );

    let mut shown = 0;
    if let Ok(entries) = std::fs::read_dir(&log) {
        for entry in entries.flatten() {
            let Ok(body) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for line in body.lines() {
                if line.contains("a query took") || line.contains("held the analysis") {
                    eprintln!("    {line}");
                    shown += 1;
                    if shown > 40 {
                        break;
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&log);
}

/// Every file under `root`, skipping the directories a project's sources are never in.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if !matches!(
                    name.as_str(),
                    ".git" | ".cppls" | "build" | "build-cppls" | "node_modules"
                ) {
                    pending.push(path);
                }
            } else {
                found.push(path);
            }
        }
    }
    found
}
