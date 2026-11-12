//! **The reported line, asked of the real server process over stdio** — not of the analysis crate's own harness.
//!
//! ```text
//! cargo test --release -p cpp_ls --test real_project_hover_probe -- --ignored --nocapture
//! ```
//!
//! Why this exists beside `cpp_code_analysis/tests/real_project_probe.rs`: that one calls `Session` directly, and a
//! probe that builds its own session can be wrong in exactly the way the editor is not — a different file set, a
//! different request order, a cursor offset it computed itself. This one speaks the protocol: the same binary the
//! editor spawns, `initialize`/`initialized`, `textDocument/didOpen` with the file's real text, and then
//! `textDocument/hover` at a position computed from that text. If the two disagree, the harness is what is wrong.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// One request may legitimately arrive before the project has been read — the index is lazy — so the probe re-asks
/// until it sees an answer worth printing or runs out of patience.
const PATIENCE: Duration = Duration::from_secs(300);

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\github\EmmyLuaCodeStyle"));
    named.join("CodeFormatLib").is_dir().then_some(named)
}

fn uri_of(path: &Path) -> String {
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace(' ', "%20");
    format!("file:///{}", text.trim_start_matches('/'))
}

/// Where the byte offset falls, as the protocol's line and character.
fn position_of(source: &str, offset: usize) -> (u32, u32) {
    let before = &source[..offset];
    let line = before.matches('\n').count();
    let character = before
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .count();
    (line as u32, character as u32)
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
        self.wait_for(|message| message["id"] == json!(id))
    }

    fn wait_for(&mut self, accept: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("the server did not answer within {PATIENCE:?}"));
            let message = match self.messages.recv_timeout(remaining) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => {
                    panic!("the server did not answer within {PATIENCE:?}")
                }
                Err(RecvTimeoutError::Disconnected) => panic!("the server closed its stdout"),
            };
            // A request from the server is part of being a client.
            if message.get("method").is_some() && message.get("id").is_some() {
                let id = message["id"].clone();
                self.send(json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }));
                continue;
            }
            if accept(&message) {
                return message;
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

/// Ask `method` at one position until the answer stops changing, and print every distinct answer with its time.
fn ask_until_stable(
    client: &mut Client,
    first_id: i64,
    method: &str,
    uri: &str,
    at: (u32, u32),
    label: &str,
) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    let mut id = first_id;
    loop {
        let answer = client.request(
            id,
            method,
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": at.0, "character": at.1 },
            }),
        );
        id += 1;
        let printed = serde_json::to_string(&answer["result"]).unwrap_or_default();
        let changed = printed != serde_json::to_string(&last["result"]).unwrap_or_default();
        if changed {
            eprintln!(
                "[hover-probe] {label} at {}:{} after {:?}: {printed}",
                at.0,
                at.1,
                started.elapsed()
            );
        }
        // **Two identical non-`null` answers** is what "settled" means here: the first says the server has an
        // answer, the second says it is not about to change under a still-running index pass.
        if !changed && !answer["result"].is_null() {
            return answer;
        }
        last = answer.clone();
        if started.elapsed() >= PATIENCE {
            return last;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[test]
#[ignore = "a probe: needs the external project and the real standard library"]
fn what_the_real_server_answers_about_the_reported_line() {
    let Some(root) = project() else {
        eprintln!("[hover-probe] the project is not here");
        return;
    };
    let file = root.join("CodeFormatLib/src/LuaCodeFormat.cpp");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("[hover-probe] no LuaCodeFormat.cpp");
        return;
    };

    let mut client = Client::start(&root);
    let initialized = client.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": {
                "textDocument": { "diagnostic": { "dynamicRegistration": false } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    eprintln!(
        "[hover-probe] initialize: {}",
        if initialized.get("result").is_some() {
            "answered"
        } else {
            "no result"
        }
    );
    client.notify("initialized", json!({}));
    let uri = uri_of(&file);
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": text }
        }),
    );

    // --- `result` in `auto &result = results.emplace_back();`, line 304 -----------------------------
    let Some(at) = text.find("auto &result = results.emplace_back") else {
        panic!("the reported line is not in the file");
    };
    let position = position_of(&text, at + "auto &".len());
    ask_until_stable(
        &mut client,
        100,
        "textDocument/hover",
        &uri,
        position,
        "`result`",
    );

    // --- `results` at its use, the expression the type comes from ----------------------------------
    let Some(use_at) = text.find("results.emplace_back") else {
        panic!("the use is not in the file");
    };
    ask_until_stable(
        &mut client,
        2000,
        "textDocument/hover",
        &uri,
        position_of(&text, use_at),
        "`results` (use)",
    );

    // --- `diagnostics`, the parameter whose element type is `LuaDiagnostic` ------------------------
    if let Some(parameter_at) = text.find("for (auto &diagnostic: diagnostics)") {
        let inside = parameter_at + "for (auto &diagnostic: ".len();
        ask_until_stable(
            &mut client,
            3000,
            "textDocument/hover",
            &uri,
            position_of(&text, inside),
            "`diagnostics` (use)",
        );
    }

    // --- The **hover** on the same name, in the same session: the two handlers ask different views ---------
    //
    // `inlay_hint/mod.rs:105` asks `view_of_the_file` (the file's own tokens, because a hint's position must be in
    // the file's coordinates) and the hover asks the cooked one. If they disagree about one name, that is the defect.
    ask_until_stable(
        &mut client,
        5000,
        "textDocument/hover",
        &uri,
        (130, 12),
        "`formattedText`",
    );

    // --- `textDocument/inlayHint` over the reported line, as the editor asks for it ------------------
    //
    // The deduced type of `auto formattedText = f.GetFormatResult(t);` is what is drawn after the name, and the
    // screenshot has it as `basic_string<char, char_traits<char>, allocater<char>>` — the alias expanded — where the
    // header writes `std::string`. This is the request that produces it.
    let started = Instant::now();
    let mut id = 4000;
    let mut last = String::new();
    // **Long enough for the index to be read**, because the question is not "what does the first answer say" but
    // "does the answer ever become the alias": a hint drawn from a half-read index and never refreshed is exactly
    // the screenshot.
    let deadline = started + Duration::from_secs(90);
    while Instant::now() < deadline {
        let answer = client.request(
            id,
            "textDocument/inlayHint",
            json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": 125, "character": 0 },
                    "end": { "line": 136, "character": 0 },
                },
            }),
        );
        id += 1;
        let hints = answer["result"].as_array().cloned().unwrap_or_default();
        let described: Vec<String> = hints
            .iter()
            .map(|hint| {
                format!(
                    "{}:{} {:?}",
                    hint["position"]["line"],
                    hint["position"]["character"],
                    hint["label"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| hint["label"].to_string())
                )
            })
            .collect();
        let printed = described.join(" | ");
        if printed != last {
            eprintln!(
                "[hover-probe] inlay hints after {:?}: {printed}",
                started.elapsed()
            );
            last = printed;
        }
        std::thread::sleep(Duration::from_secs(2));
    }

    // --- `auto pos = _events.size();` in `LuaParser.cpp:51`, where the inlay drew `_Alloc_traits::size_type` ------
    let parser = root.join("LuaParser/src/Parse/LuaParser.cpp");
    if let Ok(parser_text) = std::fs::read_to_string(&parser) {
        client.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri_of(&parser),
                    "languageId": "cpp",
                    "version": 1,
                    "text": parser_text,
                }
            }),
        );
        let parser_uri = uri_of(&parser);
        // `auto pos` — `pos` begins at column 9 of the line, which is line 50 when counted from zero.
        ask_until_stable(
            &mut client,
            6000,
            "textDocument/hover",
            &parser_uri,
            (50, 10),
            "`pos`",
        );
    }

    client.request(9000, "shutdown", Value::Null);
    client.notify("exit", Value::Null);
}
