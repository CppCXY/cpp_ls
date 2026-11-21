//! **A C project's diagnostics, file by file** — what a reader would actually see underlined.
//!
//! ```text
//! $env:CPPLS_C_PROJECT = "E:\corpora\libuv"
//! cargo test --release -p cpp_ls --test c_diagnostics_probe -- --ignored --nocapture
//! ```
//!
//! The project is read through its **compile database**, because that is what a reader's editor sees: a file the
//! database does not name is not part of the build, and diagnosing it is how a POSIX-only source tree produces a
//! thousand `include-not-found` findings for a Linux checkout on Windows. So the file list is the database's (80
//! entries for libuv's Windows build), and everything else is reported separately rather than mixed in.
//!
//! Three answers, in the order they matter for C:
//!
//! 1. **Does the server parse the project?** A `syntax-error` finding on a file the build compiles is a defect in this
//!    server — the file is valid C by definition, because the project's own build accepts it.
//! 2. **Are the findings real?** Errors and warnings are listed per file; hints are counted, not listed, because a hint
//!    is an opinion (`braces-around-statements` on libuv's `if (!handle) return UV_EINVAL;` is one) and a page of them
//!    would bury the two lines above.
//! 3. **Is the project read at all?** A file with no findings can mean "clean" or "never read", so the number of
//!    answered reports and the files that answered nothing are both printed.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long one `textDocument/diagnostic` may take. Generous for a cook, short enough that a broken project fails.
const PATIENCE: Duration = Duration::from_secs(60);

fn project() -> Option<PathBuf> {
    let root = std::env::var_os("CPPLS_C_PROJECT").map(PathBuf::from)?;
    root.join("compile_commands.json").is_file().then_some(root)
}

fn uri_of(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        text.insert(0, '/');
    }
    format!("file://{text}")
}

/// The files the project's build compiles, from its own database.
///
/// A `file` that does not exist on this machine is skipped and counted: a checked-in database written somewhere else
/// is the ordinary case, and a list of files that are not here would measure nothing.
fn database_files(root: &Path) -> (Vec<PathBuf>, usize) {
    let text = std::fs::read_to_string(root.join("compile_commands.json")).expect("a database");
    let entries: Vec<Value> = serde_json::from_str(&text).expect("a JSON database");
    let mut files = Vec::new();
    let mut missing = 0;
    for entry in entries {
        let Some(named) = entry.get("file").and_then(Value::as_str) else {
            continue;
        };
        let path = PathBuf::from(named);
        if path.is_file() {
            if !files.contains(&path) {
                files.push(path);
            }
        } else {
            missing += 1;
        }
    }
    (files, missing)
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: i64,
}

impl Client {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-level")
            .arg("warn")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if std::env::var_os("CPPLS_C_PROBE_STDERR").is_some() {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()
            .expect("the server starts");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Client {
            child,
            stdin,
            stdout,
            next: 1,
        }
    }

    fn send(&mut self, message: &Value) {
        let body = serde_json::to_string(message).expect("serializes");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len())
            .expect("the message is written");
        self.stdin.flush().expect("flushed");
    }

    fn receive_within(&mut self, within: Duration) -> Option<Value> {
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
            let value: Value = serde_json::from_slice(&body).ok()?;
            // **A message with a `method` is the server asking *us* something** — `workspace/configuration` is the
            // one it sends before it knows the client's settings, and treating it as an answer is how the first
            // version of this probe reported 40 null reports.
            if let Some(method) = value.get("method").and_then(Value::as_str) {
                if let Some(id) = value.get("id").cloned() {
                    let method = method.to_string();
                    self.send(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": if method == "workspace/configuration" {
                            json!([null])
                        } else {
                            Value::Null
                        },
                    }));
                }
                continue;
            }
            if value.get("id").is_some() {
                return Some(value);
            }
        }
    }

    fn ask(&mut self, method: &str, params: Value) -> Option<Value> {
        let id = self.next;
        self.next += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let answer = self.receive_within(PATIENCE)?;
        Some(answer)
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One finding, reduced to what a table needs.
struct Finding {
    file: String,
    line: u32,
    code: String,
    severity: i64,
    message: String,
}

#[test]
#[ignore = "opens a real C project; set CPPLS_C_PROJECT and run with --ignored"]
fn the_diagnostics_a_c_project_gets() {
    let Some(root) = project() else {
        eprintln!("no project with a compile_commands.json: set CPPLS_C_PROJECT");
        return;
    };
    let (files, missing) = database_files(&root);
    println!(
        "[c] {} file(s) from the database ({} entries named files that are not here)",
        files.len(),
        missing
    );

    let mut client = Client::start(&root);
    client.ask(
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": {
                "textDocument": { "diagnostic": { "dynamicRegistration": false } },
                "workspace": { "configuration": true },
            },
        }),
    );
    client.notify("initialized", json!({}));

    let mut findings: Vec<Finding> = Vec::new();
    let mut answered = 0usize;
    let mut null_reports = 0usize;
    let mut unanswered: Vec<String> = Vec::new();
    let started = Instant::now();

    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let uri = uri_of(path);
        let short = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "c", "version": 1, "text": text } }),
        );
        let Some(report) = client.ask(
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri } }),
        ) else {
            unanswered.push(short);
            continue;
        };
        let items = report["result"]["items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if report["result"].is_null() {
            null_reports += 1;
        }
        answered += 1;
        for item in items {
            let start = &item["range"]["start"];
            findings.push(Finding {
                file: short.clone(),
                line: start["line"].as_u64().unwrap_or(0) as u32 + 1,
                code: item["code"].as_str().unwrap_or("(none)").to_string(),
                severity: item["severity"].as_i64().unwrap_or(0),
                message: item["message"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    let took = started.elapsed();

    // **The second answer.** A finding can be true of the *reading* rather than of the text: a file diagnosed before
    // its includes were indexed says "nothing this file can see names it" about a name a header declares. Asking the
    // files with findings a second time, after the whole project is read, tells the two apart — a check's bug and a
    // probe's ordering are different repairs.
    let mut second_pass: Vec<(String, String, u32)> = Vec::new();
    for path in &files {
        let short = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if !findings.iter().any(|finding| finding.file == short) {
            continue;
        }
        let uri = uri_of(path);
        let Some(report) = client.ask(
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri } }),
        ) else {
            continue;
        };
        for item in report["result"]["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let severity = item["severity"].as_i64().unwrap_or(0);
            if severity > 2 {
                continue;
            }
            second_pass.push((
                short.clone(),
                item["code"].as_str().unwrap_or("(none)").to_string(),
                item["range"]["start"]["line"].as_u64().unwrap_or(0) as u32 + 1,
            ));
        }
    }
    client.kill();

    // **Errors first, then warnings, then the hint count** — the ordering is the report's argument.
    let by_severity = |severity: i64| findings.iter().filter(|f| f.severity == severity).count();
    let syntax: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.code == "syntax-error")
        .collect();
    println!("[c] null report(s): {null_reports}");
    println!(
        "[c] {} report(s) in {:.1?}, {} unanswered; {} error(s) ({} syntax), {} warning(s), {} hint(s)",
        answered,
        took,
        unanswered.len(),
        by_severity(1),
        syntax.len(),
        by_severity(2),
        by_severity(4).max(
            findings
                .iter()
                .filter(|f| !matches!(f.severity, 1 | 2))
                .count()
        )
    );
    if !unanswered.is_empty() {
        println!("[c] files that answered no report: {unanswered:?}");
    }

    println!(
        "\n[c] second pass over the files with findings: {} error/warning(s) remain",
        second_pass.len()
    );
    for (file, code, line) in &second_pass {
        println!("  {file}:{line} {code}");
    }

    println!("\n[c] errors and warnings, file by file:");
    let mut listed = 0usize;
    for finding in findings.iter().filter(|f| f.severity <= 2) {
        listed += 1;
        println!(
            "  {:<34} {:<5} {:<26} {}",
            finding.file,
            format!("{}:{}", "", finding.line),
            finding.code,
            finding.message.lines().next().unwrap_or("")
        );
    }
    if listed == 0 {
        println!("  (none)");
    }

    println!("\n[c] hints, by code:");
    let mut hints: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for finding in findings.iter().filter(|f| !matches!(f.severity, 1 | 2)) {
        *hints.entry(finding.code.as_str()).or_default() += 1;
    }
    for (code, count) in &hints {
        println!("  {count:>5}  {code}");
    }

    assert!(
        syntax.is_empty(),
        "a file the project's own build compiles must parse; {} syntax error(s): {:?}",
        syntax.len(),
        syntax
            .iter()
            .map(|f| format!("{}:{}", f.file, f.line))
            .collect::<Vec<_>>()
    );
}
