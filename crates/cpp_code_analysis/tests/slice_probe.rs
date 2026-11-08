//! **A probe, not a test**: what the parse of one file's own rendering leaves open, and where.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test slice_probe -- --ignored --nocapture
//! ```
//!
//! The slice is a **complete file rendering** taken from a stitched unit (written by the earlier dump + probe), so a
//! namespace that stays open in it is not an artifact of cutting a file in the middle: the text is what a compiler
//! would have seen for that file. This prints the open namespace and the innermost nodes at the end of the text, which
//! is the construct that swallowed the closing `}`.

use cpp_parser::{CppAstNode, CppNamespaceDecl, CppParser, CppSyntaxKind, ParserConfig};

#[test]
#[ignore]
fn what_one_file_rendering_leaves_open() {
    let path = std::env::var("CPPLS_SLICE").unwrap_or_else(|_| {
        let temporary = std::env::temp_dir().join("slice97.parsed");
        temporary.to_string_lossy().into_owned()
    });
    let text = std::fs::read_to_string(&path).expect("the slice reads");
    println!("slice {} bytes", text.len());

    let started = std::time::Instant::now();
    let tree = CppParser::parse(&text, ParserConfig::default());
    let root = tree.get_red_root();
    println!(
        "parsed in {:.0} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    println!("{} error(s)", tree.get_errors().len());
    for error in tree.get_errors().iter().take(5) {
        let range = cpp_parser::source_range(error.range);
        let around = text
            .get(range.start_offset.saturating_sub(80)..)
            .map(|rest| &rest[..rest.len().min(200)])
            .unwrap_or("");
        println!("  error at {}: {around:?}", range.start_offset);
    }

    let end = text.trim_end().len();
    for node in root.descendants() {
        let Some(namespace) = CppNamespaceDecl::cast(node.clone()) else {
            continue;
        };
        let range = cpp_parser::source_range(namespace.syntax().text_range());
        if range.end_offset() >= end {
            let name = namespace.get_name_text().unwrap_or_default();
            println!(
                "  `namespace {name}` opened at {} runs to the end",
                range.start_offset
            );
        }
    }

    // **The nodes nearest the end of the text**, innermost first: whatever is still open there is what ate the `}`.
    let mut nearest: Vec<(usize, CppSyntaxKind)> = Vec::new();
    for node in root.descendants() {
        let range = cpp_parser::source_range(node.text_range());
        if range.end_offset() >= end {
            nearest.push((range.start_offset, CppSyntaxKind::from(node.kind())));
        }
    }
    nearest.sort_unstable_by_key(|(start, _)| *start);
    println!("open nodes, outermost first ({}):", nearest.len());
    for (start, kind) in nearest.iter().rev().take(12) {
        let around = text
            .get(*start..)
            .map(|rest| &rest[..rest.len().min(120)])
            .unwrap_or("");
        println!("  {kind:?} at {start}: {around:?}");
    }
}
