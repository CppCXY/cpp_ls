//! Probe: **which directives did not become directive nodes, and what swallowed them?**
//!
//! ```text
//! cargo run -p cpp_parser --example where_directives_go -- <file> [offset]
//! ```
//!
//! # The failure this exists for
//!
//! The analysis layer's directive scanner (`scan_directives`) walks the syntax tree and takes every
//! `PreprocessorDirective` node. A directive the parser *consumed as something else* is therefore not just one
//! unread line: it is **missing** from the scanner's view, which shifts the conditional nesting of everything after
//! it — and conditional nesting is what "which macro is in force here", the branch rule, and every include-guard
//! question are computed from. A file can lose its whole conditional structure while its own text is fine.
//!
//! `winnt.h` was the measured case: 936 of its 4090 `#` lines were not directive nodes, it saw 204 `#if`s against
//! 196 `#endif`s, and the reason turned out to be one construct — a linkage block whose loop had no branch for a
//! directive (see B44 in `docs/grammar-gaps.md`). After that fix the file reads 4090/4090.
//!
//! So this probe answers the two questions that matter when a whole-file analysis refuses to run:
//!
//! ```text
//! how many `#` lines are outside every directive node   → is the damage real?
//! what node is the first one inside                    → what swallowed it (walk the parent chain)
//! ```
//!
//! With an `offset` argument it also prints that node's children three levels deep, which is how a runaway
//! declaration (one whose span reaches the end of the file) is identified.
//!
//! # Why it compares by offset and not by line
//!
//! A `PreprocessorDirective` node starts at the trivia *before* its `#`, so "the line the node starts on" is the
//! line above the directive. The first version of this file compared line numbers and reported every directive in
//! the file as missing — a measurement bug that looks exactly like a parser bug.

use cpp_parser::{CppParser, CppSyntaxKind, ParserConfig, source_range};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        "C:/Users/zc/Desktop/mingw64/x86_64-w64-mingw32/include/winnt.h".to_string()
    });
    let source = std::fs::read_to_string(&path).expect("readable");
    let tree = CppParser::parse(&source, ParserConfig::default());

    let line_of = |offset: usize| source[..offset.min(source.len())].lines().count();

    // Every `#` in the text, by offset, with its line.
    let mut hashes: Vec<(usize, usize)> = Vec::new();
    let mut offset = 0usize;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            hashes.push((offset + (line.len() - trimmed.len()), index + 1));
        }
        offset += line.len() + 1;
    }

    // Every directive node's span.
    let directives: Vec<(usize, usize)> = tree
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::PreprocessorDirective)
        .map(|node| {
            let range = source_range(node.text_range());
            (range.start_offset, range.end_offset())
        })
        .collect();

    let missing: Vec<(usize, usize)> = hashes
        .iter()
        .copied()
        .filter(|(at, _)| {
            !directives
                .iter()
                .any(|(start, end)| *start <= *at && *at < *end)
        })
        .collect();

    println!(
        "{}: {} `#` lines, {} directive nodes, {} `#`s outside every directive node, {} errors",
        path.rsplit('/').next().unwrap_or(&path),
        hashes.len(),
        directives.len(),
        missing.len(),
        tree.get_errors().len()
    );
    println!(
        "  the first missing ones: {:?}",
        missing.iter().take(20).map(|(_, line)| *line).collect::<Vec<_>>()
    );

    // What swallowed the first few?
    for (at, line) in missing.iter().take(4) {
        let token = tree
            .get_red_root()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| {
                let range = source_range(token.text_range());
                range.start_offset <= *at && *at < range.end_offset()
            });

        let mut chain: Vec<String> = Vec::new();
        if let Some(token) = &token {
            let mut parent = token.parent();
            while let Some(node) = parent {
                chain.push(format!(
                    "{:?}@{}(line {})..{}",
                    CppSyntaxKind::from(node.kind()),
                    usize::from(node.text_range().start()),
                    line_of(usize::from(node.text_range().start())),
                    usize::from(node.text_range().end())
                ));
                parent = node.parent();
            }
        }

        println!("\nline {line}: {}", source.lines().nth(line - 1).unwrap_or("").trim());
        println!(
            "  token {:?}  inside: {}",
            token.as_ref().map(|token| token.text().to_string()),
            chain.join("  <  ")
        );
    }

    println!("\nfirst errors:");
    for error in tree.get_errors().iter().take(6) {
        let range = source_range(error.range);
        println!(
            "  line {:>5} col {:>3}  {}",
            line_of(range.start_offset),
            range.start_offset
                - source[..range.start_offset]
                    .rfind('\n')
                    .map(|index| index + 1)
                    .unwrap_or(0),
            error.message
        );
    }

    // The children of a node, three levels deep, to see what a runaway declaration swallowed.
    if let Some(offset) = std::env::args().nth(2).and_then(|value| value.parse::<usize>().ok()) {
        println!("\nchildren of the node at {offset}:");
        let node = tree
            .get_red_root()
            .descendants()
            .find(|node| {
                let range = source_range(node.text_range());
                range.start_offset == offset
            });

        if let Some(node) = node {
            dump(&node, &source, &line_of, 0, &mut 60);
        } else {
            println!("  no node starts there");
        }
    }
}

/// Print a node and its children, depth-first, a few levels deep.
fn dump(
    node: &cpp_parser::CppSyntaxNode,
    source: &str,
    line_of: &impl Fn(usize) -> usize,
    depth: usize,
    budget: &mut usize,
) {
    if *budget == 0 || depth > 3 {
        return;
    }
    *budget -= 1;

    let range = source_range(node.text_range());
    let text: String = source[range.start_offset..range.end_offset().min(source.len())]
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(60)
        .collect();

    println!(
        "{:indent$}{:?}@{} (line {})..{}  {:?}",
        "",
        CppSyntaxKind::from(node.kind()),
        range.start_offset,
        line_of(range.start_offset),
        range.end_offset(),
        text,
        indent = depth * 2
    );

    for child in node.children() {
        dump(&child, source, line_of, depth + 1, budget);
    }
}
