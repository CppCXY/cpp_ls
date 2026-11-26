//! **Which formatter the server runs, and what it says when the one it was told about is not there.**
//!
//! Read back off the wire, from a real server process, because the interesting half of this feature is the *choice*:
//! `[format] formatter` decides between a library call and a child process, and neither the fallback nor the warning
//! is visible from a unit test.
//!
//! Three configurations, one fixture:
//!
//! ```text
//! cpp_formatter          the built-in formatter formats the file, and nothing is said
//! a program on PATH      its stdout is the answer — measured against real clang-format, when there is one
//! a program that is not   the built-in answer *and* one warning that says so — once, not once per save
//! ```
//!
//! The third is the one the configuration's default depends on: `clang-format` is what a project gets without writing
//! anything, so a machine without clang-format has to end up with working formatting and one honest message rather
//! than a format-on-save that quietly stops.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// **A project on disk, its own configuration, and a server started in it.**
struct Fixture {
    root: PathBuf,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Everything the server sent that was not a response — `window/showMessage` is the one this test reads.
    notifications: Vec<Value>,
    opened: bool,
}

impl Fixture {
    /// A project whose `.cppls.toml` says `formatter = <configured>`, with one file that is not formatted.
    fn new(name: &str, configured: Option<&str>) -> Fixture {
        let root = std::env::temp_dir().join(format!("cppls-formatter-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a project directory");
        let mut configuration = String::new();
        if let Some(configured) = configured {
            configuration = format!("[format]\nformatter = {configured:?}\n");
        }
        std::fs::write(root.join(".cppls.toml"), configuration).expect("the configuration writes");
        std::fs::write(
            root.join("main.cpp"),
            "int  main( ) {\nint x=1;\nreturn x;\n}\n",
        )
        .expect("the source writes");

        let log_dir = root.join("logs");
        std::fs::create_dir_all(&log_dir).expect("a log directory");
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg(&log_dir)
            .arg("--log-level")
            .arg("debug")
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the server starts");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Fixture {
            root,
            child,
            stdin,
            stdout,
            notifications: Vec::new(),
            opened: false,
        }
    }

    fn file(&self) -> PathBuf {
        self.root.join("main.cpp")
    }

    fn uri(&self) -> String {
        let mut text = self.file().to_string_lossy().replace('\\', "/");
        if !text.starts_with('/') {
            text.insert(0, '/');
        }
        format!("file://{text}")
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_string(&message).expect("serializes");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("written");
        self.stdin.flush().expect("flushed");
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            assert!(
                Instant::now() < deadline,
                "the server did not answer {method}"
            );
            let message = self.read().unwrap_or_else(|| {
                panic!("the server closed before answering {method}");
            });
            if message["id"] == json!(id) {
                return message;
            }
            if message.get("method").is_some() {
                self.notifications.push(message);
            }
        }
    }

    fn read(&mut self) -> Option<Value> {
        let mut length = 0usize;
        loop {
            let mut header = String::new();
            if self.stdout.read_line(&mut header).ok()? == 0 {
                return None;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some(rest) = header.strip_prefix("Content-Length: ") {
                length = rest.trim().parse().ok()?;
            }
        }
        let mut body = vec![0u8; length];
        self.stdout.read_exact(&mut body).ok()?;
        serde_json::from_slice(&body).ok()
    }

    /// **Start the server and open the file** — the two steps every case needs before formatting.
    fn open(&mut self) {
        let root_uri = format!("file:///{}", self.root.to_string_lossy().replace('\\', "/"));
        let answer = self.request(
            1,
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {},
            }),
        );
        assert!(
            answer.get("result").is_some(),
            "the server initialized: {answer}"
        );
        self.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
        let text = std::fs::read_to_string(self.file()).expect("the fixture reads");
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": self.uri(),
                "languageId": "cpp",
                "version": 1,
                "text": text,
            }},
        }));
        self.opened = true;
    }

    /// One `textDocument/formatting` request, answered as the edited text — which is what a client does with the
    /// edit list.
    fn format(&mut self, id: i64) -> String {
        let answer = self.request(
            id,
            "textDocument/formatting",
            json!({
                "textDocument": { "uri": self.uri() },
                "options": { "tabSize": 4, "insertSpaces": true },
            }),
        );
        let edits = answer["result"]
            .as_array()
            .unwrap_or_else(|| panic!("a formatting answer: {answer}"));
        apply(&self.file(), &mut self.notifications, edits)
    }

    /// One `textDocument/rangeFormatting` request over whole lines, from `first` to `last` (0-based, `last`
    /// exclusive — the protocol's own convention).
    fn format_range(&mut self, id: i64, first: u32, last: u32) -> String {
        let answer = self.request(
            id,
            "textDocument/rangeFormatting",
            json!({
                "textDocument": { "uri": self.uri() },
                "range": {
                    "start": { "line": first, "character": 0 },
                    "end": { "line": last, "character": 0 },
                },
                "options": { "tabSize": 4, "insertSpaces": true },
            }),
        );
        let edits = answer["result"]
            .as_array()
            .unwrap_or_else(|| panic!("a range formatting answer: {answer}"));
        apply(&self.file(), &mut self.notifications, edits)
    }

    /// The warnings the server sent, in the order they arrived.
    fn warnings(&mut self) -> Vec<String> {
        // Anything still in the pipe belongs to these requests: drain what is there without waiting for more.
        while let Some(message) = self.read_while_ready() {
            self.notifications.push(message);
        }
        self.notifications
            .iter()
            .filter(|message| message["method"] == json!("window/showMessage"))
            .filter(|message| message["params"]["type"] == json!(2))
            .map(|message| {
                message["params"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    /// A read that does not block for long: the response has already arrived, so anything else is on its way and a
    /// server that sent nothing leaves the pipe empty rather than making the test wait.
    fn read_while_ready(&mut self) -> Option<Value> {
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            if !self.stdout.buffer().is_empty() {
                return self.read();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// **Apply a `TextEdit` list to the file's text**, the way a client does — the point being that the test reads the
/// *result*, not the shape of the edit.
fn apply(_path: &Path, _notifications: &mut Vec<Value>, edits: &[Value]) -> String {
    let mut text = std::fs::read_to_string(_path).expect("the fixture reads");
    // LSP positions are (line, character) and our edits are whole lines, so this is a line splice — which is also
    // what makes the answer readable in a failure message.
    for edit in edits {
        let range = &edit["range"];
        let first = range["start"]["line"].as_u64().unwrap_or(0) as usize;
        let last = range["end"]["line"].as_u64().unwrap_or(0) as usize;
        let replacement = edit["newText"].as_str().unwrap_or_default();
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let mut rebuilt = String::new();
        for line in lines.iter().take(first) {
            rebuilt.push_str(line);
        }
        rebuilt.push_str(replacement);
        for line in lines.iter().skip(last) {
            rebuilt.push_str(line);
        }
        text = rebuilt;
    }
    text
}

#[test]
fn the_built_in_formatter_is_what_cpp_formatter_is_named() {
    let mut fixture = Fixture::new("built-in", Some("cpp_formatter"));
    fixture.open();
    let formatted = fixture.format(2);
    assert_ne!(
        formatted, "int  main( ) {\nint x=1;\nreturn x;\n}\n",
        "the file was formatted"
    );
    assert!(
        fixture.warnings().is_empty(),
        "the built-in formatter has nothing to warn about: {:?}",
        fixture.warnings()
    );
}

/// **A program that is not on the machine still formats the file**, and says so exactly once.
///
/// This is the shape of the default configuration on a machine without clang-format, and the two halves matter
/// equally: formatting working (the fallback) and the reader being told (the warning) — once, because a
/// format-on-save would otherwise open a notification on every save.
#[test]
fn a_missing_program_falls_back_to_the_built_in_formatter_and_says_so_once() {
    let mut fixture = Fixture::new(
        "missing",
        Some("a-formatter-that-is-not-on-this-machine-anywhere"),
    );
    fixture.open();

    let first = fixture.format(2);
    assert_ne!(
        first, "int  main( ) {\nint x=1;\nreturn x;\n}\n",
        "the built-in formatter answered the request"
    );

    // A second request must not produce a second warning.
    let second = fixture.format(3);
    assert_eq!(first, second, "both requests are formatted the same way");

    let warnings = fixture.warnings();
    assert_eq!(
        warnings.len(),
        1,
        "one warning for the missing program, not one per request: {warnings:?}"
    );
    assert!(
        warnings[0].contains("a-formatter-that-is-not-on-this-machine-anywhere"),
        "the warning names what the configuration asked for: {warnings:?}"
    );
    assert!(
        warnings[0].contains("cpp_formatter"),
        "and what answered instead: {warnings:?}"
    );
}

/// **The external path, against the real program** — a probe, because it needs a clang-format to run.
///
/// ```text
/// $env:CPPLS_CLANG_FORMAT = "C:\Program Files\LLVM\bin\clang-format.exe"   # or leave it to PATH
/// cargo test --release -p cpp_ls --test suite formatter_choice -- --ignored --nocapture
/// ```
///
/// It asserts the two things a probe can assert exactly: **the server's answer is clang-format's own answer** for the
/// same file (same `--assume-filename`, so the same `.clang-format` is found), and a **range** format answers what
/// clang-format answers for `--lines`, which is what keeps the edit inside the selection. The built-in formatter's
/// answer is printed beside them, because the difference between the two is the reason the default is clang-format.
#[test]
#[ignore = "a probe: it runs a real clang-format"]
fn the_real_clang_format_answers_and_keeps_a_range_to_its_lines() {
    use cpp_ls::handlers::formatting::formatter::{clang_format_arguments, is_available};

    let configured = Option::<String>::None
        .or_else(|| is_available("clang-format").then(|| "clang-format".to_string()));
    let Some(configured) = configured else {
        panic!("no clang-format: set CPPLS_CLANG_FORMAT or put one on PATH");
    };

    /// `clang-format` run by hand, with the arguments the server uses.
    fn ask(
        program: &str,
        file: &Path,
        directory: &Path,
        source: &str,
        lines: Option<(u32, u32)>,
    ) -> String {
        let arguments = clang_format_arguments(file, lines);
        let mut child = Command::new(program)
            .args(&arguments)
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("clang-format runs");
        child
            .stdin
            .take()
            .expect("piped")
            .write_all(source.as_bytes())
            .expect("the text is written");
        let output = child.wait_with_output().expect("clang-format finishes");
        String::from_utf8(output.stdout).expect("clang-format writes UTF-8")
    }

    let mut fixture = Fixture::new("external", Some(&configured));
    fixture.open();
    let source = std::fs::read_to_string(fixture.file()).expect("the fixture reads");
    let file = fixture.file();
    let directory = fixture.root.clone();

    // **The whole file.**
    let answered_whole = fixture.format(2);
    let direct_whole = ask(&configured, &file, &directory, &source, None);
    println!("[external] clang-format said:\n{direct_whole}");
    assert_eq!(
        answered_whole, direct_whole,
        "the server's whole-file answer is clang-format's own"
    );

    // **Line 2 only** (0-based line 1, one line): the line outside the selection must be exactly as the file wrote it.
    let answered_range = fixture.format_range(3, 1, 2);
    let direct_range = ask(&configured, &file, &directory, &source, Some((2, 2)));
    println!("[external] clang-format --lines=2:2 said:\n{direct_range}");
    // **The server's answer is clang-format's, clamped to the lines the client asked for**: `--lines` alone answers
    // the neighbour reformatted too (see `keep_only_the_lines`), and the clamp is what keeps the promise that a range
    // never edits outside itself.
    let clamped =
        cpp_ls::handlers::formatting::formatter::keep_only_the_lines(2, 2, &source, &direct_range);
    assert_eq!(
        answered_range, clamped,
        "the server's range answer is clang-format's, kept to its lines"
    );
    assert_ne!(
        clamped, direct_range,
        "…which is not the same as clang-format's own --lines answer on this input"
    );
    assert_eq!(
        answered_range.lines().next(),
        source.lines().next(),
        "the line outside the selection is untouched"
    );
    assert_eq!(
        answered_range.lines().nth(1),
        direct_range.lines().nth(1),
        "and the selected line is formatted"
    );

    // The built-in formatter, printed rather than asserted: this probe is about the program path, and the distance
    // between the two formatters is `crates/cpp_formatter/tests/suite/against_clang_format.rs`'s subject.
    let built_in = cpp_formatter::format_file(
        &file,
        &source,
        &cpp_formatter::config::style_for_file(&file),
    );
    println!("[external] the built-in formatter said:\n{built_in}");
    println!(
        "[external] the two formatters disagree on {} line(s) of this fixture",
        built_in
            .lines()
            .zip(direct_whole.lines())
            .filter(|(left, right)| left != right)
            .count()
    );
    assert!(
        fixture.warnings().is_empty(),
        "a formatter that is there is not warned about: {:?}",
        fixture.warnings()
    );
}
