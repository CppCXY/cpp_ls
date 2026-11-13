//! **A module project, asked the two questions an editor asked it** — where `import :shapes;` goes, and what a
//! hover on an overloaded call says.
//!
//! ```text
//! cargo test --release -p cpp_ls --test module_definition_probe -- --ignored --nocapture
//! ```
//!
//! `CPPLS_MODULE_PROJECT` names the project (default `C:\Users\zc\Desktop\test_module`).
//!
//! The report: a jump on `:shapes` in `src/geometry.cppm` (`export import :shapes;`) landed in `src/shapes.cppm` —
//! a *different* module that happens to share the partition's spelling. The partition is `geometry:shapes`, the
//! unit is `src/geometry_shapes.cppm`, and `Session::module_unit_at` is what names it.
//!
//! The hover half is the same file's `std::println("…", …)` call: four declarations of a variadic template were
//! shown with "which one a call means is decided by the arguments", which is the question the hoover could answer.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn project() -> PathBuf {
    std::env::var_os("CPPLS_MODULE_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\test_module"))
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
            // Inherited so the server's own traces (`CPPLS_TRACE_OVERLOAD`) reach this output.
            .stderr(Stdio::inherit())
            .spawn()
            .expect("the server starts");
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, messages) = channel();
        std::thread::spawn(move || read_messages(stdout, tx));
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
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::from_secs(1));
            match self.messages.recv_timeout(remaining) {
                Ok(message) => {
                    if message.get("method").is_some() && message.get("id").is_some() {
                        let id = message["id"].clone();
                        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }));
                        continue;
                    }
                    if message["id"] == json!(id) {
                        return message;
                    }
                }
                Err(_) => return Value::Null,
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

fn read_messages(stdout: impl Read, tx: std::sync::mpsc::Sender<Value>) {
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

fn open(client: &mut Client, root: &Path, relative: &str) -> String {
    let file = root.join(relative);
    let text = std::fs::read_to_string(&file).expect("the file is here");
    let uri = uri_of(&file);
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
    uri
}

#[test]
#[ignore = "a probe: needs the test_module checkout"]
fn a_partition_import_jumps_to_its_own_unit() {
    let root = project();
    let mut client = Client::start(&root);
    client.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": {
                "textDocument": { "hover": { "contentFormat": ["markdown"] }, "definition": {} },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    client.notify("initialized", json!({}));
    let uri = open(&mut client, &root, "src/geometry.cppm");

    let answer = client.request(
        2,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 12, "character": 14 },
        }),
    );
    eprintln!("[module] definition -> {answer}");
    let landed = answer["result"]
        .as_array()
        .and_then(|list| list.first())
        .or_else(|| answer["result"].as_object().map(|_| &answer["result"]))
        .and_then(|location| location["uri"].as_str())
        .unwrap_or_default()
        .to_string();
    eprintln!("[module] landed in {landed}");

    let answer = client.request(
        3,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 12, "character": 14 },
        }),
    );
    eprintln!("[module] hover on the partition -> {answer}");
}

#[test]
#[ignore = "a probe: needs the test_module checkout"]
fn a_hover_on_an_overloaded_call_names_the_overload() {
    let root = project();
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
    let relative =
        std::env::var("CPPLS_MODULE_HOVER_FILE").unwrap_or_else(|_| "src/main.cpp".to_string());
    let uri = open(&mut client, &root, &relative);
    let file = root.join(&relative);
    let text = std::fs::read_to_string(&file).expect("the file is here");
    // **The reported line**, when the file has it: `std::println("矩形 3x4 …`, which is the hover in the
    // screenshot. Falling back to the first call keeps the probe useful on a project without it.
    let needle =
        std::env::var("CPPLS_MODULE_HOVER_TEXT").unwrap_or_else(|_| "矩形 3x4".to_string());
    let at = text.find(&needle).and_then(|line| {
        let start = text[..line].rfind('\n').map_or(0, |newline| newline + 1);
        text[start..line]
            .find("println")
            .map(|offset| start + offset)
    });
    let Some(at) = at.or_else(|| text.find("std::println")) else {
        eprintln!("[module] no `std::println` in {relative}");
        return;
    };
    let line = text[..at].matches('\n').count();
    let column = at - text[..at].rfind('\n').map_or(0, |newline| newline + 1);
    eprintln!("[module] hovering {relative}:{}:{}", line + 1, column + 1);
    let answer = client.request(
        2,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": column + 5 },
        }),
    );
    eprintln!("[module] hover -> {answer}");
}
