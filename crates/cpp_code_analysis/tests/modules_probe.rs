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
//!
//! # What it says now, and what it said
//!
//! The `import std;` read — `read_the_modules_a_file_imports`, which is one drain of a 401-file closure — measured
//! **24.7 s cold and 112–506 s warm** when this probe was written, and **2.5 s cold and 1.2–1.7 s warm** after the
//! three fixes it found (five warm runs: 1.22, 1.24, 1.24, 1.62, 1.67 s). The database is not part of either number:
//! `cache_db::io` reports **0.04 s of reads** in a run, and the process is CPU-bound throughout.
//!
//! Warm is now **faster than cold**, which is what the cache is for. The three fixes, in the order they were found:
//!
//! * a reading the store **served** was queued for a full cook, so a warm start rendered and parsed every fact it had
//!   just read (`prepare_a_wave`);
//! * a **batch** read files as programs when a timeline was merely *held*, so a drain that had just built timelines
//!   walked a closure per file (`will_read_it_as_the_program`);
//! * the second pass walked **one timeline per candidate** — 44 of them for a six-file slice, 46.8 s
//!   (`finish_a_slice`, `units_for_the_pass`).
//!
//! Still not done, and visible here: `format` is not offered after `std::`. The names come from the headers of the
//! closure; the standard *module's* exports are P5's work.
//!
//! **And reading programs does not produce them**, which was worth measuring rather than assuming. With
//! `CPPLS_MODULES_UNIT=1` the probe reads up to three programs before asking:
//!
//! ```text
//! unit read 1: 1 file(s) in 368.8µs      (main.cpp — `import std;` is not an `#include`, so its closure is itself)
//! unit read 2: 81 file(s) in 1.04 s
//! unit read 3: 81 file(s) in 1.06 s
//! completion after `std::`: [… no `format` …]
//! ```
//!
//! So a program read of the *importing* file places nothing from the module: what `std::format` needs is the
//! **interface unit's** own exports (`std.ixx`'s `export namespace std { using … }` block), read as the program it is.
//! The probe's `unit read 2` is one of the project's own units, not `std.ixx`.

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

    // **A cold store, only when asked** — `CPPLS_MODULES_COLD=1`. A probe that deleted a real checkout's cache
    // unasked would be destroying a warm start to take a measurement.
    // **Unset or empty means off** — an empty variable is what a shell leaves behind after `$env:X=""`, and treating
    // it as "on" is how the second half of a cold/warm comparison silently deletes the warm cache.
    if std::env::var_os("CPPLS_MODULES_COLD").is_some_and(|value| !value.is_empty()) {
        let _ = std::fs::remove_dir_all(root.join(".cppls"));
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

    // **What the database itself cost** — see `cache_db::io`. The question is why the same operation on the same
    // cache takes 0.8 s in one run and 51 s in the next, and every phase inside the analysis measured milliseconds in
    // both: the answer is either in here or outside the process.
    cpp_code_analysis::cache::cache_db::io::reset();
    let mut cumulative = StoreStats::default();
    let mut total = std::time::Duration::ZERO;
    for file in &order {
        let before = session.stats();
        let started = Instant::now();

        let report = session.memory_report();
        if std::env::var_os("CPPLS_MODULES_MEMORY").is_some_and(|value| !value.is_empty()) {
            println!(
                "  memory: vfs {:.0} MB ({} files), summaries {:.0} MB ({}), cooked {:.0} MB ({}), units {:.0} MB ({}), renderings {:.0} MB ({}), views {:.0} MB ({})",
                report.vfs_text_bytes as f64 / 1e6,
                report.vfs_files,
                report.summary_bytes as f64 / 1e6,
                report.summaries,
                report.cooked_bytes as f64 / 1e6,
                report.cooked_files,
                report.unit_bytes as f64 / 1e6,
                report.units,
                report.rendering_bytes as f64 / 1e6,
                report.renderings,
                report.view_bytes as f64 / 1e6,
                report.views,
            );
        }
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
    let (reads, read_nanos, writes, write_nanos) =
        cpp_code_analysis::cache::cache_db::io::statistics();
    println!(
        "database: {reads} read(s) in {:.2} s, {writes} write(s) in {:.2} s",
        read_nanos as f64 / 1e9,
        write_nanos as f64 / 1e9,
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
        // **Does reading the program make the module's exports visible?** The question the completion line below asks
        // in its failure note, answered by forcing the read and asking again.
        if std::env::var_os("CPPLS_MODULES_UNIT").is_some_and(|value| !value.is_empty()) {
            for round in 1..=3 {
                let one = Instant::now();
                let read = session.read_a_unit_of_the_project();
                println!(
                    "unit read {round}: {} file(s) in {:?}",
                    read.map_or(0, |reading| reading.files),
                    one.elapsed()
                );
            }
        }
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
            // **The completion is capped**, so a missing `format` here means "past the thousandth name" as often as
            // it means "not exported". The index is asked directly instead of inferring from a truncated list.
            let declared = session
                .index()
                .declarations_in("std", main)
                .iter()
                .filter(|declaration| declaration.fact.name.as_str() == "format")
                .count();
            println!(
                "  ← `format` is not among the {} offered (truncated: {}); the index holds {declared} `std::format`",
                labels.len(),
                offered.truncated
            );
            // **Which standard the module was read as.** `_MSVC_LANG` is the compiler flags` own answer and
            // `_HAS_CXX20` is what every C++20 header in the standard library branches on.
            let macros = session.index().macros();
            println!(
                "  _MSVC_LANG defined: {}, _HAS_CXX20 defined: {}",
                macros.is_defined("_MSVC_LANG"),
                macros.is_defined("_HAS_CXX20"),
            );
            println!(
                "  the program that read main.cpp: {:?}",
                session.index().the_program_that_read(main)
            );
            if let Some(std_ixx) = session
                .project_files()
                .iter()
                .find(|path| path.file_name().and_then(|name| name.to_str()) == Some("std.ixx"))
            {
                println!(
                    "  the program that read std.ixx: {:?}",
                    session.index().the_program_that_read(std_ixx)
                );
            }
            for name in ["vformat", "make_format_args", "format", "to_string"] {
                let held = session
                    .index()
                    .declarations_in("std", main)
                    .iter()
                    .filter(|declaration| declaration.fact.name.as_str() == name)
                    .count();
                println!("  std::{name} visible from main.cpp: {held}");
            }
            for header in ["/format", "/vector", "/ranges"] {
                let path = session
                    .index()
                    .summaries()
                    .map(|summary| summary.clone())
                    .find(|summary| summary.path.to_string_lossy().ends_with(header));
                if let Some(summary) = path {
                    let sample: Vec<String> = summary
                        .declarations
                        .iter()
                        .filter(|fact| fact.name.as_str() == "format")
                        .map(|fact| format!("format in scope {:?}", fact.scope))
                        .take(3)
                        .collect();
                    let first: Vec<String> = summary
                        .declarations
                        .iter()
                        .take(3)
                        .map(|fact| format!("{} in {:?}", fact.name, fact.scope))
                        .collect();
                    println!(
                        "  summary of <{}>: {} declaration(s), {} macro fact(s); format facts {:?}; first {:?}",
                        &header[1..],
                        summary.declarations.len(),
                        summary.macros.len(),
                        sample,
                        first,
                    );
                }
            }
            // **Which of the three links is broken** — the header in the index, the interface unit's own reading, or
            // the importer's view of it. `CPPLS_MODULES_WHY=1` answers each in one line instead of guessing.
            if std::env::var_os("CPPLS_MODULES_WHY").is_some_and(|value| !value.is_empty()) {
                let formats: Vec<String> = session
                    .index()
                    .summaries()
                    .map(|summary| summary.path.to_string_lossy().to_string())
                    .filter(|path| path.contains("format"))
                    .collect();
                println!("  summaries naming format: {formats:?}");
                let imports = session
                    .index()
                    .summary(main)
                    .map(|summary| format!("{:?}", summary.modules.imports));
                println!("  main.cpp imports: {imports:?}");
                let std_ixx = session
                    .project_files()
                    .iter()
                    .find(|path| path.file_name().and_then(|name| name.to_str()) == Some("std.ixx"))
                    .cloned();
                println!("  std.ixx among the project files: {std_ixx:?}");
                if let Some(std_ixx) = std_ixx {
                    let own = session
                        .index()
                        .declarations_in("std", &std_ixx)
                        .iter()
                        .filter(|declaration| declaration.fact.name.as_str() == "format")
                        .count();
                    let from_main = session.index().declarations_in("std", main).len();
                    println!(
                        "  declarations_in(std) holding format from std.ixx: {own}; \
                         all visible from main.cpp: {from_main}"
                    );
                }
                // **The declaration itself**: is `format` in the index at all, and is it visible from its own
                // header? A `0` here is a fact about the index; a `0` only from `main.cpp` is a fact about visibility.
                if let Some(header) = formats
                    .iter()
                    .find(|path| path.ends_with("/format"))
                    .map(std::path::PathBuf::from)
                {
                    let held = session
                        .index()
                        .declarations_in("std", &header)
                        .iter()
                        .filter(|declaration| declaration.fact.name.as_str() == "format")
                        .count();
                    println!("  declarations_in(std) holding format from <format>: {held}");
                }
            }
        }
    }
}
