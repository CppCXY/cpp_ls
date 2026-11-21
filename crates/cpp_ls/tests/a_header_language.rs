//! **A header is read as C when the client says it is C.**
//!
//! The rule that produced this file: `DiagnosticCode::is_cpp_only`'s answer is "not told is not C", so a file the
//! project states nothing about is judged by C++ rules. A **header** is exactly that file — `CompilerConfig` refuses
//! to guess a family from `.h` (a header is included by whichever language includes it) — and with no configuration
//! the project-wide default (`c++23` in an editor's settings) then made an unconfigured C project's `.h` files C++.
//! Measured on redis 8.x: **73 `cstyle-cast` findings, every one of them in a `.h`**, telling a C program to write
//! `static_cast` (`docs/c-support.md` §8).
//!
//! The client knows what no configuration does: the editor's language mode, which `didOpen` carries as `languageId`
//! and which the server ignored. This test is the property: the same header, the same text, one difference — what the
//! client said — and the C++-only finding is there without it and gone with it.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// `(int)p` is a `cstyle-cast` in C++ and not a cast to criticise in C.
const HEADER: &str = "static int widen(void *p) { return (int)p; }\n";

#[test]
fn a_header_the_client_calls_c_is_not_judged_by_cpp_rules() {
    let project = Project::new("c-header-language");

    // **The same file twice, in two projects**: one where the client says `c`, one where it says `cpp`.
    let as_c = find_cstyle_cast(&project, "c");
    let as_cpp = find_cstyle_cast(&project, "cpp");

    assert!(
        as_cpp,
        "`(int)p` in a header read as C++ is a `cstyle-cast` — without this the test proves nothing"
    );
    assert!(
        !as_c,
        "the client said the header is C, so a C++-only rule must not fire in it"
    );
}

/// Open `a.h` (and the `a.c` that includes it) saying `language_id`, and report whether `cstyle-cast` came back.
fn find_cstyle_cast(project: &Project, language_id: &str) -> bool {
    let root = project.root().join(language_id);
    std::fs::create_dir_all(&root).expect("a directory");
    std::fs::write(root.join("a.h"), HEADER).expect("the header");
    std::fs::write(
        root.join("a.c"),
        "#include \"a.h\"\nint f(void) { return widen(0); }\n",
    )
    .expect("the source");

    let mut server = Server::start(&root);
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": {
                "textDocument": { "diagnostic": { "dynamicRegistration": true, "refreshSupport": true } },
            },
        }),
    );
    server.notify("initialized", json!({}));
    for name in ["a.c", "a.h"] {
        let path = root.join(name);
        server.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri_of(&path),
                    "languageId": language_id,
                    "version": 1,
                    "text": std::fs::read_to_string(&path).expect("the text"),
                }
            }),
        );
    }

    // A pull is answered once the file is cooked, so a few tries is the honest wait — the first may land before the
    // index has read the header at all (see `prepare_for_a_diagnostic`).
    let header_path = root.join("a.h");
    for id in 10..30 {
        std::thread::sleep(Duration::from_millis(250));
        let answer = server.request(
            id,
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri_of(&header_path) } }),
        );
        let text = answer.to_string();
        if text.contains("cstyle-cast") {
            return true;
        }
        if text.contains("\"kind\":\"full\"") {
            // A full report with no `cstyle-cast` in it: the answer this project gives.
            return false;
        }
    }
    panic!("the server never answered a full report for the header");
}

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir().join("cpp_ls-language").join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a project directory");
        Project { root }
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for Project {
    fn drop(&mut self) {
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
