//! **How big the `std` closure is, and what it costs to discover it without parsing** — the prototype of `FileFacts`.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test closure_probe -- --ignored --nocapture
//! ```
//!
//! The walk below never builds a syntax tree: it lexes a file, reads its directives, and follows the includes. That is
//! the whole claim a `FileFacts` cache rests on — "which files does this translation unit consist of" does not need
//! the parser — so this measures it on the worst case a C++23 project has: MSVC's `std.ixx`, whose hundred
//! `#include <…>` lines pull the standard library *and* the Windows SDK through `windows.h`, `winrt` and `combaseapi.h`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

const MSVC: &str =
    r"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215\include";
const SDK: &str = r"C:\Program Files (x86)\Windows Kits\10\Include\10.0.22621.0";

fn search_directories(includer: &Path) -> Vec<PathBuf> {
    let mut directories: Vec<PathBuf> = Vec::new();
    if let Some(parent) = includer.parent() {
        directories.push(parent.to_path_buf());
    }
    directories.push(PathBuf::from(MSVC));
    for part in ["ucrt", "shared", "um", "winrt"] {
        directories.push(Path::new(SDK).join(part));
    }
    directories
}

/// The file an `#include` names, searched the way a compiler searches: the includer's directory, then the paths.
fn resolve(includer: &Path, name: &str) -> Option<PathBuf> {
    for directory in search_directories(includer) {
        let candidate = directory.join(name.replace('/', "\\"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[test]
#[ignore = "reads the toolchain's own headers; run with --ignored"]
fn the_std_closure_without_the_parser() {
    let start = PathBuf::from(
        r"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215\modules\std.ixx",
    );
    if !start.is_file() {
        eprintln!("skipped: no std.ixx");
        return;
    }

    let config = cpp_parser::LexerConfig::default();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut pending: Vec<PathBuf> = vec![start.clone()];
    let mut files = 0usize;
    let mut bytes = 0usize;
    let mut lex_time = std::time::Duration::ZERO;
    let mut directive_time = std::time::Duration::ZERO;
    let mut read_time = std::time::Duration::ZERO;
    let mut max_depth = 0usize;
    let mut depth_of: std::collections::HashMap<PathBuf, usize> = std::collections::HashMap::new();
    depth_of.insert(start.clone(), 0);

    let started = Instant::now();
    while let Some(path) = pending.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let depth = depth_of.get(&path).copied().unwrap_or(0);
        max_depth = max_depth.max(depth);

        let read_started = Instant::now();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        read_time += read_started.elapsed();
        files += 1;
        bytes += text.len();

        let lex_started = Instant::now();
        let (tokens, _errors) = cpp_parser::lex(&text, &config);
        lex_time += lex_started.elapsed();

        let directive_started = Instant::now();
        let preprocessing = cpp_code_analysis::preprocess::preprocess(&text, &tokens);
        directive_time += directive_started.elapsed();

        for directive in &preprocessing.directives {
            if let cpp_code_analysis::preprocess::directive::Directive::Include(include) =
                &directive.directive
                && let Some(target) = resolve(&path, &include.target)
            {
                depth_of.entry(target.clone()).or_insert(depth + 1);
                pending.push(target);
            }
        }
    }
    let total = started.elapsed();
    println!(
        "std closure: {} file(s), {:.1} MB, discovered in {:?} (read {:?}, lex {:?}, directives {:?}), depth {}",
        files,
        bytes as f64 / (1024.0 * 1024.0),
        total,
        read_time,
        lex_time,
        directive_time,
        max_depth,
    );
}
