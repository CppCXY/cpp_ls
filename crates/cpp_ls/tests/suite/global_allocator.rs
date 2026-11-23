//! **The global allocator is a library and nothing around it**, checked by a test rather than by review.
//!
//! `crates/cpp_ls/src/bin/cpp_ls.rs` used to install a `Counting` wrapper around `MiMalloc` so a process-wide byte
//! counter could feed `CPPLS_TRACE_ALLOCS` and the server's memory traces. The instrument is gone — every allocation
//! and every free in every release paid a relaxed atomic load for a measurement nobody was taking — and this is the
//! rule that keeps it out.
//!
//! A global allocator has no runtime API to ask, so the check reads the source: the binary installs `MiMalloc`
//! directly, and no shipped crate implements `GlobalAlloc`. A *test* may install its own — counting allocations has no
//! other way to work — which is why the second check is scoped to `src/`.

use std::path::{Path, PathBuf};

#[test]
fn the_binary_installs_mimalloc_and_nothing_else() {
    let binary =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/cpp_ls.rs"))
            .expect("the binary's source is beside this test");
    assert!(
        binary.contains("#[global_allocator]"),
        "the binary must install the allocator the server ships with"
    );
    assert!(
        binary.contains("static GLOBAL: MiMalloc = MiMalloc;"),
        "the global allocator is `MiMalloc` itself: a wrapper is a per-allocation cost paid by every user, for an \
         instrument that belongs in a profiler. Source: {binary}"
    );
}

#[test]
fn no_shipped_source_wraps_a_global_allocator() {
    let mut offenders = Vec::new();
    for (path, source) in sources() {
        for (number, line) in source.lines().enumerate() {
            if line.contains("impl GlobalAlloc") || line.contains("impl std::alloc::GlobalAlloc") {
                offenders.push(format!("{}:{}", path.display(), number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "an allocator is a library, not a measurement: a per-allocation counter is paid for by every user. Take the \
         measurement with a profiler, or install a counting allocator inside the test that needs it: {offenders:#?}"
    );
}

/// Every `.rs` file under the shipped crates' `src/` directories, with its text.
fn sources() -> Vec<(PathBuf, String)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crates directory");
    let mut files: Vec<PathBuf> = Vec::new();
    let Ok(entries) = std::fs::read_dir(crates) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let directory = entry.path();
        if directory.is_dir() {
            collect(&directory.join("src"), &mut files);
        }
    }
    files
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            Some((path, text))
        })
        .collect()
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
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
