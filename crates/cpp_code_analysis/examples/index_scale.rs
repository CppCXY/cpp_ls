//! How the name queries scale with the size of the project.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example index_scale -- 20000
//! ```
//!
//! Builds a synthetic project of N files (each a header with a few classes, some members and a chain of includes),
//! then times the queries that used to scan every file — a workspace-symbol search, a name lookup, a member list
//! and a completion-shaped prefix query — against the same question answered by a scan over `summaries()`, and the
//! cost of one edit (forget + insert). The scan columns are the old implementation's shape; they are here so the
//! number that matters is a ratio measured on one machine in one run, not a claim.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::index::summarize;
use cpp_code_analysis::{ProjectIndex, SummaryKey};

fn source_of(number: usize) -> String {
    let mut text = String::new();
    if number > 0 {
        text.push_str(&format!("#include \"f{}.h\"\n", number - 1));
    }
    if number > 8 {
        text.push_str(&format!("#include \"f{}.h\"\n", number / 2));
    }
    text.push_str(&format!("namespace ns{} {{\n", number % 40));
    for class in 0..4 {
        text.push_str(&format!(
            "struct Widget{number}_{class} {{ int size; int count{class}; void run(); void stop{class}(); }};\n"
        ));
    }
    text.push_str("}\n");
    text.push_str(&format!("int global{number};\nvoid helper{number}(int a, int b) {{ int local; }}\n"));
    text
}

fn timed<T>(label: &str, repeat: usize, mut work: impl FnMut() -> T) -> T {
    let mut last = work();
    let started = Instant::now();
    for _ in 0..repeat {
        last = work();
    }
    let each = started.elapsed() / repeat as u32;
    println!("  {label:<46} {each:>12.3?}");
    last
}

fn main() {
    let files: usize = std::env::args().nth(1).and_then(|n| n.parse().ok()).unwrap_or(5000);

    let started = Instant::now();
    let mut index = ProjectIndex::new();
    for number in 0..files {
        let path = format!("/p/f{number}.h");
        let mut summary = summarize(Path::new(&path), &source_of(number), SummaryKey::new(0, 0));
        for include in &mut summary.includes {
            include.resolved = Some(PathBuf::from(format!("/p/{}", include.spelling)));
        }
        index.insert(summary);
    }
    let declarations: usize = index.summaries().map(|summary| summary.declarations.len()).sum();
    println!(
        "{files} files, {declarations} declarations, {} distinct names, built in {:.2?}",
        index.distinct_names(),
        started.elapsed()
    );

    let from = PathBuf::from(format!("/p/f{}.h", files - 1));

    println!("\nworkspace symbol (limit 100)");
    for query in ["widget7_1", "wid", "run", "ns7::Widget7_1", "e"] {
        let found = timed(&format!("index   {query:?}"), 20, || index.symbols_matching(query, 100));
        // The scan's *cost* is the point; its matching is cruder (substring of the qualified name), so only the
        // index's answer is checked, by the unit test that compares it with the real scan.
        let _ = timed(&format!("scan    {query:?}"), 3, || scan_symbols(&index, query, 100));
        assert!(!found.is_empty() || query == "zzz");
    }

    println!("\nname lookup from the last file (its closure is a chain, so it is deep)");
    let found = timed("index   files_declaring(\"Widget7_1\")", 20, || {
        index.files_declaring("Widget7_1", &from).len()
    });
    let scanned = timed("scan    files_declaring(\"Widget7_1\")", 3, || scan_named(&index, "Widget7_1", &from));
    assert_eq!(found, scanned);

    println!("\nmember list");
    let found = timed("index   declarations_in(\"ns7\")", 20, || index.declarations_in("ns7", &from).len());
    let scanned = timed("scan    declarations_in(\"ns7\")", 3, || scan_scope(&index, "ns7", &from));
    assert_eq!(found, scanned);

    println!("\nmacro definers");
    timed("index   files_defining_macro(\"NOPE\")", 1000, || index.files_defining_macro("NOPE").len());

    println!("\none edit (forget + insert of one file)");
    let source = source_of(files / 2);
    timed("index   edit", 20, || {
        let path = format!("/p/f{}.h", files / 2);
        index.forget(Path::new(&path));
        let mut summary = summarize(Path::new(&path), &source, SummaryKey::new(0, 0));
        for include in &mut summary.includes {
            include.resolved = Some(PathBuf::from(format!("/p/{}", include.spelling)));
        }
        index.insert(summary);
    });
}

fn scan_symbols(index: &ProjectIndex, query: &str, limit: usize) -> usize {
    let wanted = query.trim().to_lowercase();
    let mut matches: Vec<(bool, String)> = Vec::new();

    for summary in index.summaries() {
        for fact in summary.declarations.iter().filter(|fact| !fact.local) {
            let qualified = fact.qualified_name();
            let lowered = qualified.to_lowercase();
            if lowered.contains(wanted.trim_start_matches("::")) {
                matches.push((fact.name.to_lowercase() == wanted, lowered));
            }
        }
    }

    matches.sort();
    matches.len().min(limit)
}

fn scan_named(index: &ProjectIndex, name: &str, from: &Path) -> usize {
    let visible: std::collections::HashSet<String> =
        index.visible_files(from).into_iter().map(|(key, _)| key).collect();

    index
        .summaries()
        .filter(|summary| visible.contains(&summary.path.to_string_lossy().to_lowercase()))
        .flat_map(|summary| summary.declarations.iter())
        .filter(|fact| !fact.local && fact.name == name)
        .count()
}

fn scan_scope(index: &ProjectIndex, scope: &str, from: &Path) -> usize {
    let visible: std::collections::HashSet<String> =
        index.visible_files(from).into_iter().map(|(key, _)| key).collect();

    index
        .summaries()
        .filter(|summary| visible.contains(&summary.path.to_string_lossy().to_lowercase()))
        .flat_map(|summary| summary.declarations.iter())
        .filter(|fact| fact.scope.as_deref() == Some(scope))
        .count()
}
