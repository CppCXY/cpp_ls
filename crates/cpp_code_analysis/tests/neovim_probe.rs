//! **A heavy real project, read the way an editor reads it** — neovim's C sources, censused.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test neovim_probe -- --ignored --nocapture
//! ```
//!
//! `CPPLS_NEOVIM` names the checkout; `CPPLS_NEOVIM_LIMIT` caps how many files are asked (for a quick run).
//!
//! # What it answers
//!
//! Three things an editor reported about this project, each as a number rather than an anecdote:
//!
//! * **`new` as a name.** C++'s `new` is a keyword; C's is an identifier, and neovim writes
//!   `struct trie_node_arr *new = …`. The lexer has one keyword table for both languages, so the question is how
//!   many files the parser cannot read because of it.
//! * **A missing `<uv.h>`.** neovim's compile commands point at `build/include`, and a checkout whose dependencies
//!   were never built has no `uv.h` there. Everything libuv declares is then unknown, which is a property of the
//!   machine rather than of the file — but a reader sees the consequences.
//! * **The parse errors themselves**, by message, with one example each: this is the list a grammar round works
//!   from.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let root = std::env::var_os("CPPLS_NEOVIM")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\neovim_test\neovim"));
    root.join("src").is_dir().then_some(root)
}

fn sources_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if name == "build" || name == ".git" || name == ".cppls" || name == "test" {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let extension = path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase();
            if matches!(extension.as_str(), "c" | "h" | "cpp" | "cc" | "hpp") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn is_a_c_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("c"))
}

/// **The byte offset at which `line` starts**, counting `\n` the way the parser does.
fn line_start(text: &str, line: usize) -> Option<usize> {
    let mut offset = 0;
    for (index, line_text) in text.split('\n').enumerate() {
        if index == line {
            return Some(offset);
        }
        offset += line_text.len() + 1;
    }
    None
}

/// **The line a byte offset falls on**, one-based.
fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}

/// **Is `new` used as a name on this line** — declared, assigned, or dereferenced?
///
/// A C++ `new` is followed by a type or a `(`, a C name by `=`, `->`, `[`, `)`, `,` or a declarator's `*`/`&`. Comments
/// are cut first: neovim's prose writes "a new buffer" on almost every other line.
fn new_as_a_name(line: &str) -> bool {
    let line = match line.find("//") {
        Some(at) => &line[..at],
        None => line,
    };
    let mut offset = 0;
    while let Some(at) = line[offset..].find("new") {
        let at = offset + at;
        offset = at + "new".len();
        let before = line[..at].chars().next_back();
        let after = line[at + "new".len()..].chars().next();
        let is_a_word = |character: Option<char>| {
            character.is_none_or(|character| !(character.is_alphanumeric() || character == '_'))
        };
        if !(is_a_word(before) && is_a_word(after)) {
            continue;
        }
        let rest = line[at + "new".len()..].trim_start();
        let before = line[..at].trim_end();
        if rest.starts_with('=') && !rest.starts_with("==")
            || rest.starts_with("->")
            || rest.starts_with('[')
            || rest.starts_with(')')
            || rest.starts_with(',')
            || before.ends_with('*')
            || before.ends_with('&')
        {
            return true;
        }
    }
    false
}

#[test]
#[ignore = "a probe: needs the neovim checkout"]
fn what_the_analysis_says_about_neovim() {
    let Some(root) = project() else {
        eprintln!("[nvim] the checkout is not here");
        return;
    };
    let all = sources_under(&root);
    let limit: usize = std::env::var("CPPLS_NEOVIM_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX);
    let files: Vec<PathBuf> = all.iter().take(limit).cloned().collect();
    eprintln!(
        "[nvim] {} file(s) of {} withheld; {} are C",
        files.len(),
        all.len(),
        files.iter().filter(|path| is_a_c_file(path)).count()
    );

    let mut session = Session::open_with_config_file(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        None,
    );
    session.add_project_files(files.iter().cloned());

    let mut errors_by_message: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut findings: BTreeMap<String, usize> = BTreeMap::new();
    let mut include_misses: BTreeMap<String, usize> = BTreeMap::new();
    let mut undefined_names: BTreeMap<String, usize> = BTreeMap::new();
    let mut files_with_errors = 0usize;
    let mut files_whose_first_error_is_new = 0usize;
    let mut other_first_errors: Vec<String> = Vec::new();
    let mut files_asked = 0usize;
    let mut files_without_a_reading = 0usize;
    let mut new_sites = 0usize;
    let mut new_sites_that_error = 0usize;
    let mut new_examples: Vec<String> = Vec::new();
    let mut uv_files: Vec<PathBuf> = Vec::new();
    let mut files_with_unplaced = 0usize;
    let started = Instant::now();

    for (index, file) in files.iter().enumerate() {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        if text.contains("#include <uv.h>")
            || text.contains("#include \"uv.h\"")
            || text.contains("<uv/")
        {
            uv_files.push(file.clone());
        }
        session.want_cooked_reading(file);
        for _ in 0..6 {
            session.index_everything();
            session.catch_up(file);
            if session.pending_work() == 0 {
                break;
            }
        }
        let Some(found) = session.diagnostics(file) else {
            files_without_a_reading += 1;
            continue;
        };
        files_asked += 1;
        for finding in &found.checks {
            *findings.entry(format!("{:?}", finding.code)).or_default() += 1;
            match finding.code {
                cpp_code_analysis::DiagnosticCode::IncludeNotFound => {
                    *include_misses.entry(finding.message.clone()).or_default() += 1;
                }
                cpp_code_analysis::DiagnosticCode::UndefinedName => {
                    *undefined_names.entry(finding.name.clone()).or_default() += 1;
                }
                _ => {}
            }
        }
        if found.unplaced > 0 {
            files_with_unplaced += 1;
        }
        if !found.errors.is_empty() {
            files_with_errors += 1;
            // **The first error of each file**, because the rest are usually cascades: a single unparsed declarator
            // makes every line after it a complaint about a brace. This is the list a grammar round works from.
            if let Some(first) = found.errors.first() {
                let line = line_of(&text, first.start);
                let on_a_new_line = text.lines().nth(line - 1).is_some_and(new_as_a_name);
                if on_a_new_line {
                    files_whose_first_error_is_new += 1;
                } else if other_first_errors.len() < 40 {
                    other_first_errors.push(format!(
                        "{}:{} {}",
                        file.strip_prefix(&root)
                            .unwrap_or(file)
                            .display()
                            .to_string()
                            .replace('\\', "/"),
                        line,
                        first.message
                    ));
                }
            }
        }
        for error in &found.errors {
            let line = line_of(&text, error.start);
            let sample = format!(
                "{}:{}",
                file.strip_prefix(&root)
                    .unwrap_or(file)
                    .display()
                    .to_string()
                    .replace('\\', "/"),
                line
            );
            let entry = errors_by_message
                .entry(error.message.clone())
                .or_insert((0, sample));
            entry.0 += 1;
        }

        if !is_a_c_file(file) {
            continue;
        }
        for (line_number, line) in text.lines().enumerate() {
            if !new_as_a_name(line) {
                continue;
            }
            new_sites += 1;
            let Some(start) = line_start(&text, line_number) else {
                continue;
            };
            let end = start + line.len();
            if found
                .errors
                .iter()
                .any(|error| start <= error.start && error.start <= end)
            {
                new_sites_that_error += 1;
                if new_examples.len() < 12 {
                    new_examples.push(format!(
                        "{}:{} {}",
                        file.strip_prefix(&root)
                            .unwrap_or(file)
                            .display()
                            .to_string()
                            .replace('\\', "/"),
                        line_number + 1,
                        line.trim()
                    ));
                }
            }
        }

        if index % 50 == 0 {
            eprintln!(
                "[nvim] … {index}/{} after {:?}",
                files.len(),
                started.elapsed()
            );
        }
    }

    eprintln!(
        "[nvim] {files_asked} file(s) diagnosed, {files_with_errors} with parse errors, {files_without_a_reading} without a reading, {files_with_unplaced} with unplaced errors, in {:?}",
        started.elapsed()
    );
    eprintln!("[nvim] parse errors by message (top 15):");
    let mut messages: Vec<(String, usize, String)> = errors_by_message
        .into_iter()
        .map(|(message, (count, sample))| (message, count, sample))
        .collect();
    messages.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (message, count, sample) in messages.iter().take(15) {
        eprintln!("[nvim]   {count:5}  {message}  e.g. {sample}");
    }
    eprintln!("[nvim] check findings by code:");
    let mut codes: Vec<(String, usize)> = findings.into_iter().collect();
    codes.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (code, count) in codes.iter().take(20) {
        eprintln!("[nvim]   {count:5}  {code}");
    }
    eprintln!(
        "[nvim] files whose first parse error is a `new` line: {files_whose_first_error_is_new}"
    );
    eprintln!("[nvim] the other files failing to parse, first error of each:");
    for error in &other_first_errors {
        eprintln!("[nvim]   {error}");
    }
    eprintln!(
        "[nvim] `new` as a name: {new_sites} site(s), {new_sites_that_error} on a line that does not parse"
    );
    for example in &new_examples {
        eprintln!("[nvim]   {example}");
    }
    eprintln!("[nvim] include-not-found, by message (top 10):");
    let mut misses: Vec<(String, usize)> = include_misses.into_iter().collect();
    misses.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (message, count) in misses.iter().take(10) {
        eprintln!("[nvim]   {count:5}  {message}");
    }
    eprintln!("[nvim] undefined-name, by name (top 10):");
    let mut names: Vec<(String, usize)> = undefined_names.into_iter().collect();
    names.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (name, count) in names.iter().take(10) {
        eprintln!("[nvim]   {count:5}  {name}");
    }
    eprintln!("[nvim] {} file(s) include uv.h", uv_files.len());
    if let Some(file) = uv_files.first() {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let at = text.find("<uv.h>").or_else(|| text.find("\"uv.h\""));
        if let (Some(at), Some(view)) = (at, session.view_of_the_file(file)) {
            match session.header_at(&view, FileOffset::new(at)) {
                Known::Yes(header) => {
                    eprintln!("[nvim]   {} -> {header:?}", file.display())
                }
                other => eprintln!(
                    "[nvim]   {} -> uv.h is {other:?}",
                    file.strip_prefix(&root).unwrap_or(file).display()
                ),
            }
        }
    }
}
