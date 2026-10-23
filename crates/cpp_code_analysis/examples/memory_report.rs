//! **What the analysis holds, in bytes, from "open" to a thousand keystrokes.**
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example memory_report -- <root> [<file>] [<keystrokes>]
//! ```
//!
//! The report this probe exists for is "the server sat at five gigabytes on `EmmyLuaCodeStyle`". A number like that
//! has three possible shapes, and they have three different fixes:
//!
//! ```text
//! · the index is simply large        → the summaries belong on disk (redb) with a bounded working set
//! · it grows per keystroke           → a cache whose key includes the content hash and whose eviction is missing
//! · it does not come back on drop    → a global (a `static`, a thread-local, a process-wide table)
//! ```
//!
//! So the probe prints the resident set at every phase boundary, and again after the session is dropped. The resident
//! set is read from the kernel (`K32GetProcessMemoryInfo`), not estimated from the data structures: an estimate is
//! what the last report already had, and it was off by an order of magnitude.

use std::path::{Path, PathBuf};

/// **The allocator the server uses**, so that "is this the live tables or pages the allocator holds" is measured on
/// the same allocator the answer has to be true for.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
use std::time::Instant;

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("a project root"));
    let file = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("main.cpp"));
    let keys: usize = std::env::args()
        .nth(3)
        .and_then(|count| count.parse().ok())
        .unwrap_or(200);

    let on_disk = std::fs::read_to_string(&file).expect("the file to edit");
    let documents = OpenDocuments::new();
    let files = SessionFiles::new(documents.clone(), DiskFiles);

    let started = Instant::now();
    let mut session = match std::env::var_os("CPPLS_PROJECT_CONFIG") {
        Some(config) => Session::open_with_config_file(
            root.clone(),
            files.clone(),
            WatchFilter::new(&root),
            Some(Path::new(&config)),
        ),
        None => Session::open(root.clone(), files.clone(), WatchFilter::new(&root)),
    };
    checkpoint(
        &session,
        &format!("opened in {:?}", started.elapsed()),
    );

    session.did_open(&file, &on_disk);
    let started = Instant::now();
    let indexed = session.index_everything();
    checkpoint(
        &session,
        &format!("indexed {indexed} file(s) in {:?}", started.elapsed()),
    );

    let started = Instant::now();
    let mut slices = 0usize;
    while session.re_read_backlog() > 0 && slices < 20_000 {
        session.advance(64);
        slices += 1;
    }
    checkpoint(
        &session,
        &format!(
            "tail drained: {slices} slice(s) in {:?}, {} owed",
            started.elapsed(),
            session.re_read_backlog()
        ),
    );

    // The cursor: inside a function body, which is where a reader types.
    let text = session.text(&file).unwrap_or(on_disk.clone());
    let cursor = text.find('{').map(|index| index + 1).unwrap_or(text.len());

    // **Is the gap between the live tables and the resident set *free* memory?** Trim the working set and touch every
    // live summary: if the trim takes the process to ~the accounted total and touching does not bring it back, the
    // pages were free and the answer is "return them" (M5); if touching brings it back, the memory is live and
    // unaccounted, and the answer is elsewhere.
    if std::env::var_os("CPPLS_TRIM").is_some() {
        let before = resident();
        trim_working_set();
        let trimmed = resident();
        let live: usize = session
            .index()
            .summaries()
            .map(|summary| summary.declarations.len() + summary.macros.len())
            .sum();
        let touched = resident();
        println!(
            "  trim: {} -> {} (touched {live} facts: {}), accounted {}",
            kilobytes(before),
            kilobytes(trimmed),
            kilobytes(touched),
            kilobytes(session.memory_report().total_with_the_process())
        );
    }

    // **Does the allocator give the pages back?** The trim above shows the pages are free; this asks whether the
    // allocator can release them without paging out live data (`mi_collect(true)` is mimalloc's "release what you
    // can"), which is the call `cpp_ls` would make when the queue drains.
    {
        let before = resident();
        // Declared here rather than through `libmimalloc-sys`, whose bindings for it sit behind a feature: the
        // symbol is in the mimalloc library this probe already links as its allocator.
        unsafe extern "C" {
            fn mi_collect(force: bool);
        }
        // SAFETY: `mi_collect` takes a flag and no pointers, and is safe to call at any time.
        unsafe { mi_collect(true) };
        println!(
            "  mi_collect: {} -> {}",
            kilobytes(before),
            kilobytes(resident())
        );
    }

    let mut buffer = text;
    let mut baseline: Option<usize> = None;
    for key in 0..keys {
        let at = cursor.min(buffer.len());
        buffer.insert(at, if key % 2 == 0 { ' ' } else { 'x' });

        session.did_change(&file, &buffer);
        session.advance(64);
        // The diagnostics pull, which a client asks for after every keystroke.
        let _ = session.diagnostics(&file);
        if let Some(view) = session.view(&file) {
            let _ = session.completions(&view, FileOffset::new(at.min(view.source.len())));
        }

        if key == 24 {
            baseline = Some(resident());
            checkpoint(&session, "after 25 keystrokes");
        }
        if (key + 1) % 100 == 0 {
            checkpoint(&session, &format!("after {} keystrokes", key + 1));
        }
    }

    // **Editing one file after another**, twice: every edit rebuilds that file's translation unit and re-cooks it,
    // and the unit table keeps the last `MAX_UNITS` of them. The second round visits the *same* files, so it tells
    // the two shapes apart: a table keyed by path growing per edit grows again, while an allocator holding on to the
    // pages of a transient multi-megabyte rendering does not.
    let mut visited = 0usize;
    let others: Vec<PathBuf> = session.project_files().to_vec();
    let candidates: Vec<PathBuf> = others
        .iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "cpp"))
        .take(24)
        .cloned()
        .collect();
    for round in 0..2 {
        for other in &candidates {
            if *other == file {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(other) else {
                continue;
            };
            session.did_open(other, &text);
            let mut buffer = text;
            let at = buffer
                .find('{')
                .map(|index| index + 1)
                .unwrap_or(buffer.len());
            buffer.insert(at, ' ');
            session.did_change(other, &buffer);
            let mut slices = 0usize;
            while session.pending_work() > 0 && slices < 400 {
                session.advance(64);
                slices += 1;
            }
            let _ = session.diagnostics(other);
            if let Some(view) = session.view(other) {
                let _ = session.completions(&view, FileOffset::new(at.min(view.source.len())));
            }
            visited += 1;
            if visited % 4 == 0 {
                checkpoint(
                    &session,
                    &format!("round {round}, edited {visited} file(s) in total"),
                );
            }
        }
        checkpoint(&session, &format!("round {round} finished"));
    }

    if let (Some(baseline), now) = (baseline, resident()) {
        println!(
            "  growth over {} keystrokes: {} ({} bytes per keystroke)",
            keys.saturating_sub(25),
            kilobytes(now.saturating_sub(baseline)),
            now.saturating_sub(baseline) / keys.saturating_sub(25).max(1)
        );
    }

    let before_drop = resident();
    drop(session);
    let after_drop = resident();
    println!(
        "  dropped the session: {} -> {} ({} came back)",
        kilobytes(before_drop),
        kilobytes(after_drop),
        kilobytes(before_drop.saturating_sub(after_drop))
    );

    // **A second session over the same project** — after the first one is gone. Whatever is still held from the
    // first run is a process-wide table, and the difference is its size.
    let documents = OpenDocuments::new();
    let files = SessionFiles::new(documents.clone(), DiskFiles);
    let second = Session::open(root.clone(), files, WatchFilter::new(&root));
    checkpoint(&second, "a second session opened after the first was dropped");
    drop(second);
    println!(
        "  after dropping the second: {}",
        kilobytes(resident())
    );
}

fn checkpoint(session: &Session<DiskFiles>, what: &str) {
    let sizes = session.cache_sizes();
    let report = session.memory_report();
    println!(
        "  [{:>52}]  rss {:>8} (peak {:>8}) | {}",
        what,
        kilobytes(resident()),
        kilobytes(peak_resident()),
        report,
    );
    let _ = (&sizes, report.total());
}

/// The **peak** resident set this process has ever had — the number that says whether a moment's memory was transient
/// (a wave of parses) or became the steady state.
#[cfg(windows)]
fn peak_resident() -> usize {
    use std::mem::MaybeUninit;

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCounters,
            size: u32,
        ) -> i32;
    }

    let mut counters = MaybeUninit::<ProcessMemoryCounters>::zeroed();
    let size = std::mem::size_of::<ProcessMemoryCounters>() as u32;
    // SAFETY: the struct is `#[repr(C)]` and `size` is its own size, which is the contract of the call.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), counters.as_mut_ptr(), size) };
    if ok == 0 {
        return 0;
    }
    // SAFETY: the call succeeded, so the struct was filled in.
    unsafe { counters.assume_init().peak_working_set_size }
}

#[cfg(not(windows))]
fn peak_resident() -> usize {
    0
}

/// Page the working set out and back? See the trim experiment in `main`.
#[cfg(windows)]
fn trim_working_set() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn SetProcessWorkingSetSize(process: isize, minimum: usize, maximum: usize) -> i32;
    }
    // SAFETY: the arguments are documented: `-1` for both sizes means "trim the working set".
    unsafe {
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(not(windows))]
fn trim_working_set() {}

fn kilobytes(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// The resident set size of this process, in bytes.
#[cfg(windows)]
fn resident() -> usize {
    use std::mem::MaybeUninit;

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCounters,
            size: u32,
        ) -> i32;
    }

    let mut counters = MaybeUninit::<ProcessMemoryCounters>::zeroed();
    let size = std::mem::size_of::<ProcessMemoryCounters>() as u32;
    // SAFETY: the struct is `#[repr(C)]` and `size` is its own size, which is the contract of the call.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), counters.as_mut_ptr(), size) };
    if ok == 0 {
        return 0;
    }
    // SAFETY: the call succeeded, so the struct was filled in.
    unsafe { counters.assume_init().working_set_size }
}

#[cfg(not(windows))]
fn resident() -> usize {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|text| {
            text.split_whitespace()
                .nth(1)
                .and_then(|pages| pages.parse::<usize>().ok())
        })
        .map(|pages| pages * 4096)
        .unwrap_or(0)
}
