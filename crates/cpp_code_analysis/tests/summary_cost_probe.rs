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

    // **What the number is made of.** `heap_bytes` charges the struct once per fact and then the strings; splitting
    // them says whether the cost is the fact or what it owns — the question a refactor of either has to answer.
    let struct_bytes = std::mem::size_of::<cpp_code_analysis::DeclFact>();
    let mut inline_names = 0usize;
    let mut heap_names = 0usize;
    for summary in session.index().summaries() {
        for fact in &summary.declarations {
            if fact.name.is_heap_allocated() {
                heap_names += 1;
            } else {
                inline_names += 1;
            }
        }
    }
    println!(
        "\n# the hot structs, and how many of each\ndeclaration facts: {} at {} bytes\nmacro facts: {} at {} bytes\n\
         include facts: {} at {} bytes\nfiles: {} at {} bytes (a summary)",
        total_declarations,
        struct_bytes,
        session
            .index()
            .summaries()
            .map(|summary| summary.macros.len())
            .sum::<usize>(),
        std::mem::size_of::<cpp_code_analysis::MacroFact>(),
        session
            .index()
            .summaries()
            .map(|summary| summary.includes.len())
            .sum::<usize>(),
        std::mem::size_of::<cpp_code_analysis::IncludeFact>(),
        session.index().summaries().count(),
        std::mem::size_of::<cpp_code_analysis::FileSummary>(),
    );
    println!(
        "\n# the fact itself\nsize_of::<DeclFact>() = {struct_bytes} bytes ({}, {:.0}% of the total), the structs alone \
         = {} MB\nnames: {inline_names} inline (no allocation), {heap_names} on the heap",
        struct_bytes * total_declarations / (1024 * 1024),
        (struct_bytes * total_declarations) as f64 * 100.0 / total as f64,
        struct_bytes * total_declarations / (1024 * 1024),
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

    // **Is every file's summary keyed by its own macro environment?** — the question a cache's hit rate is: the disk
    // entry is keyed by `(path, env_hash, context_hash)`, so N distinct environments over the same closure means N
    // copies on disk and no reuse of a summary built under a different one.
    let mut envs: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut contexts: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut paths: std::collections::HashSet<&Path> = std::collections::HashSet::new();
    let mut keys: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    for summary in session.index().summaries() {
        envs.insert(summary.key.env_hash);
        contexts.insert(summary.key.context_hash);
        paths.insert(&summary.path);
        keys.insert((summary.key.env_hash, summary.key.context_hash));
    }
    println!(
        "\n# the keys the summaries are stored under\n{} distinct file(s), {} distinct env hash(es), \
         {} distinct context hash(es), {} distinct (env, context) pair(s)",
        paths.len(),
        envs.len(),
        contexts.len(),
        keys.len(),
    );

    // **Is the context multiplying entries, or is it one key per file?** A key is `(content, context)`; the contexts
    // vary because a file's own **directory** is part of the hash (a quoted `#include` searches it first), so what
    // matters is whether one *content* is stored under several contexts — that would be the multiplier.
    let mut contents: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    let mut pairs: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    let mut directories: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    for summary in session.index().summaries() {
        *contents.entry(summary.key.content_hash).or_default() += 1;
        pairs.insert((summary.key.content_hash, summary.key.context_hash));
        if let Some(parent) = summary.path.parent() {
            directories.insert(parent.to_path_buf());
        }
    }
    let shared_contents = contents.values().filter(|count| **count > 1).count();
    println!(
        "\n# and what a context is made of\n{} distinct content hash(es) — {} of them stored more than once \
         ({:?} copies at the worst) — {} distinct (content, context) pair(s), {} distinct director(ies)\n\
         so a context varies **across files** (one per directory) and does not multiply any file's entries",
        contents.len(),
        shared_contents,
        contents.values().copied().max().unwrap_or(0),
        pairs.len(),
        directories.len(),
    );
}
