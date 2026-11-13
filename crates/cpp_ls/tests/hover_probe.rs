//! **One hover, over the wire** — a position in a real file, asked of the real server, printed.
//!
//! ```text
//! $env:CPPLS_HOVER_PROJECT = "C:\Users\zc\Desktop\neovim_test\neovim"
//! $env:CPPLS_HOVER_FILE = "src/nvim/decoration.c"
//! $env:CPPLS_HOVER_LINE = "1040"
//! $env:CPPLS_HOVER_COL = "21"
//! cargo test --release -p cpp_ls --test hover_probe -- --ignored --nocapture
//! ```
//!
//! The file is opened, one diagnostic is pulled (which is what cooks it), and then the hover is asked for. The
//! answer is printed verbatim: a probe is a measurement, not an assertion.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn environment(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn project() -> PathBuf {
    environment("CPPLS_HOVER_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\neovim_test\neovim"))
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
            .arg("none")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
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
        let deadline = Instant::now() + Duration::from_secs(240);
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

#[test]
#[ignore = "a probe: needs a real project and a position"]
fn a_hover_over_the_wire() {
    let root = project();
    let relative = environment("CPPLS_HOVER_FILE").expect("CPPLS_HOVER_FILE");
    let line: u32 = environment("CPPLS_HOVER_LINE")
        .expect("CPPLS_HOVER_LINE")
        .parse()
        .expect("a line number");
    let column: u32 = environment("CPPLS_HOVER_COL")
        .expect("CPPLS_HOVER_COL")
        .parse()
        .expect("a column");
    let file = root.join(&relative);
    let text = std::fs::read_to_string(&file).expect("the file is here");
    let uri = uri_of(&file);

    let mut client = Client::start(&root);
    client.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": {
                "textDocument": { "hover": { "contentFormat": ["markdown"] } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    client.notify("initialized", json!({}));
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "c", "version": 1, "text": text }
        }),
    );
    // The pull is what reads the file as a compiler would; without it there is no rendering to fall back to.
    client.request(
        2,
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    // **The cook is what puts the rendering in the cache**, and it runs on the pump: a hover straight after the
    // pull can arrive before it. `CPPLS_HOVER_SETTLE_MS` says how long to give it.
    let settle: u64 = environment("CPPLS_HOVER_SETTLE_MS")
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_000);
    std::thread::sleep(Duration::from_millis(settle));
    let answer = client.request(
        3,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line - 1, "character": column },
        }),
    );
    eprintln!("[hover] {relative}:{line}:{column} -> {answer}");
}
