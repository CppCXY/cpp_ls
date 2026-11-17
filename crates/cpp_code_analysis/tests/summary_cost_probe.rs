//! **What a `FileSummary` costs, and what the cost is made of** — the measurement behind "is this design right
//! for a very large project".
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "C:\Users\zc\Desktop\github\EmmyLuaCodeStyle"
//! $env:CPPLS_PROBE_DIRS = "CodeFormatCore,CodeFormatLib,LuaParser,Util"
//! cargo test --release -p cpp_code_analysis --test summary_cost_probe -- --ignored --nocapture
//! ```
//!
//! `CPPLS_PROBE_DIRS` is the comma-separated list of directories to index, and `CPPLS_PROBE_TOP` how many files to
//! print (default 5).
//!
//! # The question
//!
//! `ProjectIndex::summaries` holds **every** summary a session has read, with no budget and no eviction: for a
//! session whose closure is 1 761 files the memory report said 240 MB, which is 136 KB a file. Whether that is the
//! right design is a question about the *per-file* size and about what it is made of, so this prints both: the total
//! and the split between declarations, macros, includes, the use filter and the module reading.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\test_cpp".to_string()),
    )
}

fn the_sources(root: &Path) -> Vec<PathBuf> {
    let directories: Vec<String> = std::env::var("CPPLS_PROBE_DIRS")
        .map(|dirs| dirs.split(',').map(|dir| dir.trim().to_string()).collect())
        .unwrap_or_else(|_| vec!["src".to_string()]);
    let mut files = Vec::new();
    for directory in directories {
        let mut pending = vec![root.join(directory)];
        while let Some(next) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "cpp") {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    files
}

fn mb(bytes: usize) -> usize {
    bytes / (1024 * 1024)
}

#[test]
#[ignore = "a probe: indexes a real project and reports instead of asserting"]
fn what_a_summary_costs_and_why() {
    let root = project();
    let sources = the_sources(&root);
    println!("project  {} ({} source(s))", root.display(), sources.len());

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(sources);
    let started = Instant::now();
    session.index_everything();
    while session.pending_work() > 0 {
        session.advance(64);
    }
    println!(
        "indexed in {:?} — {} summaries, {} MB",
        started.elapsed(),
        session.memory_report().summaries,
        mb(session.memory_report().summary_bytes),
    );

    // **Per file, split by field.** The total is what the report prints; the split is what says whether the cost is
    // the declarations (a fact per name) or something else that could be dropped.
    let mut files: Vec<(&Path, usize, usize, usize, usize, usize, usize)> = Vec::new();
    let mut total = 0usize;
    let mut total_declarations = 0usize;
    for summary in session.index().summaries() {
        let declarations: usize = summary
            .declarations
            .iter()
            .map(cpp_code_analysis::DeclFact::heap_bytes)
            .sum();
        let macros: usize = summary
            .macros
            .iter()
            .map(cpp_code_analysis::MacroFact::heap_bytes)
            .sum();
        let includes: usize = summary
            .includes
            .iter()
            .map(cpp_code_analysis::IncludeFact::heap_bytes)
            .sum();
        let bytes = summary.heap_bytes();
        total += bytes;
        total_declarations += summary.declarations.len();
        files.push((
            &summary.path,
            bytes,
            summary.declarations.len(),
            declarations,
            macros,
            includes,
            summary.use_filter.len(),
        ));
    }
    println!(
        "\n{} summary(ies), {total} bytes ({0} MB), {total_declarations} declaration(s) — average {} bytes a file, \
         {} bytes a declaration",
        mb(total),
        total / files.len().max(1),
        total / total_declarations.max(1),
    );

    files.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let top: usize = std::env::var("CPPLS_PROBE_TOP")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5);
    println!("\n# the largest {top}");
    for (path, bytes, declarations, declaration_bytes, macros, includes, filter) in
        files.iter().take(top)
    {
        println!(
            "{:>8} bytes  {:>5} decl {:>9} B  {:>9} B macros {:>7} B includes {:>6} B filter  {}",
            bytes,
            declarations,
            declaration_bytes,
            macros,
            includes,
            filter,
            path.display()
        );
    }

    // **The three worst files, and what a large project would hold.** A closure of `n` files at this project's
    // average is the number the design has to justify.
    println!(
        "\nat the average above: 1 000 files = {} MB, 10 000 = {} MB, 100 000 = {} MB",
        total / files.len().max(1) * 1_000 / (1024 * 1024),
        total / files.len().max(1) * 10_000 / (1024 * 1024),
        total / files.len().max(1) * 100_000 / (1024 * 1024),
    );
}
