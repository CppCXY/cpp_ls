//! **A classic C project, through the server** — the C half of `docs/lsp-status.md`.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\neovim"
//! cargo test --release -p cpp_ls --test c_project -- --ignored --nocapture
//! ```
//!
//! `lsp_status` asks every registered feature of a C++ project (EmmyLuaCodeStyle's `CodeFormat.cpp`). This asks the
//! same kind of question of a **C** project — neovim's `src/nvim/arglist.c` and the header it includes — because C
//! is where the analysis's own assumptions are least exercised: no classes, no templates, `typedef struct` for
//! everything, and members whose names are **macros** (`#define b_fnum handle`, written inside `struct file_buffer`).
//!
//! Each row answers a question whose shape the reader can check: a type name, a file and line, a count, a spelling
//! in a list. A row that says `no`, or `0` where the file plainly has an answer, is a defect and the note says what
//! the answer should have been. Milliseconds are for comparing one commit against the next on this machine only.
//!
//! The files are configurable (`CPPLS_C_FILE`, `CPPLS_C_HEADER`) and the test **skips** when the project is not
//! there, for the reason every real-project probe here does: a path from one machine must not fail on another.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_REAL_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\neovim"));
    named.is_dir().then_some(named)
}

fn named_or(project: &Path, variable: &str, fallback: &str) -> Option<PathBuf> {
    let path = std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| project.join(fallback));
    path.is_file().then_some(path)
}

fn uri_of(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        text.insert(0, '/');
    }
    format!("file://{text}")
}

/// One position in a file, in line/character form.
#[derive(Clone, Copy)]
struct Place {
    line: u32,
    character: u32,
}

/// Where a spelling is: the first occurrence on or after `from_line`, **just past** its first character.
///
/// Past the first character for the reason `lsp_status` learned: a cursor on the first byte of a name asks "what is
/// this node", and the member queries want to be told which member — one byte in is inside the same identifier.
fn at(text: &str, needle: &str, from_line: usize) -> Option<Place> {
    for (line, body) in text.lines().enumerate() {
        if line < from_line {
            continue;
        }
        if let Some(column) = body.find(needle) {
            return Some(Place {
                line: line as u32,
                character: column as u32 + 1,
            });
        }
    }
    None
}

/// Where the cursor goes for "the members of what was just written": right after the `->` or `.`.
fn after_the_operator(text: &str, needle: &str, from_line: usize) -> Option<Place> {
    for (line, body) in text.lines().enumerate() {
        if line < from_line {
            continue;
        }
        if let Some(column) = body.find(needle) {
            return Some(Place {
                line: line as u32,
                character: column as u32 + needle.len() as u32,
            });
        }
    }
    None
}

/// Where the cursor goes for a **call's arguments**: right after the callee's `(`.
fn inside_the_call(text: &str, callee: &str, from_line: usize) -> Option<Place> {
    let written = format!("{callee}(");
    for (line, body) in text.lines().enumerate() {
        if line < from_line {
            continue;
        }
        if let Some(column) = body.find(&written) {
            return Some(Place {
                line: line as u32,
                character: column as u32 + written.len() as u32,
            });
        }
    }
    None
}

fn position(uri: &str, at: Place) -> Value {
    json!({
        "textDocument": { "uri": uri },
        "position": { "line": at.line, "character": at.character },
    })
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(project: &Path) -> Server {
        let log_dir = std::env::temp_dir().join("cppls-c-project");
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
        let deadline = Instant::now() + Duration::from_secs(240);
        loop {
            if Instant::now() > deadline {
                panic!("the server did not answer {method}");
            }
            let Some(message) = self.read() else {
                panic!("the server closed before answering {method}");
            };
            if message["id"] == json!(id) {
                return message["result"].clone();
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

/// One row of the table.
struct Row {
    feature: &'static str,
    answered: bool,
    count: usize,
    ms: u128,
    note: String,
}

impl Row {
    fn print(&self) {
        println!(
            "{:<26} {:<8} {:<5} {:<7} {}",
            self.feature,
            if self.answered { "yes" } else { "no" },
            self.count,
            format!("{} ms", self.ms),
            self.note
        );
    }
}

/// The first line of a hover's markdown, stripped to something a table can hold.
fn first_line(result: &Value) -> String {
    let contents = match result.get("contents") {
        Some(contents) => contents,
        None => return "(nothing)".to_string(),
    };
    let text = if let Some(text) = contents.get("value").and_then(Value::as_str) {
        text.to_string()
    } else if let Some(text) = contents.as_str() {
        text.to_string()
    } else if let Some(list) = contents.as_array() {
        list.iter()
            .filter_map(|entry| {
                entry
                    .get("value")
                    .and_then(Value::as_str)
                    .or(entry.as_str())
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        String::new()
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("```"))
        .unwrap_or("(empty)")
        .to_string()
}

/// Where a definition/declaration list points, as `file:line`.
fn places(result: &Value) -> Vec<String> {
    let list = if result.is_array() {
        result.as_array().cloned().unwrap_or_default()
    } else if result.is_null() {
        Vec::new()
    } else {
        vec![result.clone()]
    };
    list.iter()
        .filter_map(|entry| {
            let uri = entry.get("uri").and_then(Value::as_str)?;
            let line = entry
                .get("range")
                .and_then(|range| range.get("start"))
                .and_then(|start| start.get("line"))
                .and_then(Value::as_u64)
                .map(|line| line + 1)
                .unwrap_or(0);
            Some(format!(
                "{}:{line}",
                Path::new(uri.trim_start_matches("file://"))
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| uri.to_string())
            ))
        })
        .collect()
}

#[test]
#[ignore = "spawns a server over a real project; run with --ignored"]
fn a_classic_c_file_answers_what_it_holds() {
    let Some(project) = project() else {
        eprintln!("skipped: no project (set CPPLS_REAL_PROJECT)");
        return;
    };
    let Some(source) = named_or(&project, "CPPLS_C_FILE", "src/nvim/arglist.c") else {
        eprintln!("skipped: the project has no src/nvim/arglist.c (set CPPLS_C_FILE)");
        return;
    };
    let header = named_or(&project, "CPPLS_C_HEADER", "src/nvim/ex_cmds_defs.h");
    let text = std::fs::read_to_string(&source).expect("the source is readable");
    let header_text = header
        .as_ref()
        .and_then(|header| std::fs::read_to_string(header).ok())
        .unwrap_or_default();

    let mut server = Server::start(&project);
    let source_uri = uri_of(&source);
    let started = Instant::now();
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&project),
            "capabilities": {
                "window": { "workDoneProgress": true },
                "textDocument": {
                    "diagnostic": { "dynamicRegistration": true, "relatedInformation": true },
                    "completion": { "completionItem": { "snippetSupport": true } },
                    "hover": { "contentFormat": ["markdown", "plaintext"] },
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    "publishDiagnostics": { "relatedInformation": true },
                    "inlayHint": { "dynamicRegistration": true },
                },
                "workspace": { "diagnostics": { "refreshSupport": true } },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": source_uri, "languageId": "c", "version": 1, "text": text } }),
    );
    if let (Some(header), false) = (&header, header_text.is_empty()) {
        server.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri_of(header),
                    "languageId": "c",
                    "version": 1,
                    "text": header_text,
                }
            }),
        );
    }

    // **Let the index drain.** A pull every 250 ms is what a reader's editor does while the squiggles fill in; the
    // table is about answers, not about a cold start.
    //
    // **The readiness question is one the server answers itself.** `semanticTokens` returns `null` until the file's
    // *closure* has been read — "ask again later", by design (see the handler) — so polling it is the honest way to
    // wait for the state this table describes. Measured on neovim: the first 50 pulls (12 s) were not enough on an
    // empty cache, and a table measured then reported 0 tokens for a file the server simply was not ready to colour.
    let mut pulls = 0;
    let mut answered = 0usize;
    for id in 10..400 {
        std::thread::sleep(Duration::from_millis(250));
        let _ = server.request(
            id,
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": source_uri } }),
        );
        let answer = server.request(
            id + 1000,
            "textDocument/semanticTokens/full",
            json!({ "textDocument": { "uri": source_uri } }),
        );
        let coloured = answer
            .get("data")
            .and_then(Value::as_array)
            .is_some_and(|data| !data.is_empty());
        // **Three times in a row, not once.** A single answer says the closure was read at that instant; the pump
        // keeps committing renderings and summaries afterwards, and a question asked inside that window queues
        // behind it (measured: a row that answered `null` after a one-answer wait, and 10 ms when asked again).
        // Three consecutive answers is the cheapest stable reading of "the project has finished with this file".
        answered = if coloured { answered + 1 } else { 0 };
        if answered >= 3 {
            break;
        }
        pulls += 1;
    }
    println!(
        "--- settled after {pulls} pull(s) ({answered} consecutive colouring answer(s)), {:?}",
        started.elapsed()
    );

    // **Then let the index finish filling.** The colouring answers as soon as this file's closure is read, and the
    // pump is still committing summaries for the rest of the project afterwards — every insert invalidates the memos a
    // query uses, so a question asked in that window pays for a fresh include-graph walk (measured: 1.6 s per
    // cross-file question while the index was filling, 10 ms once it was quiet). Thirty seconds of pulls is what took
    // that window away; the table is about answers in a session a reader is using, not about its first ten seconds.
    for id in 400..520 {
        std::thread::sleep(Duration::from_millis(250));
        let _ = server.request(
            id,
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": source_uri } }),
        );
    }
    // **And then stop asking for anything**, so that the work the last pulls started can drain: the colouring
    // refuses while the pump has a file in hand, and a pull every 250 ms keeps handing it work.
    for _ in 520..530 {
        std::thread::sleep(Duration::from_millis(250));
    }
    println!("--- the project is quiet after {:?}", started.elapsed());

    // **One warm-up question, so the table measures answers rather than the cook.** The first query after opening
    // pays for the rendering (measured: 1.4 s against 10 ms once it exists), and that cost belongs to the cold-start
    // measurement, not to a row about whether a feature answers.
    if let Some(place) = at(&text, "addr_count", 0) {
        let _ = server.request(900_001, "textDocument/hover", position(&source_uri, place));
    }
    if let Some(place) = at(&text, "alist_add(", 60) {
        let _ = server.request(
            900_002,
            "textDocument/definition",
            position(&source_uri, place),
        );
    }
    println!(
        "{:<26} {:<8} {:<5} {:<7} {}",
        "feature", "answered", "n", "ms", "note"
    );

    let mut rows: Vec<Row> = Vec::new();
    let mut id = 1000;
    let table_started = Instant::now();
    let mut next = || {
        id += 1;
        id
    };

    // **A member of a struct the file reaches through a typedef** — `exarg_T *eap; eap->addr_count`.
    if let Some(place) = at(&text, "addr_count", 0) {
        let started = Instant::now();
        let result = server.request(next(), "textDocument/hover", position(&source_uri, place));
        rows.push(Row {
            feature: "hover (member)",
            answered: !result.is_null(),
            count: 0,
            ms: started.elapsed().as_millis(),
            note: first_line(&result),
        });
    }

    // **A member whose name is a macro**: `buf->b_fnum` is `buf->handle` after the preprocessor, and `handle` is what
    // `struct file_buffer` declares. This is the C shape that only a *rendering* can answer.
    if let Some(place) = at(&text, "b_fnum", 0) {
        let started = Instant::now();
        let result = server.request(next(), "textDocument/hover", position(&source_uri, place));
        rows.push(Row {
            feature: "hover (macro member)",
            answered: !result.is_null(),
            count: 0,
            ms: started.elapsed().as_millis(),
            note: first_line(&result),
        });
    }

    // **Go to the declaration of that member**, which is the header's `handle` field.
    if let Some(place) = at(&text, "addr_count", 0) {
        let started = Instant::now();
        let result = server.request(
            next(),
            "textDocument/definition",
            position(&source_uri, place),
        );
        let found = places(&result);
        rows.push(Row {
            feature: "definition (member)",
            answered: !found.is_empty(),
            count: found.len(),
            ms: started.elapsed().as_millis(),
            note: found.join(" | "),
        });
    }

    // **A function the file defines**, asked at a call site.
    if let Some(place) = at(&text, "alist_add(", 60) {
        let started = Instant::now();
        let result = server.request(
            next(),
            "textDocument/definition",
            position(&source_uri, place),
        );
        let found = places(&result);
        rows.push(Row {
            feature: "definition (function)",
            answered: !found.is_empty(),
            count: found.len(),
            ms: started.elapsed().as_millis(),
            note: found.join(" | "),
        });
    }

    // **Every use of a file-local function** — the question a rename starts with.
    if let Some(place) = at(&text, "check_arglist_locked", 0) {
        let started = Instant::now();
        let result = server.request(
            next(),
            "textDocument/references",
            json!({
                "textDocument": { "uri": source_uri },
                "position": { "line": place.line, "character": place.character },
                "context": { "includeDeclaration": true },
            }),
        );
        let found = places(&result);
        rows.push(Row {
            feature: "references",
            answered: !found.is_empty(),
            count: found.len(),
            ms: started.elapsed().as_millis(),
            note: found
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | "),
        });
    }

    // **The completion a reader opens by typing `->`** — the C member list, asked with nothing written yet.
    if let Some(place) = after_the_operator(&text, "eap->", 0) {
        let started = Instant::now();
        let result = server.request(
            next(),
            "textDocument/completion",
            position(&source_uri, place),
        );
        let items = result
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| result.as_array().cloned())
            .unwrap_or_default();
        let has = items.iter().any(|item| {
            item.get("label")
                .and_then(Value::as_str)
                .is_some_and(|label| label == "addr_count")
        });
        rows.push(Row {
            feature: "completion (member)",
            answered: has,
            count: items.len(),
            ms: started.elapsed().as_millis(),
            note: format!(
                "address_count: {has}; first: {:?}",
                items
                    .first()
                    .and_then(|item| item.get("label"))
                    .and_then(Value::as_str)
            ),
        });
    }

    // **The symbols of the file**, which is the outline.
    {
        let started = Instant::now();
        let result = server.request(
            next(),
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": source_uri } }),
        );
        let items = result.as_array().cloned().unwrap_or_default();
        rows.push(Row {
            feature: "documentSymbol",
            answered: !items.is_empty(),
            count: items.len(),
            ms: started.elapsed().as_millis(),
            note: items
                .iter()
                .take(3)
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(", "),
        });
    }

    // **The colouring**, whose source is the same type facts the rest of this table asks about.
    //
    // **A `null` here is the protocol's "ask again later", not a defect**: the handler declines while the file's
    // closure is still being read, and a client refreshes on the server's own `workspace/semanticTokens/refresh`.
    // So this asks up to three times, and the row says how many it took — a table that reported the first `null` as
    // "no tokens" measured a project that had not finished loading, which is what it did before this loop existed.
    {
        let started = Instant::now();
        let mut asked = 0;
        let mut result = Value::Null;
        let mut count = 0;
        // Six asks, three seconds: the closure check is `pending() == 0`, and a diagnostics pull just before this
        // enqueues work, so a shorter budget reported `null` for a file that colours a moment later.
        while asked < 6 {
            asked += 1;
            result = server.request(
                next(),
                "textDocument/semanticTokens/full",
                json!({ "textDocument": { "uri": source_uri } }),
            );
            count = result
                .get("data")
                .and_then(Value::as_array)
                .map(|data| data.len() / 5)
                .unwrap_or(0);
            if count > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        rows.push(Row {
            feature: "semanticTokens",
            answered: count > 0,
            count,
            ms: started.elapsed().as_millis(),
            // A count of zero is the row worth reading, so the raw answer is what the note carries then: an error
            // object and an empty `data` are different defects.
            note: if count > 0 {
                format!("{asked} ask(s)")
            } else {
                result.to_string().chars().take(160).collect()
            },
        });
    }

    // **The hints**: `auto`-like deductions and parameter names, all of which need the declarations this table asks
    // about to be in place.
    {
        let started = Instant::now();
        let result = server.request(
            next(),
            "textDocument/inlayHint",
            json!({
                "textDocument": { "uri": source_uri },
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 400, "character": 0 } },
            }),
        );
        let items = result.as_array().cloned().unwrap_or_default();
        rows.push(Row {
            feature: "inlayHint",
            answered: true,
            count: items.len(),
            ms: started.elapsed().as_millis(),
            note: items
                .iter()
                .take(2)
                .filter_map(|item| item.get("label").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(", "),
        });
    }

    // **Nothing is wrong with this file**, and a C file of 2 000 lines is where a false positive is worth finding.
    {
        let started = Instant::now();
        let result = server.request(
            next(),
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": source_uri } }),
        );
        let items = result
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        rows.push(Row {
            feature: "diagnostic",
            answered: true,
            count: items.len(),
            ms: started.elapsed().as_millis(),
            note: items
                .iter()
                .take(3)
                .filter_map(|item| item.get("message").and_then(Value::as_str))
                .map(|message| message.chars().take(50).collect::<String>())
                .collect::<Vec<_>>()
                .join(" | "),
        });
    }

    // **A call's parameter names**, asked inside the parentheses of a call the file itself makes — a function the
    // file defines, so nothing about visibility stands between the question and the answer. The needle is the call
    // (`do_argfile(eap`), not the definition: signature help at a *definition*'s parameter list has nothing to say,
    // and asking there would report a working feature as broken.
    if let Some(place) = after_the_operator(&text, "do_argfile(eap", 0) {
        let started = Instant::now();
        // **The same "ask again later" contract as the colouring**: the handler declines while files are queued, so
        // it is asked up to three times.
        let mut asked = 0;
        let mut result = Value::Null;
        let mut signatures = Vec::new();
        while asked < 3 {
            asked += 1;
            result = server.request(
                next(),
                "textDocument/signatureHelp",
                json!({
                    "textDocument": { "uri": source_uri },
                    "position": { "line": place.line, "character": place.character },
                }),
            );
            signatures = result
                .get("signatures")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if !signatures.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        rows.push(Row {
            feature: "signatureHelp",
            answered: !signatures.is_empty(),
            count: signatures.len(),
            ms: started.elapsed().as_millis(),
            note: if signatures.is_empty() {
                result.to_string().chars().take(160).collect()
            } else {
                format!(
                    "{} (asked {asked} time(s))",
                    signatures
                        .first()
                        .and_then(|signature| signature.get("label"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                )
            },
        });
    }

    for row in &rows {
        row.print();
    }

    // **The same two cross-file questions again**, because their first answer includes reading the header: a second
    // editor request is what a reader actually experiences while moving around the file, and the difference between
    // the two numbers is the difference between "this feature is slow" and "this file was cold".
    if let Some(place) = at(&text, "b_fnum", 0) {
        let started = Instant::now();
        let _ = server.request(950_001, "textDocument/hover", position(&source_uri, place));
        println!(
            "again: hover (macro member)     {} ms",
            started.elapsed().as_millis()
        );
    }
    if let Some(place) = at(&text, "addr_count", 0) {
        let started = Instant::now();
        let _ = server.request(
            950_002,
            "textDocument/definition",
            position(&source_uri, place),
        );
        println!(
            "again: definition (member)      {} ms",
            started.elapsed().as_millis()
        );
    }
    if let Some(place) = after_the_operator(&text, "do_argfile(eap", 0) {
        let started = Instant::now();
        let _ = server.request(
            950_003,
            "textDocument/signatureHelp",
            json!({
                "textDocument": { "uri": source_uri },
                "position": { "line": place.line, "character": place.character },
            }),
        );
        println!(
            "again: signatureHelp           {} ms",
            started.elapsed().as_millis()
        );
    }

    // **The rendering is what made two of those rows possible**, and saying how long the table took is the difference
    // between "the answer is cheap" and "the answer waited for a cook".
    println!(
        "--- the table's own questions took {:?}",
        table_started.elapsed()
    );
}
