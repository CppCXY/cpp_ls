//! **How long a request waits, asserted rather than hoped for.**
//!
//! Every other test in this crate asks whether an answer is *right*. This one asks whether it arrives **while
//! somebody is typing**, which is the only property a person notices — the report that produced this file was
//! *"I type `s` and the completion takes forever"*, and it was true: measured over the wire on one open file, the
//! first four completions came back in 1623 / 1049 / 2552 / 685 ms.
//!
//! # Why a bound and not a benchmark
//!
//! A benchmark says what the server did on one machine on one day. A bound says what it **may** do — and making that
//! assertion is what turns "we should keep it fast" into something a change can fail. The number is deliberately
//! generous: this test is not looking for 20 ms, it is looking for a *second*, and everything it has caught so far
//! was between 700 ms and 5.8 s.
//!
//! # What the bound is a bound on
//!
//! A query and a background update take the same lock, so the server cannot hide work in the background: whatever an
//! update holds the session for, a request waits for. `AnalysisState::worst_write_hold` is that number, reported by
//! the server itself, and this asserts on **both** — the hold and the request — because they failed separately:
//!
//! ```text
//!   a lock held for 5.8 s            the request waits, and the request's own work is irrelevant
//!   a lock held for 12 ms, request 2 s   the wait is inside the request, and no lock change will help
//! ```
//!
//! # Run it with `--release`, or it is not measuring the product
//!
//! `env!("CARGO_BIN_EXE_cpp_ls")` is the **profile the test was built in**, so `cargo test` runs the server with no
//! optimisations and every number below is 10–50× what a user sees. Measured, both files of each:
//!
//! ```text
//!   cargo test           #0 4211 ms … #5–#11  902–1081 ms   the closure is walked unoptimised
//!   cargo test --release #0  607 ms … #5–#11    4–5 ms      what the server actually does
//! ```
//!
//! The budgets are chosen from the *release* numbers (`STEADY_BUDGET` is 50 ms, against a measured 4 ms), so a debug
//! run fails on arithmetic rather than on behaviour and says nothing about a change. This is the same rule
//! `tests/handshake.rs` states for its own `#[ignore]`d cases.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// **The bound a request waits for the background**, in milliseconds.
///
/// Chosen from what the background work is and what a person tolerates, in that order: an update *inserts* what a
/// step produced, and the insert half of an index slice measured 8–19 ms on this project. A second is two orders of
/// magnitude of headroom, so a failure here is not a slow machine — it is a closure that did something it should
/// have done outside the lock.
const UPDATE_BUDGET: Duration = Duration::from_millis(1000);

/// **The bound a completion takes while the index is still being filled**, in milliseconds.
///
/// Larger than `UPDATE_BUDGET` on purpose: a completion's own work — resolving the scope, listing the members,
/// reading what the index has — is real and is not what this test is about. What it is about is the *shape*: a
/// request that waits for the indexer grows with the project, and a request that does its own work does not.
///
/// # The two regimes, and which one this bound is for
///
/// Measured, the twelve attempts below fall into two groups and they have different causes:
///
/// ```text
///   the first four     300–1600 ms   the query's own work, while the index is being written to
///   from the fifth on  4–10 ms       the same query against an index that has stopped changing
/// ```
///
/// The first group is **not** queueing any more — a completion's three write-lock acquisitions cost 0 ms each once
/// `AnalysisState::prepare` and `AnalysisState::catch_up` ask whether they have anything to do first, and the pump
/// holds the write lock for 0–25 ms a slice. It is the query's own work, and the cause is recorded where it lives:
/// `ProjectIndex::visibility_answers` is cleared whole by every insert, so each completion re-evaluates every
/// guarded `#include` in its closure while the pump is inserting. Fixing that is the next round's work; the
/// steady-state bound below is what must not regress while it is done.
const COMPLETION_BUDGET: Duration = Duration::from_millis(2500);

/// **The bound a completion takes once the index has settled**, in milliseconds.
///
/// The doc's own keystroke budget, and the number `docs/latency.md` §1 records for the tenth request: *"steady
/// state, tenth request — 9 ms"*. It is asserted separately from `COMPLETION_BUDGET` because the two fail for
/// different reasons and a single loose bound cannot tell them apart — a regression that made every completion a
/// hundred milliseconds would pass a 2500 ms bound and must not.
///
/// Measured on this fixture: 4–10 ms, against 353–1479 ms before the write-lock work of this round.
const STEADY_BUDGET: Duration = Duration::from_millis(50);

/// How many attempts must be inside `STEADY_BUDGET`. The index in this fixture takes about six seconds to fill, so
/// by the tenth request the pump has been quiet for a while — which is exactly the state the number is about.
const STEADY_FROM: usize = 9;

/// A file whose include closure is large enough that the index is still filling while the requests arrive.
///
/// This is the user's own file, near enough: the same four standard headers, the same class with one member, the
/// same calls. A three-line fixture is indexed before the first request lands and would assert nothing.
const MAIN_CPP: &str = r#"#include <vector>
#include <string>
#include <format>
#include <iostream>

class Sux {
public:
    Sux() : data(0) {}
    Sux(int value) : data(value) {}

    void print(int i) {
        printf("Sux class\n %d\n", this->data);
    }
private:
    int data;
};

int main() {
    Sux sux;
    std::string ixx;
    sux.print(ixx.size());
    std::cout << std::format("{}", 0) << std::endl;
    std::vector<int> v;
    v.push_back(42);
    std::string s = std::format("{}", 1);
    auto x = std::vector<int>();
    return 0;
}
"#;

/// **A completion asked while the index is filling comes back inside the budget.**
///
/// The requests start the instant `didOpen` is acknowledged and continue for as long as the server is behind — which
/// is exactly the window the report was about, and the window a "wait until the workspace is indexed" test never
/// looks at.
#[test]
fn a_completion_asked_while_the_index_fills_comes_back_within_the_budget() {
    let project = Project::new("latency");
    project.write("main.cpp", MAIN_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "window": { "workDoneProgress": true } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    // **`std::format`'s line**, which is where a person types `std::` and waits.
    let (line, character) = position_of(MAIN_CPP, "std::cout");

    let mut slowest = Duration::ZERO;
    let mut over: Vec<String> = Vec::new();
    let mut every: Vec<String> = Vec::new();
    let mut steady: Vec<Duration> = Vec::new();

    for attempt in 0..12u32 {
        let started = Instant::now();
        let response = server.request(
            100 + i64::from(attempt),
            "textDocument/completion",
            json!({
                "textDocument": { "uri": main_uri },
                "position": { "line": line, "character": character },
            }),
        );
        let took = started.elapsed();
        slowest = slowest.max(took);

        let items = response["result"]["items"]
            .as_array()
            .or_else(|| response["result"].as_array())
            .map(|items| items.len())
            .unwrap_or(0);
        // **Every number, not only the bad ones.** The bound is what the test fails on, but the *shape* — the first
        // attempt against the twelfth — is what the feature is judged by, and a run that reports only its failures
        // cannot show a fix that moved the first four from 1.6 s to 40 ms.
        eprintln!(
            "  completion #{attempt}: {} ms, {items} item(s)",
            took.as_millis()
        );
        every.push(format!("#{attempt}: {} ms, {items} item(s)", took.as_millis()));
        if attempt as usize >= STEADY_FROM {
            steady.push(took);
        }
        if took > COMPLETION_BUDGET {
            over.push(format!("  #{attempt}: {} ms, {items} item(s)", took.as_millis()));
        }
    }

    assert!(
        over.is_empty(),
        "a completion asked while the index fills must come back inside {budget} ms — every request waits for \
         whatever an update holds the session for, and the numbers below are the wait:\n{list}\n\
         (slowest {slowest} ms; every attempt was: {every})",
        budget = COMPLETION_BUDGET.as_millis(),
        list = over.join("\n"),
        slowest = slowest.as_millis(),
        every = every.join(", "),
    );

    // **And the steady state, at the number a keystroke is actually budgeted.** Asserted apart from the bound above
    // because the two fail for different reasons: a completion that is slow while the index fills has one cause
    // (`ProjectIndex::visibility_answers` is cleared by every insert), and one slow *after* it has settled has
    // another, and only the second is a regression in what this round of work achieved.
    let slow_steady: Vec<String> = steady
        .iter()
        .enumerate()
        .filter(|(_, took)| **took > STEADY_BUDGET)
        .map(|(at, took)| format!("  #{}: {} ms", STEADY_FROM + at, took.as_millis()))
        .collect();
    assert!(
        slow_steady.is_empty(),
        "once the index has settled a completion must come back inside {budget} ms — that is `docs/latency.md`'s \
         own steady-state number (\"steady state, tenth request: 9 ms\") and the one a keystroke is budgeted \
         against:\n{list}\n(every attempt was: {every})",
        budget = STEADY_BUDGET.as_millis(),
        list = slow_steady.join("\n"),
        every = every.join(", "),
    );
}

/// **And the server's own worse number — how long an update held the analysis — is inside its own budget.**
///
/// The test above can only see the sum. This one reads the part of it the server measures on the inside: if the
/// hold is small and the requests were still slow, the fix is in the query; if the hold is large, no query change
/// helps. Both are failures, but they are different failures, and only one of them is about a lock.
#[test]
fn no_update_holds_the_analysis_for_longer_than_its_budget() {
    let project = Project::new("latency-hold");
    project.write("main.cpp", MAIN_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "window": { "workDoneProgress": true } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    // **The server says when it is done, and says the number with it.** Waiting for the line is what makes this a
    // measurement of the whole indexing pass rather than of its first slice.
    let (line, character) = position_of(MAIN_CPP, "return 0;");
    let deadline = Instant::now() + Duration::from_secs(180);

    loop {
        let _ = server.request(
            200,
            "textDocument/completion",
            json!({
                "textDocument": { "uri": main_uri },
                "position": { "line": line, "character": character },
            }),
        );

        // The line is written once the queue is empty, and the number on it is the worst hold of the whole pass.
        if let Some(held) = server.worst_write_hold() {
            assert!(
                held <= UPDATE_BUDGET.as_millis() as u64,
                "an update held the analysis for {held} ms, over the {} ms budget — every request in flight waits \
                 for exactly this, so no amount of query tuning can make a completion faster than this number",
                UPDATE_BUDGET.as_millis(),
            );
            return;
        }

        assert!(
            Instant::now() < deadline,
            "the server never finished indexing, so it never reported how long an update held the analysis"
        );
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The harness. Duplicated from `handshake.rs` rather than shared, because an integration test is a separate binary
// and a `tests/common` module would have to be a crate of its own to be usable from both — and what is here is
// forty lines of process plumbing, not behaviour.
// ---------------------------------------------------------------------------------------------------------------

fn uri_of(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    format!("file:///{}", text.trim_start_matches('/'))
}

fn position_of(source: &str, needle: &str) -> (u32, u32) {
    let at = source.find(needle).expect("the needle is in the fixture");
    let before = &source[..at];
    let line = before.matches('\n').count() as u32;
    let character = before.rsplit('\n').next().map(|last| last.len()).unwrap_or(0) as u32;
    (line, character)
}

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir().join(format!("cppls-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the test project");
        Project { root }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.root.join(name), text).expect("the fixture file");
    }
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    /// Where the server writes its log — see [`Server::worst_write_hold`].
    log_dir: PathBuf,
}

impl Server {
    fn start(root: &Path) -> Server {
        // **The log goes to a file, because the test reads a number out of it.** `AnalysisState::update` reports the
        // longest it held the analysis, and that number is the one the bound is really about — the requests can only
        // see the sum. `--log-path none` would be quieter; it would also throw away the measurement.
        let log_dir = root.join("log");
        let _ = std::fs::create_dir_all(&log_dir);

        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg(&log_dir)
            .arg("--log-level")
            .arg("debug")
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
            log_dir,
        }
    }

    /// **The longest an update held the analysis**, as the server itself measured it — `None` if it has not said yet.
    ///
    /// Read from the log rather than asked for: there is no request for it, and the line is written when the
    /// indexing pass ends, which is the moment this test is about.
    fn worst_write_hold(&self) -> Option<u64> {
        let entry = std::fs::read_dir(&self.log_dir).ok()?.flatten().next()?;
        let text = std::fs::read_to_string(entry.path()).ok()?;
        text.lines()
            .filter_map(|line| line.split_once("held the analysis was "))
            .filter_map(|(_, rest)| rest.split_whitespace().next())
            .filter_map(|value| value.parse::<u64>().ok())
            .max()
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_string(&message).expect("a message serializes");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("the message is written");
        self.stdin.flush().expect("the message is flushed");
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        self.wait_for(|message| message["id"] == json!(id))
    }

    fn wait_for(&mut self, accept: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.messages.recv_timeout(left) {
                Ok(message) => {
                    if accept(&message) {
                        return message;
                    }
                }
                Err(err) => panic!("the server did not answer: {err}"),
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

/// `Read` is needed by `read_exact` above and is not otherwise named.
#[allow(unused_imports)]
use std::io::Read as _;
