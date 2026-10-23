//! **A rule, checked by a test instead of by review**: the analysis keeps its state in its own objects, not in
//! threads or in the process.
//!
//! `docs/memory-audit.md` §3.2 states the rule and lists what broke it:
//!
//! ```text
//! · a `thread_local!` projection memo in the walk census  — fixed: one mutex-guarded map, because a diagnostic
//!   that duplicates per thread is unbounded in threads
//! · a process-wide walk-contribution table                — fixed: a field of `SummaryStore`
//! · three process-wide caches (fuzzy, redb handles, …)     — the two below, each with a reason
//! ```
//!
//! The remaining two are **named here rather than left to be discovered**, and this test is what makes the list
//! honest: a new `thread_local!` or a new process-wide cache fails the build until it is either removed or added to
//! the list with a reason. A rule that exists only in a document is a rule that erodes.

use std::path::Path;

/// Files allowed to hold a process-wide cache, and why.
const ALLOWED: &[(&str, &str)] = &[
    (
        "walk_census.rs",
        "the walk census is a diagnostic that is off unless CPPLS_WALK_CENSUS is set, and its two tables are counters \
         and a memo whose single copy is the point",
    ),
    (
        "fuzzy.rs",
        "one index per project root, bounded by `FuzzyIndex::build`'s own depth and file count; moving it needs the \
         resolver to take the cache as a parameter, which is a change to every resolver construction",
    ),
    (
        "cache_db.rs",
        "one writer per database **path** is a cross-session invariant, not a session's state: two redb handles on \
         one file fail to open, so the table is what makes a reload reuse the open writer",
    ),
];

#[test]
fn no_thread_local_state() {
    let mut offenders = Vec::new();
    for (path, source) in sources() {
        for (number, line) in source.lines().enumerate() {
            if line.trim_start().starts_with("thread_local!") {
                offenders.push(format!("{}:{}", path.display(), number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "state belongs to an owner, not to a thread (docs/memory-audit.md §3.2): {offenders:#?}"
    );
}

#[test]
fn no_new_process_wide_caches() {
    let mut offenders = Vec::new();
    for (path, source) in sources() {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        for (number, line) in source.lines().enumerate() {
            let trimmed = line.trim_start();
            let is_a_cache = trimmed.starts_with("static ")
                && (trimmed.contains("OnceLock<Mutex<") || trimmed.contains("OnceLock<RwLock<"));
            if is_a_cache && !ALLOWED.iter().any(|(allowed, _)| *allowed == name) {
                offenders.push(format!("{}:{} — {}", path.display(), number + 1, trimmed));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a process-wide cache outlives the session that owns it (docs/memory-audit.md §3.2); either own it in a \
         session or add it to ALLOWED with a reason: {offenders:#?}"
    );
}

/// Every `.rs` file of the analysis crates, with its text.
fn sources() -> Vec<(std::path::PathBuf, String)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crates directory");
    let mut files = Vec::new();
    for name in ["cpp_code_analysis", "cpp_ls", "cpp_parser"] {
        collect(&crates.join(name).join("src"), &mut files);
    }
    files
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            Some((path, text))
        })
        .collect()
}

fn collect(directory: &Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}
