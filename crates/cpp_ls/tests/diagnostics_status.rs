//! **Does an edit change the diagnostics?** — the probe for the reported "stale finding" defect.
//!
//! ```text
//! $env:CPPLS_FORMAT_PROJECT = "C:\Users\zc\Desktop\cpp"     # the default when it exists
//! $env:CPPLS_FORMAT_FILE    = "main.cpp"
//! cargo test --release -p cpp_ls --test diagnostics_status -- --ignored --nocapture
//! ```
//!
//! # What it does
//!
//! It opens the file, and then walks the sequence a reader actually types: **clean → broken → clean**, pulling
//! diagnostics at every step and printing what came back. The reported defect was that the third step answered with
//! the findings of the second — text that no longer exists — and that editing again did not necessarily clear them.
//!
//! ```text
//! step                         expected                  what the bug did
//! 1. open (clean)              0 items
//! 2. didChange, `;` removed    1+ items, about the `;`
//! 3. didChange, `;` put back   0 items                   the 6→2 items from step 2, for ever
//! ```
//!
//! # Why a probe and not a test
//!
//! The rule is tested (`document_diagnostic::a_report_is_the_answer_for_the_text_that_is_there_now`) and so is the
//! wire (`handshake::a_finding_about_deleted_text_does_not_survive_a_busy_index`). This is the third leg: the same
//! sequence against a **real project** on this machine, where the index is busy in a way a fixture cannot be. It
//! prints rather than asserts for the reason the other probes do — the numbers are the answer, and a reader compares
//! them with the screenshot that reported it.
//!
//! `CPPLS_FORMAT_PROJECT` and `CPPLS_FORMAT_FILE` are shared with `format_status.rs` on purpose: one env pair says
//! "the project I am looking at", and both probes are about that project.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn project() -> PathBuf {
    std::env::var_os("CPPLS_FORMAT_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\cpp"))
}

fn the_file(project: &Path) -> PathBuf {
    let named = std::env::var("CPPLS_FORMAT_FILE").unwrap_or_else(|_| "main.cpp".to_string());
    project.join(named)
}

fn uri_of(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        text.insert(0, '/');
    }
    format!("file://{text}")
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(project: &Path) -> Server {
        let log_dir = std::env::temp_dir().join("cppls-diagnostics-status");
        let _ = std::fs::remove_dir_all(&log_dir);
        let _ = std::fs::create_dir_all(&log_dir);
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg(&log_dir)
            .arg("--log-level")
            .arg("debug")
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(
                std::fs::File::create(log_dir.join("server-stderr.log")).expect("a stderr file"),
            ))
            .spawn()
            .expect("the server starts");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Server {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_string(&message).expect("serializes");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("written");
        self.stdin.flush().expect("flushed");
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if Instant::now() >= deadline {
                panic!("the server did not answer {method}");
            }
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
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One pull, printed as `count: [code @ line:col] …`.
fn pull(server: &mut Server, id: i64, uri: &str, step: &str) -> usize {
    let started = Instant::now();
    let response = server.request(
        id,
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    let took = started.elapsed().as_millis();
    let items = response["result"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("a full report was expected, got {response}"));
    let shown: Vec<String> = items
        .iter()
        .map(|item| {
            format!(
                "{} @ {}:{}",
                item["code"].as_str().unwrap_or("?"),
                item["range"]["start"]["line"].as_u64().unwrap_or(0) + 1,
                item["range"]["start"]["character"].as_u64().unwrap_or(0) + 1,
            )
        })
        .collect();
    println!(
        "{step:<28} {} item(s) in {took} ms  [{}]",
        items.len(),
        shown.join(", ")
    );
    items.len()
}

#[test]
#[ignore = "spawns a server over a real project; run with --ignored"]
fn does_an_edit_change_the_diagnostics() {
    let project = project();
    if !project.is_dir() {
        eprintln!("skipped: {} is not here", project.display());
        return;
    }
    let file = the_file(&project);
    let Ok(original) = std::fs::read_to_string(&file) else {
        eprintln!("skipped: {} is not readable", file.display());
        return;
    };

    // **The two texts are the file itself**, so nothing about the fixture is invented: the broken one drops the
    // first `;` after `format(` — the exact edit the reported screenshot was made after.
    let broken = match original.find(';') {
        Some(at) => {
            let mut text = original.clone();
            text.remove(at);
            text
        }
        None => {
            eprintln!("skipped: the file has no `;` to remove");
            return;
        }
    };

    let mut server = Server::start(&project);
    let uri = uri_of(&file);
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&project),
            "capabilities": {
                "textDocument": { "diagnostic": { "dynamicRegistration": true, "refreshSupport": true } },
                "workspace": { "diagnostics": { "refreshSupport": true } },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": original } }),
    );

    println!("--- {} ({} bytes)", file.display(), original.len());
    let clean = pull(&mut server, 10, &uri, "1. as opened");

    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": broken }],
        }),
    );
    let broken_count = pull(&mut server, 20, &uri, "2. one `;` removed");

    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 3 },
            "contentChanges": [{ "text": original }],
        }),
    );
    let fixed = pull(&mut server, 30, &uri, "3. the `;` put back");
    let again = pull(&mut server, 31, &uri, "4. and again, no edit");

    println!(
        "verdict: {} -> {} -> {} -> {}",
        clean, broken_count, fixed, again
    );
    if clean == fixed && fixed == again && broken_count > clean {
        println!("CORRECT: the broken edit is reported and the fix clears it");
    } else {
        println!(
            "WRONG: the answer does not follow the text (step 1 {clean}, step 3 {fixed}, step 4 {again})"
        );
    }
}
