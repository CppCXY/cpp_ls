//! **The first completion on a real project, and where its time went.**
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_ls --test real_project -- --ignored --nocapture
//! ```
//!
//! # Why this exists, and why it is `#[ignore]`d
//!
//! Every end-to-end latency figure this repository has comes from `latency.rs`, whose fixture is one file and a
//! standard-library closure the index finishes in **1.7 s**. The project the user actually opens takes **70 s** to
//! index, and **no completion has ever been timed on one** — so "why is the first completion slow there" has had no
//! answer but an argument. This is the missing measurement, and it is `#[ignore]`d because it spawns a server over a
//! project of a hundred thousand lines and eats a core for a minute.
//!
//! # What it prints, and why those five numbers
//!
//! The server already separates the two halves of a completion, at `debug`:
//!
//! ```text
//!   completion: prepare … ms, catch up … ms, modules … ms, … before the query    ← the write lock + one parse
//!   completion: view … ms, completions … ms, … item(s)                           ← the query itself
//! ```
//!
//! `prepare` takes the session's **write** lock, and the pump holds that lock in slices this repository has measured
//! at **84–2430 ms** against a 50 ms budget (`analysis_state.rs`) while it indexes. So if `prepare` dominates, the
//! answer is the lock and the fix is in the pump's slice budget; if `completions` dominates, the answer is the query
//! degrading on a cold index. The two have nothing in common but the millisecond, which is why this prints both
//! rather than a total.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The project to open, and the file to open in it. Defaulted to the one this was written for, so that the command
/// above works with no environment at all; `CPPLS_REAL_PROJECT` overrides the directory.
fn project() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    )
}

/// The file whose completion is timed — the one a person would have open, and the one the project's own source
/// listing puts first.
fn entry(project: &std::path::Path) -> std::path::PathBuf {
    std::env::var("CPPLS_REAL_ENTRY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            project
                .join("CodeFormatServer")
                .join("src")
                .join("main.cpp")
        })
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    /// Every message the server has sent, filled by the reader thread — see [`Server::read`].
    incoming: std::sync::mpsc::Receiver<Value>,
    log_dir: std::path::PathBuf,
}

impl Server {
    fn start(project: &std::path::Path) -> Self {
        let log_dir = std::env::temp_dir().join("cppls-real-project");
        let _ = std::fs::remove_dir_all(&log_dir);
        std::fs::create_dir_all(&log_dir).expect("a log directory");

        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg(&log_dir)
            // The five numbers this probe exists for are `debug`, and the default level is `info`.
            .arg("--log-level")
            .arg(std::env::var("CPPLS_LOG_LEVEL").unwrap_or_else(|_| "debug".to_string()))
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // **Kept, not discarded.** A panic in a `tokio::spawn`ed task does not reach the LSP channel — it prints to
            // the server's stderr — and a pump task that dies that way leaves its progress bar on screen for ever,
            // which is the report this probe exists for. `/dev/null` is why the first run of it could not say so.
            .stderr(Stdio::from(
                std::fs::File::create(log_dir.join("stderr.txt")).expect("a stderr file"),
            ))
            .spawn()
            .expect("the server starts");

        // Taken rather than cloned: a child's pipes are not files and have no `try_clone`.
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        // **From the first byte, on its own thread** — see `read`. `read_messages` answers every message including the
        // ones nobody asked for, which is the whole point: the progress reports are server-initiated.
        let (incoming, from_the_server) = std::sync::mpsc::channel();
        std::thread::spawn(move || Self::read_messages(stdout, incoming));
        Server {
            child,
            stdin,
            incoming: from_the_server,
            log_dir,
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

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let Some(message) = self.read() else {
                panic!("the server closed before answering {method}");
            };
            if message["id"] == json!(id) {
                return message;
            }
        }
    }

    fn read(&mut self) -> Option<Value> {
        // **The reader thread's queue, not the pipe.** A client that only reads while it waits for an answer of its
        // own is not a client: the server writes `$/progress` on its own rhythm, and a pipe nobody drains fills at
        // 64 KB and blocks the server's writer — which parks the pump on a *write*, with no CPU and a progress bar
        // that never moves. That is what the first version of the idle probe measured instead of the server.
        self.incoming.recv_timeout(Duration::from_secs(30)).ok()
    }

    /// Read the server's stdout until it closes, one LSP message at a time.
    fn read_messages(stdout: ChildStdout, into: std::sync::mpsc::Sender<Value>) {
        let mut stdout = BufReader::new(stdout);
        loop {
            let mut length = 0usize;
            loop {
                let mut header = String::new();
                match stdout.read_line(&mut header) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                if let Some(rest) = header.strip_prefix("Content-Length: ") {
                    length = rest.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            if stdout.read_exact(&mut body).is_err() {
                return;
            }
            if let Ok(message) = serde_json::from_slice(&body) {
                if into.send(message).is_err() {
                    return;
                }
            }
        }
    }

    /// Everything the server has written about completions so far.
    fn completion_lines(&self) -> Vec<String> {
        self.lines_with("completion")
    }

    /// Every `index slice` line, in the order the log files were read.
    fn slice_lines(&self) -> Vec<String> {
        self.lines_with("index slice")
    }

    /// Every line the server wrote at `warn` or worse — the pump's own admission that something cannot be paid.
    fn warning_lines(&self) -> Vec<String> {
        let mut lines = self.lines_with("WARN");
        lines.extend(self.lines_with("ERROR"));
        lines
    }

    fn lines_with(&self, needle: &str) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.log_dir) else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        let mut paths: Vec<_> = entries.filter_map(Result::ok).map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            for line in text.lines() {
                if line.contains(needle) {
                    lines.push(line.to_string());
                }
            }
        }
        lines
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
fn the_first_completion_on_a_real_project_says_where_its_time_went() {
    let project = project();
    let entry = entry(&project);
    let text = std::fs::read_to_string(&entry).expect("the entry file is readable");
    let uri = format!("file:///{}", entry.display().to_string().replace('\\', "/"));

    // **A cursor right after a `.`,** which is the completion a person actually waits for: `full.` needs the type of
    // `full` resolved, and that type is declared in another file whose summary may not be read yet. A cursor on a
    // `}` (the first version of this probe) asks an easy question — names in scope — and measured 377 ms first and
    // 9 ms after, which is not the number anyone is waiting for.
    //
    // The first `.` that is not inside a comment or a `#include` is used, and the cursor is one past it. A file with
    // no such line falls back to the last non-empty line, which at least keeps the probe honest about which it
    // measured by printing the line it chose.
    let mut line = 0;
    let mut character = 4;
    let mut on_a_member = false;
    for (index, text_line) in text.lines().enumerate() {
        let trimmed = text_line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with('*') {
            continue;
        }
        if let Some(dot) = text_line.find('.') {
            line = index;
            character = dot + 1;
            on_a_member = true;
            break;
        }
    }
    if !on_a_member {
        for (index, text_line) in text.lines().enumerate() {
            if !text_line.trim().is_empty() {
                line = index;
            }
        }
        character = 4;
    }
    println!(
        "cursor: line {line}, character {character}, on_a_member {on_a_member} — {:?}",
        text.lines().nth(line).unwrap_or_default()
    );

    let mut server = Server::start(&project);

    let started = Instant::now();
    let answer = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": format!("file:///{}", project.display().to_string().replace('\\', "/")),
            "capabilities": { "textDocument": { "completion": { "completionItem": { "snippetSupport": true } } } },
        }),
    );
    println!("initialize answered in {:?}", started.elapsed());
    assert!(answer["result"].is_object(), "initialize failed: {answer}");

    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": text } }),
    );

    // **Asked immediately, with no waiting at all** — the question is what a person who opens a project and types
    // gets, not what a patient client gets.
    let started = Instant::now();
    let answer = server.request(
        2,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        }),
    );
    let wall = started.elapsed();
    let items = answer["result"]
        .as_array()
        .map(|items| items.len())
        .or_else(|| {
            answer["result"]["items"]
                .as_array()
                .map(|items| items.len())
        })
        .unwrap_or(0);
    println!("\nthe first completion: {wall:?}, {items} item(s)");

    // The same question again, once the first one has warmed whatever it warms.
    let started = Instant::now();
    let answer = server.request(
        3,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        }),
    );
    let items = answer["result"]
        .as_array()
        .map(|items| items.len())
        .or_else(|| {
            answer["result"]["items"]
                .as_array()
                .map(|items| items.len())
        })
        .unwrap_or(0);
    println!(
        "the second completion: {:?}, {items} item(s)",
        started.elapsed()
    );

    // The pump is still running; let it write what it has before the log is read.
    std::thread::sleep(Duration::from_millis(1500));
    println!("\n--- what the server said about it ---");
    for line in server.completion_lines() {
        println!("{line}");
    }
}

/// **The two hovers `docs/remaining.md` §A reports, asked of the project they were reported on.**
///
/// ```text
/// A1  `std::map<std::string, LuaTokenKind, std::less<>> LuaLexer::LuaReserved = {`
///     hovering `map` — the card showed `class map` and the *declaration's* parameter names, never the spelling
///     the file in front of the reader says;
/// A2  `static const struct { unsigned char left; unsigned char right; } priority[] = { … }`
///     hovering `priority` — the card said `void[] priority`, because an unnamed class body contributed no name
///     and the specifier reader fell back to `void`.
/// ```
///
/// The two files are the project's own, so this is the report's own fixture: `LuaParser/src/Lexer/LuaLexer.cpp` and
/// `LuaParser/include/LuaParser/Lexer/LuaDefine.h`. A checkout without them is skipped rather than failed — the
/// point of an `#[ignore]`d real-project probe is that it says nothing about a machine that has no project.
#[test]
#[ignore = "opens a real project; run with --ignored"]
fn the_two_reported_hovers_answer_on_the_project_they_came_from() {
    let project = project();
    let lexer = project
        .join("LuaParser")
        .join("src")
        .join("Lexer")
        .join("LuaLexer.cpp");
    let define = project
        .join("LuaParser")
        .join("include")
        .join("LuaParser")
        .join("Lexer")
        .join("LuaDefine.h");
    if !lexer.is_file() || !define.is_file() {
        eprintln!("skipped: the project has no {:?}", lexer);
        return;
    }

    let lexer_text = std::fs::read_to_string(&lexer).expect("the file reads");
    let define_text = std::fs::read_to_string(&define).expect("the file reads");
    let lexer_uri = format!("file:///{}", lexer.display().to_string().replace('\\', "/"));
    let define_uri = format!("file:///{}", define.display().to_string().replace('\\', "/"));

    // Where the two names are written, read off the file rather than stated by hand.
    let (instance_line, instance_character) = position_of(&lexer_text, "LuaReserved = ", "map");
    let (priority_line, priority_character) = position_of(&define_text, "priority[]", "priority");
    println!(
        "`map` at {instance_line}:{instance_character}, `priority` at {priority_line}:{priority_character}"
    );

    let mut server = Server::start(&project);
    let answer = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": format!("file:///{}", project.display().to_string().replace('\\', "/")),
            "capabilities": { "textDocument": { "hover": { "contentFormat": ["markdown"] } } },
        }),
    );
    assert!(answer["result"].is_object(), "initialize failed: {answer}");
    server.notify("initialized", json!({}));
    for (uri, text) in [(&lexer_uri, &lexer_text), (&define_uri, &define_text)] {
        server.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": text } }),
        );
    }

    // **Asked until the index has the answer**, which is the deal the protocol offers (`isIncomplete`): a project
    // this size is still being read when the first request arrives, and `null` there means *ask again*, not *there
    // is nothing*. The deadline is what turns a missing answer into a failure worth reading.
    let mut hover = |server: &mut Server, id: i64, uri: &str, line: u64, character: u64| {
        let deadline = Instant::now() + Duration::from_secs(90);
        let mut next = id;
        loop {
            let answer = server.request(
                next,
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                }),
            );
            if !answer["result"].is_null() {
                return answer;
            }
            if Instant::now() >= deadline {
                return answer;
            }
            next += 1;
            std::thread::sleep(Duration::from_millis(300));
        }
    };

    let card = hover(
        &mut server,
        100,
        &lexer_uri,
        instance_line,
        instance_character,
    );
    let text = card["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {card}"));
    println!("\n--- hover on `map` ---\n{text}\n");
    assert!(
        text.contains("std::map<std::string, LuaTokenKind, std::less<>>"),
        "the instantiation the file writes: {text}"
    );

    let card = hover(
        &mut server,
        200,
        &define_uri,
        priority_line,
        priority_character,
    );
    let text = card["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {card}"));
    println!("--- hover on `priority` ---\n{text}\n");
    assert!(
        !text.contains("void"),
        "an anonymous struct's variable is not `void`: {text}"
    );
    assert!(
        text.contains("unsigned char"),
        "the declaration's own text is what the reader can see: {text}"
    );
}

/// The protocol position of `needle` on the first line that contains `anchor`.
fn position_of(text: &str, anchor: &str, needle: &str) -> (u64, u64) {
    let line = text
        .lines()
        .position(|line| line.contains(anchor))
        .unwrap_or_else(|| panic!("{anchor:?} is not in the fixture"));
    let character = text
        .lines()
        .nth(line)
        .and_then(|text| text.find(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not on the line that writes {anchor:?}"));
    (line as u64, character as u64)
}

/// **Does the pump go idle on its own?** — the `Load workspace: re-reading N file(s)` bar that never retired.
///
/// Reported from an editor as a bar stuck at `re-reading 9 file(s) for the compiler's view` with **no CPU in use**.
/// The bar retires when `pending_work()` reaches zero, so a bar that stays is a count nothing pays: either a slice keeps
/// claiming work it cannot build, or the count holds something no slice takes. This runs the real server over the
/// project with one file open and watches its own log for two minutes — the state the report was in, where the bar is
/// the only thing the server is saying and no request nudges it.
#[test]
#[ignore = "opens a real project; run with --ignored"]
fn the_pump_goes_idle_without_a_client_asking() {
    let project = project();
    let source = project
        .join("LuaParser")
        .join("src")
        .join("File")
        .join("LuaSource.cpp");
    if !source.is_file() {
        eprintln!("skipped: the project has no {source:?}");
        return;
    }
    let text = std::fs::read_to_string(&source).expect("the file reads");
    let uri = format!("file:///{}", source.display().to_string().replace('\\', "/"));

    let mut server = Server::start(&project);
    let answer = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": format!("file:///{}", project.display().to_string().replace('\\', "/")),
            "capabilities": {
                "workspace": { "configuration": true },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    assert!(answer["result"].is_object(), "initialize failed: {answer}");
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": text } }),
    );

    // **Two minutes without asking for anything.** An idle session logs no slice at all — that is the property the
    // `wait_for_work` fix bought — so a pump that keeps logging is a pump that keeps finding work it cannot do.
    let mut counted_at_60 = 0usize;
    let mut counted_at_90 = 0usize;
    for round in 0..18 {
        std::thread::sleep(Duration::from_secs(5));
        match round {
            11 => counted_at_60 = server.slice_lines().len(),
            17 => counted_at_90 = server.slice_lines().len(),
            _ => {}
        }
    }

    let slices = server.slice_lines();
    let warnings = server.warning_lines();
    println!("\n--- the last slice the pump reported ---");
    for line in slices.iter().rev().take(3) {
        println!("{line}");
    }
    println!(
        "\n--- {} slice line(s) in total, {} between 60 s and 90 s, {} breaker warning(s) ---",
        slices.len(),
        counted_at_90 - counted_at_60,
        warnings.len()
    );
    for line in warnings.iter().take(5) {
        println!("{line}");
    }

    let _ = server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);

    assert!(
        counted_at_90 - counted_at_60 <= 5,
        "the pump is still slicing on an idle session: {counted_at_90} lines at 90 s against {counted_at_60} at 60 s"
    );
    assert!(
        !slices
            .last()
            .is_some_and(|line| line.contains("to cook,") && line.contains(", 0 left")),
        "the pump ended on work it keeps claiming: {:?}",
        slices.last()
    );
    assert!(
        warnings.is_empty(),
        "the pump admitted it could not pay a count it was holding: {warnings:?}"
    );
}
