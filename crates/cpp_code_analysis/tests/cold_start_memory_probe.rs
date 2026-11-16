//! **Where a cold start's memory goes, in live bytes** — the measurement that names the spike instead of inferring it.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "C:\Users\zc\Desktop\test_cpp"
//! cargo test --release -p cpp_code_analysis --test cold_start_memory_probe -- --ignored --nocapture
//! ```
//!
//! # The claim this exists to check
//!
//! "The server uses four and a half gigabytes" is the report every memory change in this repository was written
//! against, and none of them measured it: the counter they were read from **only counted allocations**, so a stage
//! that allocated a gigabyte and freed it read exactly like one that kept it. See
//! [`cpp_code_analysis::alloc`]. This probe installs an allocator that counts both sides, drives the session the way
//! the pump does — a cold index, then one program read after another — and prints what the process is **holding** at
//! each step.
//!
//! # What it prints
//!
//! ```text
//! slice   live / peak / churn, and the session's own tables, once per `advance`
//! read    live / peak before and after each whole-program read
//! final   the report `Session::memory_report` keeps, which is what the tables cost
//! ```
//!
//! The difference between `live` and the tables is the transient working set: a live number that climbs while the
//! tables stay flat is a stage holding something it should have freed, and that is the question this asks.
//!
//! # It stops itself
//!
//! A live number over [`THE_CEILING`] ends the process with a line saying so. A probe that reproduces a five-gigabyte
//! spike must not take the machine down to report it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// Live bytes, as the process holds them.
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// The high-water mark of [`LIVE`].
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// Every byte ever asked for — churn, and the number the old counter reported.
static TOTAL: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TOTAL.fetch_add(layout.size(), Ordering::Relaxed);
        let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        TOTAL.fetch_add(layout.size(), Ordering::Relaxed);
        let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        TOTAL.fetch_add(new_size, Ordering::Relaxed);
        if new_size >= layout.size() {
            let grown = new_size - layout.size();
            let now = LIVE.fetch_add(grown, Ordering::Relaxed) + grown;
            PEAK.fetch_max(now, Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
        }
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// **Where the probe gives up.** Not a product decision: a measurement that can crash the machine is not a
/// measurement, and the report is worth as much at one and a half gigabytes as at five.
const THE_CEILING: usize = 1536 * 1024 * 1024;

fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

fn mb(bytes: usize) -> usize {
    bytes / (1024 * 1024)
}

fn guard(where_: &str) {
    if live() > THE_CEILING {
        println!("STOPPED at {where_}: holding {} MB", mb(live()));
        std::process::exit(3);
    }
}

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\test_cpp".to_string()),
    )
}

fn the_sources(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("src"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "cpp"))
        .collect();
    files.sort();
    files
}

#[test]
#[ignore = "a probe: opens a real project and reports instead of asserting"]
fn where_a_cold_start_puts_its_memory() {
    let root = project();
    let sources = the_sources(&root);
    println!(
        "project    {} ({} source(s))",
        root.display(),
        sources.len()
    );

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(sources.clone());

    println!("\n# the cold index");
    let started = Instant::now();
    session.index_everything();
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 1000 {
        session.advance(64);
        slices += 1;
        guard("a cold slice");
        let report = session.memory_report();
        println!(
            "slice {slices:3}  live {:5} MB  peak {:5} MB  churn {:6} MB  | tables {:5} MB (summaries {} / cooked {} / \
             units {} / renderings {})",
            mb(live()),
            mb(peak()),
            mb(TOTAL.load(Ordering::Relaxed)),
            mb(report.summary_bytes + report.cooked_bytes + report.view_bytes + report.unit_bytes),
            report.summaries,
            report.cooked_files,
            report.units,
            report.renderings,
        );
    }
    println!(
        "settled after {slices} slice(s) in {:?}; live {} MB, peak {} MB",
        started.elapsed(),
        mb(live()),
        mb(peak())
    );

    println!("\n# the pump, with the reader's files open");
    for path in &sources {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        session.did_open(path, &text);
    }
    let started = Instant::now();
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 1000 {
        session.advance(64);
        slices += 1;
        guard("an open-file slice");
        let report = session.memory_report();
        println!(
            "slice {slices:3}  live {:5} MB  peak {:5} MB  churn {:6} MB  | tables {:5} MB (summaries {} / cooked {} / \
             units {} / renderings {})",
            mb(live()),
            mb(peak()),
            mb(TOTAL.load(Ordering::Relaxed)),
            mb(report.summary_bytes + report.cooked_bytes + report.view_bytes + report.unit_bytes),
            report.summaries,
            report.cooked_files,
            report.units,
            report.renderings,
        );
    }
    println!(
        "settled after {slices} slice(s) in {:?}; live {} MB, peak {} MB, units {:?}, pending {}",
        started.elapsed(),
        mb(live()),
        mb(peak()),
        session.unit_stats(),
        session.pending_work()
    );

    println!("\n# one whole-program read at a time, the way the pump takes them");
    let mut reads = 0usize;
    for path in &sources {
        let before = live();
        let started = Instant::now();
        let read = session.read_the_unit(path);
        reads += 1;
        guard("a program read");
        let report = session.memory_report();
        println!(
            "read {reads:3}  {:5} MB -> {:5} MB in {:>7.1} ms  peak {:5} MB  | tables {:5} MB  | {} | {}",
            mb(before),
            mb(live()),
            started.elapsed().as_secs_f64() * 1000.0,
            mb(peak()),
            mb(report.summary_bytes + report.cooked_bytes + report.view_bytes + report.unit_bytes),
            path.file_name().unwrap_or_default().to_string_lossy(),
            read.map_or_else(
                || "declined".to_string(),
                |reading| format!(
                    "{} file(s), {} token(s), {} error(s)",
                    reading.files, reading.tokens, reading.errors
                )
            ),
        );
    }

    let report = session.memory_report();
    println!("\n# final");
    println!(
        "live {} MB, peak {} MB, churn {} MB",
        mb(live()),
        mb(peak()),
        mb(TOTAL.load(Ordering::Relaxed))
    );
    println!("{report}");
}
