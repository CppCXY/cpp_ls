//! **The answer a pull gives right after an edit is about that edit.**
//!
//! The report that produced this file: "诊断依然在频繁地闪现" — squiggles that appear and vanish while typing. The
//! server is pull-only (`textDocument/diagnostic`), so the delivery cannot be the cause: what a client is shown is
//! the answer it last pulled. And the answer used to alternate, once per keystroke:
//!
//! ```text
//!   edit        the file's cooked reading is dropped
//!   pull        no reading yet → `diagnose_file` answers None → an EMPTY report
//!               …and an empty report means "this file is clean", so the client clears what it shows
//!   cook        the pump finishes
//!   refresh     the client asks again → the findings come back
//! ```
//!
//! The first attempt at a fix was a memory — `AnalysisState::last_reports`, a pull that could not answer repeating
//! the last report the file was given. It stopped the flash and it **caused a worse defect**: a file that had been
//! corrected kept reporting the findings from the moment in between, for as long as the index was busy, which on a
//! real project is minutes (reported from an editor with a screenshot; see `document_diagnostic::the_report`).
//!
//! The memory is gone and this test is what says it is not needed: the pull path **cooks the file before it answers**
//! (`AnalysisState::prepare_for_a_diagnostic`), so the answer right after an edit is already the answer for the new
//! text. What this test proves is therefore the property a user sees — the finding is there on the pull that follows
//! the edit, and it is still there three pulls later with no edit in between — and it proves it *without* a memory:
//! the number comes from the current reading each time.
//!
//! It cannot force the race that used to lose the finding (a pull landing between the edit and the cook), because the
//! pull path does the cook itself. That is the point rather than a gap: there is no window left to race in.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A file with one finding `assign-type-check` is certain about: a string initialising an `int`.
///
/// See `assign_type_check`'s module documentation — `int count = "three";` is the pair the conversion relation
/// answers `Known::No` for, which is why it is the fixture every measurement in this workspace uses.
const MAIN_CPP: &str = "void f() {\n    int count = \"three\";\n    (void)count;\n}\n";

#[test]
fn a_finding_does_not_vanish_between_first_and_second_pull() {
    let project = Project::new("diagnostics-stable");
    project.write("main.cpp", MAIN_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "window": { "workDoneProgress": true },
                "textDocument": { "diagnostic": { "dynamicRegistration": true, "refreshSupport": true } },
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

    // **Wait for the finding**, which is the state a client is in before the edit.
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut first = 0usize;
    while Instant::now() < deadline {
        first = pull(&mut server, 10, &main_uri);
        if first > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        first > 0,
        "the fixture has a finding, so the first pull that sees the file must report it"
    );

    // **The edit**, then the pull a client sends in the same breath — the moment the old answer became empty.
    let edited = format!("{MAIN_CPP}// note\n");
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": main_uri, "version": 2 },
            "contentChanges": [{ "text": edited }],
        }),
    );
    let immediately = pull(&mut server, 11, &main_uri);
    assert!(
        immediately > 0,
        "a pull right after the edit must still report the finding: 0 means the client was told \
         the file became clean, which is the flash"
    );

    // **And it stays.** Three more pulls, no further edits: the count may change (a cook can find or drop a finding
    // the raw reading could not), but it must never fall back to zero.
    for id in 12..=14 {
        let after = pull(&mut server, id, &main_uri);
        assert!(
            after > 0,
            "the finding vanished on pull {id} with no edit in between: the answer alternates, which is the flash"
        );
    }

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// Ask for the file's report and answer how many items it holds.
fn pull(server: &mut Server, id: i64, uri: &str) -> usize {
    let response = server.request(
        id,
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    response["result"]["items"]
        .as_array()
        .map(|items| items.len())
        .unwrap_or_else(|| panic!("a full report was expected, got {response}"))
}

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir().join("cpp_ls-diagnostics").join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a project directory");
        Project { root }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.root.join(name), text).expect("a file");
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        // `CPPLS_KEEP_FIXTURE=1` leaves the directory (and its log) behind, for reading a failure.
        if std::env::var_os("CPPLS_KEEP_FIXTURE").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
}

impl Server {
    fn start(root: &Path) -> Server {
        let log_dir = root.join("log");
        let _ = std::fs::create_dir_all(&log_dir);
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg(&log_dir)
            .arg("--log-level")
            .arg("info")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the server starts");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, messages): (Sender<Value>, Receiver<Value>) = std::sync::mpsc::channel();
        std::thread::spawn(move || read_messages(stdout, tx));
        Server {
            child,
            stdin,
            messages,
        }
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_string(&message).expect("a message serializes");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len())
            .expect("the message is written");
        self.stdin.flush().expect("the message is flushed");
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.messages.recv_timeout(left) {
                Ok(message) if message.get("id") == Some(&json!(id)) => return message,
                Ok(_) => {}
                Err(error) => panic!("the server did not answer: {error}"),
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_messages(stdout: ChildStdout, tx: Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut length = None;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                return;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some(rest) = header.strip_prefix("Content-Length:") {
                length = rest.trim().parse::<usize>().ok();
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
            continue;
        };
        if tx.send(message).is_err() {
            return;
        }
    }
}

fn uri_of(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        text.insert(0, '/');
    }
    format!("file://{text}")
}
