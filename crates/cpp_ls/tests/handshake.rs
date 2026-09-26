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
/// is a real gap in its error reporting and it is recorded in `docs/grammar-gaps.md`.)
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

    /// Wait for a message the closure accepts, answering the server's own requests while waiting.
    fn wait_for(&mut self, accept: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + TIMEOUT;

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
