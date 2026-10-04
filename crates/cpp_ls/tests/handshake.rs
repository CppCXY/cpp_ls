//! An end-to-end test over the server's own stdio: the handshake, one edit, and the answers this server gives.
//!
//! Everything below `server/` is unit-tested — the analysis engine has a thousand tests of its own — and none of
//! that proves the *shell* works: that a client can say `initialize`, get capabilities back, open a file, and
//! receive a diagnostic, a definition and a hover over the wire. That is what this test is for, and it is the only
//! place in the crate that speaks the protocol as a **client** does.
//!
//! ```text
//! initialize ──▶ capabilities          definition ──▶ the declaration's file and range
//! initialized ─▶ the workspace opens   hover ──────▶ markdown about the name under the cursor
//! didOpen ────▶ publishDiagnostics
//! ```
//!
//! # Why the definition is asked for more than once
//!
//! Because the index is **lazy on purpose**: `didOpen` queues the file, the background loop reads it and follows
//! its includes, and a query that arrives in between legitimately answers "not declared here yet" — the analysis
//! says so rather than guessing (`Session::pending` is how a caller tells that apart from a real absence). A test
//! that asked once would be asserting about the scheduler, so this one asks until it gets an answer, with a
//! deadline. A wrong answer is a wrong answer whenever it arrives; a slow one is not a failure.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long any single expectation may take. Generous: a debug build parses a project's first file, and CI is slow.
/// How long a test waits for an answer before calling it a failure.
///
/// Thirty seconds is generous for a project of fixture files and it is **not** generous for one that pulls in the
/// standard library: `#include <string>` is a closure of thousands of MSVC headers, and the first request about a
/// name inside it waits for the index to read them. A test whose subject *is* that closure therefore waits longer —
/// see [`STD_LIBRARY_TIMEOUT`] — rather than the whole file being given a longer deadline, because a fixture that
/// hangs should still fail in half a minute.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The deadline for a test that makes the analysis read a standard-library header, which is real work.
const STD_LIBRARY_TIMEOUT: Duration = Duration::from_secs(180);

const WIDGET_H: &str = "struct Widget { int size; };\n";

/// A file with two things to find and one thing to report: a use of `Widget`, a member access, and a class whose
/// brace is never closed.
///
/// The unclosed `{` is deliberate and it is the *only* error in the file: the parser reports `expected ;` at the
/// end of the last line, which gives the diagnostic assertion a line and a column to check rather than a count.
/// (`int g() { return 1 }` — a missing semicolon before a closing brace — is **not** reported by this parser; that
/// is a real gap in its error reporting and it is recorded.)
const MAIN_CPP: &str = "#include \"widget.h\"\n\nvoid f() {\n    Widget w;\n    w.size = 1;\n}\nstruct Unclosed { int x;\n";

#[test]
fn a_client_can_open_a_file_and_get_diagnostics_a_definition_and_a_hover() {
    let project = Project::new("handshake");
    project.write("widget.h", WIDGET_H);
    project.write("main.cpp", MAIN_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    // --- initialize -------------------------------------------------------------------------------
    let initialized = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                // No `textDocument.diagnostic`: this client wants diagnostics **pushed**, which is the half of the
                // diagnostic design that has no request in it.
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );

    let capabilities = &initialized["result"]["capabilities"];
    assert_eq!(capabilities["definitionProvider"], json!(true));
    assert_eq!(capabilities["hoverProvider"], json!(true));
    assert_eq!(
        capabilities["diagnosticProvider"]["workspaceDiagnostics"],
        json!(false),
        "a file's parse errors are the whole answer, so there is nothing a workspace request would add"
    );
    assert!(
        capabilities["textDocumentSync"].is_object(),
        "the document lifecycle is what every other capability needs: {capabilities}"
    );

    server.notify("initialized", json!({}));

    // --- one open document ------------------------------------------------------------------------
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    let diagnostics = server.wait_for(|message| {
        message["method"] == json!("textDocument/publishDiagnostics")
            && message["params"]["uri"] == json!(main_uri)
    });
    let reported = diagnostics["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    assert!(
        !reported.is_empty(),
        "the last class is never closed, so a parse error is expected: {diagnostics}"
    );
    assert_eq!(
        reported[0]["severity"],
        json!(1),
        "a parse error is an error, not a warning"
    );
    assert_eq!(
        reported[0]["range"]["start"]["line"],
        json!(6),
        "the error is on the last line: {diagnostics}"
    );
    assert!(
        reported[0]["message"]
            .as_str()
            .is_some_and(|message| message.contains(';')),
        "and it is the parser's own message about the missing `;`: {diagnostics}"
    );

    // --- definition: the member declared in the included header ------------------------------------
    let location = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 4, "character": 6 },  // `w.size`
            },
        })
    });

    let found_uri = location["result"]["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("a definition was expected, got {location}"));
    assert!(
        found_uri.ends_with("widget.h"),
        "`size` is declared in the header: {location}"
    );
    assert_eq!(
        location["result"]["range"]["start"]["line"],
        json!(0),
        "and the range is the name in that file, not the top of it: {location}"
    );
    assert_eq!(location["result"]["range"]["start"]["character"], json!(20));

    // --- definition on the `#include` itself: the header it names -----------------------------------
    // The fourth answer a jump can give, and the one that used to be missing entirely: `#include "widget.h"`
    // **declares nothing**, so the scope walk, the index by name and the macro table all answer "nothing found"
    // for a line whose whole purpose is to name a file. The cursor is inside the spelling (character 12 is the `d`
    // of `widget.h`), and the range that comes back is the file's first line rather than a point at its start —
    // an editor *selects* a scalar location, and a zero-width range would select nothing.
    let header = server.ask_until(150, |id| {
        json!({
            "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 0, "character": 12 },  // inside `"widget.h"`
            },
        })
    });

    let header_uri = header["result"]["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("a header was expected, got {header}"));
    assert!(
        header_uri.ends_with("widget.h"),
        "the include names widget.h: {header}"
    );
    assert_eq!(
        header["result"]["range"]["start"],
        json!({ "line": 0, "character": 0 }),
        "from the top of the header: {header}"
    );
    assert_eq!(
        header["result"]["range"]["end"],
        json!({ "line": 1, "character": 0 }),
        "to the start of the second line, which is where the header's first line ends: {header}"
    );

    // --- hover on the same name: **which file the search found** ------------------------------------
    // The other half of a jump whose target the reader cannot see: the line they wrote names a *spelling*, and a
    // search turns it into one of the thousands of files on this machine's include path. The popup says which.
    let about_the_header = server.ask_until(170, |id| {
        json!({
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 0, "character": 12 },
            },
        })
    });
    let markdown = about_the_header["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a hover was expected, got {about_the_header}"));
    assert!(
        markdown.contains("#include \"widget.h\""),
        "the line as the file writes it: {markdown}"
    );
    // The path is compared **case-insensitively**, because the session resolves it through the VFS and the
    // temporary directory the fixture lives in is spelled in lower case there on Windows. What is being asserted
    // is that the popup carries the resolved *file*, not the spelling the reader wrote.
    let resolved = markdown.to_lowercase();
    assert!(
        resolved.contains("widget.h") && resolved.contains("cppls-handshake-tests"),
        "and where it resolved to: {markdown}"
    );

    // The `#` is part of the directive rather than of the name, and the analysis answers `null` there — which is
    // the same "nothing to go to" it gave before this round for the *whole* line. Asserted so that the boundary is
    // pinned: a client that sends a ctrl-click on the `#` must not be sent to a file the analysis guessed at.
    let on_the_hash = server.request(
        160,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 0, "character": 0 },
        }),
    );
    assert_eq!(
        on_the_hash["result"],
        Value::Null,
        "the `#` is not the header's name: {on_the_hash}"
    );

    // --- hover: the type of a local, then the declaration of the member ---------------------------
    // The two positions below are `Widget w;` on line 3 and the `size` of `w.size` on line 4.
    let hover = server.ask_until(200, |id| {
        json!({
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 3, "character": 6 },  // `Widget w;`
            },
        })
    });
    let markdown = hover["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a hover was expected, got {hover}"));
    assert!(
        markdown.contains("struct Widget"),
        "the declaration as the file writes it: {markdown}"
    );
    assert!(markdown.contains("widget.h"), "{markdown}");

    let member = server.ask_until(300, |id| {
        json!({
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 4, "character": 7 },  // `w.size`
            },
        })
    });
    let markdown = member["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a hover for the member was expected, got {member}"));
    assert!(markdown.contains("size"), "{markdown}");
    assert!(markdown.contains("int"), "its type is shown: {markdown}");

    // --- a hover where there is nothing to say answers null, rather than an empty popup ------------
    let nothing = server.request(
        900,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 1, "character": 0 },  // the blank line
        }),
    );
    assert_eq!(
        nothing["result"],
        Value::Null,
        "there is nothing at the position, and that is what the client is told"
    );

    // --- shutdown ---------------------------------------------------------------------------------
    let shutdown = server.request(901, "shutdown", Value::Null);
    assert_eq!(shutdown["result"], Value::Null);
    server.notify("exit", Value::Null);
    assert!(
        server.wait_for_exit(),
        "the server exits when the client says exit"
    );
}

/// The project's own `.cppls.toml` is read by the engine and honoured by the **server**.
///
/// The engine's tests prove the file is parsed; what they cannot prove is that a setting in it reaches the layer
/// that acts on it — the one file is parsed once, in the engine, and the server reads the same struct
/// (`Session::project_config`). Hover is the setting with an observable answer: with `hover.enable = false` the
/// same request that answers with a declaration in the test above answers `null`, and nothing else changes.
#[test]
fn a_project_configuration_reaches_the_servers_behaviour() {
    let project = Project::new("configured");
    project.write("widget.h", WIDGET_H);
    project.write("main.cpp", MAIN_CPP);
    project.write(".cppls.toml", "[hover]\nenable = false\n");

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    // The same position the test above gets a declaration from — `Widget w;` on line 3.
    let hover = server.request(
        2,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 3, "character": 6 },
        }),
    );

    assert_eq!(
        hover["result"],
        Value::Null,
        "the project turned the popup off, and the server read the project's file to find that out: {hover}"
    );

    let shutdown = server.request(3, "shutdown", Value::Null);
    assert_eq!(shutdown["result"], Value::Null);
    server.notify("exit", Value::Null);
    assert!(server.wait_for_exit());
}

/// A project directory removed when the test ends, including when it fails.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir()
            .join("cppls-handshake-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the fixture directory is created");

        Project { root }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.root.join(name), text).expect("the fixture writes");
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn uri_of(path: &Path) -> String {
    // A client's own conversion, deliberately not the server's: a test that reused `util::uri` would agree with a
    // bug in it. Only what a temp path can hold is escaped (a space, which `%TEMP%` on Windows may have).
    let text = path.to_string_lossy().replace('\\', "/").replace(' ', "%20");
    format!("file:///{}", text.trim_start_matches('/'))
}

/// **Where a cursor at the end of a needle is**, as the protocol's line and character.
///
/// The offset *past* the needle — `full.` puts the cursor after the operator, which is where a reader who has just
/// typed it is. A test that states a position by hand states it wrong sooner or later, and the failure is silent:
/// the request still gets an answer, about a position nobody is looking at. Counting the line(s) and characters of
/// the fixture is a line of code and cannot drift away from the text it is about.
fn position_of(source: &str, needle: &str) -> (u32, u32) {
    let at = source
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} is not in the fixture"))
        + needle.len();

    let before = &source[..at];
    let line = before.matches('\n').count();
    let character = before
        .rsplit('\n')
        .next()
        .expect("rsplit always yields one piece")
        .chars()
        .count();

    (line as u32, character as u32)
}

/// The server, as a client sees it: a process, a pipe, and the messages it sent.
struct Server {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    /// What the server logged, kept so that a failure says *why* rather than only that.
    log: Arc<Mutex<String>>,
}

impl Server {
    fn start(root: &Path) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg("none")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the server starts");

        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let (tx, messages) = channel();
        std::thread::spawn(move || read_messages(stdout, tx));

        // The log goes to stderr; if nobody reads it a full pipe would block the server mid-test, and if nobody
        // *keeps* it a failing test says only that the server did not answer.
        let log = Arc::new(Mutex::new(String::new()));
        let sink = log.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                sink.lock().expect("the log is not poisoned").push_str(&line);
                line.clear();
            }
        });

        Server {
            child,
            stdin,
            messages,
            log,
        }
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_string(&message).expect("the message serializes");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("the message is written");
        self.stdin.flush().expect("the message is flushed");
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Send a request and wait for its response, answering anything the server asks on the way.
    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        self.wait_for(|message| message["id"] == json!(id))
    }

    /// Send a request until its answer is not `null`, or give up.
    ///
    /// What the lazy index needs: a query that arrives before the file has been read answers "not yet", and the
    /// test's job is to assert the *answer*, not the schedule. Each attempt gets its own id, so a retry can never
    /// be confused with the answer to the attempt before it.
    fn ask_until(&mut self, first_id: i64, build: impl Fn(i64) -> Value) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        let mut id = first_id;

        loop {
            let request = build(id);
            let method = request["method"]
                .as_str()
                .expect("the closure builds a request")
                .to_string();
            let response = self.request(id, &method, request["params"].clone());

            if let Some(error) = response.get("error") {
                panic!("the server answered with an error: {error}");
            }

            if !response["result"].is_null() {
                return response;
            }

            if Instant::now() >= deadline {
                panic!("no answer within {TIMEOUT:?}, only `null`: {response}");
            }

            id += 1;
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// [`Server::ask_until`] for a request whose answer can arrive **worth having but not yet complete**: keeps
    /// asking until the closure accepts one.
    ///
    /// A completion is the request that needs this. Its answer before the project has been read is an **empty
    /// list** — not `null`, which is why [`Server::ask_until`] would take it — and the protocol's own answer to
    /// that is `isIncomplete: true`, which tells the client to ask again as the user types. This is that loop, so
    /// a test asserts what a well-behaved client would eventually see rather than what a fast one happens to get.
    fn ask_until_it(
        &mut self,
        first_id: i64,
        build: impl Fn(i64) -> Value,
        accept: impl Fn(&Value) -> bool,
    ) -> Value {
        self.ask_until_it_within(first_id, TIMEOUT, build, accept)
    }

    /// [`Server::ask_until_it`] with the caller's own deadline, for a test whose subject is genuinely slow.
    fn ask_until_it_within(
        &mut self,
        first_id: i64,
        within: Duration,
        build: impl Fn(i64) -> Value,
        accept: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = Instant::now() + within;
        let mut id = first_id;

        loop {
            let request = build(id);
            let method = request["method"]
                .as_str()
                .expect("the closure builds a request")
                .to_string();
            let response = self.request(id, &method, request["params"].clone());

            if let Some(error) = response.get("error") {
                panic!("the server answered with an error: {error}");
            }

            if accept(&response) {
                return response;
            }

            if Instant::now() >= deadline {
                panic!("no answer the test accepts within {within:?}; the last one was: {response}");
            }

            id += 1;
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait for a message the closure accepts, answering the server's own requests while waiting.
    fn wait_for(&mut self, accept: impl Fn(&Value) -> bool) -> Value {        let deadline = Instant::now() + TIMEOUT;

        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("the server did not answer within {TIMEOUT:?}"));

            let message = match self.messages.recv_timeout(remaining) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => panic!("the server did not answer within {TIMEOUT:?}"),
                Err(RecvTimeoutError::Disconnected) => panic!("the server closed its stdout"),
            };

            // A request *from* the server: `workspace/configuration`, `client/registerCapability`, a progress
            // token. Answering is part of being a client, and a test that did not would leave the server waiting.
            if message.get("method").is_some() && message.get("id").is_some() {
                self.answer(&message);
                continue;
            }

            if accept(&message) {
                return message;
            }
        }
    }

    /// Answer a server→client request the way a client would.
    fn answer(&mut self, request: &Value) {
        let id = request["id"].clone();
        let method = request["method"].as_str().unwrap_or_default();
        let result = match method {
            // One `null` per requested item: "the user has configured nothing", which is what an unconfigured
            // client says and what the server's own defaults are for.
            "workspace/configuration" => json!([Value::Null]),
            _ => Value::Null,
        };

        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn wait_for_exit(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);

        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => return false,
            }
        }

        false
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Printed on the way out, which includes the way out a failing assertion takes: the server's log is the
        // first thing anyone debugging this test wants, and a panicking test would otherwise lose it.
        let log = self.log.lock().expect("the log is not poisoned");
        if !log.trim().is_empty() {
            eprintln!("--- the server logged:\n{log}");
        }
        drop(log);

        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read LSP frames off the server's stdout and hand each message to the test.
fn read_messages(stdout: impl Read, tx: std::sync::mpsc::Sender<Value>) {
    let mut reader = BufReader::new(stdout);

    loop {
        let mut length = None;
        loop {
            let mut header = String::new();
            match reader.read_line(&mut header) {
                Ok(0) => return,
                Ok(_) => {}
                Err(_) => return,
            }

            let header = header.trim_end();
            if header.is_empty() {
                break;
            }

            if let Some(value) = header.strip_prefix("Content-Length: ") {
                length = value.parse::<usize>().ok();
            }
        }

        let Some(length) = length else {
            return;
        };

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

/// The macro-declared type: the declaration is inside `DECLARE_HANDLE`'s replacement list, and `HWND h;` is a use
/// of the name it makes.
const HANDLE_H: &str = "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                        typedef struct name##__ *name\n";
const API_H: &str = "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n";
const API_CPP: &str = "#include \"api.h\"\nHWND h;\n";

/// **A declaration a macro makes is a declaration this server answers for.**
///
/// `DECLARE_HANDLE(HWND)` declares `HWND__` and `HWND` to a compiler and *nothing* to a reader of `api.cpp`: the
/// declaration is inside the replacement list, in a header the file includes. The index has it only because the
/// session reads the file a **second way** — cooked, through the translation unit that brings the macro in
/// (`Session::cook`, run by the indexing loop when its queue drains), with every range mapped back into the file.
///
/// Over the wire the difference is one jump that works, and the assertion is deliberately about the *range* rather
/// than only about the file: a definition of `HWND` that the raw reading had found would point at the text
/// `DECLARE_HANDLE(HWND)` writes — the argument of the invocation — while the cooked reading reports the
/// declaration at the **invocation** it came out of, which is the place a reader can act on.
#[test]
fn a_declaration_a_macro_makes_is_found_over_the_wire() {
    let project = Project::new("macro-declared");
    project.write("handle.h", HANDLE_H);
    project.write("api.h", API_H);
    project.write("main.cpp", API_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": API_CPP }
        }),
    );

    // `HWND h;` is the second line, and the name starts it.
    let location = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 1, "character": 1 },
            },
        })
    });

    let found = &location["result"];
    let found_uri = found["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("a definition was expected, got {location}"));
    assert!(
        found_uri.ends_with("api.h"),
        "the declaration the macro made belongs to the file that invoked it, which nobody opened: {location}"
    );
    assert_eq!(
        found["range"]["start"]["line"],
        json!(1),
        "and it is reported at the invocation: {location}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// The file whose *text* has an error in a branch the preprocessor removes: `OFF` is 0, so the unclosed class never
/// reaches a compiler — while the bytes are right there in the buffer.
const BRANCH_CPP: &str = "#include \"cfg.h\"\n#if OFF\nstruct Unclosed { int x;\n#endif\nint ok;\n";
const CFG_H: &str = "#define OFF 0\n";

/// **A branch nobody takes is published as clean.**
///
/// The two readings disagree about this file, and that disagreement is the reason the diagnostics channel reads the
/// cooked one: the raw reading reports the unclosed class — correctly, the *text* says so — and a compiler never
/// sees that branch. A server that published the raw answer would put a red squiggle on code that is not compiled,
/// which is the false positive the whole second reading exists to remove.
///
/// The assertion is about the **last** thing the client is told, and that is deliberate: the answer for an open file
/// can be published before the file has been cooked (a cooked reading needs its translation unit read), so the
/// client may first hear the raw error and then, once the indexing queue drains, hear that the file is clean. A
/// server that only published on the change would leave the first answer on screen.
#[test]
fn a_branch_nobody_takes_is_published_as_clean_once_the_file_is_cooked() {
    let project = Project::new("cooked-diagnostics");
    project.write("cfg.h", CFG_H);
    project.write("main.cpp", BRANCH_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                // No `textDocument.diagnostic`: this client wants diagnostics **pushed**, which is the half that has
                // no request in it — and the half a re-publish after the drain has to work for.
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": BRANCH_CPP }
        }),
    );

    let clean = server.wait_for(|message| {
        message["method"] == json!("textDocument/publishDiagnostics")
            && message["params"]["uri"] == json!(main_uri)
            && message["params"]["diagnostics"]
                .as_array()
                .is_some_and(|diagnostics| diagnostics.is_empty())
    });
    assert!(
        clean["params"]["diagnostics"]
            .as_array()
            .is_some_and(|diagnostics| diagnostics.is_empty()),
        "the branch is not compiled, so there is nothing to report: {clean}"
    );

    // **The falsification lives one layer down**, where the same fixture is read both ways:
    // `cpp_code_analysis::session::tests::a_branch_nobody_takes_reports_nothing_once_the_file_is_cooked` asserts
    // that this file's *raw* reading reports exactly that error and its cooked reading reports nothing. Without it
    // this test would also pass on a file that simply has no error in it.

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// The namespace is opened by a macro in a header the open file includes. **The raw reading handles this one** —
/// an indexer given the closure's macro bodies reads `BEGIN_NS` as `namespace one {`, which is what the shape rules
/// exist for — so this fixture is about the member question rather than about the second reading.
const NAMESPACE_H: &str = "#define BEGIN_NS namespace one {\n#define END_NS }\n";
const NAMESPACED_WIDGET_H: &str = "#include \"namespace.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n";

/// The fixture that **needs** the second reading: a macro that declares something. `api.h`'s own text has a call
/// where the struct and the typedef are, and no shape rule invents a declaration out of a call.
const HWND_CPP: &str = "#include \"api.h\"\nvoid f() {\n    HWND\n}\n";

/// The cursor is after a `.` — the question that needs a **type**, made interesting by the spelling: `w`'s type is
/// written `one::Widget`.
const MEMBER_CPP: &str = "#include \"widget.h\"\nvoid f() {\n    one::Widget w;\n    w.\n}\n";

/// **A name only the cooked reading declares is offered.**
///
/// `DECLARE_HANDLE(HWND)` declares `HWND__` and `HWND` to a compiler and *nothing* to a reader of `api.h`: the
/// declaration is the replacement list. So a completion in `main.cpp` that offers `HWND__` is one this server can
/// give only because the file it is declared in was read a **second** way — and the falsification (excluding the
/// cooked declarations from the index) turns this test red, which is what makes it evidence rather than a fixture
/// that happens to work.
///
/// The **edit** is asserted as well as the label: the cursor is inside a half-written name, so what comes back is
/// the range of that name to replace, not a point to insert at.
#[test]
fn a_name_only_the_cooked_reading_declares_is_offered_over_the_wire() {
    let project = Project::new("completion-macro-declared");
    project.write("handle.h", HANDLE_H);
    project.write("api.h", API_H);
    project.write("main.cpp", HWND_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let capabilities = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    assert!(
        capabilities["result"]["capabilities"]["completionProvider"].is_object(),
        "the client is told the server completes: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": HWND_CPP }
        }),
    );

    // Line 2 is `    HWND` — the cursor is at its end. Asked again until the list holds `HWND__`, because a
    // completion answered before the project has been read is an empty list that says `isIncomplete`.
    let answer = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": 2, "character": 8 },
                },
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("HWND__")))
        },
    );

    let items = answer["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));
    assert!(
        items.iter().any(|item| item["label"] == json!("HWND")),
        "and the typedef the same macro made is offered beside it: {answer}"
    );

    let generated = items
        .iter()
        .find(|item| item["label"] == json!("HWND__"))
        .expect("the closure accepted this");
    assert_eq!(generated["kind"], json!(7), "a class: {generated}");
    assert_eq!(
        generated["textEdit"]["range"]["start"],
        json!({ "line": 2, "character": 4 }),
        "the edit replaces the half-written name from where it starts: {generated}"
    );
    assert_eq!(
        generated["textEdit"]["range"]["end"],
        json!({ "line": 2, "character": 8 }),
        "…to where the cursor is: {generated}"
    );
    assert_eq!(generated["textEdit"]["newText"], json!("HWND__"), "{generated}");

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// The `std::string` fixture: a standard-library type from a header, and a `.` where the member list belongs.
///
/// Written the way a reader does — `#include <string>`, a declaration, then the operator — so that the test covers
/// the chain and not the completion layer alone: the header has to be **found** on the search path, its declarations
/// have to be **indexed**, and `std::string` has to resolve before there is a member list to collapse.
const STD_STRING_CPP: &str = "\
#include <string>
int main() {
    std::string full;
    full.
}
";

/// **The other query, over the wire: after a `.`, what the object's type has.**
///
/// `w.` asks for the members of `w`'s type, and the type is written `one::Widget` — a qualified spelling the lookup
/// has to follow before the class body can be read at all. The dependency on the second reading here is **not** the
/// point of the test (the raw reading resolves `one::Widget` too, since a namespace-opening macro is a shape the
/// rules already handle — see the fixture's note); what it covers is the dispatch itself: a cursor after a dot goes
/// to the member query, its answer arrives as members with the range a client replaces, and the file-scope names
/// are not offered beside them.
#[test]
fn a_members_members_are_offered_over_the_wire() {
    let project = Project::new("completion-member");
    project.write("namespace.h", NAMESPACE_H);
    project.write("widget.h", NAMESPACED_WIDGET_H);
    project.write("main.cpp", MEMBER_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MEMBER_CPP }
        }),
    );

    // Line 3 is `    w.` — the cursor is just past the dot; asked again until the list holds `size`, for the reason
    // the qualified test gives.
    let answer = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": 3, "character": 6 },
                },
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("size")))
        },
    );

    let items = answer["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));
    let size = items
        .iter()
        .find(|item| item["label"] == json!("size"))
        .unwrap_or_else(|| panic!("`Widget` has a `size` member: {answer}"));
    assert_eq!(
        size["kind"],
        json!(5),
        "a **field** rather than a variable, because it is reached through an object — the completion layer \
         distinguishes the two positions, and a client draws them with different icons: {size}"
    );
    assert_eq!(
        size["detail"],
        json!("int"),
        "and the type the declaration was written with, which is what the item's own line is for: {size}"
    );
    assert_eq!(
        size["textEdit"]["range"]["start"],
        json!({ "line": 3, "character": 6 }),
        "the edit is an empty range just past the dot: {size}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A `.` on a `std::string` offers the standard library's members, once each.**
///
/// The case a user reported as "补全依然存在明显错误": the popup after `full.` was a list of file-scope names —
/// `printf`, `full`, `myName`, `sum`, `main` — plus keywords, which is what a client is shown when the member
/// question has no answer. Two separate faults can produce that shape, and this test pins both:
///
/// * **the members are found at all**, through `#include <string>` and the type's declaration in a header the
///   session had to index — the whole chain has to work for `std::string` to resolve;
/// * **each spelling appears once.** MSVC's `basic_string` declares `replace` eleven times and `insert` nine, and
///   the list used to carry every one of them: 119 rows of which 64 shared a label with another row. A list is
///   something a reader picks from, so eleven rows reading `replace` are one choice offered eleven times — and a
///   client that lays out 119 rows with duplicated labels is what "the popup looks broken" means.
///
/// It is an end-to-end test rather than a unit one because the failure was a *chain*: the fix could be right in the
/// completion layer and the answer still wrong if the index never resolved `<string>`.
///
/// # `#[ignore]`, and why it is not a disabled test
///
/// It is **too slow for the debug profile**: the fixture's one `#include <string>` is a closure of thousands of
/// MSVC headers, which the index reads before it can answer, and a debug build does that in minutes rather than in
/// the sixteen seconds a release build takes. The harness's own deadline is therefore unreachable in a debug run
/// — and a test that fails for a *build profile* teaches nothing about the code, which is the one thing a test
/// must not do.
///
/// It runs in release, and that is how it was written and verified:
///
/// ```text
/// cargo test --release -p cpp_ls --test handshake -- --ignored a_standard_library
/// ```
///
/// What covers the same ground in a debug run: `cpp_code_analysis/tests/completion.rs`'s
/// `a_member_position_offers_the_members_with_their_distinctions` (the member list and its kinds, from a fixture
/// with no standard library in it) and the probe, which is the instrument this was measured with.
#[test]
#[ignore = "needs the release profile: the standard library closure takes minutes to index in debug"]
fn a_standard_library_member_list_has_each_spelling_once() {
    let project = Project::new("completion-std-string");
    project.write("main.cpp", STD_STRING_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": STD_STRING_CPP }
        }),
    );

    // **The position of the operator, computed from the fixture** rather than counted by hand.
    //
    // This test was written with the position hard-coded twice and wrong twice, and the failure is invisible: an
    // off-by-one lands on the newline (still a name position, so the answer looks like "the members were not
    // found") or on the closing brace (still an answer about a position nobody is looking at). A test whose
    // subject is *where* the cursor is cannot afford to state the where by hand.
    let (line, character) = position_of(STD_STRING_CPP, "full.");
    assert_eq!(
        (line, character),
        (3, 9),
        "the operator ends the fourth line and the cursor is just past it — written out here once, so that a \
         fixture edited above this line fails loudly instead of silently asking about the wrong place"
    );

    // Asked until the list holds `append`, because the standard library's closure is thousands of files and the
    // index is still reading when the first request arrives.
    let answer = server.ask_until_it_within(
        100,
        STD_LIBRARY_TIMEOUT,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": line, "character": character },
                },
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("append")))
        },
    );

    let items = answer["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));

    let labels: Vec<&str> = items
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();

    assert!(
        labels.contains(&"append") && labels.contains(&"size") && labels.contains(&"substr"),
        "the members of the standard library's string: {labels:?}"
    );
    assert!(
        !labels.contains(&"printf") && !labels.contains(&"main"),
        "and not the names in scope, which cannot follow the operator: {labels:?}"
    );

    let mut unique = labels.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        labels.len(),
        "every spelling appears once — {} of {} rows are repeats: {:?}",
        labels.len() - unique.len(),
        labels.len(),
        labels
            .iter()
            .filter(|label| labels.iter().filter(|other| other == label).count() > 1)
            .collect::<Vec<_>>()
    );

    // Each row carries an **edit that replaces what was typed**, so that the choice is inserted rather than
    // appended to the half-written name — the same contract the `Widget` case asserts, on a type that came from a
    // header rather than from the buffer.
    let append = items
        .iter()
        .find(|item| item["label"] == json!("append"))
        .expect("asserted above");
    assert_eq!(
        append["textEdit"]["range"]["start"],
        json!({ "line": line, "character": character }),
        "just past the dot: {append}"
    );
    assert_eq!(append["textEdit"]["newText"], json!("append"), "{append}");

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// The type a `.` is asked about is declared in a **header**, and the test's change rewrites that header: the only
/// way the new member can be in the answer is if the header's *summary* was rebuilt, which is what
/// [`cpp_code_analysis::Session::catch_up`] does before the query.
const FRESH_TYPE_H: &str = "struct Widget { int size; };\n";

/// The uses of it, and a `.` at the end of the last line.
const FRESH_TYPE_CPP: &str = "\
#include \"widget.h\"
int main() {
    Widget w;
    w.
}
";

/// **An edit to an included header is visible to the completion that follows it.**
///
/// This is the shape a user reported and diagnosed as "文件的 didchange 尚未处理完毕": an edit reaches the analysis in
/// two steps that are not the same step. `didChange` replaces the **text** immediately — the scope tree is rebuilt
/// from it on every parse — and *drops the file's summary*, which is rebuilt later, when the index pump reaches that
/// file. A request answered inside that window sees the new text with the **old facts**.
///
/// What it covers is the *atomicity of the pair*: the change and the request arrive with nothing in between, and the
/// answer is about the changed text. What it does **not** discriminate is the fix itself — the class here is in the
/// buffer's own scope tree (a view parses the text it holds, includes and all), so the members are answerable from
/// the tree whether or not the summary was rebuilt. The falsification for
/// [`cpp_code_analysis::Session::catch_up`] is at the session level, where the staleness is visible and measured:
/// `cpp_code_analysis/tests/completion.rs::a_completion_after_an_edit_is_about_the_file_the_edit_wrote`, which
/// asserts that the stale reading is *wrong* and that `catch_up` is what makes it right.
///
/// # Why the change is to a header rather than to `main.cpp`
///
/// Because a change to the file being edited moves the cursor, and the request would then be about a position that
/// did not exist before. Rewriting `widget.h` leaves `w.` where it was.
#[test]
fn a_completion_sees_a_change_to_an_included_header() {
    let project = Project::new("completion-after-header-change");
    project.write("widget.h", FRESH_TYPE_H);
    project.write("main.cpp", FRESH_TYPE_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    let header_uri = uri_of(&project.root().join("widget.h"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": FRESH_TYPE_CPP }
        }),
    );
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": header_uri, "languageId": "cpp", "version": 1, "text": FRESH_TYPE_H }
        }),
    );

    // Wait until the list is the members of the type **as it is now**, so that the assertion below is about the
    // change and not about the index being cold.
    let (line, character) = position_of(FRESH_TYPE_CPP, "w.");
    server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": line, "character": character },
                },
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("size")))
        },
    );

    // **The change**, and then the request with nothing in between.
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": header_uri, "version": 2 },
            "contentChanges": [{ "text": "struct Widget { int size; int added_by_the_edit; };\n" }],
        }),
    );

    // **Asked once, with nothing in between**: the request a client sends in the same breath as the keystroke, and
    // the one that used to be answered from the header's *old* summary — with no members at all, or with the names
    // in scope instead.
    let answer = server.request(
        200,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": line, "character": character },
        }),
    );

    let items = answer["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));

    let labels: Vec<&str> = items
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();

    assert!(
        labels.contains(&"added_by_the_edit"),
        "the member the edit added, which only a rebuilt summary can express — asked once, right after the change, \
         with no retry to hide a stale answer: {labels:?}"
    );
    assert!(
        !labels.contains(&"main"),
        "and not the names in scope, which is what a stale summary produces: {labels:?}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A `.` at the end of a body, on a `std::string` variable, offers the members.**
///
/// The third report of the same symptom — "No suggestions" under `full2.` — and the one that shows how narrow the
/// failure is: the same file answered correctly when the line below the cursor was something else, and answered
/// nothing when the operator was the **last line of the body**, with `}` after it. That is the state a reader is in
/// constantly (type the line, press the key, and the closing brace is what follows), so it is the state this test
/// holds fixed.
///
/// The answers that are *not* acceptable here, and why each of them used to happen:
///
/// * an **empty list** — the member query could not work out the type of the object;
/// * the **names in scope** (`myName`, `full2`, `main`, the keywords) — which is what a declined member query falls
///   through to, and a list of things that cannot follow a `.`;
/// * a **different list at the two positions a caret after the operator can be reported at** — measured on a live
///   server: the request one column early was answered from the tree, found the name `full2`, and offered the names
///   in scope.
///
/// **Release only**, for the reason `a_standard_library_member_list_has_each_spelling_once` gives: the answer needs
/// `std::string`, and *this project has no sources of its own for the pump to seed from* — the client's buffer is
/// the only file — so the assertion can only be met once the pump has read the 156 files of MSVC's `<iostream>`/
/// `<string>` closure. Measured: **23.8 s in release**, past the debug profile's deadline. The debug-equivalent is
/// `cpp_code_analysis/tests/completion.rs`'s
/// `a_member_access_on_the_last_line_of_a_body_offers_its_members_at_either_caret`, which holds the same shape with
/// a header of the project's own.
///
/// ```text
/// cargo test --release -p cpp_ls --test handshake -- --ignored a_member_access_on_the_last_line
/// ```
#[test]
#[ignore = "needs the release profile: the answer comes from the standard library's include closure (156 files)"]
fn a_member_access_on_the_last_line_of_a_body_offers_the_members() {
    let project = Project::new("completion-last-line");
    project.write("main.cpp", LAST_LINE_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": LAST_LINE_CPP }
        }),
    );

    // **The two characters a caret after a typed `.` can be reported at.**
    //
    // A caret is drawn *between* two characters, and `position_of` returns the offset **past** `full2.` — which is
    // where the caret sits once the operator has been typed. The other position a reader can mean is one before it:
    // the newline's own column (`full2.|`). Both mean "after the operator" to a reader and both must answer the same
    // list, which is exactly the pair a live server disagreed about — a request one column early was answered from
    // the tree, found the name `full2` and offered the names in scope instead of the members.
    //
    // The characters **at or before** the operator's own column are deliberately not asked about here: there the
    // question is about the name `full2`, which is a different question with a different (and correct) answer.
    // Getting this arithmetic wrong is how three separate reports of "the completion is wrong here" turned out to be
    // about a cursor one column away from the operator — see `position_of` and the note on
    // `log_a_member_that_produced_no_members`.
    let (line, ends_at) = position_of(LAST_LINE_CPP, "full2.");

    for character in [ends_at, ends_at - 1] {
        let answer = server.ask_until_it_within(
            100,
            STD_LIBRARY_TIMEOUT,
            |id| {
                json!({
                    "id": id,
                    "method": "textDocument/completion",
                    "params": {
                        "textDocument": { "uri": main_uri },
                        "position": { "line": line, "character": character },
                    },
                })
            },
            |response| {
                response["result"]["items"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|item| item["label"] == json!("append")))
            },
        );

        let items = answer["result"]["items"]
            .as_array()
            .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));
        let labels: Vec<&str> = items
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect();

        assert!(
            labels.contains(&"append") && labels.contains(&"size"),
            "the members of the standard library's string, at line {line} character {character}: {labels:?}"
        );
        assert!(
            !labels.contains(&"full2") && !labels.contains(&"myName"),
            "and not the names in scope, at line {line} character {character}: {labels:?}"
        );
    }

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// The fixture for the last-line case: the operator is the **final line of the body**, with `}` after it.
///
/// Written to match the report it came from, including the parts that look incidental and are not: a member
/// function with a body (so the class has a non-trivial parse), a second variable of a user-defined type (so the
/// scope is not empty), and the standard library included (so the type's members come from a header).
const LAST_LINE_CPP: &str = "\
#include <iostream>
#include <string>
struct Name {
    std::string firstName;
    std::string lastName;
    std::string getFullName() const { return firstName + lastName; }
};
int main() {
    Name myName;
    std::string full2;
    full2.
}
";

/// **A completion asked for right after a change sees the change.**
///
/// A client sends `didChange` and then, without waiting for anything, asks for a completion at a position that only
/// exists in the **new** text — which is what typing is: the request follows the keystroke that caused it. The
/// server queues notifications so that its message loop never waits for a parse, and the queue must not be visible
/// to a request: while it was, the analysis still held older text, the cursor was past the end of that line, no
/// node was there, and the answer was `null` — a completion that simply does not appear.
///
/// The burst is what makes it a test rather than a coin toss: with one change the queue may well be applied before
/// the request is served (measured: the same assertion failed on 1 run in 4), so the fixture sends a **stream** of
/// changes the way a client that is typing, pasting or formatting does, and only the last of them has the `w.` the
/// request is about. Any staleness at all therefore fails: an older text has that line blank.
#[test]
fn a_completion_right_after_a_change_sees_the_change() {
    /// Long enough that applying one change is real work — the queue has to be behind for the race to exist.
    const LINES: usize = 1200;
    /// The burst. Each round is a different text, so each is a change a client could have sent.
    const ROUNDS: usize = 24;

    let text = |round: usize, dot: bool| {
        let mut text = String::from("struct Widget { int size; int weight; };\nvoid f() {\n    Widget w;\n");
        for index in 0..LINES {
            text.push_str(&format!("    w.size = {};\n", index + round));
        }
        text.push_str(if dot { "    w.\n}\n" } else { "    \n}\n" });
        text
    };

    let project = Project::new("completion-after-change");
    project.write("main.cpp", &text(0, false));

    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": text(0, false) }
        }),
    );

    // The burst, and then **immediately** the request: no sleep, no retry, no second request. A client that typed a
    // dot sends its edits and then asks, in this order.
    let sent = Instant::now();
    for round in 1..=ROUNDS {
        server.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": round + 1 },
                "contentChanges": [{ "text": text(round, round == ROUNDS) }],
            }),
        );
    }
    let burst_written = Instant::now();

    let answer = server.request(
        100,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 3 + LINES, "character": 6 },
        }),
    );
    let served = Instant::now();

    // Reported rather than asserted: how long the burst costs the request that follows it is a measurement, and it
    // moves with the machine. What *is* asserted is the count of applications, in `context::update_queue`'s own
    // tests — a burst of changes for one document is one piece of work, so this request waited for one.
    println!(
        "a {ROUNDS}-change burst sent in {:?}, then a completion answered {:?} later",
        burst_written.elapsed(),
        served - sent
    );

    let items = answer["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("`w.` in the changed text must complete, got {answer}"));

    assert!(
        items.iter().any(|item| item["label"] == json!("weight")),
        "and the list is the *changed* class's members — `weight` is the member the change added: {answer}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// A macro defined in one header and used in two files — the shape a rename has to get right, and the shape a
/// *reference list* has to walk: the definition, and the uses that can see it.
const LIMITS_H: &str = "#define MAX_ITEMS 64\n";
const ITEMS_H: &str = "#include \"limits.h\"\nint items[MAX_ITEMS];\n";
const ITEMS_CPP: &str = "#include \"api.h\"\nvoid f() {\n    int more[MAX_ITEMS];\n}\n";

/// **References and rename: the macro is renamed everywhere, the ordinary name is refused.**
///
/// Two features in one test because they are one question asked twice — "where is this name written" — and because
/// the second half is the one that must **not** happen: a rename of an ordinary name would have to decide, for every
/// candidate file, whether the name at that offset resolves to the same declaration, which needs each candidate's
/// scopes and therefore a parse per candidate. So the handler refuses (`null`), and this test pins the refusal next
/// to the success, because "renamed half of the uses" is the one outcome an editor must never produce.
///
/// And a macro's rename *is* exact: its name is a textual thing, one table in the preprocessor, replaced wherever it
/// appears — so every edit below is a place the analysis read as that macro, in the files that can see its
/// definition.
#[test]
fn a_macro_is_renamed_everywhere_and_an_ordinary_name_is_refused() {
    let project = Project::new("rename");
    project.write("limits.h", LIMITS_H);
    project.write("api.h", ITEMS_H);
    project.write("main.cpp", ITEMS_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let capabilities = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    let capabilities = &capabilities["result"]["capabilities"];
    assert_eq!(capabilities["referencesProvider"], json!(true), "{capabilities}");
    assert_eq!(
        capabilities["renameProvider"]["prepareProvider"],
        json!(true),
        "the rename box is offered only where a rename can happen: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": ITEMS_CPP }
        }),
    );

    // The use in `main.cpp` is on line 2 at columns 13..22 (`    int more[MAX_ITEMS];`).
    let at_the_use = json!({ "line": 2, "character": 16 });

    // --- references, with and without the definition ------------------------------------------------
    // `ask_until` is what a client does with the answer this server gives while the index still has work: `null`,
    // because a reference list assembled from a partially read index is a false claim rather than a smaller truth
    // (the protocol gives this response no `isIncomplete`, unlike a completion list). The loop below is therefore
    // part of the behaviour under test, not test scaffolding.
    let mut references = |include_declaration: bool| {
        server.ask_until(100, |id| {
            json!({
                "id": id,
                "method": "textDocument/references",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": at_the_use,
                    "context": { "includeDeclaration": include_declaration },
                },
            })
        })
    };

    let with_the_definition = references(true);
    let locations = with_the_definition["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a location list was expected, got {with_the_definition}"));
    assert_eq!(
        locations.len(),
        3,
        "the `#define` and both uses: {with_the_definition}"
    );
    let files: Vec<&str> = locations
        .iter()
        .filter_map(|location| location["uri"].as_str())
        .collect();
    assert!(
        files.iter().any(|file| file.ends_with("limits.h")),
        "including the file that defines it, which nobody opened: {with_the_definition}"
    );
    assert!(
        files.iter().any(|file| file.ends_with("api.h")),
        "and the header that uses it: {with_the_definition}"
    );

    let without = references(false);
    assert_eq!(
        without["result"].as_array().map(Vec::len),
        Some(2),
        "`includeDeclaration: false` leaves the `#define` out: {without}"
    );

    // --- the rename box, and the rename --------------------------------------------------------------
    let prepared = server.request(
        200,
        "textDocument/prepareRename",
        json!({ "textDocument": { "uri": main_uri }, "position": at_the_use }),
    );
    assert_eq!(
        prepared["result"]["start"],
        json!({ "line": 2, "character": 13 }),
        "the box starts around the name itself: {prepared}"
    );
    assert_eq!(prepared["result"]["end"], json!({ "line": 2, "character": 22 }), "{prepared}");

    let renamed = server.request(
        300,
        "textDocument/rename",
        json!({
            "textDocument": { "uri": main_uri },
            "position": at_the_use,
            "newName": "MAX_ENTRIES",
        }),
    );
    let changes = renamed["result"]["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("a workspace edit was expected, got {renamed}"));
    assert_eq!(changes.len(), 3, "three files are edited: {renamed}");

    let edits: Vec<&Value> = changes
        .values()
        .flat_map(|edits| edits.as_array().into_iter().flatten())
        .collect();
    assert_eq!(edits.len(), 3, "and one edit in each: {renamed}");
    assert!(
        edits.iter().all(|edit| edit["newText"] == json!("MAX_ENTRIES")),
        "every edit writes the new name: {renamed}"
    );
    let definition_edit = changes
        .iter()
        .find(|(uri, _)| uri.ends_with("limits.h"))
        .map(|(_, edits)| &edits[0])
        .unwrap_or_else(|| panic!("the `#define` itself is edited: {renamed}"));
    assert_eq!(
        definition_edit["range"],
        json!({
            "start": { "line": 0, "character": 8 },
            "end": { "line": 0, "character": 17 },
        }),
        "and the definition's edit covers its name: {definition_edit}"
    );

    // --- the refusal, which is the other half of the feature -----------------------------------------
    // A cursor on an ordinary name: `more` is a local variable in this file, its type is not what is being asked
    // about, and its *uses* would need a parse of every candidate file. Both requests answer `null` — no rename box,
    // and no edit — rather than renaming the occurrences that happen to be visible in this file.
    let on_a_local = json!({ "line": 2, "character": 9 });

    let refused = server.request(
        400,
        "textDocument/prepareRename",
        json!({ "textDocument": { "uri": main_uri }, "position": on_a_local }),
    );
    assert!(
        refused["result"].is_null(),
        "an ordinary name has no rename box: {refused}"
    );

    let refused = server.request(
        500,
        "textDocument/rename",
        json!({
            "textDocument": { "uri": main_uri },
            "position": on_a_local,
            "newName": "renamed",
        }),
    );
    assert!(
        refused["result"].is_null(),
        "and renaming it edits nothing rather than half of it: {refused}"
    );

    // A rename to something that is not a name is refused too — two identifiers, a literal, nothing at all.
    for not_a_name in ["two words", "\"quoted\"", ""] {
        let refused = server.request(
            600,
            "textDocument/rename",
            json!({
                "textDocument": { "uri": main_uri },
                "position": at_the_use,
                "newName": not_a_name,
            }),
        );
        assert!(
            refused["result"].is_null(),
            "{not_a_name:?} is not a name: {refused}"
        );
    }

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// The selection fixture: one expression nested in a statement, a body and a function.
const SELECTION_CPP: &str = "int f() {\n    return w.size;\n}\n";

/// **Expanding a selection walks a chain**, and the chain is the answer's *shape* rather than a list.
///
/// A client walks it outward: the word under the cursor, then each construct containing it, out to the file — and
/// every step has to be strictly larger, or the keystroke does nothing. Over the wire that is a nested object, each
/// range pointing at its parent, which is the part a client cannot reconstruct and therefore the part worth pinning.
#[test]
fn a_selection_expands_one_rung_at_a_time() {
    let project = Project::new("selection-range");
    project.write("main.cpp", SELECTION_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let capabilities = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    assert_eq!(
        capabilities["result"]["capabilities"]["selectionRangeProvider"],
        json!(true),
        "the client is told the server can expand a selection: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": SELECTION_CPP }
        }),
    );

    // Line 1 is `    return w.size;` — the cursor is inside `size`, at the `i`.
    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/selectionRange",
            "params": {
                "textDocument": { "uri": main_uri },
                "positions": [{ "line": 1, "character": 13 }],
            },
        })
    });

    let answers = answer["result"]
        .as_array()
        .unwrap_or_else(|| panic!("one answer per position was expected, got {answer}"));
    assert_eq!(answers.len(), 1, "one position, one chain: {answer}");

    // Walk the parent chain the way a client does, and record each rung's text by line and column.
    let mut rungs = Vec::new();
    let mut current = Some(&answers[0]);
    while let Some(range) = current {
        rungs.push((
            range["range"]["start"]["line"].as_u64().unwrap_or_default(),
            range["range"]["start"]["character"].as_u64().unwrap_or_default(),
            range["range"]["end"]["line"].as_u64().unwrap_or_default(),
            range["range"]["end"]["character"].as_u64().unwrap_or_default(),
        ));
        current = range.get("parent").filter(|parent| !parent.is_null());
    }

    assert_eq!(
        rungs,
        vec![
            (1, 13, 1, 17), // `size`
            (1, 11, 1, 17), // `w.size`
            (1, 4, 2, 0),   // `return w.size;`
            (0, 8, 3, 0),   // the function body
            (0, 0, 3, 0),   // the function definition
        ],
        "the chain, innermost first: {answer}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// The workspace-symbol fixture: a class in a namespace, in a header — and the client opens only the `.cpp`.
const SEARCH_H: &str = "namespace ns {\nstruct Widget { int size; };\n}\nstruct WidgetRegistry { int count; };\n";
const SEARCH_CPP: &str = "#include \"search.h\"\nvoid f() { ns::Widget w; }\n";

/// **A symbol in a file nobody opened is found by a project search.**
///
/// Every other feature here is asked about a cursor in a file the client is showing; this one is asked about the
/// project, and the answer has to include declarations from files the user has never looked at — which is what the
/// index is for. The query is matched the way the index documents it (`Widget::si` finds the member, `ns::widget`
/// does not drag it in), and the one thing this test pins beyond that is the qualification travelling with the
/// symbol, because a client shows it beside the name.
#[test]
fn a_workspace_search_finds_a_symbol_in_a_file_nobody_opened() {
    let project = Project::new("workspace-symbol");
    project.write("search.h", SEARCH_H);
    project.write("main.cpp", SEARCH_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let capabilities = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    assert_eq!(
        capabilities["result"]["capabilities"]["workspaceSymbolProvider"],
        json!(true),
        "the client is told the server searches: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": SEARCH_CPP }
        }),
    );

    // Asked again until it answers, because a search that arrives while the index still has work is refused
    // (`null`) rather than answered partially — see the handler's module note.
    let answer = server.ask_until_it(
        100,
        |id| json!({ "id": id, "method": "workspace/symbol", "params": { "query": "widget" } }),
        |response| {
            response["result"]
                .as_array()
                .is_some_and(|symbols| !symbols.is_empty())
        },
    );

    let found = answer["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a symbol list was expected, got {answer}"));
    let names: Vec<&str> = found
        .iter()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec!["Widget", "WidgetRegistry"],
        "the name first, then the one that contains it: {answer}"
    );

    let widget = &found[0];
    assert!(
        widget["location"]["uri"]
            .as_str()
            .is_some_and(|uri| uri.ends_with("search.h")),
        "the class is declared in a file the client never opened: {widget}"
    );
    assert_eq!(
        widget["containerName"],
        json!("ns"),
        "and the namespace it is in travels with it: {widget}"
    );
    assert_eq!(widget["kind"], json!(5), "a class: {widget}");

    // The **qualified** query finds the same class, and the member only when it is asked for by its owner.
    let qualified = server.ask_until(200, |id| {
        json!({ "id": id, "method": "workspace/symbol", "params": { "query": "Widget::si" } })
    });
    let names: Vec<&str> = qualified["result"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    assert_eq!(names, vec!["size"], "`Widget::si` asks for the member: {qualified}");

    let class_only = server.ask_until(300, |id| {
        json!({ "id": id, "method": "workspace/symbol", "params": { "query": "ns::widget" } })
    });
    let names: Vec<&str> = class_only["result"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec!["Widget"],
        "and a class query does not drag its members in: {class_only}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// The folding fixture: an include run, a comment block, a class, and a conditional — one of each kind the protocol
/// names, in an order where the lines are easy to count.
const FOLDS_CPP: &str = "#include <string>\n\
                         #include <vector>\n\
                         \n\
                         // a note\n\
                         // and another\n\
                         \n\
                         struct Widget {\n\
                             int size;\n\
                         };\n\
                         \n\
                         #if defined(_WIN32)\n\
                         int win;\n\
                         #endif\n";

/// **Folding is about the file, and the lines are the client's.**
///
/// Four regions, one of each kind the protocol names, and each one's line range is asserted exactly — because the
/// two ways this can be wrong are both off-by-one: an end taken past the region hides the line after it, and a
/// region whose ends land on one line hides nothing at all. The analysis answers in offsets and refuses the second
/// kind; the conversion to lines happens here, which is why this test goes over the wire rather than against the
/// analysis's own tests.
#[test]
fn the_folds_of_a_file_are_the_regions_a_reader_hides() {
    let project = Project::new("folding");
    project.write("main.cpp", FOLDS_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let capabilities = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    assert_eq!(
        capabilities["result"]["capabilities"]["foldingRangeProvider"],
        json!(true),
        "the client is told the server folds: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": FOLDS_CPP }
        }),
    );

    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/foldingRange",
            "params": { "textDocument": { "uri": main_uri } },
        })
    });

    let ranges = answer["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a folding range list was expected, got {answer}"));
    let seen: Vec<(u64, u64, Option<&str>)> = ranges
        .iter()
        .map(|range| {
            (
                range["startLine"].as_u64().unwrap_or_default(),
                range["endLine"].as_u64().unwrap_or_default(),
                range["kind"].as_str(),
            )
        })
        .collect();

    assert_eq!(
        seen,
        vec![
            (0, 1, Some("imports")),
            (3, 4, Some("comment")),
            (6, 8, None),
            (10, 12, Some("region")),
        ],
        "the include run, the comment block, the class (no kind: the protocol calls that code), the conditional"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}
const OUTLINE_CPP: &str = "#include \"cfg.h\"\n\
                           #include \"handle.h\"\n\
                           #if OFF\n\
                           struct Dead { int x; };\n\
                           #endif\n\
                           struct Live { int y; };\n\
                           DECLARE_HANDLE(HWND);\n";

/// **The outline shows the file, not the compiler** — both halves of that, in one fixture.
///
/// `#if OFF` is false once the header says so, so a compiler never sees `Dead` — and a reader editing that branch
/// very much does, which is why an outline is built from the file's **own** declarations rather than from the
/// cooked reading every other feature uses. The other direction is `DECLARE_HANDLE(HWND)`: the file writes a call,
/// so `HWND__` and `HWND` are in the index and in completions and are *not* here — an outline listing a name the
/// file never writes would be claiming something about the file that is not true.
#[test]
fn the_outline_shows_the_file_and_not_the_compiler() {
    let project = Project::new("outline");
    project.write("cfg.h", CFG_H);
    project.write("handle.h", HANDLE_H);
    project.write("main.cpp", OUTLINE_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let capabilities = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    assert_eq!(
        capabilities["result"]["capabilities"]["documentSymbolProvider"],
        json!(true),
        "the client is told the server outlines: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": OUTLINE_CPP }
        }),
    );

    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": main_uri } },
        })
    });

    let symbols = answer["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a nested outline was expected, got {answer}"));

    // Every name in the tree, however deep, for the assertions about what is *not* there.
    fn all_names(symbols: &[Value], into: &mut Vec<String>) {
        for symbol in symbols {
            into.push(symbol["name"].as_str().unwrap_or_default().to_string());
            if let Some(children) = symbol["children"].as_array() {
                all_names(children, into);
            }
        }
    }

    let mut names = Vec::new();
    all_names(symbols, &mut names);
    assert_eq!(
        names,
        vec!["Dead", "x", "Live", "y"],
        "the branch nobody takes is in the outline, the file's own class, and nothing else: {answer}"
    );

    assert_eq!(symbols[0]["kind"], json!(5), "a class: {}", symbols[0]);
    assert_eq!(
        symbols[0]["children"][0]["kind"],
        json!(13),
        "a variable: {}",
        symbols[0]
    );

    // The pair of ranges: the name is what a client selects, and it is inside the declaration it folds.
    assert_eq!(
        symbols[0]["selectionRange"],
        json!({ "start": { "line": 3, "character": 7 }, "end": { "line": 3, "character": 11 } }),
        "`Dead` is selected by its own name: {}",
        symbols[0]
    );
    assert_eq!(
        symbols[0]["range"]["start"]["line"],
        json!(3),
        "and the declaration starts on the same line: {}",
        symbols[0]
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// The semantic-token fixture: a macro, a namespace, a class with a method, and a parameter.
const COLOURS_CPP: &str = "\
#define LIMIT 8
namespace shapes {
struct Widget {
    int size;
    int scaled(int factor) const;
};
}
";

/// **Semantic colours: each name is drawn as what declares it**, over the wire.
///
/// The answer is the protocol's delta walk — five numbers per token, the first two relative to the token before —
/// so the test decodes it back into absolute positions the way a client does, and asserts that a specific name is
/// drawn as a *specific legend entry*. That last part is the one a unit test cannot check: the numbers only mean
/// something against the legend the server advertised in `initialize`, and a legend that disagreed with the
/// encoding would colour every name as whatever sits at that index.
#[test]
fn semantic_colours_are_the_kinds_the_declarations_have() {
    let project = Project::new("semantic-tokens");
    project.write("main.cpp", COLOURS_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let capabilities = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
                "textDocument": {
                    "semanticTokens": {
                        "requests": { "full": true },
                        "tokenTypes": ["namespace", "type", "typeParameter", "enumMember", "function",
                                       "method", "variable", "parameter", "macro"],
                        "tokenModifiers": ["declaration"],
                        "formats": ["relative"],
                    },
                },
            },
        }),
    );
    let legend = capabilities["result"]["capabilities"]["semanticTokensProvider"]["legend"].clone();
    let index_of = |name: &str| -> u64 {
        legend["tokenTypes"]
            .as_array()
            .expect("a legend")
            .iter()
            .position(|kind| kind == name)
            .unwrap_or_else(|| panic!("the legend has no `{name}`: {legend}")) as u64
    };
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": COLOURS_CPP }
        }),
    );

    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/semanticTokens/full",
            "params": { "textDocument": { "uri": main_uri } },
        })
    });

    let data = answer["result"]["data"]
        .as_array()
        .unwrap_or_else(|| panic!("a token array was expected, got {answer}"));
    let numbers: Vec<u64> = data
        .iter()
        .map(|value| value.as_u64().expect("the encoding is numbers"))
        .collect();
    assert_eq!(numbers.len() % 5, 0, "five numbers per token: {numbers:?}");

    // The client's own reconstruction: absolute line and character, then the length and the kind.
    let mut line = 0u64;
    let mut character = 0u64;
    let mut tokens: Vec<(u64, u64, u64, u64, u64)> = Vec::new();
    for token in numbers.chunks(5) {
        if token[0] == 0 {
            character += token[1];
        } else {
            line += token[0];
            character = token[1];
        }
        tokens.push((line, character, token[2], token[3], token[4]));
    }

    let at = |needle: &str| -> (u64, u64) {
        let line = COLOURS_CPP
            .lines()
            .position(|text| text.contains(needle))
            .expect("the fixture writes it") as u64;
        let character = COLOURS_CPP
            .lines()
            .nth(line as usize)
            .and_then(|text| text.find(needle))
            .expect("the fixture writes it") as u64;
        (line, character)
    };

    let coloured = |needle: &str| -> Option<(u64, u64, u64)> {
        let (line, character) = at(needle);
        tokens
            .iter()
            .find(|token| token.0 == line && token.1 == character)
            .map(|token| (token.2, token.3, token.4))
    };

    assert_eq!(
        coloured("LIMIT").map(|(_, kind, _)| kind),
        Some(index_of("macro")),
        "a macro this file defines: {tokens:?}"
    );
    assert_eq!(
        coloured("shapes").map(|(_, kind, _)| kind),
        Some(index_of("namespace")),
        "{tokens:?}"
    );
    assert_eq!(
        coloured("Widget").map(|(_, kind, _)| kind),
        Some(index_of("type")),
        "a class is a type here: {tokens:?}"
    );
    assert_eq!(
        coloured("size").map(|(_, kind, _)| kind),
        Some(index_of("variable")),
        "a field is a variable: {tokens:?}"
    );
    assert_eq!(
        coloured("scaled").map(|(_, kind, _)| kind),
        Some(index_of("method")),
        "a function in a class is a method: {tokens:?}"
    );
    assert_eq!(
        coloured("factor").map(|(_, kind, _)| kind),
        Some(index_of("parameter")),
        "and a parameter is not a variable: {tokens:?}"
    );

    // The length is the name's own, and the modifier says it is where the name is declared.
    let (length, _, modifiers) = coloured("LIMIT").expect("the macro is coloured");
    assert_eq!(length, "LIMIT".len() as u64);
    assert_eq!(modifiers, 1, "the declaration modifier: {tokens:?}");

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// The signature fixture: a documented declaration, and a call being typed into.
const SIGNATURE_CPP: &str = "\
/// Scales a count.
/// @param count the count
int scale(int count, double factor);

int f() {
    return scale(1, );
}
";

/// **The signature popup, while the second argument is being typed.**
///
/// The cursor sits right after the comma — the state a signature exists for — and what has to come back is the
/// declaration's own text, the two parameter spans *inside that label*, the second one active, and the
/// documentation the file wrote above the declaration. A client bolds the active parameter by slicing the label
/// with the span, which is what this test does with it.
#[test]
fn a_signature_is_shown_while_the_arguments_are_typed() {
    let project = Project::new("signature-help");
    project.write("main.cpp", SIGNATURE_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": SIGNATURE_CPP }
        }),
    );

    let call_line = SIGNATURE_CPP
        .lines()
        .position(|line| line.contains("scale(1, )"))
        .expect("the fixture writes the call") as u64;
    let after_the_comma = SIGNATURE_CPP
        .lines()
        .nth(call_line as usize)
        .and_then(|line| line.find("scale(1, "))
        .expect("the fixture writes the call")
        + "scale(1, ".len();

    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/signatureHelp",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": call_line, "character": after_the_comma },
            },
        })
    });

    let signature = &answer["result"]["signatures"][0];
    assert_eq!(
        signature["label"], "scale(int count, double factor)",
        "the declaration's own text: {answer}"
    );
    assert_eq!(answer["result"]["activeSignature"], json!(0));
    assert_eq!(
        answer["result"]["activeParameter"],
        json!(1),
        "the cursor is past the first comma, so the second parameter is active: {answer}"
    );

    // The parameter spans are offsets into the label — the client's own use of them, done here.
    let label = signature["label"].as_str().expect("a label");
    let spans: Vec<&str> = signature["parameters"]
        .as_array()
        .expect("the parameters are sent")
        .iter()
        .map(|parameter| {
            let offsets = parameter["label"].as_array().expect("offsets");
            let start = offsets[0].as_u64().expect("a start") as usize;
            let end = offsets[1].as_u64().expect("an end") as usize;
            &label[start..end]
        })
        .collect();
    assert_eq!(spans, vec!["int count", "double factor"]);

    let documentation = signature["documentation"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("the documentation above the declaration: {answer}"));
    assert!(documentation.contains("Scales a count."), "{documentation}");
    assert!(documentation.contains("@param count"), "{documentation}");

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}
/// The completion-resolve fixture: a documented member, offered through a member access.
const RESOLVE_CPP: &str = "\
struct Widget {
    /// The widget's size in cells.
    int size;
};
int f() {
    Widget w;
    w.
}
";

/// **`completionItem/resolve` fetches the documentation for the one item a client is showing.**
///
/// The list itself carries no comments — that is the point of the round trip, and why the list stays cheap for a
/// hundred names — so this test asks the two questions a client asks, in order: complete, then resolve the item the
/// user highlighted. What comes back is the declaration's own comment, rendered by the same function the hover and
/// the signature help use.
#[test]
fn a_completion_item_resolves_to_its_declarations_documentation() {
    let project = Project::new("completion-resolve");
    project.write("main.cpp", RESOLVE_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": RESOLVE_CPP }
        }),
    );

    let line = RESOLVE_CPP
        .lines()
        .position(|text| text.trim() == "w.")
        .expect("the fixture writes the access") as u64;
    let character = RESOLVE_CPP
        .lines()
        .nth(line as usize)
        .and_then(|text| text.find("w."))
        .expect("the fixture writes the access")
        + 2;

    let listed = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": line, "character": character },
                },
            })
        },
        |answer| {
            answer["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("size")))
        },
    );

    let item = listed["result"]["items"]
        .as_array()
        .expect("an item list")
        .iter()
        .find(|item| item["label"] == json!("size"))
        .expect("the member is offered")
        .clone();

    // The item carries **where the declaration is** — the file and the name's offset — which is what a resolve is
    // asked with. A client echoes `data` back untouched; this test does the same.
    assert!(
        item["data"]["file"].is_string() && item["data"]["offset"].is_number(),
        "the item carries its declaration's identity: {item}"
    );
    assert!(
        item["documentation"].is_null(),
        "the list itself carries no comments: {item}"
    );

    let resolved = server.ask_until_it(
        200,
        |id| {
            json!({
                "id": id,
                "method": "completionItem/resolve",
                "params": item.clone(),
            })
        },
        |answer| !answer["result"]["documentation"].is_null(),
    );

    let documentation = resolved["result"]["documentation"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("markdown documentation was expected, got {resolved}"));
    assert!(
        documentation.contains("The widget's size in cells."),
        "the member's own comment: {documentation}"
    );
    assert_eq!(
        resolved["result"]["label"], "size",
        "and the item is the one that was asked about: {resolved}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// The hover fixture: a documented function, one with a plain comment above it, and a `this` in a member function.
const HOVER_CPP: &str = "\
/// Adds two counts.
/// @param a the first
/// @param b the second
int add(int a, int b);

// A note to the reader, which is not documentation.
int undocumented(int c);

struct Widget {
    int size;
    int scaled(int factor) const { return size * factor; }
    int go() const { return this->scaled(2); }
};
";

/// **Hover shows what the file wrote about a declaration — and only what it wrote as documentation.**
///
/// Three positions in one fixture, because the interesting part is the *boundary*: the documented declaration
/// carries its comment (Doxygen markers and all, the way clangd shows them), the one under a plain `//` comment
/// carries none — a `// TODO` dressed as an interface description is the failure the parser's own classification
/// exists to prevent — and `this`, which names no declaration at all, is answered by the expression question
/// instead: the class it points at.
#[test]
fn hover_shows_the_documentation_and_refuses_a_plain_comment() {
    let project = Project::new("hover");
    project.write("main.cpp", HOVER_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": HOVER_CPP }
        }),
    );

    let documented = hover_over(&mut server, 100, &main_uri, HOVER_CPP, "add(int a, int b);");
    let text = documented["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {documented}"));
    assert!(
        text.contains("Adds two counts."),
        "the comment above the declaration is the documentation: {text}"
    );
    assert!(
        text.contains("@param a the first"),
        "shown as the file wrote it, markers included: {text}"
    );
    assert!(
        text.contains("add(int a, int b)"),
        "with the declaration itself: {text}"
    );

    let plain = hover_over(&mut server, 200, &main_uri, HOVER_CPP, "undocumented(int c);");
    let text = plain["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {plain}"));
    assert!(
        !text.contains("A note to the reader"),
        "a plain `//` comment is not documentation: {text}"
    );
    assert!(
        text.contains("undocumented(int c)"),
        "the declaration is still what the hover is about: {text}"
    );

    let this = hover_over(&mut server, 300, &main_uri, HOVER_CPP, "this->scaled(2)");
    let text = this["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected for `this`, got {this}"));
    assert!(
        text.contains("enclosing class `Widget`"),
        "`this` names no declaration, so the expression question answers: {text}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// A hover whose position is the first character of the line containing `needle`.
fn hover_over(
    server: &mut Server,
    first_id: i64,
    uri: &str,
    fixture: &str,
    needle: &str,
) -> Value {
    let line = line_of(fixture, needle);
    let character = fixture
        .lines()
        .nth(line as usize)
        .and_then(|text| text.find(needle))
        .expect("the fixture writes it");

    server.ask_until(first_id, |id| {
        json!({
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            },
        })
    })
}

/// The line of a fixture that contains `needle`, counted from zero.
fn line_of(text: &str, needle: &str) -> u64 {
    text.lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not in the fixture"))
        .try_into()
        .expect("a fixture this size fits in a u32")
}

/// **The inlay fixture whose callee is in a header** — the shape a reported file has, and the one that makes the
/// refusal visible.
///
/// The fixture below declares `scale` in the same file it is called from, which hides the bug the refusal caused:
/// an edit drops that file's summary, so the analysis itself answers nothing until the drain, and a test cannot tell
/// "refused" from "honestly empty". A callee in a header is untouched by an edit to the file that calls it, so the
/// answer is available the whole time — and anything that suppresses it is then the only thing being measured.
const INLAY_HEADER: &str = "\
#pragma once
double scale(int count, double factor);
";

const INLAY_ACROSS_FILES: &str = "\
#include \"widget.h\"

double f(int count) {
    return scale(count, 1);
}
";

/// The inlay fixture: a call whose argument already spells the parameter, one that does not, and a member call.
const INLAY_CPP: &str = "\
double scale(int count, double factor);

struct Widget {
    int size_of(int scale) const;
};

double f(Widget& w, int count) {
    return scale(count, 1) + w.size_of(2);
}
";

/// **A parameter name is drawn at the argument — and not where the argument already says it.**
///
/// **The hints a person is actually looking at**: asked one turn after an edit, with the queue still busy.
///
/// This is the case that made the feature look broken, and the report was exact: *"as soon as I have edited the code
/// the inlay hints stop appearing"*. The handler answered `null` — not an empty list, a refusal — while
/// `Session::pending() > 0`, on the reasoning that a callee in a file nobody has read yet resolves to nothing. The
/// reasoning is right about the cause and wrong about the remedy: `parameter_hints` **skips** an unresolved callee,
/// so a filling index yields fewer hints and never a wrong one.
///
/// What that cost, measured on a 31-line file with four `#include`s: an edit leaves the queue busy for **~48 ms**,
/// question about the same file returns **4 hints on the file's own tokens with the index complete** — so the
/// reading was never the problem and the refusal was the whole of it.
/// two apart deliberately; it was removed with the rest of the scaffolding once the cause was found.
/// question about the same file returns **4 hints on the file's own tokens with the index complete** — so the
/// reading was never the problem and the refusal was the whole of it.
///
/// The ask here waits for **the first answer that is not a refusal**, and then edits and asks **once**, with no
/// retry: a retry would let the queue drain and measure the wrong thing.
///
/// # What this test does not prove, recorded because it was tried
///
/// It **passes with the refusal put back**. The notification and the request are two messages and the server drains
/// in between — the window is ~48 ms, and this test does not control the race. The instrument that does show the
/// in between — the window is ~48 ms, and this test does not control the race. The instrument that did show it
/// was a probe that held the two apart deliberately, and it was removed with the rest of the scaffolding.
/// reports the depth and the count together.
///
/// And even there one number is ambiguous, which is worth knowing before trusting it: for a fixture whose callees
/// are declared **in the edited file**, the analysis itself answers nothing until the drain, because the edit
/// dropped that file's summary — so "the refusal" and "the honest empty answer" read the same. The reported file is
/// the other shape: its calls are into headers (`std::format`, `printf`), whose declarations the edit did not
/// touch, so the analysis had the answer all along and only the refusal was hiding it.
#[test]
fn the_hints_survive_the_edit_that_asks_for_them() {
    let project = Project::new("inlay-hints-after-an-edit");
    project.write("widget.h", INLAY_HEADER);
    project.write("main.cpp", INLAY_ACROSS_FILES);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    let whole = json!({
        "start": { "line": 0, "character": 0 },
        "end": { "line": INLAY_ACROSS_FILES.lines().count(), "character": 0 },
    });

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "inlayHint": {} } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": main_uri,
                "languageId": "cpp",
                "version": 1,
                "text": INLAY_ACROSS_FILES,
            }
        }),
    );

    // Settled first, so that what is measured below is the **edit** and not the initial index.
    let settled = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/inlayHint",
                "params": { "textDocument": { "uri": main_uri }, "range": whole },
            })
        },
        |answer| answer["result"].as_array().is_some_and(|hints| !hints.is_empty()),
    );
    assert!(
        settled["result"].as_array().is_some_and(|hints| hints.len() == 1),
        "one argument that does not spell its parameter: {settled}"
    );

    // **One keystroke**, in a function body: no directive moves, so this is the cheapest edit there is — and it is
    // the one whose hint request used to be refused. The callee is in `widget.h`, which the edit does not touch, so
    // the analysis can answer immediately and nothing but a refusal can be suppressing it.
    let edited = INLAY_ACROSS_FILES.replace("return scale(count, 1);", "return scale(count, 1) ;");
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": main_uri, "version": 2 },
            "contentChanges": [{ "text": edited }],
        }),
    );

    let after = server.request(
        200,
        "textDocument/inlayHint",
        json!({ "textDocument": { "uri": main_uri }, "range": whole }),
    );
    let hints = after["result"].as_array().unwrap_or_else(|| {
        panic!("**the hints were refused outright after an edit**: {after}")
    });
    assert_eq!(
        hints.len(),
        1,
        "an edit that moves no directive changes nothing a hint depends on, and the callee is in a header: {after}"
    );
}

/// Three arguments, two hints: `scale(count, …)` needs no `count:` in front of `count`, `1` is the second
/// parameter however it is spelled, and the member call's argument is named by the *member's* declaration rather
/// than by anything in the calling file.
///
/// The `ask_until` below is about the **index**, not about a refusal: this request is made before the workspace has
/// been read at all, when the callee genuinely has no declaration to take parameter names from, and the answer is an
/// empty list. It is the loop that gets it past that moment — see
/// [`the_hints_survive_the_edit_that_asks_for_them`] for the case that used to be answered with `null` instead.
#[test]
fn parameter_names_are_drawn_at_the_arguments() {
    let project = Project::new("inlay-hints");
    project.write("main.cpp", INLAY_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let capabilities = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    assert_eq!(
        capabilities["result"]["capabilities"]["inlayHintProvider"]["resolveProvider"],
        json!(false),
        "the client is told the server hints, and that there is nothing to resolve: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": INLAY_CPP }
        }),
    );

    let call_line = line_of(INLAY_CPP, "return scale(count, 1)");
    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/inlayHint",
            "params": {
                "textDocument": { "uri": main_uri },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": INLAY_CPP.lines().count(), "character": 0 },
                },
            },
        })
    });

    let hints = answer["result"]
        .as_array()
        .unwrap_or_else(|| panic!("an inlay hint list was expected, got {answer}"));
    let seen: Vec<(u64, String)> = hints
        .iter()
        .map(|hint| {
            (
                hint["position"]["line"].as_u64().expect("a line"),
                hint["label"].as_str().expect("a label").to_string(),
            )
        })
        .collect();

    assert_eq!(
        seen,
        vec![
            (call_line, "factor:".to_string()),
            (call_line, "scale:".to_string()),
        ],
        "`count` spells its parameter and gets no hint; the other two do: {answer}"
    );

    // The position is on the argument itself and the padding is after the label, so a client draws `factor: 1`
    // rather than `factor :1`.
    let factor = &hints[0];
    let argument = INLAY_CPP
        .lines()
        .nth(call_line as usize)
        .and_then(|line| line.find("scale(count, 1)"))
        .expect("the fixture writes the call")
        + "scale(count, ".len();
    assert_eq!(
        factor["position"]["character"],
        json!(argument),
        "the hint sits on the argument: {factor}"
    );
    assert_eq!(factor["paddingRight"], json!(true), "{factor}");
    assert_eq!(factor["kind"], json!(2), "a parameter hint: {factor}");

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// An overloaded name answers with **every** location, which is the shape a client shows as a peek list.
///
/// One name covering several declarations is ordinary C++ — `find` in `std::basic_string` is seventeen of them in
/// MSVC's library — and `null` for all of them is a dead end for the reader. What must *not* happen is the other
/// mistake: a name with one declaration still answers `Scalar`, so nothing about the common case changes.
#[test]
fn an_overload_set_answers_with_every_location() {
    const OVERLOADS_H: &str = "int parse(int value);\nint parse(const char* text);\nint parse(double value);\n";
    const OVERLOADS_CPP: &str = "#include \"overloads.h\"\nint f() { return parse(1); }\n";

    let project = Project::new("definition-overloads");
    project.write("overloads.h", OVERLOADS_H);
    project.write("main.cpp", OVERLOADS_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    let header_uri = uri_of(&project.root().join("overloads.h"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": OVERLOADS_CPP }
        }),
    );

    // The cursor is inside `parse` on line 1 — `int f() { return parse(1); }` — and the request is repeated until
    // the header has been read, for the reason every query here is: the index is lazy on purpose.
    let answer = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/definition",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": 1, "character": 20 },
                },
            })
        },
        |response| {
            response["result"]
                .as_array()
                .is_some_and(|locations| locations.len() == 3)
        },
    );

    let locations = answer["result"]
        .as_array()
        .unwrap_or_else(|| panic!("three declarations, three locations: {answer}"));

    assert_eq!(locations.len(), 3);
    for (index, location) in locations.iter().enumerate() {
        assert_eq!(
            location["uri"],
            json!(header_uri),
            "all three are in the header: {location}"
        );
        assert_eq!(
            location["range"]["start"]["line"],
            json!(index),
            "one per line, in the order the header writes them: {answer}"
        );
    }

    // A name with **one** declaration is still `Scalar`: the common case does not change shape, which is what lets
    // a client that predates lists keep working.
    let single = server.request(
        200,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 1, "character": 4 },
        }),
    );
    assert!(
        single["result"]["uri"].is_string(),
        "`f` is declared on this line, and one declaration is one location: {single}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// The fixture for [`a_completion_inside_a_macros_argument_is_about_that_argument`]: a macro whose second argument is
/// where the cursor goes, and two types with one member each so that a wrong subject is visible in the list.
const MACRO_ARGUMENT_CPP: &str = "\
struct alpha { int one; };
struct beta { int gamma; };

#define PICK(first, second) pick_impl((first), (second))

beta bet;

int f() {
    return PICK(1, bet.);
}
";

/// **A cursor inside a macro's argument list is still a cursor in the code.**
///
/// A view is of the **rendering** — the file with its macros replaced — so a position a client sends has to be
/// translated into the reading before anything is resolved. A macro invocation is where that translation is hardest
/// and where getting it wrong is invisible: `PICK(1, bet.)` becomes something else entirely in the rendering, and a
/// mapping that dropped the cursor at the invocation's start rather than inside the argument would still produce a
/// completion list — of the wrong scope, at the wrong place, with no error anywhere to say so.
///
/// The criterion is that the **argument's own type** is what is offered. `bet` is a `beta` and the file also holds
/// an `alpha`, so a list holding `one` is a list answering about some other expression.
#[test]
fn a_completion_inside_a_macros_argument_is_about_that_argument() {
    let project = Project::new("completion-in-a-macro-argument");
    project.write("main.cpp", MACRO_ARGUMENT_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "completion": {} } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MACRO_ARGUMENT_CPP }
        }),
    );

    // The cursor is right after `bet.` inside `PICK(1, bet.)` — a member access whose subject is a macro argument.
    // Line 8 is `    return PICK(1, bet.);` and column 23 is the byte after the dot: the first version of this test
    // asked at line 9, which is the closing brace, and the server answered a name completion for `}` — a correct
    // answer to a question about the wrong place, which is the failure mode this test exists to catch and which it
    // caught in itself first.
    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 8, "character": 23 },
            },
        })
    });

    let items = answer["result"]["items"]
        .as_array()
        .or_else(|| answer["result"].as_array())
        .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();

    assert!(
        labels.contains(&"gamma"),
        "the members of the argument's own type are offered: {labels:?}"
    );
    assert!(
        !labels.contains(&"one"),
        "and not the members of the other type in the same file: {labels:?}"
    );
}

/// The fixture for [`a_signature_survives_being_inside_a_macro_argument`]: a real declaration, a call to it, and the
/// whole call written as a macro's argument so that the reading the server parses has been rewritten around it.
const SIGNATURE_IN_A_MACRO_CPP: &str = "\
#define PICK(first, second) pick_impl((first), (second))

/// Scales a count.
int scale(int count, double factor);

int f() {
    return PICK(1, scale(2, ));
}
";

/// **The signature popup survives being inside a macro's argument.**
///
/// The same seam [`a_completion_inside_a_macros_argument_is_about_that_argument`] tests, asked by the other handler
/// that takes a cursor: signature help resolves the call around the offset out of the **rendering**, and the
/// rendering is where `PICK(1, scale(2, ))` has been rewritten into something with the call still in it but at a
/// different offset. A mapping that landed the cursor on the macro's own name, or on the first argument, would ask
/// about a different call — `pick_impl` here — and either answer nothing or answer with the wrong signature.
///
/// What has to come back is `scale`'s own label and its second parameter active, which is the state a reader sees
/// while typing the argument.
#[test]
fn a_signature_survives_being_inside_a_macro_argument() {
    let project = Project::new("signature-in-a-macro");
    project.write("main.cpp", SIGNATURE_IN_A_MACRO_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "signatureHelp": {} } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": SIGNATURE_IN_A_MACRO_CPP }
        }),
    );

    // `    return PICK(1, scale(2, ));` is line 6, and column 28 is right after the comma of `scale`'s arguments.
    let answer = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/signatureHelp",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": 6, "character": 28 },
                },
            })
        },
        |answer| !answer["result"].is_null(),
    );

    let signatures = answer["result"]["signatures"]
        .as_array()
        .unwrap_or_else(|| panic!("a signature list was expected, got {answer}"));
    let labels: Vec<&str> = signatures
        .iter()
        .filter_map(|signature| signature["label"].as_str())
        .collect();
    assert!(
        labels.iter().any(|label| label.contains("scale")),
        "the signature of the call the cursor is in, not of the macro around it: {labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label.contains("PICK")),
        "and not the macro's own name: {labels:?}"
    );
    assert_eq!(
        answer["result"]["activeParameter"],
        json!(1),
        "the second argument is the one being typed: {answer}"
    );
}

/// **What the popup says, for every shape a declaration comes in** — printed, not asserted.
///
/// A hover is the one answer a reader judges by how it *looks*, and no assertion states that well. This exists so
/// the shapes can be read side by side while the rendering is being changed: a template, a member function with a
/// body, a documented class, a variable the file gave no type to, and a member of a class template from another
/// file. Ignored by default — run it with `--ignored --nocapture`.
#[test]
#[ignore = "a reading aid: it prints popups rather than checking them"]
fn what_the_popups_look_like() {
    let project = Project::new("hover-shapes");
    project.write(
        "box.h",
        "#pragma once\n\
         /// A box that holds one `T`.\n\
         ///\n\
         /// The documentation is the file's own words.\n\
         template <class T>\n\
         class Box {\n\
         public:\n\
             /// What is in the box.\n\
             T get() const { return v; }\n\
             T v;\n\
         };\n",
    );
    project.write(
        "main.cpp",
        "#include \"box.h\"\n\
         \n\
         int main() {\n\
             auto n = 1;\n\
             Box<int> b;\n\
             b.v = n;\n\
             return 0;\n\
         }\n",
    );

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    let main_text = "#include \"box.h\"\n\nint main() {\n    auto n = 1;\n    Box<int> b;\n    b.v = n;\n    \
                     return 0;\n}\n";

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "hover": {} } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": main_text }
        }),
    );

    // (line, character, what is being hovered) — 0-based, as a client sends them.
    let asked: [(u64, u64, &str); 5] = [
        (3, 9, "the local `n`, whose type is an `auto` the file wrote"),
        (4, 4, "`Box` in `Box<int> b;` — a class template from a header"),
        (5, 6, "the member `v` of that class template"),
        (4, 13, "the variable `b`, whose type is that class template"),
        (1, 4, "`main` itself, a function this file defines"),
    ];

    for (line, character, what) in asked {
        let mut answer = server.ask_until_it(
            100,
            |id| {
                json!({
                    "id": id,
                    "method": "textDocument/hover",
                    "params": {
                        "textDocument": { "uri": main_uri },
                        "position": { "line": line, "character": character },
                    },
                })
            },
            |answer| !answer["result"].is_null(),
        );
        // Asked more than once so the server has had the turns it needs to read the file the way a client that
        // waited gets it — see the note in the coordinate test above.
        for _ in 0..4 {
            answer = server.ask_until_it(
                100,
                |id| {
                    json!({
                        "id": id,
                        "method": "textDocument/hover",
                        "params": {
                            "textDocument": { "uri": main_uri },
                            "position": { "line": line, "character": character },
                        },
                    })
                },
                |answer| !answer["result"].is_null(),
            );
        }
        let markdown = answer["result"]["contents"]["value"]
            .as_str()
            .unwrap_or("<no popup>")
            .to_string();
        println!("=== {line}:{character} — {what} ===\n{markdown}\n");
    }
}

/// **A declaration's own line, when the file it is in is the file being read.**
///
/// The tooltip's last line is `file:line:column`, and the line is the one part of this server that can be wrong
/// without anything looking broken: a fact about a declaration in **another** file carries that file's offsets,
/// while a fact about a declaration in **the file being viewed** comes from the view's own tree — whose offsets are
/// the **rendering's**, because the rendering is what the parser was handed, and whose line numbers are therefore
/// short by however many `#include` and `#define` lines the directives took out.
///
/// Measured on a real report: a class on line 6 of a file with five `#include`s above it was announced as
/// `main.cpp:1:7` — **the column right and the line five short**, which is exactly the number of lines between them.
/// The existing hover test cannot catch it: its declaration is in a header, so it takes the other branch.
///
/// The criterion is the line and the column together, because the column is what was right in the report — a test
/// that only checked "which file" would pass on the broken answer.
#[test]
fn a_declaration_in_the_viewed_file_is_reported_on_its_own_line() {
    let project = Project::new("hover-coordinates");
    // Five lines of directives above the class, so a rendering that drops them is five lines short — the number in
    // the report, and the number that makes the difference visible rather than rounded away.
    project.write(
        "main.cpp",
        "#include <vector>\n\
         #include <string>\n\
         #include <format>\n\
         #include <iostream>\n\
         \n\
         class Sux {\n\
         public:\n\
             void print() {}\n\
         };\n\
         \n\
         int main() {\n\
             Sux sux;\n\
             return 0;\n\
         }\n",
    );

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "hover": {} } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": main_uri,
                "languageId": "cpp",
                "version": 1,
                "text": "#include <vector>\n#include <string>\n#include <format>\n#include <iostream>\n\n\
                         class Sux {\npublic:\n    void print() {}\n};\n\nint main() {\n    Sux sux;\n    \
                         return 0;\n}\n",
            }
        }),
    );

    // **Asked until the file has been read the way a client that waited gets it.**
    //
    // `Session::view` answers from the file's own tokens until a rendering exists and builds it for the next query —
    // a deliberate cost, because a rendering cannot be built behind an `&self`. In that first reading every offset is
    // the file's, so both readers below agree by accident and a test that asked once would pass on the broken answer.
    // Measured: with the fix disabled this test still passed, which is what sent it back here.
    let mut answer = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/hover",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": 11, "character": 4 },
                },
            })
        },
        |answer| !answer["result"].is_null(),
    );
    for _ in 0..4 {
        answer = server.ask_until_it(
            100,
            |id| {
                json!({
                    "id": id,
                    "method": "textDocument/hover",
                    "params": {
                        "textDocument": { "uri": main_uri },
                        "position": { "line": 11, "character": 4 },
                    },
                })
            },
            |answer| !answer["result"].is_null(),
        );
    }

    let markdown = answer["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a hover was expected, got {answer}"));
    println!("--- the popup about `Sux` ---\n{markdown}\n---");
    assert!(
        markdown.contains("main.cpp:6:7"),
        "the class is written on line 6, column 7 — not on a line measured in the rendering: {markdown}"
    );
    // **And the popup's own text, read at the fact's offsets.** Those offsets are the rendering's, so reading the
    // file's bytes at them lands wherever the directives had taken lines out: measured on the same report, a popup
    // about `main` began with `tream>`, the tail of `#include <iostream>`, because the four `#include`s above `main`
    // had moved it 60 bytes. The line *number* was right by then — the two are separate readers and this is the one
    // that was still wrong.
    assert!(
        markdown.contains("class Sux"),
        "the declaration as it is written, not the bytes at that offset in another document: {markdown}"
    );
    assert!(
        !markdown.contains("tream>"),
        "and no fragment of an `#include` line: {markdown}"
    );
}

/// **The reported file itself, asked at every position a cursor sits in** — printed rather than asserted.
///
/// A report in three sentences: *"completion outside a namespace is gone too, and the inlay hints no longer appear
/// at all"*. The server's own log agrees that the member path declined for both positions it names — a plain name
/// and a qualifier are not member accesses, so that part is correct — and that the index held 157 files with **0
/// queued**. So whatever completion answered, it answered from a complete index, and a wrong answer here is the
/// handler's rather than the analysis's.
///
/// A list of a hundred names and an empty list need different fixes, and no assertion states which one came back.
#[test]
#[ignore = "a reading aid for a reported file"]
fn what_the_completion_answers_here() {
    let Ok(source) = std::fs::read_to_string(r"C:\Users\ZC\Desktop\stdproj\main.cpp") else {
        println!("the reported file is not on this machine");
        return;
    };

    let project = Project::new("completion-report");
    project.write("main.cpp", &source);
    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "completion": {} } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": source }
        }),
    );

    // **Every position a cursor can be typed at**, which is the survey a "completion is gone" report needs: the
    // handler's own log had already said the member path declined for the two positions it named — correctly, since
    // a plain name and a qualifier are not member accesses — so what is missing is *which* positions come back
    // empty, and a list of them is the finding.
    let mut asked: Vec<(u64, u64)> = Vec::new();
    for (number, line) in source.lines().enumerate() {
        let line = line.trim_end();
        for (column, _) in line.char_indices() {
            let at = &line[..column];
            let ends = at
                .chars()
                .last()
                .is_some_and(|last| last.is_alphanumeric() || last == '_' || last == ':' || last == '.');
            if ends {
                asked.push((number as u64, column as u64));
            }
        }
    }
    for (number, character) in asked {
        let number = number as usize;
        let body = source.lines().nth(number).unwrap_or("").trim_end();

        let answer = server.request(
            100 + number as i64,
            "textDocument/completion",
            json!({
                "textDocument": { "uri": main_uri },
                "position": { "line": number, "character": character },
            }),
        );
        let items = match &answer["result"] {
            Value::Array(items) => items.len(),
            Value::Object(object) => object["items"].as_array().map(Vec::len).unwrap_or(0),
            _ => 0,
        };
        let first: Vec<&str> = match &answer["result"] { serde_json::Value::Array(items) => items.iter().filter_map(|i| i["label"].as_str()).take(8).collect(), serde_json::Value::Object(o) => o["items"].as_array().map(|a| a.iter().filter_map(|i| i["label"].as_str()).take(8).collect()).unwrap_or_default(), _ => Vec::new() };
        println!("{number:>3}: {:<40} -> {items} item(s): {first:?}", body.trim());
    }
}
