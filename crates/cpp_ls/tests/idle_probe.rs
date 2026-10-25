//! **What an idle server does** — the answer used to be "a full index slice every second, for ever".
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_ls --test idle_probe -- --ignored --nocapture
//! ```
//!
//! The pump loop ended with `wait_for_work(IDLE_WAIT).await` and went round whatever the answer was, so a *timeout*
//! ran a slice: `prepare_a_wave`, `commit_a_wave`, `claim_cooking`, a pending count, a progress-bar update — all empty,
//! once a second, plus a no-progress warning every eighth. Measured from a real session's log: 339 `index slice: … 0 to
//! cook, 0 left` lines and a warning every eight seconds while nothing was happening.
//!
//! This probe counts the lines a server writes while **nothing is asked of it**: start it on the real project, wait for
//! the first index to drain, then watch for ten seconds without sending a request.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    )
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn an_idle_server_does_no_work() {
    let project = project();
    let log_dir = std::env::temp_dir().join("cppls-idle-probe");
    let _ = std::fs::remove_dir_all(&log_dir);
    let _ = std::fs::create_dir_all(&log_dir);

    let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
        .arg("--log-path")
        .arg(&log_dir)
        .arg("--log-level")
        .arg("debug")
        .current_dir(&project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // **Kept, not discarded**: a probe that starts a process and never looks at its stderr cannot say why the
        // process exited.
        .stderr(Stdio::from(
            std::fs::File::create(log_dir.join("server-stderr.log")).expect("a stderr file"),
        ))
        .spawn()
        .expect("the server starts");

    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    let root = project.to_string_lossy().replace('\\', "/");
    let root_uri = format!("file:///{}", root.trim_start_matches('/'));
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {},
        }
    });
    // **`Content-Length` framing, not JSON lines.** `lsp_server` speaks the protocol's header framing, and a bare JSON
    // line makes its reader thread give up: the server then answered `ProtocolError("disconnected channel")` and
    // exited before responding to `initialize`.
    let body = serde_json::to_string(&initialize).expect("serializes");
    write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("initialize is written");
    stdin.flush().expect("flushed");
    let mut line = String::new();
    let read = stdout.read_line(&mut line);
    eprintln!("initialize answer: read {read:?} bytes, {line:?}");
    let body = serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {},
    }))
    .expect("serializes");
    write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("initialized is written");
    stdin.flush().expect("flushed");

    // **Let the first index drain** — 40 s is generous for this project (measured: ~2.2 s cold, 1.5 s warm) and the
    // window is what the count below is about, not the start.
    std::thread::sleep(Duration::from_secs(40));

    let log = log_dir.join("E_EmmyLuaCodeStyle.log");
    let slices_before = count(&log, "index slice: prepared");
    let warnings_before = count(&log, "made no progress");

    let started = Instant::now();
    std::thread::sleep(Duration::from_secs(10));
    let slices = count(&log, "index slice: prepared") - slices_before;
    let warnings = count(&log, "made no progress") - warnings_before;
    println!(
        "10 s idle: {slices} index slice(s), {warnings} no-progress warning(s) ({:?} wall)",
        started.elapsed()
    );

    let _ = child.kill();
    let _ = child.wait();
    // **The warning is the visible half.** Without the fix an idle server ran sixteen empty slices and two
    // no-progress warnings in ten seconds (measured by stashing the fix and re-running this probe); the slice log line
    // is only written when a slice does something, which is why the count alone would have said "nothing is wrong".
    assert!(
        slices <= 2,
        "an idle server runs no slices: {slices} in ten seconds"
    );
    assert_eq!(
        warnings, 0,
        "and no no-progress warnings: {warnings} in ten seconds"
    );
}

fn count(path: &std::path::Path, needle: &str) -> usize {
    std::fs::read_to_string(path)
        .map(|text| text.lines().filter(|line| line.contains(needle)).count())
        .unwrap_or(0)
}
