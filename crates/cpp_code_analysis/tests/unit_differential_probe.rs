//! **A probe, not a test**: which segment of the stitched stream poisons the parse after it?
//!
//! ```text
//! $env:CPPLS_DUMP_RENDERING_DIR = "$env:TEMP\unit-dump"
//! cargo test --release -p cpp_code_analysis --test unit_differential_probe -- --ignored --nocapture
//! ```
//!
//! A file's own rendering must be brace-balanced — the cook emits what the file's text has, with each macro replaced
//! by its body. So the test is **differential**:
//!
//! * parse segment *k* alone: does it leave a `std` namespace open at its end? (a file's own text should not)
//! * parse the whole stream: are the namespaces that begin inside segment *k* closed by the end?
//!
//! The first *k* where the second answers "unclosed" and the first answers "closed" is the segment whose text is
//! **fine** but which the whole-stream parse files wrong — so the culprit construct is in the segment **before** it, or
//! in the way the two were joined. That is the reduction a fix is written against, and it never relies on cutting a file
//! in the middle, which would make "unclosed" true for an innocent reason.

use cpp_parser::{CppAstNode, CppNamespaceDecl, CppParser, ParserConfig};

/// `std` namespaces that reach the end of `text` — opened and never closed **in this text**.
fn unclosed_std(text: &str) -> Vec<usize> {
    let tree = CppParser::parse(text, ParserConfig::default());
    let root = tree.get_red_root();
    let end = text.trim_end().len();
    let mut open = Vec::new();
    for node in root.descendants() {
        let Some(namespace) = CppNamespaceDecl::cast(node.clone()) else {
            continue;
        };
        if namespace.get_name_text().as_deref() != Some("std") {
            continue;
        }
        let range = cpp_parser::source_range(namespace.syntax().text_range());
        if range.end_offset() >= end {
            open.push(range.start_offset);
        }
    }
    open
}

/// Every `std` namespace opening, with the offset its node ends at — the whole-stream answer.
fn std_namespaces(text: &str) -> Vec<(usize, usize)> {
    let tree = CppParser::parse(text, ParserConfig::default());
    let root = tree.get_red_root();
    let mut found = Vec::new();
    for node in root.descendants() {
        let Some(namespace) = CppNamespaceDecl::cast(node.clone()) else {
            continue;
        };
        if namespace.get_name_text().as_deref() != Some("std") {
            continue;
        }
        let range = cpp_parser::source_range(namespace.syntax().text_range());
        found.push((range.start_offset, range.end_offset()));
    }
    found
}

#[test]
#[ignore]
fn the_segment_that_poisons_the_parse_after_it() {
    let Some(directory) = std::env::var_os("CPPLS_DUMP_RENDERING_DIR") else {
        println!("skipped: CPPLS_DUMP_RENDERING_DIR is not set");
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    let text = std::fs::read_to_string(directory.join("unit.stream")).expect("the dump reads");
    let boundaries: Vec<usize> = std::fs::read_to_string(directory.join("unit.boundaries"))
        .expect("the boundaries read")
        .lines()
        .filter_map(|line| line.trim().parse::<usize>().ok())
        .collect();
    let files: Vec<String> = std::fs::read_to_string(directory.join("unit.files"))
        .expect("the file list reads")
        .lines()
        .map(|line| line.to_string())
        .collect();

    let whole = std_namespaces(&text);
    let stream_end = text.trim_end().len();
    println!(
        "whole stream: {} `std` namespace(s), {} of them open at EOF",
        whole.len(),
        whole.iter().filter(|(_, end)| *end >= stream_end).count()
    );

    // **Each segment on its own**, and the whole stream's answer for the namespaces starting in it.
    let segment = |k: usize| -> (usize, usize) {
        let from = if k == 0 { 0 } else { boundaries[k - 1] };
        let to = boundaries.get(k).copied().unwrap_or(text.len());
        (from, to)
    };
    let mut reported = 0;
    for k in 0..boundaries.len() {
        let (from, to) = segment(k);
        let text_of_segment = &text[from..to];
        let alone = unclosed_std(text_of_segment);
        let in_whole: Vec<usize> = whole
            .iter()
            .filter(|(start, end)| *start >= from && *start < to && *end >= stream_end)
            .map(|(start, _)| *start)
            .collect();
        if alone.is_empty() && !in_whole.is_empty() {
            let name = files
                .get(k)
                .map(|file| file.rsplit('/').next().unwrap_or(file).to_string())
                .unwrap_or_default();
            println!(
                "segment {k} ({name}, {from}..{to}): closed alone, but {} namespace(s) starting in it stay open in the \
                 whole stream: {in_whole:?}",
                in_whole.len()
            );
            reported += 1;
        }
    }
    println!("segments with the differential: {reported}");

    // **The first one, with its neighbour**, parsed as the pair a compiler would have seen.
    for k in 0..boundaries.len() {
        let (from, to) = segment(k);
        let alone = unclosed_std(&text[from..to]);
        let in_whole = whole
            .iter()
            .any(|(start, end)| *start >= from && *start < to && *end >= stream_end);
        if !alone.is_empty() || !in_whole {
            continue;
        }
        let pair_from = if k >= 1 { segment(k - 1).0 } else { 0 };
        let pair = &text[pair_from..to];
        let pair_unclosed = unclosed_std(pair);
        println!(
            "first differential at segment {k}: pair {pair_from}..{to} ({} bytes) leaves {} `std` open",
            pair.len(),
            pair_unclosed.len()
        );
        if pair_unclosed.is_empty() {
            println!("  the pair is fine: the poisoning needs an earlier segment still");
        }
        break;
    }
}
