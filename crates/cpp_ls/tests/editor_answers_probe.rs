//! **What the editor is actually told about one file** — the findings, the inlay hints, and the semantic tokens.
//!
//! ```text
//! $env:CPPLS_ANSWERS_PROJECT = "C:\Users\zc\Desktop\test_cpp"
//! $env:CPPLS_ANSWERS_FILE    = "src/strings.cpp"
//! $env:CPPLS_ANSWERS_NEEDLES = "Pair,x,foo"
//! cargo test --release -p cpp_ls --test editor_answers_probe -- --ignored --nocapture
//! ```
//!
//! # Why a probe
//!
//! Each of these three answers is assembled from the index, the cooked reading and the open buffer, and the way they
//! go wrong in a screenshot is a way they do not go wrong in a unit test: a finding attributed to the wrong line, a
//! hint that is missing for one callee shape, a token that a client renders as plain text. This drives the real
//! server over a real project and prints what came back, so the answer can be compared with the screenshot byte for
//! byte.
//!
//! It prints rather than asserts on purpose — the numbers are the answer, and a probe that fails is a probe nobody
//! runs on the next report.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn project() -> PathBuf {
    std::env::var_os("CPPLS_ANSWERS_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\test_cpp"))
}

fn the_file(project: &Path) -> PathBuf {
    let named =
        std::env::var("CPPLS_ANSWERS_FILE").unwrap_or_else(|_| "src/strings.cpp".to_string());
    project.join(named)
}

fn needles() -> Vec<String> {
    std::env::var("CPPLS_ANSWERS_NEEDLES")
        .unwrap_or_else(|_| "Pair,x,foo".to_string())
        .split(',')
        .map(|needle| needle.trim().to_string())
        .filter(|needle| !needle.is_empty())
        .collect()
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
        let log_dir =
            std::env::temp_dir().join(format!("cppls-editor-answers-{}", std::process::id()));
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
                let status = self.child.try_wait().ok().flatten();
                panic!("the server closed before answering {method}: status {status:?}");
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

/// The byte offset of `needle`'s first occurrence, for a hint that has to be looked up by position.
fn offset_of(text: &str, needle: &str) -> Option<usize> {
    text.find(needle)
}

/// `(line, character)` for a byte offset — the coordinates the protocol speaks in.
fn line_col(text: &str, offset: usize) -> (u64, u64) {
    let mut line = 0u64;
    let mut column = 0u64;
    for byte in text.bytes().take(offset) {
        if byte == b'\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    (line, column)
}

#[test]
#[ignore = "spawns a server over a real project; run with --ignored"]
fn what_the_editor_is_told_about_a_file() {
    let root = project();
    if !root.is_dir() {
        eprintln!("skipped: {} is not here", root.display());
        return;
    }
    let file = the_file(&root);
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("skipped: {} is not readable", file.display());
        return;
    };
    let uri = uri_of(&file);

    let mut server = Server::start(&root);
    let initialized = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": {
                "textDocument": {
                    "diagnostic": { "dynamicRegistration": false },
                    "inlayHint": { "dynamicRegistration": true },
                    // **The token capability is two lists and a request flag**, and a client that sends a partial one
                    // is rejected at `initialize`: `lsp_types` requires `tokenTypes`, and the server answers
                    // `-32602` with "missing field `tokenTypes`". A probe is a client, so it sends the spec's list.
                    "semanticTokens": {
                        "dynamicRegistration": false,
                        "requests": { "full": true },
                        "tokenTypes": [
                            "namespace", "type", "class", "enum", "interface", "struct", "typeParameter",
                            "parameter", "variable", "property", "enumMember", "event", "function", "method",
                            "macro", "keyword", "modifier", "comment", "string", "number", "regexp", "operator",
                            "decorator"
                        ],
                        "tokenModifiers": [
                            "declaration", "definition", "readonly", "static", "deprecated", "abstract", "async",
                            "modification", "documentation", "defaultLibrary"
                        ],
                        "formats": ["relative"],
                    },
                },
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    println!("initialize: {initialized}");
    let legend: Vec<String> =
        initialized["result"]["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"]
            .as_array()
            .map(|types| {
                types
                    .iter()
                    .filter_map(|kind| kind.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": text } }),
    );

    // **Three pulls, a second apart**: the first one usually arrives while the index is still reading, and the
    // reported defect was about what the *settled* answer is.
    //
    // **`CPPLS_ANSWERS_SETTLE_MS` slows the loop down** for a project whose index takes longer than a second to
    // read: a server withholds every *check* finding until the file's closure is indexed (`is_closure_ready`), so a
    // 2 000-file project answers `0 item(s)` on three pulls inside five seconds — measured on a production project,
    // where the same probe on a smaller one had reported findings. The default keeps the old timing.
    let settle = std::env::var("CPPLS_ANSWERS_SETTLE_MS")
        .ok()
        .and_then(|written| written.parse::<u64>().ok())
        .unwrap_or(1500);
    let mut items = Vec::new();
    for pull in 0..3 {
        let response = server.request(
            10 + pull,
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri } }),
        );
        items = response["result"]["items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        println!("pull {pull}: {} item(s)", items.len());
        std::thread::sleep(Duration::from_millis(settle));
    }

    println!("\n# findings ({} item(s))", items.len());
    for item in &items {
        let start = &item["range"]["start"];
        let end = &item["range"]["end"];
        println!(
            "  {}:{}  - {}:{}  [{}] {}",
            start["line"].as_u64().unwrap_or(0) + 1,
            start["character"].as_u64().unwrap_or(0) + 1,
            end["line"].as_u64().unwrap_or(0) + 1,
            end["character"].as_u64().unwrap_or(0) + 1,
            item["code"]
                .as_str()
                .unwrap_or(item["source"].as_str().unwrap_or("?")),
            item["message"].as_str().unwrap_or(""),
        );
    }

    // **One hover, when a report names a spelling to ask about** — the card is assembled by the handler, so nothing
    // below the LSP can show it.
    if let Ok(needle) = std::env::var("CPPLS_ANSWERS_HOVER") {
        // **A needle ending in `$` is the *last* occurrence** — a report's card is often about a use, and the
        // declaration of the same spelling comes first in the file.
        let (needle, last) = match needle.strip_suffix('$') {
            Some(without) => (without.to_string(), true),
            None => (needle, false),
        };
        let found = if last {
            text.rfind(&needle)
        } else {
            text.find(&needle)
        };
        if let Some(at) = found {
            let (line, character) = line_col(&text, at);
            let hover = server.request(
                35,
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                }),
            );
            println!("\n# hover on `{needle}` at {}:{}", line + 1, character + 1);
            println!(
                "{}",
                hover["result"]["contents"]["value"]
                    .as_str()
                    .unwrap_or("<no popup>")
            );
        }
    }

    let hints = server.request(
        40,
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": uri },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 10_000, "character": 0 } },
        }),
    );
    let hints = hints["result"].as_array().cloned().unwrap_or_default();
    println!("\n# inlay hints ({} item(s))", hints.len());
    for hint in &hints {
        let line = hint["position"]["line"].as_u64().unwrap_or(0) + 1;
        let character = hint["position"]["character"].as_u64().unwrap_or(0) + 1;
        let label = match &hint["label"] {
            Value::String(text) => text.clone(),
            other => other
                .as_array()
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| part["value"].as_str())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default(),
        };
        println!("  {line}:{character}  {label}");
    }

    let tokens = server.request(
        50,
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    let data: Vec<u64> = tokens["result"]["data"]
        .as_array()
        .map(|values| values.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    let wanted = needles();
    println!("\n# semantic tokens ({} item(s))", data.len() / 5);
    let mut line = 0u64;
    let mut character = 0u64;
    for chunk in data.chunks(5) {
        let (delta_line, delta_start, length, kind, modifiers) =
            (chunk[0], chunk[1], chunk[2], chunk[3], chunk[4]);
        line += delta_line;
        character = if delta_line == 0 {
            character + delta_start
        } else {
            delta_start
        };
        let kind = legend
            .get(kind as usize)
            .cloned()
            .unwrap_or_else(|| format!("kind {kind}"));
        // The token's text, so a reader can see which one is meant.
        //
        // **At a character boundary, or not at all.** The protocol counts this column in *bytes*, and a file with a
        // comment in Chinese has tokens whose span falls inside a multi-byte character — slicing at the byte offset
        // panicked ("start byte index 512 is not a char boundary; it is inside '的'"), which took the whole probe
        // down after the findings had already been printed. A token that starts mid-character is one this reader
        // cannot quote, and quoting nothing is the answer.
        let at = text
            .lines()
            .take(line as usize)
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + character as usize;
        let spelling: String = text
            .get(at.min(text.len())..)
            .unwrap_or_default()
            .chars()
            .take(length as usize)
            .collect();
        if wanted.is_empty() || wanted.contains(&spelling) {
            println!(
                "  {}:{}  `{spelling}`  {kind} (modifiers {modifiers})",
                line + 1,
                character + 1
            );
        }
    }
    if let Some(at) = offset_of(&text, "Pair<int, int>{1, 2}") {
        let (needle_line, needle_column) = line_col(&text, at);
        println!(
            "\n# the token at the reported position (Pair, {}:{})",
            needle_line + 1,
            needle_column + 1
        );
        let mut line = 0u64;
        let mut character = 0u64;
        for chunk in data.chunks(5) {
            let (delta_line, delta_start, length, kind, modifiers) =
                (chunk[0], chunk[1], chunk[2], chunk[3], chunk[4]);
            line += delta_line;
            character = if delta_line == 0 {
                character + delta_start
            } else {
                delta_start
            };
            if line == needle_line && character == needle_column {
                println!(
                    "  found: length {length}, kind {} (modifiers {modifiers})",
                    legend
                        .get(kind as usize)
                        .cloned()
                        .unwrap_or_else(|| format!("kind {kind}"))
                );
            }
        }
    }

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}
