//! **What the server does when the native watcher reports a storm** — asked, and answered without a reload.
//!
//! The report this exists for came from a Linux kernel checkout and its log:
//!
//! ```text
//! 332  native file watcher queue overflowed; the workspace will be rescanned
//!   4  reloading the workspace: native watcher requested a rescan
//!   4  `read a program` held the analysis for 5 020 ms, over the 50 ms budget
//!   3  request textDocument/diagnostic … took 4.9 s
//! ```
//!
//! Each reload re-opened the workspace: a compiler run, a 3 200-file index and a 361-file program read, with the
//! program's commit holding the write lock for five seconds — the window every diagnostic pull waited in. The
//! overflow itself is ordinary on a tree that a build is writing to, so the reload was both the wrong answer and the
//! expensive one.
//!
//! What this test asserts is the contract that replaced it: a watcher storm, and a directory appearing in the tree,
//! **do not reload the workspace**; they ask the disk what moved (`Session::reconcile`) and the server keeps answering.
//!
//! ```text
//! $env:CPPLS_TEST_LOG_LEVEL = "info"    (optional; the default here is info)
//! cargo test -p cpp_ls --test suite watch_rescan -- --nocapture
//! ```
//!
//! The client capabilities are the ones that route a server to its own watcher: a client that does **not** advertise
//! `workspace.didChangeWatchedFiles.dynamicRegistration`. Neovim does not, which is why the native watcher runs there.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

/// How long the first index may take. Generous: this is a debug binary and the machine may be busy.
const INDEX_TIMEOUT: Duration = Duration::from_secs(90);

/// A project directory, removed when the test ends — including when it fails.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .join("cppls-watch-rescan")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the fixture directory");
        Project { root }
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the fixture directory");
        }
        std::fs::write(&path, text).expect("the fixture writes");
        path
    }

    /// A compile database over the files that exist, so the index is the database's list rather than a walk.
    fn write_database(&self, files: &[&str]) {
        let entries: Vec<String> = files
            .iter()
            .map(|file| {
                format!(
                    r#"{{"directory": "{}", "file": "{}", "command": "cc -c {}"}}"#,
                    self.root.display(),
                    self.root.join(file).display(),
                    file
                )
            })
            .collect();
        self.write("compile_commands.json", &format!("[{}]", entries.join(",")));
    }

    fn log_file(&self) -> Option<PathBuf> {
        let directory = self.root.join("log");
        std::fs::read_dir(directory)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|extension| extension == "log"))
    }

    fn log(&self) -> String {
        self.log_file()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A server over the project's stdio, spoken to with the protocol's own framing.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Server {
    fn start(project: &Project) -> Self {
        let log_dir = project.root.join("log");
        std::fs::create_dir_all(&log_dir).expect("the log directory");
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg(&log_dir)
            .arg("--log-level")
            .arg("info")
            .current_dir(&project.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the server starts");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Server {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, message: &serde_json::Value) {
        let body = serde_json::to_string(message).expect("serializes");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len())
            .expect("the request is written");
        self.stdin.flush().expect("flushed");
    }

    /// One message, or `None` when the deadline passed first.
    fn receive_within(&mut self, within: Duration) -> Option<serde_json::Value> {
        let deadline = Instant::now() + within;
        loop {
            if Instant::now() > deadline {
                return None;
            }
            let mut length = 0usize;
            let mut line = String::new();
            loop {
                line.clear();
                if self.stdout.read_line(&mut line).ok()? == 0 {
                    return None;
                }
                let trimmed = line.trim_end();
                if trimmed.is_empty() {
                    break;
                }
                if let Some(value) = trimmed.strip_prefix("Content-Length: ") {
                    length = value.parse().ok()?;
                }
            }
            let mut body = vec![0u8; length];
            self.stdout.read_exact(&mut body).ok()?;
            let value: serde_json::Value = serde_json::from_slice(&body).ok()?;
            // Notifications (progress, log messages) arrive between the request and its answer.
            if value.get("id").is_some() {
                return Some(value);
            }
        }
    }

    fn initialize(&mut self, project: &Project) {
        let root = project
            .root
            .to_string_lossy()
            .replace('\\', "/")
            .replace(' ', "%20");
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": format!("file:///{}", root.trim_start_matches('/')),
                // **No `workspace.didChangeWatchedFiles.dynamicRegistration`**: the client does not watch, so the
                // server starts its own watcher — the configuration this test is about.
                "capabilities": {},
            },
        }));
        let answer = self
            .receive_within(Duration::from_secs(30))
            .expect("the server answers `initialize`");
        assert!(
            answer.get("result").is_some(),
            "initialize answered an error: {answer}"
        );
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {},
        }));
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// **A storm of watcher events must not reload the workspace.**
///
/// The storm is made of the two things the old code turned into a reload: more files touched at once than the watcher
/// can carry (so it reports lost events) and a **directory** appearing in the tree. Both now ask the disk what moved.
#[test]
fn a_watcher_storm_reconciles_instead_of_reloading() {
    // **The size is the measurement's** — the default keeps the test quick, and `CPPLS_WATCH_STORM_FILES=3000` is
    // what a kernel-sized tree looks like (the machine's own project was 3 200 files from a 2 978-entry database).
    let count: usize = 120;
    let project = Project::new("storm");
    let files: Vec<String> = (0..count)
        .map(|index| format!("src/f{index:03}.c"))
        .collect();
    project.write("include/base.h", "#pragma once\nint base(void);\n");
    for file in &files {
        project.write(
            file,
            "#include \"include/base.h\"\nint a_name(void) { return base(); }\n",
        );
    }
    let names: Vec<&str> = files.iter().map(String::as_str).collect();
    project.write_database(&names);

    let mut server = Server::start(&project);
    server.initialize(&project);

    // Wait for the first index to drain: that is the state the storm arrives in.
    let indexing_started = Instant::now();
    let deadline = Instant::now() + INDEX_TIMEOUT;
    while Instant::now() < deadline {
        if project.log().contains("the workspace is indexed") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        project.log().contains("the workspace is indexed"),
        "the fixture project is indexed within {INDEX_TIMEOUT:?}"
    );

    eprintln!(
        "[storm] {count} file(s) indexed in {:.2?}",
        indexing_started.elapsed()
    );

    // **The storm**: every file rewritten, a directory created with files in it, and files deleted. Written as fast as
    // the process can write them, which is what a build's output looks like to a watcher.
    // **Six rounds over more than a window (2 s)**: the reports from rounds that arrive while a reconcile is running
    // must arm the next one rather than being dropped — the folded half is the change nobody would ever read.
    for round in 0..6 {
        for (index, file) in files.iter().enumerate() {
            project.write(
                file,
                &format!(
                    "#include \"include/base.h\"\nint a_name_{round}_{index}(void) {{ return base(); }}\n"
                ),
            );
        }
        for index in 0..20 {
            project.write(
                &format!("generated/round{round}/g{index:02}.c"),
                "int generated(void) { return 0; }\n",
            );
        }
        project.write(
            "generated/round0/extra.c",
            "int extra(void) { return 0; }\n",
        );
    }
    for index in 0..20 {
        let _ = std::fs::remove_file(project.root.join(format!("src/f{index:03}.c")));
    }

    // Give the watcher its batch delay and the reconcile its window, then ask the server a real question: a server
    // that reloaded has a workspace to rebuild, and the answer is the proof it stayed alive and usable.
    std::thread::sleep(Duration::from_secs(4));
    server.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/diagnostic",
        "params": { "textDocument": { "uri": format!(
            "file:///{}/include/base.h",
            project.root.to_string_lossy().replace('\\', "/").trim_start_matches('/')
        ) } },
    }));
    let answer = server.receive_within(Duration::from_secs(60));
    server.kill();

    let log = project.log();
    let reloads: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("reloading the workspace"))
        .collect();
    for line in log
        .lines()
        .filter(|line| line.contains("reconciled") || line.contains("asking the disk"))
    {
        eprintln!("[storm] {line}");
    }
    eprintln!("[storm] reload(s): {}", reloads.len());
    let reconciles = log
        .lines()
        .filter(|line| line.contains("reconciled with the disk"))
        .count();
    eprintln!("[storm] reconcile(s): {reconciles}");
    assert!(
        reloads.is_empty(),
        "a watcher storm must not reload the workspace; the log holds {} reload(s):\n{}",
        reloads.len(),
        reloads.join("\n")
    );
    assert!(
        log.contains("asking the disk what moved") || log.contains("reconciled with the disk"),
        "the storm is answered by a reconcile instead:\n{log}"
    );
    let answer = answer.unwrap_or_else(|| panic!("the server answers after the storm:\n{log}"));
    assert!(
        answer.get("result").is_some(),
        "and the answer is a report, not an error: {answer}\n{log}"
    );
}
