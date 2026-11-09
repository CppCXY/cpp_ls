//! **A probe, not a test**: parse every header under a directory and rank the constructs that do not parse.
//!
//! ```text
//! $env:CPPLS_SWEEP_DIR = "C:\Program Files\Microsoft Visual Studio\...\include"
//! cargo test --release -p cpp_parser --test extension_sweep -- --ignored --nocapture
//! ```
//!
//! # Why the ranking is by *construct* and not by file
//!
//! A header nobody can parse is a fact; a construct no header can parse is a **defect**, and the second is what this
//! probe is for. Every error is keyed by `(message, the token the error points at)`, so one missing extension shows up
//! as one row with a count instead of two hundred files — which is how `__real__` was found
//! (`docs/buglist.md` section 12) and what a comprehensive sweep of GCC's, Clang's and MSVC's own headers needs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn headers_under(directory: &Path, into: &mut Vec<PathBuf>, limit: usize) {
    if into.len() >= limit {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    children.sort();
    for path in children {
        if into.len() >= limit {
            return;
        }
        if path.is_dir() {
            headers_under(&path, into, limit);
            continue;
        }
        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            continue;
        };
        if matches!(
            extension.to_ascii_lowercase().as_str(),
            "h" | "hpp" | "hh" | "hxx" | "inl" | "ipp" | "ixx" | "cppm"
        ) {
            into.push(path);
        }
    }
}

/// The token an error points at, which is the construct's own spelling.
fn the_token(text: &str, start: usize, end: usize) -> String {
    let mut at = start.min(text.len());
    // The error's range may be empty; take the word at it.
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    let slice = text.get(at..end.max(at).min(text.len())).unwrap_or("");
    let word: String = slice
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    if word.is_empty() {
        slice.chars().take(12).collect()
    } else {
        word
    }
}

/// **Is this error a macro's?** — the question that decides whether a row is a defect.
///
/// The sweep parses a header **as written**, and a header is full of macro *invocations* that expand to statements:
/// `_TRY_BEGIN` is `__try {`, `PB_DS_ASSERT_VALID(x)` is a whole `if`, `_COLLECTION_TRANSLATE` is a `throw`. A raw
/// parse cannot expand them, so the statement's first token is a bare identifier and the error lands on the `}` or
/// `if` after it. Those are not constructs this parser lacks — the cook replaces them before the parser sees anything
/// — so they are counted apart, and the ranked list holds what is left.
///
/// The test is the statement's first token: an identifier that the file `#define`s, or the conventional macro
/// spelling (`[A-Z_][A-Z0-9_]*`).
fn a_macro_shadows(text: &str, at: usize) -> bool {
    // The last few tokens before the error, not the last line: `_TRY_BEGIN` stands on the line **before** the `if`
    // whose error it causes, and a line-based scan called that `if` an ordinary statement.
    let before = &text[..at.min(text.len())];
    let script: Vec<&str> = before
        .split(|character: char| !(character.is_alphanumeric() || character == '_'))
        .filter(|word| !word.is_empty())
        .rev()
        .take(4)
        .collect();
    script.iter().any(|word| {
        let defined = text.lines().any(|line| {
            line.trim_start()
                .strip_prefix("#define")
                .map(|rest| rest.trim_start().starts_with(word))
                .unwrap_or(false)
        });
        defined
            || (word
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
                && word
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
    })
}

/// A little text around the error, so a reader sees the construct rather than a name.
fn around(text: &str, start: usize) -> String {
    let from = text[..start.min(text.len())]
        .char_indices()
        .rev()
        .nth(40)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let to = (start + 80).min(text.len());
    text.get(from..to).unwrap_or("").replace('\n', " ")
}

#[test]
#[ignore]
fn what_a_toolchains_headers_do_not_parse() {
    let Some(directory) = std::env::var_os("CPPLS_SWEEP_DIR").map(PathBuf::from) else {
        println!("skipped: CPPLS_SWEEP_DIR is not set");
        return;
    };
    let limit: usize = std::env::var("CPPLS_SWEEP_LIMIT")
        .ok()
        .and_then(|limit| limit.parse().ok())
        .unwrap_or(4000);
    let mut headers = Vec::new();
    headers_under(&directory, &mut headers, limit);

    // `(message, token) -> (count, one example)`
    let mut ranked: HashMap<(String, String), (usize, String)> = HashMap::new();
    let mut files = 0usize;
    let mut files_with_errors = 0usize;
    let mut errors = 0usize;
    let mut macro_shaped = 0usize;
    for header in &headers {
        let Ok(text) = std::fs::read_to_string(header) else {
            continue;
        };
        // **A fragment is not a header.** `bits/text_encoding-data.h` is included *inside* an initializer list — its
        // first token is a `{`, and `#error "This is not a public header, do not include it directly"` is how the
        // library says so. It was 894 of the 1 500 errors the first sweep of GCC's headers reported, and none of them
        // is a construct the parser lacks.
        {
            let first = text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with("//") && !line.starts_with("/*"))
                .unwrap_or("");
            if first.starts_with('{')
                || first.starts_with('}')
                || first.starts_with(',')
                || first.starts_with(';')
                || text.contains("not a public header")
                || text.contains("do not include it directly")
            {
                continue;
            }
        }
        files += 1;
        let tree = cpp_parser::CppParser::parse(&text, cpp_parser::ParserConfig::default());
        let found = tree.get_errors();
        if !found.is_empty() {
            files_with_errors += 1;
        }
        errors += found.len();
        for error in found {
            let range = cpp_parser::source_range(error.range);
            if a_macro_shadows(&text, range.start_offset) {
                macro_shaped += 1;
                continue;
            }
            let token = the_token(&text, range.start_offset, range.end_offset());
            let header = header
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let entry = ranked
                .entry((error.message.clone(), token))
                .or_insert_with(|| {
                    (
                        0,
                        format!("{header}: {}", around(&text, range.start_offset)),
                    )
                });
            entry.0 += 1;
        }
    }

    println!(
        "{} file(s) under {}, {} with errors, {} error(s) in total, {} of them macro-shaped (not defects)",
        files,
        directory.display(),
        files_with_errors,
        errors,
        macro_shaped
    );
    let mut rows: Vec<((String, String), (usize, String))> = ranked.into_iter().collect();
    rows.sort_unstable_by_key(|(_, (count, _))| std::cmp::Reverse(*count));
    println!(
        "
{} distinct construct(s):",
        rows.len()
    );
    for ((message, token), (count, example)) in rows.iter().take(40) {
        println!("{count:6}  `{token}`  {message}\n          …{example}…");
    }
}
