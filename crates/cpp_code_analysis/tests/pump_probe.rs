//! **Does the pump reach idle?** — the counters behind a `Load workspace` bar that never retires.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_code_analysis --test pump_probe -- --ignored --nocapture
//! ```
//!
//! The bar reports `re-reading N file(s) for the compiler's view` while `pending_work()` is non-zero, and a reader
//! reported exactly that, with **no CPU in use**. That is the signature of a count that is not a promise: something is
//! in `pending_work()` that no slice can ever take. The three counters it is made of are printed here separately —
//! the index queue, the cook backlog and the second pass — because "N left" says nothing about which of them is stuck.
//!
//! A stall is a **failure**, not a warning: it is the one state a session must never be in, because the progress bar
//! it produces looks like work.

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_REAL_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    named.is_dir().then_some(named)
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn the_pump_reaches_idle() {
    let Some(project) = project() else {
        eprintln!("skipped: no project (set CPPLS_REAL_PROJECT)");
        return;
    };

    let mut session = Session::open(
        project.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project),
    );

    // **The whole workspace, the way `Load workspace` starts it.** `project_files()` is the scan the session made when
    // it opened; queueing all of them is what the server does before its pump loop.
    let all = session.project_files().to_vec();
    println!("the project has {} file(s)", all.len());
    session.add_project_files(all);

    let started = std::time::Instant::now();
    let mut idle_rounds = 0usize;
    let mut slices = 0usize;

    for slice in 0..20_000usize {
        let steps = session.advance(64);
        slices += 1;
        let work = session.pending_work();

        if slice % 100 == 0 || work == 0 {
            println!(
                "slice {slice:>5}: {:<5} work (queue {}, cooking {}, re-read {}) in {:?}",
                work,
                session.pending(),
                session.pending_cooking(),
                session.re_read_backlog(),
                started.elapsed()
            );
        }

        if work == 0 {
            break;
        }

        // **Idle with work left is the stall.** `advance` returning nothing means the slice found nothing it could
        // do; a handful of those is a boundary, a hundred is a count that will never be paid.
        if steps.is_empty() {
            idle_rounds += 1;
        } else {
            idle_rounds = 0;
        }
        if idle_rounds >= 100 {
            panic!(
                "the pump is stuck with {work} file(s) of work left and nothing to do: queue {}, cooking {}, \
                 re-read {} (after {slices} slices, {:?})",
                session.pending(),
                session.pending_cooking(),
                session.re_read_backlog(),
                started.elapsed()
            );
        }
    }

    println!(
        "drained {slices} slice(s) in {:?}: queue {}, cooking {}, re-read {}",
        started.elapsed(),
        session.pending(),
        session.pending_cooking(),
        session.re_read_backlog()
    );
    assert_eq!(
        session.pending_work(),
        0,
        "and the session has nothing left to say it is busy with"
    );
}
