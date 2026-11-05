//! **What a refinement costs, today** — the number the lazy design is judged by.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! $env:CPPLS_TRACE_COOKING = "1"          # optional: the drain's own lines, per slice
//! cargo test --release -p cpp_code_analysis --test refinement_probe -- --ignored --nocapture
//! ```
//!
//! Phase 0 of `docs/indexing-design.md` made refinement **request-driven**: a start refines nothing, and a file is
//! refined when something asks for it. That is only affordable if a refinement is cheap, and the numbers the design
//! quotes are from before phase 2 ("one file: 93.6 ms", "150 ms per file"). This probe measures what the code does
//! now, on a real project.
//!
//! **Bounded on purpose.** It indexes a named handful of files rather than the project, because the *batch* path
//! (`index_everything`) records a refinement ask for every file it reads — see `Session::index_everything` — and that
//! is a different measurement, taken separately below with the count and the time it implies rather than by waiting
//! for it.
//!
//! The three questions it answers:
//!
//! * **what does one refinement cost**, split into the re-read and the commit;
//! * **is a refinement done once** — asking twice must cost nothing the second time;
//! * **is the second time served from the disk cache**, and when it is not, why
//!   (`the_stored_refinement`'s own reason lines, printed by `CPPLS_TRACE_COOKING`).

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// Every distinct word of a text, the way the pass counts them: identifier-shaped runs, deduplicated.
fn words_of(text: &str) -> Vec<&str> {
    let mut seen = std::collections::HashSet::new();
    for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if !word.is_empty() {
            seen.insert(word);
        }
    }
    seen.into_iter().collect()
}

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_REAL_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    named.is_dir().then_some(named)
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn what_a_refinement_costs() {
    let Some(project) = project() else {
        eprintln!("skipped: no project (set CPPLS_REAL_PROJECT)");
        return;
    };

    let mut session = Session::open(
        project.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project),
    );

    // **A handful of files, not the project.** The first few the scan found, which are the project's own sources
    // rather than a vendor tree.
    let n: usize = std::env::var("CPPLS_REFINE_COUNT")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(2);
    let all = session.project_files().to_vec();
    let chosen: Vec<PathBuf> = all.iter().take(n).cloned().collect();
    for path in &chosen {
        println!("  refining {}", path.display());
    }
    println!(
        "project: {} file(s); refining {} of them",
        all.len(),
        chosen.len()
    );

    session.add_project_files(chosen.clone());
    let started = std::time::Instant::now();
    let mut slices = 0usize;
    while session.pending() > 0 && slices < 200 {
        session.advance(16);
        slices += 1;
    }
    println!(
        "index: {} slice(s) in {:?}, re-read backlog {}",
        slices,
        started.elapsed(),
        session.re_read_backlog()
    );

    // **The asks, one at a time, the way a hover makes them.** `advance` serves an ask only when the index queue is
    // idle, which it is here, so each round is exactly one refinement (or none, when the file is already refined).
    let mut refined = 0usize;
    let mut total = std::time::Duration::ZERO;
    let mut worst = std::time::Duration::ZERO;
    for path in &chosen {
        let before = session.is_refined(path);
        println!(
            "  asking for {} (refined already: {before})",
            path.display()
        );
        // **What the pass will consider**, which is the number the cost scales with: the words the file mentions,
        // and the files that define a macro of one of those names. Printed before the ask so a refinement that
        // never returns still leaves the two numbers behind.
        if let Some(text) = session.text(path) {
            let words = words_of(&text);
            let candidates = session
                .index()
                .files_defining_any_macro(words.iter().copied())
                .len();
            println!(
                "    {} distinct word(s) mentioned, {} file(s) define a macro of one of them",
                words.len(),
                candidates
            );
        }
        session.want_refinement(path);
        let started = std::time::Instant::now();
        session.advance(4);
        let took = started.elapsed();
        let after = session.is_refined(path);
        if after && !before {
            refined += 1;
            total += took;
            worst = worst.max(took);
        }
        println!(
            "  {:<44} refined {} -> {} in {:?}",
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default(),
            before,
            after,
            took
        );
    }
    println!(
        "\nrefined {refined} of {n}: {:.1} ms each on average, {:.1} ms worst",
        total.as_secs_f64() * 1000.0 / refined.max(1) as f64,
        worst.as_secs_f64() * 1000.0
    );

    // **The same asks again** — a refinement that is not remembered is a bug rather than a cost.
    let mut repeated = std::time::Duration::ZERO;
    let mut queued = 0usize;
    for path in &chosen {
        let started = std::time::Instant::now();
        session.want_refinement(path);
        queued += session.refinements_wanted();
        session.advance(4);
        repeated += started.elapsed();
    }
    println!(
        "the same {n} ask(s) again: {:.3} ms each, {} ask(s) queued (0 means the mark is what refuses them)",
        repeated.as_secs_f64() * 1000.0 / n as f64,
        queued
    );

    println!("\nmemory: {}", session.memory_report());
}
