//! **A long session against one project, so a leak has somewhere to show** — and the numbers to say whether it did.
//!
//! ```text
//! cargo test --release -p cpp_ls --test memory_probe -- --ignored --nocapture
//! ```
//!
//! `CPPLS_MEMORY_PROJECT` names the project (default `C:\Users\zc\Desktop\test_cpp`);
//! `CPPLS_MEMORY_FILE` names the file to open (default `src/basics.cpp`); `CPPLS_MEMORY_TICKS` how many rounds.
//!
//! Why a probe and not a test: a leak is a property of *state*, and the state is what a session accumulates while an
//! editor sits on a file — hovers, pulls, hints, syntactic refreshes. A unit test cannot hold a session that long,
//! and a test that measured the allocator would be measuring the allocator. So this drives the real server over
//! stdio, and the wrapper script samples the process's working set while it runs. The probe's own job is to keep
//! asking the questions an editor asks.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn project() -> PathBuf {
    std::env::var_os("CPPLS_MEMORY_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\test_cpp"))
}

/// **Every source file the project holds**, for the "open them all" mode — an editor with a project open has them
/// visited, and that is the state the report is about.
fn source_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.join("src"), root.join("include")];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let extension = path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase();
            if matches!(extension.as_str(), "cpp" | "cc" | "h" | "hpp" | "c") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
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
            // `none` unless a run is being read for its log lines (`CPPLS_MEMORY_LOG`).
            .arg(std::env::var("CPPLS_MEMORY_LOG").unwrap_or_else(|_| "none".to_string()))
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Inherited, so the probe's trace switches (`CPPLS_TRACE_UNITS`) reach the test's own output.
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
        // **Answers are drained, not required.** The probe's subject is the server's memory, and a request whose
        // answer arrives late still made the server do the work.
        let deadline = Instant::now() + Duration::from_secs(120);
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

/// How many messages the server has sent — a flood is a memory report of its own.
static MESSAGES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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
        MESSAGES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if tx.send(message).is_err() {
            return;
        }
    }
}

#[test]
#[ignore = "a probe: drives the real server against a project on this machine"]
fn a_long_session_does_not_grow_without_bound() {
    let root = project();
    let relative = std::env::var_os("CPPLS_MEMORY_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("src/basics.cpp"));
    let file = root.join(&relative);
    if std::fs::read_to_string(&file).is_err() {
        eprintln!("[memory] the file is not here: {}", file.display());
        return;
    }
    let ticks: usize = std::env::var("CPPLS_MEMORY_TICKS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(120);
    eprintln!("[memory] {} tick(s) on {}", ticks, file.display());

    // **The state the report is about**: a project opened, every file visited, and then one file sat on. Opening
    // them one by one is what a reader does, and each open is a closure the session must read and keep.
    let open_all = std::env::var_os("CPPLS_MEMORY_OPEN_ALL").is_some();
    let mut opened: Vec<PathBuf> = if open_all {
        source_files(&root)
    } else {
        vec![file.clone()]
    };
    if let Some(limit) = std::env::var("CPPLS_MEMORY_FILES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        opened.truncate(limit);
    }
    eprintln!("[memory] opening {} file(s)", opened.len());

    let mut client = Client::start(&root);
    client.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": {
                "textDocument": {
                    "hover": { "contentFormat": ["markdown"] },
                    "diagnostic": { "dynamicRegistration": false },
                    "inlayHint": { "dynamicRegistration": false },
                    "syntaxToken": {},
                    "codeLens": {},
                    "documentSymbol": {},
                    "foldingRange": {},
                    "selectionRange": {},
                    "signatureHelp": {},
                    "completion": {},
                },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    client.notify("initialized", json!({}));
    for (index, path) in opened.iter().enumerate() {
        let Ok(opened_text) = std::fs::read_to_string(path) else {
            continue;
        };
        client.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri_of(path),
                    "languageId": "cpp",
                    "version": 1,
                    "text": opened_text,
                }
            }),
        );
        if index % 4 == 0 {
            eprintln!("[memory] opened {index} of {}", opened.len());
        }
        // **A request per open**, because a file is read and cooked *when something asks*: a `didOpen` alone only
        // puts the text in the session. `CPPLS_MEMORY_NO_PULL` leaves it out, which is how a run can tell what the
        // pull itself starts.
        if std::env::var_os("CPPLS_MEMORY_NO_PULL").is_some() {
            continue;
        }
        client.request(
            1000 + index as i64,
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri_of(path) } }),
        );
    }

    // **What a client asks when it opens a file** — `CPPLS_MEMORY_EDITOR=1`. A `didOpen` alone is not what an editor
    // does: a language client asks for the outline, the lenses, the folds, the tokens, the hints and the pull, and
    // each of those is a different query over the same closure. The `containers.cpp` report is about *opening* the
    // file, so the probe has to make the same requests the client does, or it measures a smaller program.
    if std::env::var_os("CPPLS_MEMORY_EDITOR").is_some() {
        let Some(target) = opened.first() else { return };
        let target_uri = uri_of(target);
        let position = json!({ "line": 20, "character": 8 });
        let range = json!({
            "start": { "line": 0, "character": 0 },
            "end": { "line": 200, "character": 0 },
        });
        let requests = [
            (
                "textDocument/documentSymbol",
                json!({ "textDocument": { "uri": target_uri } }),
            ),
            (
                "textDocument/codeLens",
                json!({ "textDocument": { "uri": target_uri } }),
            ),
            (
                "textDocument/foldingRange",
                json!({ "textDocument": { "uri": target_uri } }),
            ),
            (
                "textDocument/selectionRange",
                json!({ "textDocument": { "uri": target_uri }, "positions": [position.clone()] }),
            ),
            (
                "textDocument/signatureHelp",
                json!({ "textDocument": { "uri": target_uri }, "position": position.clone() }),
            ),
            (
                "textDocument/completion",
                json!({ "textDocument": { "uri": target_uri }, "position": position.clone() }),
            ),
            (
                "textDocument/semanticTokens/full",
                json!({ "textDocument": { "uri": target_uri } }),
            ),
            (
                "textDocument/inlayHint",
                json!({ "textDocument": { "uri": target_uri }, "range": range.clone() }),
            ),
        ];
        for (offset, (method, params)) in requests.into_iter().enumerate() {
            let started = Instant::now();
            client.request(500 + offset as i64, method, params);
            eprintln!(
                "[memory] editor: {method} took {} ms",
                started.elapsed().as_millis()
            );
        }
    }

    // **The questions an editor asks while a reader sits on one file.** Every iteration repeats them: a query that
    // allocates per answer without freeing shows up as a slope, and one that is merely slow does not.
    // **Which questions to ask**, so a growth can be attributed to one of them instead of guessed at:
    // `CPPLS_MEMORY_ONLY=hover|diagnostic|tokens|hints`.
    let only = std::env::var("CPPLS_MEMORY_ONLY").unwrap_or_default();
    let asks = |what: &str| only.is_empty() || only == what;
    // **No request at all, for a while** — the attribution: a growth with the editor still and no question asked is
    // the pump's (indexing, cooking, refining), and one that needs a request is the request's.
    if let Some(milliseconds) = std::env::var("CPPLS_MEMORY_IDLE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        eprintln!("[memory] idle for {milliseconds} ms, no requests");
        std::thread::sleep(Duration::from_millis(milliseconds));
        // **One pull after the idle half**, so the session's own report (`CPPLS_TRACE_MEMORY`) is printed at the
        // moment the process's private bytes are high: the tables it names against what the operating system sees.
        let Some(first) = opened.first() else { return };
        let answer = client.request(
            99,
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri_of(first) } }),
        );
        eprintln!("[memory] idle pull answered {}", answer != Value::Null);
    }
    let mut id = 100;
    for tick in 0..ticks {
        let target = &opened[tick % opened.len()];
        let target_uri = uri_of(target);
        for character in [4u32, 12, 20, 28, 36] {
            if !asks("hover") {
                continue;
            }
            client.request(
                id,
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": target_uri },
                    "position": { "line": 20, "character": character },
                }),
            );
            id += 1;
        }
        if asks("diagnostic") {
            client.request(
                id,
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": target_uri } }),
            );
            id += 1;
        }
        if asks("tokens") {
            client.request(
                id,
                "textDocument/semanticTokens/full",
                json!({ "textDocument": { "uri": target_uri } }),
            );
            id += 1;
        }
        if asks("hints") {
            client.request(
                id,
                "textDocument/inlayHint",
                json!({
                    "textDocument": { "uri": target_uri },
                    "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 60, "character": 0 } },
                }),
            );
            id += 1;
        }
        if tick % 10 == 0 {
            eprintln!("[memory] tick {tick}");
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    eprintln!("[memory] done: {ticks} tick(s)");
    eprintln!(
        "[memory] the server sent {} message(s)",
        MESSAGES.load(std::sync::atomic::Ordering::Relaxed)
    );

    client.request(9000, "shutdown", Value::Null);
    client.notify("exit", Value::Null);
}
