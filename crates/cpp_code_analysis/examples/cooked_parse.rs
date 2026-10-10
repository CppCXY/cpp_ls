//! **Does the COOKED reading parse cleanly where the raw one does not?**
//!
//! The decisive measurement for the architecture: C++ source with macros unexpanded is not a program, so the raw
//! reading's parse errors may be an artefact of reading the wrong text. This renders a unit the way the preprocessor
//! does — every file, stitched in include order, branches taken — parses the result, and reports both readings'
//! errors side by side.
//!
//! ```text
//! cargo run --release --example cooked_parse -p cpp_code_analysis -- <dir> <file>
//! ```
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use cpp_parser::{CppParser, CppSyntaxKind, ParserConfig};
use std::path::PathBuf;

fn error_nodes(source: &str) -> Vec<String> {
    let tree = CppParser::parse(source, ParserConfig::default());
    tree.get_red_root()
        .descendants()
        .filter(|node| format!("{:?}", CppSyntaxKind::from(node.kind())) == "ErrorNode")
        .map(|node| node.text().to_string())
        .collect()
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("usage: cooked_parse <dir> <file>"));
    let file = PathBuf::from(std::env::args().nth(2).expect("usage: cooked_parse <dir> <file>"));

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.index_everything();

    // The raw reading, for comparison.
    if let Some(source) = session.text(&file) {
        let errors = error_nodes(&source);
        println!("raw reading  ({} bytes): {:>4} error node(s)", source.len(), errors.len());
        let mut grouped: std::collections::BTreeMap<&str, usize> = Default::default();
        for e in &errors { *grouped.entry(e.trim()).or_default() += 1; }
        let mut rows: Vec<_> = grouped.into_iter().collect();
        rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (text, count) in rows.into_iter().take(6) {
            println!("    {count:>4}x {:?}", text.chars().take(40).collect::<String>());
        }
    }

    // The cooked reading: the whole unit, rendered the way the preprocessor renders it.
    let Some(unit) = session.render_the_unit(&file) else {
        println!("cooked reading: none could be built");
        return;
    };
    let text = unit.text.as_str();
    let errors = error_nodes(text);
    println!(
        "\ncooked reading ({} bytes, {} file(s) stitched): {:>4} error node(s)",
        text.len(),
        unit.files.len().max(1),
        errors.len()
    );
    let mut grouped: std::collections::BTreeMap<&str, usize> = Default::default();
    for e in &errors { *grouped.entry(e.trim()).or_default() += 1; }
    let mut rows: Vec<_> = grouped.into_iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (message, count) in rows.into_iter().take(10) {
        println!("    {count:>4}x {}", message.chars().take(70).collect::<String>());
    }
}
