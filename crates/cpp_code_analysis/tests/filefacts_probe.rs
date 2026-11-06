//! **What a file's includes cost to discover, against what the whole summary costs** — a probe, not a test.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test filefacts_probe -- --ignored --nocapture
//! ```
//!
//! The question a `FileFacts` cache answers is "which headers does this file include", and the point of caching it is
//! that the answer needs only the **lexer** and the directive pass — not the parser, not the scopes, not the facts.
//! This probe puts a number on the difference, on the largest headers the toolchain has, because that is where a
//! quadratic discovery cost hurts most.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::SummaryKey;

/// The MSVC and Windows SDK headers the standard module pulls in, largest first.
fn headers() -> Vec<PathBuf> {
    let msvc = PathBuf::from(
        r"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215\include",
    );
    let sdk = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\Include\10.0.22621.0");
    let mut files: Vec<PathBuf> = Vec::new();
    for name in [
        "xutility",
        "string",
        "vector",
        "type_traits",
        "xtree",
        "xlocinfo",
        "windows.h",
        "winbase.h",
        "combaseapi.h",
    ] {
        let candidates = [
            msvc.join(name),
            sdk.join("um").join(name),
            sdk.join("ucrt").join(name),
            sdk.join("shared").join(name),
        ];
        for candidate in candidates {
            if candidate.is_file() {
                files.push(candidate);
                break;
            }
        }
    }
    files
}

/// The directives a file's `FileFacts` would record, read from the **lexer** alone.
fn directives_of(path: &Path, text: &str) -> usize {
    let config = cpp_parser::LexerConfig::default();
    let (tokens, _errors) = cpp_parser::lex(text, &config);
    let preprocessing = cpp_code_analysis::preprocess::preprocess(text, &tokens);
    let _ = path;
    preprocessing.directives.len()
}

#[test]
#[ignore = "reads the toolchain's own headers; run with --ignored"]
fn discovery_against_a_summary() {
    let mut total_text = 0usize;
    let mut total_lex = std::time::Duration::ZERO;
    let mut total_directives = std::time::Duration::ZERO;
    let mut total_summary = std::time::Duration::ZERO;

    println!(
        "{:<14} {:>10} {:>10} {:>10} {:>10} {:>8}",
        "file", "size", "lex", "directives", "summary", "lines"
    );
    for path in headers() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let size = text.len();
        total_text += size;
        let lines = text.lines().count();

        let started = Instant::now();
        let config = cpp_parser::LexerConfig::default();
        let (tokens, _errors) = cpp_parser::lex(&text, &config);
        let lex = started.elapsed();

        let started = Instant::now();
        let preprocessing = cpp_code_analysis::preprocess::preprocess(&text, &tokens);
        let directive_count = preprocessing.directives.len();
        let directive_pass = started.elapsed();

        // **The whole summary**, which is what the index pays today for a file it has not seen.
        let started = Instant::now();
        let summary = cpp_code_analysis::summarize(&path, &text, SummaryKey::new(0, 0));
        let summary_time = started.elapsed();

        println!(
            "{:<14} {:>9.0}K {:>8.1}ms {:>8.1}ms {:>8.1}ms {:>8}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?"),
            size as f64 / 1024.0,
            lex.as_secs_f64() * 1000.0,
            directive_pass.as_secs_f64() * 1000.0,
            summary_time.as_secs_f64() * 1000.0,
            lines,
        );
        let _ = directive_count;
        let _ = summary.declarations.len();

        total_lex += lex;
        total_directives += directive_pass;
        total_summary += summary_time;
    }

    println!(
        "\ntotal {:.1} MB: lex {:.1} ms, directive pass {:.1} ms, full summary {:.1} ms — discovery is {:.1}% of a summary",
        total_text as f64 / (1024.0 * 1024.0),
        total_lex.as_secs_f64() * 1000.0,
        total_directives.as_secs_f64() * 1000.0,
        total_summary.as_secs_f64() * 1000.0,
        (total_lex + total_directives).as_secs_f64() / total_summary.as_secs_f64() * 100.0,
    );
}
