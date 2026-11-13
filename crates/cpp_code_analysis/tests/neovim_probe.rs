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

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
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
            // **The checkout's own sources, not its build products.** `build/` holds generated C, `.deps/` holds the
            // downloaded libuv/Lua/tree-sitter sources, and `test/` is a Lua test suite — none of them is what a reader
            // of this project edits, and counting them would make the census a measure of the dependencies.
            if name.starts_with('.') || name == "build" || name == "test" {
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

/// **Whether a type is the question to ask about this name.** A macro has no type, a namespace has none and a keyword
/// is not a name — asking `type_at` about them would measure the probe rather than the analysis. A *function* is
/// excluded for a different reason: its type is a function type, which a reader asks the signature for (the hover)
/// rather than `type_at`, and the analysis answers the value question — `type_at` is documented for a variable. The
/// denominator is the names a reader hovers to learn a *value's* type: variables, parameters and fields.
///
/// An **enumerator** is not one of them, and the hover is why: its card says "an enumerator of (an anonymous
/// enum)", which is the declaration rather than a type, and `type_at` has no better answer to give (`enum { Red };`
/// declares no type name, and C gives the member `int` only by conversion). Asking it measures a question no client
/// asks.
fn a_type_is_expected(kind: cpp_code_analysis::semantic::NameKind) -> bool {
    use cpp_code_analysis::semantic::NameKind;
    matches!(
        kind,
        NameKind::Variable | NameKind::Parameter | NameKind::Property
    )
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
    // **Every `n`th file**, for a census that is representative rather than alphabetical. With the dependency tree
    // built, a *cold* file costs ~30 s (its closure's summaries are keyed by the environment in force, and each root
    // file has its own), so the whole checkout is a two-hour run the first time; every eighth file is 65 files and
    // still covers every directory.
    let every: usize = std::env::var("CPPLS_NEOVIM_EVERY")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1)
        .max(1);
    // **One path only**, for a question about one file: `CPPLS_NEOVIM_ONLY=event/loop.c` reads the files whose path
    // contains that text. `CPPLS_NEOVIM_LIMIT` and `CPPLS_NEOVIM_EVERY` still apply, so this narrows rather than
    // replaces the sample.
    let only = std::env::var("CPPLS_NEOVIM_ONLY").ok();
    let files: Vec<PathBuf> = all
        .iter()
        .filter(|path| {
            only.as_ref().is_none_or(|text| {
                path.to_string_lossy()
                    .replace('\\', "/")
                    .contains(text.as_str())
            })
        })
        .step_by(every)
        .take(limit)
        .cloned()
        .collect();
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
    // **What a reader can ask of a name**, which is the measure that matters for C support: an identifier the pass
    // classifies is one the editor can colour, hover and jump from; one it does not is a name with no answer.
    let mut identifiers = 0usize;
    let mut classified = 0usize;
    let mut classified_by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut definition_asked = 0usize;
    let mut definition_yes = 0usize;
    let mut definition_ambiguous = 0usize;
    let mut definition_unknown = 0usize;
    let mut type_asked = 0usize;
    let mut type_known = 0usize;
    let mut type_unknown_examples: Vec<String> = Vec::new();
    let mut definition_unknown_examples: Vec<String> = Vec::new();
    let mut uv_names_asked = 0usize;
    let mut uv_names_resolved = 0usize;
    let closure = std::env::var_os("CPPLS_NEOVIM_CLOSURE").is_some_and(|value| !value.is_empty());
    let started = Instant::now();

    // **The project is read once, not once per file.** `index_everything` drains a queue, and asking it after every
    // `want_cooked_reading` re-walked the whole project for every file: measured on this checkout, the first ten
    // files cost 12.8 s — eleven minutes for 517 — and almost all of it was the same headers read again. The
    // per-file loop below asks for the reading and waits for it; work that is still pending is drained there.
    for _ in 0..6 {
        session.index_everything();
        if session.pending_work() == 0 {
            break;
        }
    }

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
        // **What an editor does with the file it is looking at.** The pump cooks the open file's direct includes and
        // one level under them (`Session::want_the_closure_cooked`), and that is where the declarations behind an
        // `#include` come from: a header that was never cooked contributes no *cooked* declarations, and
        // `UV_EXTERN int uv_run(…)` is not in its shape-only summary either. `CPPLS_NEOVIM_CLOSURE=1` asks the census
        // to do the same, which is the difference between "a file read on its own" and "a file being looked at".
        if closure {
            session.want_the_closure_cooked(file);
        }
        session.catch_up(file);
        if session.pending_work() > 0 {
            for _ in 0..6 {
                session.index_everything();
                if closure {
                    session.advance(8);
                }
                session.catch_up(file);
                if session.pending_work() == 0 {
                    break;
                }
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
        // **The editor's own questions**, asked of the file's plain reading — the one a hover and a completion read.
        if let Some(view) = session.view_of_the_file(file) {
            let names = session.classified_names(&view);
            let by_start: std::collections::HashMap<usize, &cpp_code_analysis::semantic::Name> =
                names
                    .iter()
                    .map(|name| (name.range.start_offset, name))
                    .collect();
            classified += names.len();
            for name in &names {
                *classified_by_kind
                    .entry(format!("{:?}", name.kind))
                    .or_default() += 1;
            }
            let tokens: Vec<&cpp_parser::CppTokenData> = view
                .tree
                .get_tokens()
                .iter()
                .filter(|token| token.kind == cpp_parser::CppTokenKind::Identifier)
                .collect();
            identifiers += tokens.len();
            // A hover on a macro is answered by the macro question and one on a member through the object's type, so
            // those are not `definition`'s to answer — asking would understate the editor.
            let step = (tokens.len() / 40).max(1);
            for token in tokens.iter().step_by(step) {
                // **The token's own spelling**, kept apart from the file's text: the examples below print
                // `file:line spelling`, and the line has to be counted in the file, not in the token.
                let Some(spelling) = view.get_token_text(token.range) else {
                    continue;
                };
                let at = token.range.start_offset;
                // **Only a name the analysis classified is a name the editor draws.** An unclassified token is
                // measured by the coverage number above; asking `definition` about `#define`'s own words would measure
                // the lexer's tokens rather than the analysis's answers.
                let Some(name) = by_start.get(&at) else {
                    continue;
                };
                if name.kind == cpp_code_analysis::semantic::NameKind::Macro
                    || (!name.declaration
                        && matches!(
                            name.kind,
                            cpp_code_analysis::semantic::NameKind::Property
                                | cpp_code_analysis::semantic::NameKind::Method
                        ))
                {
                    continue;
                }
                let reading = ReadingOffset::new(at);
                definition_asked += 1;
                let answered = session.definition(&view, reading);
                let described = format!("{answered:?}");
                if matches!(answered, Known::Yes(_)) {
                    definition_yes += 1;
                } else if described.starts_with("Unknown(Ambiguous") {
                    definition_ambiguous += 1;
                } else {
                    definition_unknown += 1;
                    if definition_unknown_examples.len() < 20 {
                        let relative = file
                            .strip_prefix(&root)
                            .unwrap_or(file)
                            .display()
                            .to_string()
                            .replace('\\', "/");
                        definition_unknown_examples.push(format!(
                            "{relative}:{} {spelling} {described}",
                            line_of(&text, at)
                        ));
                    }
                }
                if spelling.starts_with("uv_") || spelling.starts_with("UV_") {
                    uv_names_asked += 1;
                    if matches!(answered, Known::Yes(_)) {
                        uv_names_resolved += 1;
                    }
                }
                // **Only a name that can have a type is asked about a type** — see `a_type_is_expected`.
                if a_type_is_expected(name.kind) {
                    type_asked += 1;
                    if matches!(session.type_at(&view, reading), Known::Yes(_)) {
                        type_known += 1;
                    } else if type_unknown_examples.len() < 25 {
                        let relative = file
                            .strip_prefix(&root)
                            .unwrap_or(file)
                            .display()
                            .to_string()
                            .replace('\\', "/");
                        type_unknown_examples.push(format!(
                            "{relative}:{} {spelling} ({:?})",
                            line_of(&text, at),
                            name.kind
                        ));
                    }
                }
            }
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

        if index % 10 == 0 {
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
    eprintln!(
        "[nvim] names: {identifiers} identifier(s), {classified} classified ({:.1}%)",
        100.0 * classified as f64 / identifiers.max(1) as f64
    );
    let mut kinds: Vec<(String, usize)> = classified_by_kind.into_iter().collect();
    kinds.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (kind, count) in kinds.iter().take(12) {
        eprintln!("[nvim]   {count:7}  {kind}");
    }
    eprintln!(
        "[nvim] definition: {definition_asked} asked, {definition_yes} resolved ({:.1}%), {definition_ambiguous} ambiguous, {definition_unknown} unknown",
        100.0 * definition_yes as f64 / definition_asked.max(1) as f64
    );
    eprintln!(
        "[nvim] type_at: {type_asked} asked, {type_known} known ({:.1}%)",
        100.0 * type_known as f64 / type_asked.max(1) as f64
    );
    eprintln!("[nvim] uv_* names: {uv_names_asked} asked, {uv_names_resolved} resolved");
    if !type_unknown_examples.is_empty() {
        eprintln!("[nvim] type_at unknown, by example:");
        for example in &type_unknown_examples {
            eprintln!("[nvim]   {example}");
        }
    }
    if !definition_unknown_examples.is_empty() {
        eprintln!("[nvim] definition unknown, by example:");
        for example in &definition_unknown_examples {
            eprintln!("[nvim]   {example}");
        }
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
