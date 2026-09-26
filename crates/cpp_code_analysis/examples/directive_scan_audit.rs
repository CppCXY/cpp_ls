//! Audit: **does the token-stream directive scan agree with the tree's directive nodes?**
//!
//! ```text
//! cargo run --release -q -p cpp_code_analysis --example directive_scan_audit -- <list>
//! ```
//!
//! The preprocessor layer used to read directives out of the tree — one node per directive, found by the
//! parser. It now reads them out of the **token stream**, by the language's own rule (a `#` that is the
//! first token of its logical line). Two rules can disagree, and this is where the disagreement would show:
//! per file, it compares the set of offsets where a directive *begins*.
//!
//! # What it measured (the round M1 landed in)
//!
//! ```text
//! libstdc++ 128 files     node only 0     scan only 0
//! libstdc++ 455 closure   node only 0     scan only 0
//! Windows SDK 255 files   node only 0     scan only 6950   (49 of those files fail to parse)
//! ```
//!
//! So the two rules are **the same rule wherever the grammar succeeds**, and the scan is strictly more
//! complete where the grammar is lost: a file whose parse broke used to hide its remaining directives from
//! the preprocessor layer, because there was no node to find. That is the point of reading tokens instead —
//! the layer that decides `#if` no longer inherits the parser's failures.
//!
//! Both directions matter, and they mean different things:
//!
//! * **a node the scan does not find** (always 0 so far) — the parser accepted a `#` that is not at the
//!   start of a line as a directive. Real code never writes that; a *broken* file can.
//! * **a scan without a node** — a `#` at the start of a line that the grammar did not read as a directive.
//!   The preprocessor's answer is that it *is* one, so this direction is the scan being right and the tree
//!   being incomplete.
fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(list) = arguments.next() else {
        eprintln!("usage: directive_scan_audit <file-list>");
        std::process::exit(2);
    };

    let text = std::fs::read_to_string(&list).expect("the list");
    let mut files = 0usize;
    let mut node_only = 0usize;
    let mut scan_only = 0usize;
    let mut shown = 0usize;

    for path in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        files += 1;

        let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());
        let from_tokens: Vec<usize> = cpp_code_analysis::preprocess(&source, tree.get_tokens())
            .directives
            .iter()
            .map(|spanned| spanned.range.start_offset)
            .collect();

        let mut from_tree: Vec<usize> = Vec::new();
        for node in tree.get_red_root().descendants() {
            if cpp_parser::CppSyntaxKind::from(node.kind())
                == cpp_parser::CppSyntaxKind::PreprocessorDirective
            {
                from_tree.push(usize::from(node.text_range().start()));
            }
        }

        let mut only_tree: Vec<usize> = from_tree
            .iter()
            .copied()
            .filter(|offset| !from_tokens.contains(offset))
            .collect();
        let mut only_scan: Vec<usize> = from_tokens
            .iter()
            .copied()
            .filter(|offset| !from_tree.contains(offset))
            .collect();

        if only_tree.is_empty() && only_scan.is_empty() {
            continue;
        }

        node_only += only_tree.len();
        scan_only += only_scan.len();

        if shown < 24 {
            shown += 1;
            let line_of = |offset: usize| source[..offset.min(source.len())].lines().count();
            println!("{path}");
            for offset in only_tree.drain(..) {
                println!(
                    "  node only  line {:>6}  {:?}",
                    line_of(offset),
                    source[offset..source.len().min(offset + 40)]
                        .lines()
                        .next()
                        .unwrap_or("")
                );
            }
            for offset in only_scan.drain(..) {
                println!(
                    "  scan only  line {:>6}  {:?}",
                    line_of(offset),
                    source[offset..source.len().min(offset + 40)]
                        .lines()
                        .next()
                        .unwrap_or("")
                );
            }
        }
    }

    println!("\nfiles {files} | node only {node_only} | scan only {scan_only}");
}
