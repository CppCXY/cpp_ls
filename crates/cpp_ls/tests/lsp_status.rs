//! **What every LSP feature answers on a real project** — the self-test behind `docs/lsp-status.md`.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_ls --test lsp_status -- --ignored --nocapture
//! ```
//!
//! It opens two files of a real project — a `.cpp` and the header next to it — waits for the index, then asks each
//! registered feature one question whose answer has a **shape** (a count, a file, a spelling) rather than a
//! millisecond, and prints one line per feature:
//!
//! ```text
//! feature                answered  n     ms     note
//! hover                  yes       1     12     "LuaDiagnosticStyle _diagnosticStyle"
//! definition             yes       1     9      codeformat.h:14
//! …
//! ```
//!
//! The point is not to assert: it is to make "which feature is sound and which is not" a table somebody can read and
//! re-run, instead of a memory of what was measured once. `README`-level: every row that says `no` or `0` where the
//! text obviously has an answer is a defect, and the note says what the answer should have been.
//!
//! `#[ignore]`d because it spawns a server over a project of a hundred thousand lines and eats a core for a minute.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    )
}

fn uri_of(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        text.insert(0, '/');
    }
    format!("file://{text}")
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(project: &Path) -> Server {
        let log_dir = std::env::temp_dir().join("cppls-lsp-status");
        let _ = std::fs::remove_dir_all(&log_dir);
        let _ = std::fs::create_dir_all(&log_dir);
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg(&log_dir)
            .arg("--log-level")
            // `debug`, like `real_project.rs`: the log is read when a row says something surprising, and a probe that
            // hides the handler's own reasons cannot explain its own table.
            .arg("debug")
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // **The probe traces go to stderr** (`eprintln!` from the analysis), and a probe that throws them away
            // cannot answer a question about them: `CPPLS_TRACE_MEMBERS` / `CPPLS_TRACE_TIME` are read by the *server*
            // this test spawns, and the counts are the measurement.
            .stderr(Stdio::from(
                std::fs::File::create(log_dir.join("server-stderr.log")).expect("a stderr file"),
            ))
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

    fn send(&mut self, message: Value) {
        let body = serde_json::to_string(&message).expect("serializes");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("written");
        self.stdin.flush().expect("flushed");
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                panic!("the server did not answer {method}");
            }
            let Some(message) = self.read() else {
                panic!("the server closed before answering {method}");
            };
            if message["id"] == json!(id) {
                return message;
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
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Where a spelling is, as a protocol position — the first occurrence at or after `from_line`.
fn at(text: &str, needle: &str, from_line: usize) -> Option<Position> {
    let mut offset = 0;
    for (line, body) in text.lines().enumerate() {
        let start = offset;
        offset += body.len() + 1;
        if line < from_line {
            continue;
        }
        if let Some(column) = body.find(needle) {
            return Some(Position {
                line: line as u32,
                character: column as u32 + 1,
            });
        }
        let _ = start;
    }
    None
}

#[derive(Clone, Copy)]
struct Position {
    line: u32,
    character: u32,
}

fn position(uri: &str, at: Position) -> Value {
    json!({
        "textDocument": { "uri": uri },
        "position": { "line": at.line, "character": at.character },
    })
}

/// One row of the table.
struct Row {
    feature: String,
    answered: bool,
    count: usize,
    ms: u128,
    note: String,
}

impl Row {
    fn print(&self) {
        println!(
            "{:<22} {:<8} {:<5} {:<6} {}",
            self.feature,
            if self.answered { "yes" } else { "no" },
            self.count,
            format!("{} ms", self.ms),
            self.note
        );
    }
}

#[test]
#[ignore = "spawns a server over a real project; run with --ignored"]
fn every_feature_answers_what_the_project_holds() {
    let project = project();
    let source = project.join("CodeFormatCLib/src/CodeFormat.cpp");
    let header = project.join("CodeFormatCLib/src/codeformat.h");
    let text = std::fs::read_to_string(&source).expect("the source file is readable");
    let header_text = std::fs::read_to_string(&header).expect("the header is readable");

    let mut server = Server::start(&project);
    let source_uri = uri_of(&source);
    let header_uri = uri_of(&header);

    let started = Instant::now();
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&project),
            "capabilities": {
                "window": { "workDoneProgress": true },
                "textDocument": {
                    "diagnostic": { "dynamicRegistration": true, "relatedInformation": true },
                    "completion": { "completionItem": { "snippetSupport": true } },
                    "hover": { "contentFormat": ["markdown", "plaintext"] },
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    "publishDiagnostics": { "relatedInformation": true },
                },
                "workspace": { "diagnostics": { "refreshSupport": true } },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": source_uri, "languageId": "cpp", "version": 1, "text": text } }),
    );
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": header_uri, "languageId": "cpp", "version": 1, "text": header_text } }),
    );

    // **Let the index drain.** Two hundred and fifty milliseconds of pull is what a reader does while the squiggles
    // fill in, and it also lets the pump cook the two open files; the table is about answers, not about a cold start.
    let mut pulls = 0;
    for id in 10..90 {
        std::thread::sleep(Duration::from_millis(250));
        let _ = server.request(
            id,
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": source_uri } }),
        );
        pulls += 1;
    }
    println!("--- settled after {pulls} pull(s), {:?}", started.elapsed());

    // **A second file, pulled in the same session**: the across-file question. A class the first file pulled
    // (`std::vector`, `std::string`, the project's own) is a list the second file only *uses*, so its answer must be
    // the index's own and reusable — see `ProjectIndex::remember_member_list`. The header is opened above, so this
    // costs one request per pull and no new cook.
    for id in 90..110 {
        let _ = server.request(
            id,
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": header_uri } }),
        );
    }

    let mut rows: Vec<Row> = Vec::new();
    let mut id = 1000;
    // **The first symbol a reader would ask about**: a member declared in the header and used in the source.
    // **The call hierarchy over the real project**: prepare on the header's declaration, then both directions. The
    // outgoing list is empty on purpose — the header *declares* `UpdateDiagnosticStyle` and the body is in
    // `CodeFormat.cpp`, so there is nothing to walk here and the honest answer is no calls (the module says so).
    if let Some(declared) = at(&header_text, "UpdateDiagnosticStyle", 0)
        .or_else(|| at(&header_text, "UpdateCodeStyle", 0))
    {
        let started = std::time::Instant::now();
        let prepared = server.request(
            1200,
            "textDocument/prepareCallHierarchy",
            position(&header_uri, declared),
        );
        let items = prepared["result"].as_array().cloned().unwrap_or_default();
        let took = started.elapsed().as_millis();
        rows.push(Row {
            feature: "prepareCallHierarchy".to_string(),
            answered: !items.is_empty(),
            count: items.len(),
            ms: took,
            note: items
                .first()
                .and_then(|item| item["name"].as_str())
                .unwrap_or_default()
                .to_string(),
        });
        if let Some(item) = items.first().cloned() {
            for (feature, method, id) in [
                ("incomingCalls", "callHierarchy/incomingCalls", 1201),
                ("outgoingCalls", "callHierarchy/outgoingCalls", 1202),
            ] {
                let started = std::time::Instant::now();
                let answer = server.request(id, method, json!({ "item": item }));
                let calls = answer["result"].as_array().cloned().unwrap_or_default();
                rows.push(Row {
                    feature: feature.to_string(),
                    answered: answer["result"].is_array(),
                    count: calls.len(),
                    ms: started.elapsed().as_millis(),
                    note: calls
                        .iter()
                        .filter_map(|call| {
                            call["from"]["name"]
                                .as_str()
                                .or_else(|| call["to"]["name"].as_str())
                        })
                        .take(3)
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
        }
    }
    let member = at(&text, "_diagnosticStyle", 0).expect("the source uses the member");
    let member_in_header = at(&header_text, "_diagnosticStyle", 0).expect("the header declares it");
    let member_line = text
        .lines()
        .nth(member.line as usize)
        .unwrap_or_default()
        .to_string();

    ask(&mut server, &mut id, &mut rows, "hover", "textDocument/hover", position(&source_uri, member), &|result| {
        let contents = result["contents"].to_string();
        let count = contents.matches("LuaDiagnosticStyle").count();
        (count, format!("`{}` → {}", trim(&member_line), trim(&contents)))
    });
    // **A hover deep inside the out-of-line definition**, which is where a reader lives: the member used on the left
    // of a `.` (`_diagnosticStyle.code_style_check`) and the member itself. Reported as "分离定义的 cpp 文件里面没有
    // hover 结果" while Find References answered.
    // **A class name**, which is a name being *declared*, not a call: reported as "类型的 hover 有点莫名其妙" — the
    // answer listed the class's constructors as an overload set ("3 declarations … decided by the arguments").
    if let Some(the_type) = at(&text, "LuaDiagnosticStyle", 0) {
        ask(
            &mut server,
            &mut id,
            &mut rows,
            "hover (type name)",
            "textDocument/hover",
            position(&source_uri, the_type),
            &|result| {
                let contents = result["contents"].to_string();
                let declarations = contents.matches("declaration").count();
                (declarations, format!("{}", trim(&contents)))
            },
        );
    }
    for (nth, what) in [(2usize, "hover (use in a .cpp)"), (3, "hover (member after a .)")] {
        if let Some(use_at) = at(&text, "_diagnosticStyle", nth) {
            let line = text.lines().nth(use_at.line as usize).unwrap_or_default().to_string();
            ask(
                &mut server,
                &mut id,
                &mut rows,
                what,
                "textDocument/hover",
                position(&source_uri, use_at),
                &|result| {
                    let contents = result["contents"].to_string();
                    (contents.matches("```").count(), format!("`{}`", trim(&line)))
                },
            );
        }
    }
    if let Some(member_at) = at(&text, "code_style_check", 0) {
        ask(
            &mut server,
            &mut id,
            &mut rows,
            "hover (member of a member)",
            "textDocument/hover",
            position(&source_uri, member_at),
            &|result| {
                let contents = result["contents"].to_string();
                (contents.matches("bool").count(), format!("{}", trim(&contents)))
            },
        );
    }
    // **`typeDefinition` on an `auto`**: the type is written nowhere in the file, so the answer has to come from the
    // deduction. `CodeFormat.cpp:86` is `auto result = f.GetFormatResult(t);`.
    if let Some(auto_at) = at(&text, "auto result", 0) {
        ask(
            &mut server,
            &mut id,
            &mut rows,
            "typeDefinition (auto)",
            "textDocument/typeDefinition",
            position(&source_uri, Position { line: auto_at.line, character: auto_at.character + 5 }),
            &|result| {
                let locations = locations_of(result);
                (locations.len(), locations.first().cloned().unwrap_or_default())
            },
        );
    }
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "definition (member)",
        "textDocument/definition",
        position(&source_uri, member),
        &|result| {
            let locations = locations_of(result);
            (
                locations.len(),
                locations.first().cloned().unwrap_or_default(),
            )
        },
    );
    // **`typeDefinition` on the same member**: it asks what the member's *type* is and where that class is declared, so
    // the answer is the class, not the member — the one place this feature is distinguishable from Go to Definition.
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "typeDefinition",
        "textDocument/typeDefinition",
        position(&source_uri, member),
        &|result| {
            let locations = locations_of(result);
            (locations.len(), locations.first().cloned().unwrap_or_default())
        },
    );
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "definition (header)",
        "textDocument/definition",
        position(&header_uri, member_in_header),
        &|result| {
            let locations = locations_of(result);
            (locations.len(), locations.first().cloned().unwrap_or_default())
        },
    );

    // A function with a body in the source file: the cursor goes on the *declaration*, in the header.
    let function_in_header = at(&header_text, "UpdateDiagnosticStyle", 0)
        .or_else(|| at(&header_text, "UpdateCodeStyle", 0));
    if let Some(declared) = function_in_header {
        ask(
            &mut server,
            &mut id,
            &mut rows,
            "implementation",
            "textDocument/implementation",
            position(&header_uri, declared),
            &|result| {
                let locations = locations_of(result);
                (locations.len(), locations.join(" | "))
            },
        );
    }

    ask(
        &mut server,
        &mut id,
        &mut rows,
        "references",
        "textDocument/references",
        json!({ "textDocument": { "uri": source_uri }, "position": { "line": member.line, "character": member.character }, "context": { "includeDeclaration": true } }),
        &|result| {
            let locations = locations_of(result);
            (locations.len(), locations.first().cloned().unwrap_or_default())
        },
    );
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "documentHighlight",
        "textDocument/documentHighlight",
        position(&source_uri, member),
        &|result| {
            let count = result.as_array().map(|array| array.len()).unwrap_or(0);
            (count, String::new())
        },
    );
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "prepareRename",
        "textDocument/prepareRename",
        position(&source_uri, member),
        &|result| (1, trim(&result.to_string())),
    );
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "rename",
        "textDocument/rename",
        json!({ "textDocument": { "uri": source_uri }, "position": { "line": member.line, "character": member.character }, "newName": "_style" }),
        &|result| {
            let edits = result["changes"].as_object().map(|changes| {
                changes
                    .iter()
                    .map(|(uri, edits)| format!("{}: {}", file_of(uri), edits.as_array().map(|array| array.len()).unwrap_or(0)))
                    .collect::<Vec<_>>()
            });
            let count = edits.as_ref().map(|list| list.len()).unwrap_or(0);
            (count, edits.map(|list| list.join(" | ")).unwrap_or_default())
        },
    );

    // **The rest of the file-level features**, which need no particular symbol.
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "documentSymbol",
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": source_uri } }),
        &|result| {
            let count = result.as_array().map(|array| array.len()).unwrap_or(0);
            let names: Vec<String> = result
                .as_array()
                .map(|array| {
                    array
                        .iter()
                        .take(3)
                        .filter_map(|symbol| symbol["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            (count, names.join(", "))
        },
    );
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "workspaceSymbol",
        "workspace/symbol",
        json!({ "query": "Format" }),
        &|result| {
            let count = result.as_array().map(|array| array.len()).unwrap_or(0);
            (count, String::new())
        },
    );
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "foldingRange",
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": source_uri } }),
        &|result| {
            let count = result.as_array().map(|array| array.len()).unwrap_or(0);
            (count, String::new())
        },
    );
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "selectionRange",
        "textDocument/selectionRange",
        json!({ "textDocument": { "uri": source_uri }, "positions": [{ "line": member.line, "character": member.character }] }),
        &|result| {
            let depth = result
                .as_array()
                .and_then(|array| array.first())
                .map(|first| depth_of_selection(first))
                .unwrap_or(0);
            (depth, format!("{depth} nested range(s)"))
        },
    );
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "semanticTokens",
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": source_uri } }),
        &|result| {
            let count = result["data"].as_array().map(|data| data.len() / 5).unwrap_or(0);
            (count, String::new())
        },
    );
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "inlayHint",
        "textDocument/inlayHint",
        json!({ "textDocument": { "uri": source_uri }, "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 200, "character": 0 } } }),
        &|result| {
            let count = result.as_array().map(|array| array.len()).unwrap_or(0);
            (count, String::new())
        },
    );
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "documentLink",
        "textDocument/documentLink",
        json!({ "textDocument": { "uri": source_uri } }),
        &|result| {
            let count = result.as_array().map(|array| array.len()).unwrap_or(0);
            (count, String::new())
        },
    );
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "diagnostic",
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": source_uri } }),
        &|result| {
            let count = result["items"].as_array().map(|items| items.len()).unwrap_or(0);
            let first = result["items"]
                .as_array()
                .and_then(|items| items.first())
                .map(|item| trim(&item["message"].to_string()))
                .unwrap_or_default();
            (count, first)
        },
    );

    // **A member completion and a call**, which need a cursor the text actually offers.
    if let Some(completion_at) = a_member_access(&text) {
        ask(
            &mut server,
            &mut id,
            &mut rows,
            "completion (member)",
            "textDocument/completion",
            json!({ "textDocument": { "uri": source_uri }, "position": { "line": completion_at.line, "character": completion_at.character } }),
            &|result| {
                let items = result.as_array().cloned().unwrap_or_else(|| {
                    result["items"].as_array().cloned().unwrap_or_default()
                });
                let labels: Vec<String> = items
                    .iter()
                    .take(4)
                    .filter_map(|item| item["label"].as_str().map(str::to_string))
                    .collect();
                (items.len(), labels.join(", "))
            },
        );
    }
    // **A `(` that has arguments and a comma — tried in order until one answers.** A file's first such line is usually a
    // declaration or a constructor, where a signature help has nothing to say, so one fixed cursor says nothing about
    // the feature: measure the *best* of the first few candidates and print which line it was.
    let mut signature_row: Option<Row> = None;
    if let Some(candidates) = the_calls(&text) {
        // **A call the project really makes first**, so the answer is about the feature rather than about the probe:
        // `SetupStyle(style, options);` is a static member called with two arguments. The rest are the file's own
        // candidates, in order.
        let mut ordered = candidates.clone();
        if let Some(line) = text.lines().position(|body| body.contains("SetupStyle(")) {
            if let Some(column) = text.lines().nth(line).and_then(|body| body.find('(')) {
                ordered.insert(
                    0,
                    Position {
                        line: line as u32,
                        character: (column + 3) as u32,
                    },
                );
            }
        }
        let mut best: Option<(u32, u32, usize, String)> = None;
        for call_at in ordered.iter().take(12) {
            let answer = server.request(
                1_100 + best.as_ref().map_or(0, |_| 0) + call_at.line as i64,
                "textDocument/signatureHelp",
                json!({ "textDocument": { "uri": source_uri }, "position": { "line": call_at.line, "character": call_at.character } }),
            );
            let result = &answer["result"];
            if result.is_null() {
                continue;
            }
            let count = result["signatures"].as_array().map(|array| array.len()).unwrap_or(0);
            let labels: Vec<String> = result["signatures"]
                .as_array()
                .map(|array| {
                    array
                        .iter()
                        .take(2)
                        .filter_map(|signature| signature["label"].as_str().map(trim))
                        .collect()
                })
                .unwrap_or_default();
            if count > 0 {
                best = Some((call_at.line, call_at.character, count, labels.join(" | ")));
                break;
            }
        }
        signature_row = Some(Row {
            feature: "signatureHelp".to_string(),
            answered: best.is_some(),
            count: best.as_ref().map_or(0, |found| found.2),
            ms: 0,
            note: match &best {
                Some((line, character, _, labels)) => format!("at {line}:{character} → {labels}"),
                None => format!("null for the first 8 call sites ({})", candidates.len()),
            },
        });
    }
    if let Some(row) = signature_row.take() {
        rows.push(row);
    }

    // **The two formatting features and a code action**, which need a range rather than a symbol.
    let line_of = |needle: &str| -> Option<u32> {
        text.lines()
            .position(|body| body.contains(needle))
            .map(|line| line as u32)
    };
    if let Some(line) = line_of("UpdateCodeStyle") {
        ask(
            &mut server,
            &mut id,
            &mut rows,
            "codeAction",
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": source_uri },
                "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 40 } },
                "context": { "diagnostics": [] },
            }),
            &|result| {
                let count = result.as_array().map(|array| array.len()).unwrap_or(0);
                (count, String::new())
            },
        );
        ask(
            &mut server,
            &mut id,
            &mut rows,
            "rangeFormatting",
            "textDocument/rangeFormatting",
            json!({
                "textDocument": { "uri": source_uri },
                "range": { "start": { "line": line, "character": 0 }, "end": { "line": line + 3, "character": 0 } },
                "options": { "tabSize": 4, "insertSpaces": true },
            }),
            &|result| {
                let count = result.as_array().map(|array| array.len()).unwrap_or(0);
                (count, String::new())
            },
        );
    }
    ask(
        &mut server,
        &mut id,
        &mut rows,
        "formatting",
        "textDocument/formatting",
        json!({
            "textDocument": { "uri": source_uri },
            "options": { "tabSize": 4, "insertSpaces": true },
        }),
        &|result| {
            let count = result.as_array().map(|array| array.len()).unwrap_or(0);
            (count, String::new())
        },
    );
    // **The table.**
    println!("\n{:<22} {:<8} {:<5} {:<6} note", "feature", "answered", "n", "time");
    for row in &rows {
        row.print();
    }

    server.request(id + 1, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **Ask one feature and record its row** — a free function so that the signature-help block below can push its own row
/// afterwards (a closure that owns `rows` cannot be borrowed again while it is alive).
fn ask(
    server: &mut Server,
    id: &mut i64,
    rows: &mut Vec<Row>,
    feature: &str,
    method: &str,
    params: Value,
    count: &dyn Fn(&Value) -> (usize, String),
) {
    *id += 1;
    let began = Instant::now();
    let answer = server.request(*id, method, params);
    let elapsed = began.elapsed().as_millis();
    let result = &answer["result"];
    let answered = !result.is_null();
    if !answered {
        println!("  …{feature} → {}", trim(&answer.to_string()));
    }
    let (count, note) = if answered { count(result) } else { (0, "null".to_string()) };
    rows.push(Row {
        feature: feature.to_string(),
        answered,
        count,
        ms: elapsed,
        note,
    });
}
fn trim(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|character| if character == '\n' || character == '\r' { ' ' } else { character })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.len() > 70 {
        format!("{}…", &cleaned[..70])
    } else {
        cleaned
    }
}

fn file_of(uri: &str) -> String {
    uri.rsplit('/').next().unwrap_or(uri).to_string()
}

fn locations_of(result: &Value) -> Vec<String> {
    let mut locations = Vec::new();
    let mut push = |location: &Value| {
        let uri = location["uri"].as_str().unwrap_or_default();
        let line = location["range"]["start"]["line"].as_u64().unwrap_or_default();
        locations.push(format!("{}:{}", file_of(uri), line + 1));
    };
    match result {
        Value::Array(array) => {
            for entry in array {
                if entry.get("uri").is_some() {
                    push(entry);
                } else if entry.get("targetUri").is_some() {
                    push(&entry["targetSelectionRange"].clone());
                }
            }
        }
        Value::Object(map) if map.get("uri").is_some() => push(result),
        _ => {}
    }
    locations
}

fn depth_of_selection(range: &Value) -> usize {
    let mut depth = 0;
    let mut current = range;
    while let Some(parent) = current.get("parent").filter(|parent| parent.is_object()) {
        depth += 1;
        current = parent;
        if depth > 64 {
            break;
        }
    }
    depth
}



/// **A `.` or `->` in real code** — not in a comment, a string, or a `#include` line, which is where the first one in a
/// file usually is (`#include "CodeFormat.h"` — measured: the first probe asked its member completion inside that).
fn a_member_access(text: &str) -> Option<Position> {
    let mut line = 0;
    for body in text.lines() {
        let trimmed = body.trim_start();
        line += 1;
        if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with('#') {
            continue;
        }
        let code = body.split("//").next().unwrap_or(body);
        if let Some(dot) = code.find("->").or_else(|| code.find('.')) {
            let width = if code[dot..].starts_with("->") { 2 } else { 1 };
            return Some(Position {
                line: (line - 1) as u32,
                character: (dot + width) as u32,
            });
        }
    }
    None
}

/// Every position inside a call that has a comma in its arguments — up to sixteen, in file order.
fn the_calls(text: &str) -> Option<Vec<Position>> {
    let mut found = Vec::new();
    let mut line = 0;
    for body in text.lines() {
        let trimmed = body.trim_start();
        line += 1;
        if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with('#') {
            continue;
        }
        let code = body.split("//").next().unwrap_or(body);
        if let Some(open) = code.find('(')
            && let Some(close) = code[open..].find(')')
            && code[open..open + close].contains(',')
            && !code[..open].trim_end().ends_with("if")
            && !code[..open].trim_end().ends_with("while")
            && !code[..open].trim_end().ends_with("for")
        {
            found.push(Position {
                line: (line - 1) as u32,
                character: (open + 3) as u32,
            });
        }
    }
    (!found.is_empty()).then_some(found)
}