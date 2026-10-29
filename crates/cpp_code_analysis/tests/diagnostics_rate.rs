//! **How many findings a real project's files produce, by code** — the diagnostics half of the scoreboard.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\neovim"
//! $env:CPPLS_DIAGNOSTIC_FILES = "10"        # how many files to ask about (default 10)
//! cargo test --release -p cpp_code_analysis --test diagnostics_rate -- --ignored --nocapture
//! ```
//!
//! # Why a rate, and why by code
//!
//! A finding is a claim the editor underlines, and the only thing that makes such a claim worth shipping is that it
//! is **true**. One false positive per file is enough to make a reader turn the feature off, after which it reports
//! nothing at all — so the number that matters is not "does the checker work" but "how many findings does a file of
//! real code produce, and are they the type's own".
//!
//! The C-style-cast false positive this caught (`cstyle-cast` on 14 lines of a C file, because the project's
//! `-std=gnu11` never reached `CheckContext::is_c`) was found by reading one file's list. A project's list, grouped by
//! code, is how the next one is found without reading every file.
//!
//! # One session per file, opened the way the server opens it
//!
//! Each file is **opened** (`did_open`) and indexed with itself as the only project file — the shape
//! `inference_probe` uses and the shape a server sees on `textDocument/didOpen`. Indexing the whole project first and
//! then asking about a *closed* file is the slow path: `want_cooked_reading` on such a file cooks renderings for its
//! entire closure, which on a Windows checkout is the SDK and takes minutes per file (measured: over ten minutes for
//! one file, against twenty seconds this way).
//!
//! Nothing is asserted here: the codes that must stay quiet are pinned in `tests/checks.rs` against fixtures, where a
//! failure says which rule broke. This says where to look.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_REAL_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    named.is_dir().then_some(named)
}

/// The 1-based line a byte offset is on. A diagnostic's range is in the rendering's coordinates, so this is
/// indicative rather than exact — the message is what matters.
fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset.min(text.len()))
        .map(|before| before.matches('\n').count() + 1)
        .unwrap_or(0)
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn the_diagnostics_of_a_real_projects_files() {
    let Some(root) = project() else {
        eprintln!("skipped: no project (set CPPLS_REAL_PROJECT)");
        return;
    };
    let wanted: usize = std::env::var("CPPLS_DIAGNOSTIC_FILES")
        .ok()
        .and_then(|count| count.parse().ok())
        .unwrap_or(10);
    let trace = std::env::var_os("CPPLS_TRACE_PROGRESS").is_some();

    // **The project's own sources, and only those.** A header is included by many translation units, so a finding in
    // one is multiplied across them in the editor; a `.c`/`.cpp` is what a reader actually opens.
    let opened = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let mut sources: Vec<PathBuf> = opened
        .project_files()
        .iter()
        .filter(|path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("c" | "cc" | "cpp" | "cxx")
            )
        })
        .cloned()
        .collect();
    sources.sort();
    sources.truncate(wanted);
    drop(opened);
    println!("asking about {} source file(s)", sources.len());

    let mut by_code: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut files_with_findings = 0usize;

    for path in &sources {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("skipped {path:?}: {error}");
                continue;
            }
        };
        let started = std::time::Instant::now();
        let mut session = Session::open(
            root.clone(),
            SessionFiles::new(OpenDocuments::new(), DiskFiles),
            WatchFilter::new(&root),
        );
        session.did_open(path, &text);
        session.add_project_files([path.clone()]);
        session.index_everything();
        while session.pending_work() > 0 {
            session.advance(64);
        }
        session.want_cooked_reading(path);
        while session.pending_work() > 0 {
            session.advance(64);
        }

        let Some(found) = session.diagnostics(path) else {
            eprintln!(
                "[file] {}: nothing to say ({:?})",
                path.file_name().unwrap_or_default().to_string_lossy(),
                started.elapsed()
            );
            continue;
        };
        let mut here = 0usize;

        for finding in &found.checks {
            here += 1;
            let code = finding.code.name().to_string();
            *by_code.entry(code.clone()).or_default() += 1;
            let entry = samples.entry(code).or_default();
            if entry.len() < 3 {
                entry.push(format!(
                    "  {}:{}: {}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    line_of(&text, finding.range.start_offset),
                    finding.message.chars().take(90).collect::<String>()
                ));
            }
        }
        for error in &found.errors {
            here += 1;
            let code = error.code.name().to_string();
            *by_code.entry(code.clone()).or_default() += 1;
            let entry = samples.entry(code).or_default();
            if entry.len() < 3 {
                entry.push(format!(
                    "  {}:{}: {}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    line_of(&text, error.start),
                    error.message.chars().take(90).collect::<String>()
                ));
            }
        }
        if here > 0 {
            files_with_findings += 1;
        }
        if trace {
            eprintln!(
                "[file] {}: {here} finding(s), {:?}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                started.elapsed()
            );
        }
    }

    let total: usize = by_code.values().sum();
    println!(
        "\n{} file(s) asked, {total} finding(s), {files_with_findings} file(s) with at least one",
        sources.len()
    );
    let mut codes: Vec<(&String, usize)> =
        by_code.iter().map(|(code, count)| (code, *count)).collect();
    codes.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for (code, count) in codes {
        println!("  {count:>5}  {code}");
    }

    println!("\nsamples, by code — the message is what a reader would see:");
    for (code, entries) in &samples {
        println!("--- {code}");
        for entry in entries {
            println!("{entry}");
        }
    }
}
