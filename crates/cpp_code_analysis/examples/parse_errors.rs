//! **What a file's parse errors are, grouped** — the instrument that found the two-line concentration.
//!
//! ```text
//! cargo run --release --example parse_errors -p cpp_code_analysis -- <project-dir> <file.hpp>
//! ```
//!
//! A count says a file has 39 errors; a count *per message with its first line* says 34 of them are two lines —
//! and that is the difference between "39 problems" and "one construct the grammar cannot read". See
//! `docs/parse-defect.md`.
//!
//! Reads through a `Session` rather than calling the parser directly, so the errors are the ones the analysis
//! actually works with — the same text, the same configuration, the same recovery.
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("usage: parse_errors <dir> <file>"));
    let file = PathBuf::from(std::env::args().nth(2).expect("usage: parse_errors <dir> <file>"));

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.index_everything();

    let source = std::fs::read_to_string(&file).expect("the file is readable");
    session.did_open(&file, &source);
    session.index_everything();

    let Some(view) = session.view_of_the_file(&file) else {
        panic!("{} has no reading", file.display());
    };

    // Grouped by message, each with the line of its first occurrence: a message that appears 16 times starting at
    // one line is one construct, and that is the reading this is printed for.
    let mut grouped: std::collections::BTreeMap<String, (usize, u32)> = Default::default();
    for error in view.errors() {
        let (start, _) = error.offsets();
        let line = source[..start.min(source.len())].matches('\n').count() as u32 + 1;
        let entry = grouped.entry(error.message.clone()).or_insert((0, line));
        entry.0 += 1;
    }

    println!("{}: {} parse error(s)", file.display(), view.errors().len());
    let mut rows: Vec<_> = grouped.into_iter().collect();
    rows.sort_by_key(|(_, (count, _))| std::cmp::Reverse(*count));
    for (message, (count, first_line)) in rows {
        println!("  {count:>4}x  line {first_line:<7} {message}");
    }
}
