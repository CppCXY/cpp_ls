//! Shared helpers for the handshake tests: one `Project`, one `Server`, and the protocol helpers every
//! test uses. The tests themselves are split by protocol area in the sibling files.

use std::io::{BufRead, BufReader, Read, Write};
pub(crate) use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
pub(crate) use std::time::{Duration, Instant};

pub(crate) use serde_json::{Value, json};

use super::fixtures::*;

/// A project directory removed when the test ends, including when it fails.
pub(crate) struct Project {
    pub(crate) root: PathBuf,
}

impl Project {
    pub(crate) fn new(name: &str) -> Project {
        let root = std::env::temp_dir()
            .join("cppls-handshake-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the fixture directory is created");

        Project { root }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn write(&self, name: &str, text: &str) {
        std::fs::write(self.root.join(name), text).expect("the fixture writes");
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub(crate) fn uri_of(path: &Path) -> String {
    // A client's own conversion, deliberately not the server's: a test that reused `util::uri` would agree with a
    // bug in it. Only what a temp path can hold is escaped (a space, which `%TEMP%` on Windows may have).
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace(' ', "%20");
    format!("file:///{}", text.trim_start_matches('/'))
}

/// **Where a cursor at the end of a needle is**, as the protocol's line and character.
///
/// The offset *past* the needle — `full.` puts the cursor after the operator, which is where a reader who has just
/// typed it is. A test that states a position by hand states it wrong sooner or later, and the failure is silent:
/// the request still gets an answer, about a position nobody is looking at. Counting the line(s) and characters of
/// the fixture is a line of code and cannot drift away from the text it is about.
pub(crate) fn position_of(source: &str, needle: &str) -> (u32, u32) {
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

/// **How many servers may run at once**, and the permit that says so.
///
/// Every handshake test spawns its own server, and `cargo test` runs them on every core at once: on a loaded machine
/// a dozen debug servers each index their fixture closure while the rest of the workspace builds, and a request that
/// takes five seconds alone waits long enough to miss a deadline. Measured:
/// `handshake::diagnostics::the_reported_file_is_answered` failed one full-workspace run in three and passed alone in
/// 5.4 s. A cap costs nothing when the machine is idle — a waiting test is one that would have contended — and turns
/// a load-dependent timeout into an ordered run.
const AT_ONCE: usize = 6;

static RUNNING: Mutex<usize> = Mutex::new(0);
static ROOM: std::sync::Condvar = std::sync::Condvar::new();

/// One of the [`AT_ONCE`] places, given back when the server it belongs to is dropped.
struct APermit;

impl APermit {
    fn take() -> Self {
        let mut running = RUNNING.lock().expect("the count is not poisoned");
        while *running >= AT_ONCE {
            running = ROOM.wait(running).expect("the count is not poisoned");
        }
        *running += 1;
        APermit
    }
}

impl Drop for APermit {
    fn drop(&mut self) {
        let mut running = RUNNING.lock().expect("the count is not poisoned");
        *running -= 1;
        ROOM.notify_one();
    }
}

/// The server, as a client sees it: a process, a pipe, and the messages it sent.
pub(crate) struct Server {
    pub(crate) child: Child,
    pub(crate) stdin: ChildStdin,
    pub(crate) messages: Receiver<Value>,
    /// What the server logged, kept so that a failure says *why* rather than only that.
    pub(crate) log: Arc<Mutex<String>>,
    /// **How long one request waits**, which a test whose subject reads the standard library raises — see
    /// [STD_LIBRARY_TIMEOUT] and [Server::start_within].
    pub(crate) within: Duration,
    /// **The `workspace/configuration` requests the server made**, by params. Kept because the *scope* the server asks
    /// under is part of the answer and `null` — what an unconfigured client replies — says nothing about it.
    pub(crate) configuration_requests: Vec<Value>,
    /// **How many of those the client has answered** — the count a sequence test has to look at, because a request the
    /// server sent is not a setting the server has applied.
    pub(crate) configuration_answers: usize,
    /// **Configuration requests the test is holding**, read but not answered — see
    /// [`Server::read_holding_the_configuration`].
    pub(crate) held_configuration_requests: Vec<Value>,
    /// **Answer `client/registerCapability` with an error**, the way a client that advertises the capability and then
    /// refuses the registration does.
    pub(crate) refuse_registrations: bool,
    /// **A place among the [`AT_ONCE`] servers**, given back when this one is dropped — see [`APermit`].
    _permit: APermit,
}

impl Server {
    pub(crate) fn start(root: &Path) -> Server {
        Server::start_within(root, TIMEOUT)
    }

    /// **A server started with arguments a user would pass** — `--config` above all, whose whole point is a path no
    /// convention predicts.
    pub(crate) fn start_with_args(root: &Path, arguments: &[&std::ffi::OsStr]) -> Server {
        Server::start_within_and_args(root, TIMEOUT, arguments)
    }

    /// [Server::start] with the caller's own per-request deadline, for a test whose first query waits for a
    /// standard-library closure to be read.
    pub(crate) fn start_within(root: &Path, within: Duration) -> Server {
        Server::start_within_and_args(root, within, &[])
    }

    /// Both of the above.
    pub(crate) fn start_within_and_args(
        root: &Path,
        within: Duration,
        arguments: &[&std::ffi::OsStr],
    ) -> Server {
        // **The server's own level and log path, under the test's control.** Both are command-line flags
        // (`--log-level`, default `info`; `--log-path`, default `none`) rather than `RUST_LOG`, so a test could not
        // ask the server to say what it did — and the first attempt to diagnose the handshake failures ended exactly
        // there: the trace line exists in the handler and the level dropped it. Run this suite with
        // `CPPLS_TEST_LOG_LEVEL=debug` and `CPPLS_TEST_LOG_PATH=<file>` when a request has to be watched.
        let mut command = Command::new(env!("CARGO_BIN_EXE_cpp_ls"));
        // **Unless the test named one itself** — --log-path may not be given twice, and a test that reads the log file
        // has to know where it is.
        if !arguments
            .iter()
            .any(|argument| *argument == std::ffi::OsStr::new("--log-path"))
        {
            command.arg("--log-path").arg("none");
        }
        // **The level the suite was told to use** — `CPPLS_TEST_LOG_LEVEL=debug` and `CPPLS_TEST_LOG_PATH=<file>`
        // are what the note above promises, and this line used to read the level from a *literal*: the variable was
        // never consulted, so the one workflow the comment describes (watch a request that does not arrive) could
        // not be run at all. `info` stays the default.
        let level = std::env::var("CPPLS_TEST_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
        command.arg("--log-level").arg(&level);
        // **A file the test can read afterwards**, when the environment names one: a failure in CI then carries the
        // server`s own account of what it was doing instead of a bare timeout.
        if let Ok(path) = std::env::var("CPPLS_TEST_LOG_PATH") {
            command.arg("--log-path").arg(path);
        }
        for argument in arguments {
            command.arg(argument);
        }
        let mut child = command
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
                sink.lock()
                    .expect("the log is not poisoned")
                    .push_str(&line);
                line.clear();
            }
        });

        Server {
            child,
            stdin,
            messages,
            log,
            within,
            configuration_requests: Vec::new(),
            configuration_answers: 0,
            held_configuration_requests: Vec::new(),
            refuse_registrations: false,
            _permit: APermit::take(),
        }
    }

    pub(crate) fn send(&mut self, message: Value) {
        let body = serde_json::to_string(&message).expect("the message serializes");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len())
            .expect("the message is written");
        self.stdin.flush().expect("the message is flushed");
    }

    pub(crate) fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Send a request and wait for its response, answering anything the server asks on the way.
    pub(crate) fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        self.wait_for_within(self.within, |message| message["id"] == json!(id))
    }

    /// Send a request until its answer is not `null`, or give up.
    ///
    /// What the lazy index needs: a query that arrives before the file has been read answers "not yet", and the
    /// test's job is to assert the *answer*, not the schedule. Each attempt gets its own id, so a retry can never
    /// be confused with the answer to the attempt before it.
    pub(crate) fn ask_until(&mut self, first_id: i64, build: impl Fn(i64) -> Value) -> Value {
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

    /// **Ask for a file's diagnostics, the way a client does** — `textDocument/diagnostic`, the pull half.
    ///
    /// The server used to publish these; it does not any more, and every test that used to wait for a
    /// `publishDiagnostics` notification asks here instead. That is not a test convenience: it is the same request a
    /// client makes, and going through it is what makes these tests exercise the path a user's editor takes.
    ///
    /// `first_id` is the caller's, so two pulls in one test cannot be confused with each other. The answer must be a
    /// **report** — the `items` array — because a `null` result is what the handler used to return and no client can
    /// act on it.
    pub(crate) fn pull_diagnostics(&mut self, first_id: i64, uri: &str) -> Vec<Value> {
        let report = self.ask_until(first_id, |id| {
            json!({
                "id": id,
                "method": "textDocument/diagnostic",
                "params": { "textDocument": { "uri": uri } },
            })
        });
        report["result"]["items"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("a pull request is answered with a report, not `null`: {report}")
            })
            .clone()
    }

    /// [`Server::ask_until`] for a request whose answer can arrive **worth having but not yet complete**: keeps
    /// asking until the closure accepts one.
    ///
    /// A completion is the request that needs this. Its answer before the project has been read is an **empty
    /// list** — not `null`, which is why [`Server::ask_until`] would take it — and the protocol's own answer to
    /// that is `isIncomplete: true`, which tells the client to ask again as the user types. This is that loop, so
    /// a test asserts what a well-behaved client would eventually see rather than what a fast one happens to get.
    pub(crate) fn ask_until_it(
        &mut self,
        first_id: i64,
        build: impl Fn(i64) -> Value,
        accept: impl Fn(&Value) -> bool,
    ) -> Value {
        self.ask_until_it_within(first_id, TIMEOUT, build, accept)
    }

    /// [`Server::ask_until_it`] with the caller's own deadline, for a test whose subject is genuinely slow.
    pub(crate) fn ask_until_it_within(
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
                panic!(
                    "no answer the test accepts within {within:?}; the last one was: {response}"
                );
            }

            id += 1;
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait for a message the closure accepts, answering the server's own requests while waiting.
    pub(crate) fn wait_for(&mut self, accept: impl Fn(&Value) -> bool) -> Value {
        self.wait_for_within(self.within, accept)
    }

    /// **Read what the server sends for `within`, answering everything except `workspace/configuration`.**
    ///
    /// The configuration requests are kept in [`Server::held_configuration_requests`] for the caller to answer when it
    /// decides to. That is what lets a test tell two situations apart that `wait_for` blurs: the server had not
    /// **answered** the request yet, versus the client had not **answered the settings** yet. In the first the server is
    /// waiting on the settings; in the second it is not. Every other server request is answered as usual, so a progress
    /// token or a registration cannot stall the run.
    pub(crate) fn read_holding_the_configuration(&mut self, within: Duration) -> Vec<Value> {
        let deadline = Instant::now() + within;
        let mut read = Vec::new();

        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let message = match self
                .messages
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => {
                    // Once the settings have been asked for, and nothing else arrives, this is the state the test
                    // wants to look at: the server is waiting for them.
                    if !self.held_configuration_requests.is_empty() {
                        break;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => panic!("the server closed its stdout"),
            };

            if message.get("method").is_some() && message.get("id").is_some() {
                if message["method"] == json!("workspace/configuration") {
                    self.held_configuration_requests.push(message);
                } else {
                    self.answer(&message);
                }
                continue;
            }
            read.push(message);
        }

        read
    }

    /// Answer the configuration requests [`Server::read_holding_the_configuration`] kept.
    pub(crate) fn answer_the_held_configuration(&mut self) {
        for request in std::mem::take(&mut self.held_configuration_requests) {
            self.answer(&request);
        }
    }

    /// [`Server::wait_for`] with the caller's own deadline.
    pub(crate) fn wait_for_within(
        &mut self,
        within: Duration,
        accept: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = Instant::now() + within;

        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("the server did not answer within {within:?}"));

            let message = match self.messages.recv_timeout(remaining) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => {
                    panic!("the server did not answer within {within:?}")
                }
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
    pub(crate) fn answer(&mut self, request: &Value) {
        let id = request["id"].clone();
        let method = request["method"].as_str().unwrap_or_default();
        // **A registration the client refuses**, answered the way a client that does not implement the request does.
        if method == "client/registerCapability" && self.refuse_registrations {
            self.send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "method not found" },
            }));
            return;
        }
        let result = match method {
            // One `null` per requested item: "the user has configured nothing", which is what an unconfigured
            // client says and what the server's own defaults are for.
            "workspace/configuration" => {
                self.configuration_requests.push(request["params"].clone());
                // Counted **as answered**, after any delay the test asked for: a request in flight is not a setting in
                // force, which is exactly the distinction the sequence test is about.
                self.configuration_answers += 1;
                json!([Value::Null])
            }
            _ => Value::Null,
        };

        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    pub(crate) fn wait_for_exit(&mut self) -> bool {
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
pub(crate) fn read_messages(stdout: impl Read, tx: std::sync::mpsc::Sender<Value>) {
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
/// A hover whose position is the first byte of `needle` in the fixture — for a member name, which
/// [`hover_over`] cannot address (it hovers at the start of the line).
/// A hover whose position is `into` bytes into the **last** occurrence of `needle` — for a member name, which
/// [`hover_over`] cannot address (it hovers at the start of the line), searched from the end so a declaration
/// carrying the same text does not win.
pub(crate) fn hover_at(
    server: &mut Server,
    first_id: i64,
    uri: &str,
    fixture: &str,
    needle: &str,
    into: usize,
) -> Value {
    let at = fixture.rfind(needle).expect("the fixture writes it") + into;
    let before = &fixture[..at];
    let line: u64 = before
        .matches('\n')
        .count()
        .try_into()
        .expect("a fixture this size fits in a u32");
    let character: u64 = before
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .count()
        .try_into()
        .expect("a line this size fits in a u32");
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

/// A hover whose position is the first character of the line containing `needle`.
pub(crate) fn hover_over(
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
pub(crate) fn line_of(text: &str, needle: &str) -> u64 {
    text.lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not in the fixture"))
        .try_into()
        .expect("a fixture this size fits in a u32")
}
/// A completion answer, whether the server sent a list or a bare array.
pub(crate) fn completion_items(answer: &Value) -> Vec<Value> {
    if let Some(items) = answer["result"]["items"].as_array() {
        return items.clone();
    }
    answer["result"].as_array().cloned().unwrap_or_default()
}
