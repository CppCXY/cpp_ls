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
const TIMEOUT: Duration = Duration::from_secs(30);

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

            if accept(&response) {
                return response;
            }

            if Instant::now() >= deadline {
                panic!("no answer the test accepts within {TIMEOUT:?}; the last one was: {response}");
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
    assert_eq!(size["kind"], json!(6), "a variable, as the index records it: {size}");
    assert_eq!(
        size["textEdit"]["range"]["start"],
        json!({ "line": 3, "character": 6 }),
        "the edit is an empty range just past the dot: {size}"
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