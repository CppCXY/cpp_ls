//! **What `import std;` costs, and how the cost scales with the number of files** — a probe, not a test.
//!
//! ```text
//! $env:CPPLS_MODULES_PROJECT = "C:\Users\zc\Desktop\cpp_modules"
//! cargo test --release -p cpp_code_analysis --test modules_probe -- --ignored --nocapture
//! ```
//!
//! The question is the one a user asks with their hands: open file 1, then file 2, then file 3, and see how long
//! each *first* answer takes. A cost that is paid once and reused is a cache; a cost that is paid per file is a second
//! order relation, and the second one is what makes a C++23 project feel slower than the same project written with
//! `#include`s.
//!
//! Every number here is a wall-clock measurement of the analysis alone, and `summaries built` is the count of files
//! that were **parsed** — the unit of work that a cache is supposed to remove.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{
    DiskFiles, Known, OpenDocuments, Session, SessionFiles, StoreStats, WatchFilter,
};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_MODULES_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\cpp_modules"));
    named.is_dir().then_some(named)
}

/// Every source file of the project, sorted so two runs measure the same order.
fn sources(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // The build directory and a `.git` are not sources.
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                if name != "build" && name != "out" && !name.starts_with('.') {
                    pending.push(path);
                }
                continue;
            }
            let extension = path.extension().and_then(|extension| extension.to_str());
            if matches!(extension, Some("cpp" | "cppm" | "cc" | "cxx" | "ixx")) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn delta(before: StoreStats, after: StoreStats) -> String {
    let change = after.since(before);
    format!("parsed {} reused {}", change.rebuilt, change.reused)
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn what_import_std_costs() {
    let Some(root) = project() else {
        eprintln!("skipped: no project (set CPPLS_MODULES_PROJECT)");
        return;
    };
    let files = sources(&root);
    if files.is_empty() {
        eprintln!("skipped: no sources");
        return;
    }

    let started = Instant::now();
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(files.clone());
    let before_index = session.stats();
    session.index_everything();
    println!(
        "index of {} file(s): {:?} ({})",
        files.len(),
        started.elapsed(),
        delta(before_index, session.stats())
    );

    // The order an editor opens them in: `main.cpp` last, because that is the one that imports the most.
    let mut order = files.clone();
    order.sort_by_key(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| name == "main.cpp")
            .unwrap_or(false)
    });

    let mut cumulative = StoreStats::default();
    let mut total = std::time::Duration::ZERO;
    for file in &order {
        let before = session.stats();
        let started = Instant::now();

        let imports_unread = session.imports_a_module_that_is_not_read(file);
        let read = session.read_the_modules_a_file_imports(file);
        let view_time = Instant::now();
        let _ = session.view_of_the_file(file);
        let view_time = view_time.elapsed();
        let elapsed = started.elapsed();
        total += elapsed;

        let (parsed, reused) = {
            let change = session.stats().since(before);
            cumulative = session.stats();
            (change.rebuilt, change.reused)
        };
        println!(
            "{:<16} {:>8.1} ms  (modules: unread {} read {}; view {:>6.1} ms)  parsed {parsed} reused {reused}",
            file.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?"),
            elapsed.as_secs_f64() * 1000.0,
            imports_unread,
            read,
            view_time.as_secs_f64() * 1000.0,
        );
        let _ = before;
    }

    println!(
        "total {:?}; the session parsed {} file(s) and reused {}",
        total, cumulative.rebuilt, cumulative.reused
    );

    // **A completion that needs the standard library.** `std::` in `main.cpp`, where `import std;` is the only
    // reason anything is known at all.
    let main = order
        .iter()
        .find(|path| path.file_name().and_then(|name| name.to_str()) == Some("main.cpp"));
    if let Some(main) = main {
        let text = std::fs::read_to_string(main).expect("the file is readable");
        let Some(at) = text.find("std::format").map(|at| at + "std::".len()) else {
            println!("main.cpp writes no `std::`");
            return;
        };
        let started = Instant::now();
        let Some(view) = session.view_of_the_file(main) else {
            println!("main.cpp has no view");
            return;
        };
        let offered = session.completions(&view, FileOffset::new(at));
        let labels: Vec<&str> = offered
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        println!(
            "completion after `std::`: {:?} in {:?} ({} item(s){} )",
            &labels[..labels.len().min(10)],
            started.elapsed(),
            labels.len(),
            if offered.truncated { ", truncated" } else { "" }
        );
        if !labels.iter().any(|label| *label == "format") {
            println!("  ← `format` is NOT offered: the standard module was not read");
        }
    }
}
