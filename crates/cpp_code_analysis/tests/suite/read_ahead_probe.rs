//! **How much read-ahead a session holds while it indexes**, and that the budget holds it.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test suite read_ahead_probe -- --nocapture
//! CPPLS_REAL_PROJECT=… CPPLS_PROBE_DIRS=… \
//!   cargo test --release -p cpp_code_analysis --test suite read_ahead_probe -- --ignored --nocapture
//! ```
//!
//! # Why the read-ahead needed a budget at all
//!
//! The pump prepares a wave ahead of the queue: the files about to be read, and — through `prepare_closure` — every
//! file *they* include. The bound on that was a **count**, `steps * 4` entries, and a count is not a bound on bytes:
//! the largest prepared entries are standard-library headers whose summary is a megabyte. Measured on
//! `EmmyLuaCodeStyle`, a session with the project indexed held **183 MB** of prepared-but-uncommitted slices.
//!
//! `Session::READ_AHEAD_BUDGET_BYTES` is 32 MB and the two insert paths (`commit_a_wave` — the server's — and
//! `advance` — the in-process one) trim to it. The test below is the budget's own: a project whose headers are large
//! enough that an unbounded read-ahead would hold several times the budget.

use std::path::PathBuf;

use cpp_code_analysis::{
    CompilerConfig, DiskFiles, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// **A project whose prepared summaries are worth more than the budget.**
///
/// Forty headers of four thousand declarations each: at the `DeclFact` size this repository measures (160 bytes) and
/// the strings beside it, one summary is roughly 700 KB, so the read-ahead only has to reach *fifty* of them to pass
/// 32 MB — which a wave of `steps` files and their closure does within a few slices.
fn big_project() -> MemoryFiles {
    let mut memory = MemoryFiles::new();
    let mut sources: Vec<String> = Vec::new();
    for header in 0..40 {
        let mut text = String::from("#pragma once\nnamespace big {\n");
        for item in 0..4000 {
            text.push_str(&format!("int h{header}_v{item};\n"));
        }
        text.push_str("}\n");
        let path = format!("/p/h{header}.h");
        memory = memory.with_file(&path, &text);
        sources.push(path);
    }
    for unit in 0..8 {
        let mut text = String::new();
        for header in 0..40 {
            text.push_str(&format!("#include \"h{header}.h\"\n"));
        }
        text.push_str(&format!("int unit{unit};\n"));
        let path = format!("/p/u{unit}.cpp");
        memory = memory.with_file(&path, &text);
        sources.push(path);
    }
    memory
}

#[test]
fn the_read_ahead_stays_inside_its_budget() {
    let root = PathBuf::from("/p");
    let providers = SessionFiles::new(OpenDocuments::new(), big_project());
    let mut session = Session::with_config(
        &root,
        providers,
        WatchFilter::new(&root),
        CompilerConfig::default(),
    );
    let mut files = Vec::new();
    for unit in 0..8 {
        files.push(PathBuf::from(format!("/p/u{unit}.cpp")));
    }
    for header in 0..40 {
        files.push(PathBuf::from(format!("/p/h{header}.h")));
    }
    session.add_project_files(files);
    for unit in 0..8 {
        session.want_cooked_reading(std::path::Path::new(&format!("/p/u{unit}.cpp")));
    }

    let mut peak = 0usize;
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 200 {
        session.advance(16);
        let report = session.memory_report();
        peak = peak.max(report.prefetched_bytes);
        slices += 1;
    }
    println!(
        "{slices} slice(s): read-ahead peak {} MB ({} file(s) at the most), summaries {} MB",
        peak / (1024 * 1024),
        session.memory_report().prefetched,
        session.memory_report().summary_bytes / (1024 * 1024),
    );

    // **The budget is the assertion**, with the slack of the wave in hand: a wave is prepared before it is committed,
    // so its own files are not yet counted by `prefetched_bytes` when the trim runs.
    const BUDGET: usize = 32 * 1024 * 1024;
    assert!(
        peak <= BUDGET + 8 * 1024 * 1024,
        "the read-ahead peaked at {} MB, over the 32 MB budget",
        peak / (1024 * 1024),
    );
}

/// The same question on a project on this machine — `CPPLS_REAL_PROJECT`, `CPPLS_PROBE_DIRS`.
#[test]
#[ignore = "a probe: reads a project on this machine"]
fn the_read_ahead_on_a_real_project() {
    let root = Option::<std::ffi::OsString>::None
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Users\\zc\\Desktop\\test_cpp"));
    let directories: Vec<String> = std::env::var("CPPLS_PROBE_DIRS")
        .map(|dirs| dirs.split(',').map(|dir| dir.trim().to_string()).collect())
        .unwrap_or_else(|_| vec!["src".to_string(), "include".to_string()]);

    let mut files = Vec::new();
    let mut pending: Vec<PathBuf> = directories.iter().map(|dir| root.join(dir)).collect();
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("cpp" | "cc" | "h" | "hpp")
            ) {
                files.push(path);
            }
        }
    }
    files.sort();
    if files.is_empty() {
        eprintln!("[read-ahead] no sources under {}", root.display());
        return;
    }

    let providers = SessionFiles::new(OpenDocuments::new(), DiskFiles);
    let mut session = Session::with_config(
        &root,
        providers,
        WatchFilter::new(&root),
        CompilerConfig::default(),
    );
    session.add_project_files(files.iter().cloned());
    session.index_everything();

    let mut peak = 0usize;
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 400 {
        let wave = session.prepare_background_wave(24);
        let _ = session.commit_a_wave(wave, 24);
        session.advance(24);
        peak = peak.max(session.memory_report().prefetched_bytes);
        slices += 1;
    }
    println!(
        "{} file(s), {slices} slice(s): read-ahead peak {} MB, settled at {} MB of session tables",
        files.len(),
        peak / (1024 * 1024),
        // **The session's own estimate, not the process's resident set.** The allocator byte counter this line used
        // to read is gone (see `docs/open-questions.md`); what a probe about *read-ahead* wants is the bytes the
        // session says it holds, and `MemoryReport` is that number by construction.
        session.memory_report().total() / (1024 * 1024),
    );
}
