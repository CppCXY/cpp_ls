//! **What the editor's own `textDocument/hover` answers**, at the positions a report named.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "C:\Users\zc\Desktop\test_cpp"
//! cargo test --release -p cpp_ls --test hover_positions_probe -- --ignored --nocapture
//! ```
//!
//! # Why the protocol and not the session
//!
//! `crates/cpp_ls/tests/usability_probe.rs` calls `hover_text` on a session it built itself, and that is a *different
//! request* from the one the editor makes: the handler prepares the file, waits for the index, maps the position from
//! the file's text into the reading, and only then asks the same question. A report that says "the hover is empty in
//! the editor" and a probe that says "the session answers" are not disagreeing until the protocol is asked.
//!
//! Positions are `(line, character)`, 0-based, as the protocol has them.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// **One request may legitimately arrive before the project has been read** — the index is lazy, so the probe re-asks
/// until it sees a card or runs out of patience, and *reports which attempt answered*: a card that only appears on the
/// third ask is a different defect from one that never appears.
const PATIENCE: Duration = Duration::from_secs(120);

fn project() -> PathBuf {
    std::env::var_os("CPPLS_REAL_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\test_cpp"))
}

fn uri_of(path: &Path) -> String {
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace(' ', "%20");
    format!("file:///{}", text.trim_start_matches('/'))
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
}

impl Client {
    fn start(root: &Path) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg("none")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the server starts");
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, messages) = channel();
        std::thread::spawn(move || drain(stdout, tx));
        Client {
            child,
            stdin,
            messages,
        }
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_string(&message).expect("the message serializes");
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
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::from_secs(1));
            match self.messages.recv_timeout(remaining) {
                Ok(message) => {
                    // A server-to-client *request* (a progress token, say) needs an answer or the handler waits.
                    if message.get("method").is_some() && message.get("id").is_some() {
                        let id = message["id"].clone();
                        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }));
                        continue;
                    }
                    if message["id"] == json!(id) {
                        return message;
                    }
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                    return Value::Null;
                }
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn drain(stdout: impl Read, tx: std::sync::mpsc::Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut length = None;
        loop {
            let mut header = String::new();
            match reader.read_line(&mut header) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some(value) = header.strip_prefix("Content-Length: ") {
                length = value.parse::<usize>().ok();
            }
        }
        let Some(length) = length else { return };
        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&body) else {
            return;
        };
        if tx.send(message).is_err() {
            return;
        }
    }
}

#[test]
#[ignore = "a probe: drives the real server against a project on this machine"]
fn the_reported_hovers_over_the_protocol() {
    let root = project();
    let file = root.join("src/containers.cpp");
    let text = std::fs::read_to_string(&file).expect("the reported file is here");
    let uri = uri_of(&file);

    let mut client = Client::start(&root);
    client.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": {
                "textDocument": { "hover": { "contentFormat": ["markdown"] } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    client.notify("initialized", json!({}));
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "cpp",
                "version": 1,
                "text": text,
            }
        }),
    );

    // The places the report named, 0-based, as the protocol has them.
    let places: [(&str, u32, u32); 6] = [
        ("the `to_string` on line 120", 119, 32),
        ("the `value` argument on line 120", 119, 43),
        ("the `value` parameter on line 115", 114, 28),
        ("the `to_string` in the first branch", 117, 25),
        ("`std::is_same_v` on line 117", 116, 20),
        ("`decay_t` on line 116", 115, 25),
    ];

    for (what, line, character) in places {
        let mut attempt = 0;
        let mut answer = Value::Null;
        while attempt < 1 {
            attempt += 1;
            answer = client.request(
                100 + attempt as i64,
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                }),
            );
            let empty = answer["result"].is_null()
                || answer["result"]["contents"].is_null()
                || answer["result"]["contents"] == json!({ "kind": "markdown", "value": "" });
            if !empty {
                break;
            }
            std::thread::sleep(Duration::from_millis(700.min(PATIENCE.as_millis() as u64)));
        }
        let card = answer["result"]["contents"]["value"]
            .as_str()
            .map(|value| value.replace('\n', "⏎"))
            .unwrap_or_else(|| "NULL".to_string());
        let card = if card.chars().count() > 300 {
            format!("{}…", card.chars().take(300).collect::<String>())
        } else {
            card
        };
        println!(
            "{:>2}:{:<3} {what:<36} after {attempt} ask(s): {card}",
            line + 1,
            character,
        );
    }
}
