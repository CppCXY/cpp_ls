//! **How usable the server is on neovim, asked over the protocol** — the real binary, the real text, real positions.
//!
//! ```text
//! $env:CPPLS_NEOVIM = "E:\neovim"
//! cargo test --release -p cpp_ls --test neovim_usability -- --ignored --nocapture
//! ```
//!
//! # Why over the protocol rather than through `Session`
//!
//! Because the question is what a reader sees. A probe that builds its own session can be wrong in exactly the ways an
//! editor is not — a different file set, a different request order, a cursor it placed itself. This one spawns the same
//! binary the editor spawns, opens files the way a client does, and asks `textDocument/hover`,
//! `textDocument/definition` and `textDocument/diagnostic` at positions taken from the file's own text.
//!
//! # What it asks, and why those positions
//!
//! Only positions that are **uses**: `name(`, `.name`, `->name`, `::name`. Those are the three questions a reader
//! actually points at — a call, a member of an object, a member of a namespace — and sampling declarations instead
//! would measure the parser rather than the resolver. Each file contributes a spread of them, so a 20-file sample is a
//! few hundred questions rather than a few thousand.
//!
//! It reports, per category: how many answered, how many did not, and the shape of the misses (a name printed with its
//! file and line, so the next step is a file rather than a number). Diagnostics are counted by code and severity:
//! the findings a reader would see underlined, which is the other half of usability.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long one request may wait for the server's first answer about a file. The first request after `didOpen` pays
/// for the file's reading (a cook), and the index behind a name may still be filling.
const PATIENCE: Duration = Duration::from_secs(120);

fn project() -> Option<PathBuf> {
    let root = std::env::var_os("CPPLS_NEOVIM")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\neovim"));
    root.join("src").is_dir().then_some(root)
}

/// The sources a reader edits: no `build/`, no `.deps/` (vendored), no `test/` (a Lua suite).
fn sources_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            if name.starts_with('.') || name == "build" || name == "test" {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let extension = path
                .extension()
                .map(|extension| extension.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if extension == "c" || extension == "h" {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn uri_of(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/").replace(' ', "%20");
    format!("file:///{}", text.trim_start_matches('/'))
}

fn position_of(source: &str, offset: usize) -> (u32, u32) {
    let before = &source[..offset];
    let line = before.matches('\n').count();
    let character = before.rsplit('\n').next().unwrap_or_default().chars().count();
    (line as u32, character as u32)
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    next_id: i64,
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
            next_id: 1,
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

    /// One request, with the id the client assigned it.
    fn ask(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
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

/// **One position worth asking about** — an identifier that is a use, and which kind it is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Shape {
    Call,
    Dot,
    Arrow,
    Scope,
}

impl Shape {
    fn text(self) -> &'static str {
        match self {
            Shape::Call => "call",
            Shape::Dot => "dot",
            Shape::Arrow => "arrow",
            Shape::Scope => "scope",
        }
    }
}

struct Question {
    offset: usize,
    shape: Shape,
    spelling: String,
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// **The uses in one file**, spread across it: `name(`, `.name`, `->name`, `::name`.
fn questions_in(source: &str, per_file: usize) -> Vec<Question> {
    let bytes = source.as_bytes();
    let mut found: Vec<Question> = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if !is_word(bytes[at]) || at > 0 && is_word(bytes[at - 1]) {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && is_word(bytes[at]) {
            at += 1;
        }
        let end = at;
        let before = &source[..start];
        let after = &source[end..];

        // A **comment or a string** is not a use: a `// name(` in a comment would be counted as a miss forever.
        let line_start = before.rfind('\n').map(|newline| newline + 1).unwrap_or(0);
        if before[line_start..].contains("//") || before[line_start..].contains('"') {
            continue;
        }
        let shape = if before.ends_with("->") {
            Some(Shape::Arrow)
        } else if before.ends_with('.') {
            Some(Shape::Dot)
        } else if before.ends_with("::") {
            Some(Shape::Scope)
        } else if after.starts_with('(') {
            Some(Shape::Call)
        } else {
            None
        };
        let Some(shape) = shape else {
            continue;
        };
        // **Not a number**: `1.5` reads as a `.` before `5`, and a float is not a member.
        if source[start..end].chars().all(|character| character.is_ascii_digit()) {
            continue;
        }
        // A keyword is not a name: `return (`, `sizeof (`.
        let spelling = source[start..end].to_string();
        if matches!(
            spelling.as_str(),
            "if" | "for"
                | "while"
                | "switch"
                | "return"
                | "sizeof"
                | "do"
                | "else"
                | "case"
                | "goto"
                | "typeof"
                | "__typeof__"
                | "defined"
                | "static"
                | "const"
                | "int"
                | "char"
                | "void"
                | "unsigned"
                | "long"
                | "short"
                | "struct"
                | "union"
                | "enum"
        ) {
            continue;
        }
        found.push(Question {
            offset: start,
            shape,
            spelling,
        });
    }
    if found.len() <= per_file {
        return found;
    }
    let step = found.len() / per_file;
    found.into_iter().step_by(step.max(1)).take(per_file).collect()
}

/// Clicks a reader makes on a receiver and the list that follows — `ptr->` and `object.` are the two places a
/// completion is the thing being asked for.
#[derive(Default)]
struct Completion {
    asked: usize,
    answered: usize,
    items: usize,
    misses: Vec<String>,
}

#[derive(Default)]
struct Tally {
    asked: usize,
    answered: usize,
    misses: Vec<String>,
    by_shape: BTreeMap<&'static str, (usize, usize)>,
    micros: Vec<u128>,
}

impl Tally {
    fn record(&mut self, question: &Question, answered: bool, micros: u128, where_at: &str) {
        self.asked += 1;
        self.micros.push(micros);
        let entry = self.by_shape.entry(question.shape.text()).or_default();
        entry.0 += 1;
        if answered {
            self.answered += 1;
            entry.1 += 1;
        } else if self.misses.len() < 24 {
            self.misses.push(format!(
                "{where_at}  {} {} — no answer",
                question.shape.text(),
                question.spelling
            ));
        }
    }

    fn percentile(&mut self, share: f64) -> u128 {
        self.micros.sort_unstable();
        if self.micros.is_empty() {
            return 0;
        }
        let at = ((self.micros.len() as f64 * share) as usize).min(self.micros.len() - 1);
        self.micros[at]
    }
}

fn main_report(hover: &mut Tally, definition: &mut Tally) {
    let (hover_p50, hover_p95) = (hover.percentile(0.5), hover.percentile(0.95));
    println!(
        "\nhover:      {}/{} answered ({:.0} %), p50 {} ms, p95 {} ms",
        hover.answered,
        hover.asked,
        percent(hover.answered, hover.asked),
        hover_p50 / 1000,
        hover_p95 / 1000,
    );
    for (shape, (asked, answered)) in &hover.by_shape {
        println!(
            "  {shape:<6} {answered:>4}/{asked:<4} ({:.0} %)",
            percent(*answered, *asked)
        );
    }
    let (definition_p50, definition_p95) = (definition.percentile(0.5), definition.percentile(0.95));
    println!(
        "definition: {}/{} answered ({:.0} %), p50 {} ms, p95 {} ms",
        definition.answered,
        definition.asked,
        percent(definition.answered, definition.asked),
        definition_p50 / 1000,
        definition_p95 / 1000,
    );
    for (shape, (asked, answered)) in &definition.by_shape {
        println!(
            "  {shape:<6} {answered:>4}/{asked:<4} ({:.0} %)",
            percent(*answered, *asked)
        );
    }
    if !hover.misses.is_empty() {
        println!("\nhover misses (first {}):", hover.misses.len());
        for miss in &hover.misses {
            println!("  {miss}");
        }
    }
    if !definition.misses.is_empty() {
        println!("\ndefinition misses (first {}):", definition.misses.len());
        for miss in &definition.misses {
            println!("  {miss}");
        }
    }
}

fn percent(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

#[test]
#[ignore]
fn the_server_on_neovim() {
    let Some(root) = project() else {
        eprintln!("no neovim checkout: set CPPLS_NEOVIM");
        return;
    };
    let every: usize = std::env::var("CPPLS_USABILITY_EVERY")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(23);
    let files_wanted: usize = std::env::var("CPPLS_USABILITY_FILES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20);
    let per_file: usize = std::env::var("CPPLS_USABILITY_POSITIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12);

    let all = sources_under(&root.join("src"));
    let files: Vec<PathBuf> = all.iter().step_by(every.max(1)).take(files_wanted).cloned().collect();
    println!(
        "[usability] {} of {} source file(s), up to {per_file} position(s) each",
        files.len(),
        all.len()
    );

    let mut client = Client::start(&root);
    client.ask(
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    client.notify("initialized", json!({}));

    let mut hover = Tally::default();
    let mut definition = Tally::default();
    let mut completion = Completion::default();
    let mut diagnostics_by_code: BTreeMap<String, usize> = BTreeMap::new();
    let mut files_with_errors = 0usize;
    let started = Instant::now();

    for file in &files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let uri = uri_of(file);
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "c", "version": 1, "text": source } }),
        );

        // **One pull per file**, because that is what a client that supports pull diagnostics does, and it is the
        // reading that makes the hover questions cheap afterwards.
        let report = client.ask(
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri } }),
        );
        let mut file_has_errors = false;
        let mut this_file: BTreeMap<String, usize> = BTreeMap::new();
        for item in report["result"]["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let code = item["code"].as_str().unwrap_or("(none)").to_string();
            *diagnostics_by_code.entry(code.clone()).or_default() += 1;
            *this_file.entry(code).or_default() += 1;
            if item["severity"] == json!(1) {
                file_has_errors = true;
            }
        }
        if file_has_errors {
            files_with_errors += 1;
        }
        if !this_file.is_empty() {
            println!(
                "  {:<58} {}",
                short(file, &root),
                this_file
                    .iter()
                    .map(|(code, count)| format!("{code}×{count}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }

        for question in questions_in(&source, per_file) {
            let (line, character) = position_of(&source, question.offset);
            let position = json!({ "line": line, "character": character });
            let where_at = format!("{}:{}:{}", short(file, &root), line + 1, character + 1);

            let began = Instant::now();
            let answer = client.ask(
                "textDocument/hover",
                json!({ "textDocument": { "uri": uri }, "position": position }),
            );
            let answered = !answer["result"].is_null()
                && answer["result"]["contents"].as_object().is_some_and(|contents| {
                    contents.get("value").is_some_and(|value| !value.is_null())
                });
            hover.record(&question, answered, began.elapsed().as_micros(), &where_at);

            let began = Instant::now();
            let answer = client.ask(
                "textDocument/definition",
                json!({ "textDocument": { "uri": uri }, "position": position }),
            );
            let answered = match &answer["result"] {
                Value::Null => false,
                Value::Array(items) => !items.is_empty(),
                Value::Object(_) => true,
                _ => false,
            };
            definition.record(&question, answered, began.elapsed().as_micros(), &where_at);

            // **The list a reader gets after `->` or `.`** — the other half of what a pointer or a member is for.
            if matches!(question.shape, Shape::Arrow | Shape::Dot) {
                let began = Instant::now();
                let answer = client.ask(
                    "textDocument/completion",
                    json!({
                        "textDocument": { "uri": uri },
                        "position": { "line": line, "character": character },
                    }),
                );
                let count = match &answer["result"] {
                    Value::Null => 0,
                    Value::Array(items) => items.len(),
                    Value::Object(object) => object
                        .get("items")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len),
                    _ => 0,
                };
                completion.asked += 1;
                completion.items += count;
                if count > 0 {
                    completion.answered += 1;
                } else if completion.misses.len() < 12 {
                    completion.misses.push(format!("{where_at}  {}", question.spelling));
                }
                let _ = began;
            }
        }
    }

    main_report(&mut hover, &mut definition);

    println!(
        "\ncompletion: {}/{} non-empty ({:.0} %), {} item(s) in total",
        completion.answered,
        completion.asked,
        percent(completion.answered, completion.asked),
        completion.items,
    );
    for miss in &completion.misses {
        println!("  miss {miss}");
    }

    println!(
        "\ndiagnostics: {} finding(s) over {} file(s), {} with at least one error",
        diagnostics_by_code.values().sum::<usize>(),
        files.len(),
        files_with_errors,
    );
    let mut by_code: Vec<(&String, &usize)> = diagnostics_by_code.iter().collect();
    by_code.sort_by(|left, right| right.1.cmp(left.1));
    for (code, count) in by_code {
        println!("  {count:>5}  {code}");
    }
    println!("\n[usability] {:.1} s", started.elapsed().as_secs_f64());
}

fn short(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}
