//! **What the format-string colouring actually sends** — a probe against a real project, read back off the wire.
//!
//! ```text
//! $env:CPPLS_FORMAT_PROJECT = "C:\Users\zc\Desktop\cpp"     # the default when it exists
//! $env:CPPLS_FORMAT_FILE    = "main.cpp"
//! $env:CPPLS_FORMAT_CLIENT  = "standard" | "full"           # what the client says it can draw
//! cargo test --release -p cpp_ls --test format_status -- --ignored --nocapture
//! ```
//!
//! # Why this is a probe and not a test
//!
//! It says nothing about whether the answer is *right*: it prints what the server sent, decoded the way a client
//! decodes it, next to the text each token covers. A person reads it and sees whether `{}` is a specifier, whether
//! `f` is a conversion, and whether an editor whose theme knows none of these types would still see them (the
//! `standard` client). The assertions live in the unit tests; this exists so that "it works in the editor" is a
//! measurement rather than a claim.
//!
//! # What "standard" and "full" mean
//!
//! The four format types are **custom**, so a client that does not advertise them is answered with the standard
//! types they fall back to (`regexp`, `parameter`, `keyword`). `standard` is what every editor sends today, `full`
//! adds the four names — the shape a theme-aware client sends after it has been configured. Running both is how the
//! fallback path is measured on real code rather than in a unit test.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn project() -> PathBuf {
    std::env::var_os("CPPLS_FORMAT_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\cpp"))
}

fn the_file(project: &Path) -> PathBuf {
    let named = std::env::var("CPPLS_FORMAT_FILE").unwrap_or_else(|_| "main.cpp".to_string());
    project.join(named)
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
        let log_dir = std::env::temp_dir().join("cppls-format-status");
        let _ = std::fs::remove_dir_all(&log_dir);
        let _ = std::fs::create_dir_all(&log_dir);
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
            .arg("--log-path")
            .arg(&log_dir)
            .arg("--log-level")
            .arg("debug")
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
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
            if Instant::now() >= deadline {
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

/// The four custom types, and the shape a client that has been configured for them advertises.
const THE_CUSTOM_TYPES: &[&str] = &[
    "formatSpecifier",
    "formatArgument",
    "formatConversion",
    "formatEscape",
];

/// **Every token of an answer, as `(line, byte offset, length in bytes, type name, the text)`** — a client's own
/// decoding, so that the table can be read without knowing the encoding.
///
/// `data` on the wire is **one flat array of numbers**, five per token — not an array of five-element arrays. The
/// first version of this function read it the second way and reported zero tokens for an answer that held eight,
/// which is worth stating because it is the kind of mistake a probe must not make: it said "the feature sends
/// nothing" about a feature that was sending.
fn decoded(
    answer: &Value,
    legend: &[String],
    text: &str,
) -> Vec<(u32, usize, usize, String, String)> {
    let mut line_starts = vec![0usize];
    for (at, ch) in text.char_indices() {
        if ch == '\n' {
            line_starts.push(at + 1);
        }
    }

    let numbers: Vec<u64> = answer["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect();

    let mut out = Vec::new();
    let (mut line, mut character) = (0u32, 0u32);
    for token in numbers.chunks_exact(5) {
        let (delta_line, delta_start, length, token_type) = (
            token[0] as u32,
            token[1] as u32,
            token[2],
            token[3] as usize,
        );
        line += delta_line;
        character = if delta_line == 0 {
            character + delta_start
        } else {
            delta_start
        };

        let Some(&start) = line_starts.get(line as usize) else {
            continue;
        };
        let end_of_line = line_starts
            .get(line as usize + 1)
            .copied()
            .unwrap_or(text.len())
            .min(text.len());
        let body = &text[start..end_of_line];

        // The protocol's column is **UTF-16 code units**; the offsets here are bytes, so the two are converted once.
        let mut utf16 = 0u32;
        let mut at = 0usize;
        for ch in body.chars() {
            if utf16 >= character {
                break;
            }
            utf16 += ch.len_utf16() as u32;
            at += ch.len_utf8();
        }
        let mut units = 0u64;
        let mut bytes = 0usize;
        for ch in body[at..].chars() {
            if units >= length {
                break;
            }
            units += ch.len_utf16() as u64;
            bytes += ch.len_utf8();
        }

        let covered = text
            .get(start + at..start + at + bytes)
            .unwrap_or("")
            .to_string();
        out.push((
            line,
            start + at,
            bytes,
            legend.get(token_type).cloned().unwrap_or_default(),
            covered,
        ));
    }
    out
}

#[test]
#[ignore = "spawns a server over a real project; run with --ignored"]
fn what_the_format_colouring_sends() {
    let project = project();
    if !project.is_dir() {
        eprintln!("skipped: {} is not here", project.display());
        return;
    }
    let file = the_file(&project);
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("skipped: {} is not readable", file.display());
        return;
    };
    let full = std::env::var("CPPLS_FORMAT_CLIENT").as_deref() == Ok("full");

    let mut server = Server::start(&project);
    let uri = uri_of(&file);

    // A standard client's list, plus the four custom names when the run asks for them — which is exactly the
    // difference between the fallback path and the coloured one.
    let mut types: Vec<String> = [
        "namespace",
        "type",
        "class",
        "enum",
        "interface",
        "struct",
        "typeParameter",
        "parameter",
        "variable",
        "property",
        "enumMember",
        "event",
        "function",
        "method",
        "macro",
        "keyword",
        "modifier",
        "comment",
        "string",
        "number",
        "regexp",
        "operator",
        "decorator",
    ]
    .iter()
    .map(|name| name.to_string())
    .collect();
    if full {
        types.extend(THE_CUSTOM_TYPES.iter().map(|name| name.to_string()));
    }

    let started = Instant::now();
    let initialized = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&project),
            "capabilities": {
                "textDocument": {
                    "semanticTokens": {
                        "requests": { "full": true },
                        "tokenTypes": types,
                        "tokenModifiers": ["declaration", "definition", "readonly", "static", "deprecated"],
                        "formats": ["relative"],
                    },
                },
            },
        }),
    );
    let initialize_ms = started.elapsed().as_millis();
    let legend: Vec<String> =
        initialized["result"]["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect();
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": text } }),
    );

    println!(
        "--- {} ({})",
        file.display(),
        if full {
            "custom types"
        } else {
            "standard types"
        }
    );
    println!("initialize {initialize_ms} ms");
    println!("legend ({}) {}", legend.len(), legend.join(" "));
    let added: Vec<&String> = legend
        .iter()
        .filter(|name| THE_CUSTOM_TYPES.contains(&name.as_str()) || name.as_str() == "regexp")
        .collect();
    println!("format-relevant legend entries: {added:?}");

    // **Poll until the file's closure is read**: the handler answers `null` ("ask again later") until then, which is
    // its design and not a failure. The loop asks until an answer actually **holds tokens**: measured on the test
    // project, the first non-null answer came back with an empty `data` (the file had been opened but not yet
    // classified), and a client would cache that empty list until the next edit — which is the shape of a defect,
    // so the probe prints both numbers rather than taking the first answer that parses.
    let mut id = 10;
    let mut asks = 0;
    let mut empty_answers = 0usize;
    let mut first_raw = Value::Null;
    let answer = loop {
        let started = Instant::now();
        let response = server.request(
            id,
            "textDocument/semanticTokens/full",
            json!({ "textDocument": { "uri": uri } }),
        );
        let took = started.elapsed().as_millis();
        id += 1;
        asks += 1;
        if response["result"].is_object() && response["result"]["data"].is_array() {
            let count = response["result"]["data"]
                .as_array()
                .map_or(0, |data| data.len() / 5);
            if empty_answers == 0 && count == 0 {
                first_raw = response["result"].clone();
            }
            if count > 0 {
                println!("first answer that holds a token: {took} ms");
                break response["result"].clone();
            }
            empty_answers += 1;
        }
        if asks > 60 {
            println!("no answer holding a token after {asks} asks ({took} ms each)");
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    println!(
        "first non-null answer: {empty_answers} empty answer(s) before the first one that held a token \
         {first_raw}"
    );

    let again = {
        let started = Instant::now();
        let response = server.request(
            id,
            "textDocument/semanticTokens/full",
            json!({ "textDocument": { "uri": uri } }),
        );
        (started.elapsed().as_millis(), response["result"].clone())
    };

    let tokens = decoded(&answer, &legend, &text);
    println!(
        "semanticTokens: {} token(s) after {asks} ask(s); again {} ms and {} token(s)",
        tokens.len(),
        again.0,
        again.1["data"].as_array().map_or(0, |data| data.len() / 5)
    );

    // **Only the tokens inside a string literal are the feature's**, so the table is filtered by type: the four
    // custom ones, their three fallbacks, and the literal itself. Every row prints the text it covers, which is the
    // only way to read whether the answer is right.
    //
    // The literal spans are found by a **quote-to-quote scan of the file**, not by looking for the nearest `"` before
    // the token: the first version did that and reported a `return` two lines below a `printf` as "inside a literal",
    // which is the same class of mistake as reading the encoding wrong — a probe inventing a row.
    let literals = literals_of(&text);
    let inside_a_literal = |offset: usize| {
        literals
            .iter()
            .any(|(start, end)| *start < offset && offset < *end)
    };

    // **The four custom types and the three they fall back to.** The fallbacks are what a client with a standard
    // `tokenTypes` list actually receives, so leaving them out would print an empty table for the ordinary case —
    // the first version of this filter did exactly that after a rewrite, and said "0 format tokens" about a file
    // the server was colouring.
    let interesting = |kind: &str| {
        matches!(
            kind,
            "formatSpecifier"
                | "formatArgument"
                | "formatConversion"
                | "formatEscape"
                | "regexp"
                | "parameter"
                | "keyword"
        )
    };
    let mut shown = 0usize;
    for (line, offset, bytes, kind, covered) in &tokens {
        if !interesting(kind) || !inside_a_literal(*offset) {
            continue;
        }
        println!(
            "  line {:>4}  {:>12}..{:<12} {:<18} {:?}",
            line + 1,
            offset,
            offset + bytes,
            kind,
            covered
        );
        shown += 1;
    }
    println!("{shown} format token(s) inside a literal");
}

/// **Every string literal's span in a file**, by a quote-to-quote scan that honours `\` and raw strings.
///
/// Deliberately not the lexer: this is the probe's own second opinion, and a check that reused the code under test
/// would agree with it by construction. It only has to be good enough to say which tokens lie inside a `"…"`.
fn literals_of(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += 2;
                while at < bytes.len() && !(bytes[at] == b'*' && bytes.get(at + 1) == Some(&b'/')) {
                    at += 1;
                }
                at = (at + 2).min(bytes.len());
            }
            b'"' => {
                let start = at;
                at += 1;
                while at < bytes.len() {
                    match bytes[at] {
                        b'\\' => at += 2,
                        b'"' => {
                            at += 1;
                            break;
                        }
                        b'\n' => break,
                        _ => at += 1,
                    }
                }
                spans.push((start, at));
            }
            _ => at += 1,
        }
    }
    spans
}

/// **The completions inside the file's first format string**, asked at the position just inside a `{`.
///
/// A probe rather than a check, for the reason the rest of this file is: it prints what a client would show. The
/// unit and protocol tests own the assertions (`format_scanner::a_conversion_is_offered_where_one_can_go`,
/// `handshake::a_completion_inside_a_format_string_is_the_grammar_s_own`).
#[test]
#[ignore = "spawns a server over a real project; run with --ignored"]
fn what_a_completion_inside_a_format_string_offers() {
    let project = project();
    if !project.is_dir() {
        eprintln!("skipped: {} is not here", project.display());
        return;
    }
    let file = the_file(&project);
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("skipped: {} is not readable", file.display());
        return;
    };
    // Inside the first `{` **of the format string**, which is the position a reader is at after typing one. Not the
    // function body's brace, which is the first `{` in the file — the first version of this probe completed there and
    // printed the keyword list, which is correct behaviour at `int main() {` and nothing to do with this feature.
    let Some(at) = text
        .find("format(")
        .and_then(|call| text[call..].find('{').map(|at| call + at + 1))
    else {
        eprintln!("skipped: the file has no format string with a `{{`");
        return;
    };

    let mut line_starts = vec![0usize];
    for (offset, ch) in text.char_indices() {
        if ch == '\n' {
            line_starts.push(offset + 1);
        }
    }
    let line = line_starts.partition_point(|start| *start <= at) - 1;
    let character: usize = text[line_starts[line]..at]
        .chars()
        .map(char::len_utf16)
        .sum();

    let mut server = Server::start(&project);
    let uri = uri_of(&file);
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(&project),
            "capabilities": {
                "textDocument": { "completion": { "completionItem": { "snippetSupport": true } } },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": text } }),
    );

    let started = Instant::now();
    let answer = server.request(
        100,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        }),
    );
    let took = started.elapsed().as_millis();
    let items = answer["result"]["items"]
        .as_array()
        .or_else(|| answer["result"].as_array())
        .cloned()
        .unwrap_or_default();
    println!(
        "--- completion at {}:{} ({} ms), {} item(s)",
        line + 1,
        character,
        took,
        items.len()
    );
    for item in items.iter().take(40) {
        println!(
            "  {:<6} {}",
            item["label"].as_str().unwrap_or(""),
            item["detail"].as_str().unwrap_or("")
        );
    }
}
