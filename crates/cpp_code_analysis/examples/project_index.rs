//! **Cold and warm indexing of a real project, through the session** — the only path a project's `.cppls.toml`
//! is read on.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example project_index -- <dir> [<entry.cpp>]
//! ```
//!
//! # Why this exists
//!
//! `examples/workspace_probe.rs` indexes a real project and then spends minutes asking every question about every
//! offset in every file, which is what it is for and not what a warmth measurement is for. `examples/std_closure.rs`
//! is cheap and is **the wrong instrument twice over**: it builds a `SummaryStore` directly, so it never reads the
//! project's configuration, and it deletes its own cache directory at the start of every run, so it cannot be warm
//! and prints a cold number under a warm heading.
//!
//! This does one thing: open the session, drain the work queue, print what the cache did. Run it twice in a row and
//! the difference is the warm start.
//!
//! # What to read
//!
//! ```text
//!   reused    files the disk answered for — the number a warm start is made of
//!   rebuilt   files that had to be parsed
//!   unstored  files read and deliberately not written, because one of their `#include`s did not resolve
//!             (`has_unresolved_includes`) — so they are read again every session
//! ```
//!
//! `unstored` is the one that hides: a large number means the cache is working and the *resolution* is not, and the
//! second run costs the same as the first for every one of them.

use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: project_index <dir> [<entry.cpp>]");
        std::process::exit(2);
    };
    let home = PathBuf::from(&dir);
    let entry = std::env::args().nth(2).map(PathBuf::from);

    let started = Instant::now();
    let mut session = Session::open(
        home.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&home),
    );
    let opened = started.elapsed();

    // **The file a request is about**, so that the run includes the closure expansion a real session does rather
    // than only the project scan.
    if let Some(entry) = &entry {
        let text = std::fs::read_to_string(entry).unwrap_or_default();
        session.did_open(entry, &text);
    }

    let started = Instant::now();
    loop {
        let steps = session.advance(64);
        if steps.is_empty() && session.pending() == 0 {
            break;
        }
    }
    let indexed = started.elapsed();

    println!(
        "opened {opened:?} | indexed {} files in {indexed:?} | pending {} | stats {:?}",
        session.index().len(),
        session.pending(),
        session.stats()
    );
    // **Where the time went**, because "the cache is being used and it is still slow" is a question about which
    // stage is paying — and this probe exists to answer the warmth question, not to leave a second one open.
    println!("{}", cpp_code_analysis::stages::StageTimes::read().report());

    // **And who kept asking for a timeline.** `walked` in the stats above is a total: two hundred walks is either
    // one root walked two hundred times or two hundred roots walked once, and those want opposite fixes.
    let walked = session.walks_by_root();
    let total: u64 = walked.iter().map(|(_, count)| count).sum();
    println!("\ntimelines walked: {} root(s), {total} walk(s)", walked.len());
    for (path, count) in walked.iter().take(12) {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        println!("  {count:>5}  {name}");
    }
}
