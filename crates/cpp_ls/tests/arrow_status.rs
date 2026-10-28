//! **What a completion after `->` offers on a real file** — the probe for the reported pointer defect.
//!
//! ```text
//! $env:CPPLS_FORMAT_PROJECT = "C:\Users\zc\Desktop\cpp"   # the default when it exists
//! $env:CPPLS_FORMAT_FILE    = "main.cpp"
//! $env:CPPLS_ARROW          = "0"                          # which `->` in the file (0 is the first)
//! cargo test --release -p cpp_ls --test arrow_status -- --ignored --nocapture
//! ```
//!
//! Reported from an editor with a screenshot: `std::string* message = new std::string(…)`, and `message->` offered
//! one item — `operator basic_string_view<_Elem, _Traits>` — instead of `std::string`'s members. Two defects were
//! behind it, and this probe shows both:
//!
//! * the **first** answer in a cold session comes from summaries that have not been cooked, so the class's members
//!   are missing (the answer is now marked `isIncomplete`, so a client asks again);
//! * a **smart pointer** answers with its own members or nothing, because `<memory>`'s `unique_ptr::operator->`
//!   returns a dependent `typename …::type` and `element_type` was not asked for.
//!
//! It prints, per round: the item count, the protocol's `isIncomplete`, the first forty labels and details, and a
//! **hover on the object** — which is the one answer that says whether the type was read at all when the list is
//! empty.

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

/// A byte offset in the text, as a protocol position — line, and **UTF-16** column, which is what a client sends.
fn position_of(text: &str, at: usize) -> (u64, u64) {
    let mut line_starts = vec![0usize];
    for (offset, ch) in text.char_indices() {
        if ch == '\n' {
            line_starts.push(offset + 1);
        }
    }
    let line = line_starts.partition_point(|start| *start <= at) - 1;
    let character: usize = text[line_starts[line]..at]
        .chars()
        .map(char::len_utf16)
        .sum();
    (line as u64, character as u64)
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(project: &Path) -> Server {
        let log_dir = std::env::temp_dir().join("cppls-arrow-status");
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

#[test]
#[ignore = "spawns a server over a real project; run with --ignored"]
fn what_a_completion_after_an_arrow_offers() {
    let project = project();
    let file = the_file(&project);
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("skipped: {} is not readable", file.display());
        return;
    };
    let which: usize = std::env::var("CPPLS_ARROW")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let Some(arrow) = text.match_indices("->").nth(which).map(|(at, _)| at + 2) else {
        eprintln!("skipped: the file has no `->` (or not {which} of them)");
        return;
    };
    let (line, character) = position_of(&text, arrow);
    let line_text = text.lines().nth(line as usize).unwrap_or_default();

    // The object's own name, which is the identifier immediately before the operator.
    let before = text[..arrow - 2].trim_end();
    let ident = before
        .rfind(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .map_or(0, |at| at + 1);
    let (ident_line, ident_character) = position_of(&text, ident);
    let object = text[ident..arrow - 2].trim().to_string();

    let mut server = Server::start(&project);
    let uri = uri_of(&file);
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&project),
            "capabilities": {
                "textDocument": { "completion": { "completionItem": { "snippetSupport": true } } },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": text } }),
    );

    // **The first request pays the index**, so it is asked several times and every answer printed: a list that is
    // empty on the first keystroke only is a different defect from one that is always empty.
    for round in 0..6 {
        let started = Instant::now();
        let answer = server.request(
            100 + round,
            "textDocument/completion",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        );
        let took = started.elapsed().as_millis();
        let items = answer["result"]["items"]
            .as_array()
            .or_else(|| answer["result"].as_array())
            .cloned()
            .unwrap_or_default();
        println!(
            "--- round {round}: after `->` at line {} ({} ms), {} item(s), isIncomplete {}\n    the line is {:?}",
            line + 1,
            took,
            items.len(),
            answer["result"]["isIncomplete"].as_bool().unwrap_or(false),
            line_text
        );
        if round == 0 || round == 5 {
            let hover = server.request(
                900 + round,
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": ident_line, "character": ident_character },
                }),
            );
            println!(
                "    hover on `{object}`: {}",
                hover["result"]["contents"]["value"]
                    .as_str()
                    .map(|value| value.lines().next().unwrap_or_default())
                    .unwrap_or("<none>")
            );
        }
        for item in items.iter().take(40) {
            println!(
                "  {:<44} {}",
                item["label"].as_str().unwrap_or(""),
                item["detail"].as_str().unwrap_or("")
            );
        }
        if round == 0 {
            std::thread::sleep(Duration::from_millis(750));
        }
    }
}
