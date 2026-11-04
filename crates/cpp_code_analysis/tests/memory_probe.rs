//! **Where the memory is, phase by phase** — the probe behind "500 MB at open, 700 MB after a few files".
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_code_analysis --test memory_probe -- --ignored --nocapture
//! ```
//!
//! The session reports what **its own tables** hold ([`Session::memory_report`]). That estimate is deliberately *under*
//! the resident set — it counts no `malloc` overhead, no spare capacity, no allocator slack, and it does not know about
//! the process-wide tables or the parse trees a request builds and drops. So the resident set is sampled *outside* the
//! process: each phase prints a marker and then **parks for a few seconds**, and the driver polls
//! `Get-Process -Name memory_probe*` while it does.
//!
//! ```powershell
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! $job = Start-Job { Set-Location E:\cpp_ls; cargo test --release -p cpp_code_analysis --test memory_probe -- --ignored --nocapture }
//! while ($job.State -eq 'Running') { Get-Process memory_probe* -ErrorAction SilentlyContinue |
//!     Select-Object @{n='MB';e={[math]::Round($_.WorkingSet64/1MB)}}; Start-Sleep -Milliseconds 200 }
//! ```
//!
//! The phases are the shape of a real session: open, index the project, then open and *use* a few files.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_REAL_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    named.is_dir().then_some(named)
}

/// Park long enough for the driver's poll to see this phase's resident set.
fn park(phase: &str, session: &Session<DiskFiles>, files_used: usize) {
    let report = session.memory_report();
    let sizes = session.cache_sizes();
    println!("\n===== PHASE: {phase} ({files_used} file(s) used)");
    println!(
        "accounted: {:.1} MB total, {:.1} MB with the process tables",
        report.total() as f64 / (1024.0 * 1024.0),
        report.total_with_the_process() as f64 / (1024.0 * 1024.0)
    );
    println!("{report}");
    // **Timelines: asked for, held, re-walked.** `from_memory` is the sharing cache and the unit table answering;
    // `walked` is a closure read and a timeline built. The ratio is what a smaller budget spends.
    let units = session.unit_stats();
    println!(
        "units: {} from memory, {} walked, {} from disk",
        units.from_memory, units.walked, units.from_disk
    );
    println!(
        "counts: {} rendering(s) {} KB | {} unit(s) | {} summar(ies) | {} fact(s) | {} macro(s) | {} cooked file(s)",
        sizes.renderings,
        sizes.rendering_bytes / 1024,
        sizes.units,
        sizes.summaries,
        sizes.facts,
        sizes.macros,
        sizes.cooked_files,
    );
    std::thread::sleep(Duration::from_secs(4));
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn where_the_memory_goes() {
    let Some(root) = project() else {
        eprintln!("skipped: no project (set CPPLS_REAL_PROJECT)");
        return;
    };

    // **Phase 1: open.** The scan, the configuration, the compiler's search paths — no file read yet.
    let opened = Instant::now();
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    println!(
        "Session::open in {:?}, {} project file(s)",
        opened.elapsed(),
        session.project_files().len()
    );
    park("open", &session, 0);

    // **Phase 2: the index**, the way the pump does it — waves, drains, cooking, until idle.
    let all = session.project_files().to_vec();
    session.add_project_files(all.clone());
    let started = Instant::now();
    let mut slices = 0usize;
    while session.pending() > 0 && slices < 4000 {
        if session.advance(64).is_empty() {
            break;
        }
        slices += 1;
    }
    // **The index only** — a start does not cook the project, it cooks what a reader opens. Cooking all 1590 files
    // here would measure a state no editor reaches.`n
    println!("indexed in {:?} ({slices} slices)", started.elapsed());
    park("indexed", &session, 0);

    // **Phase 3: a few files, used.** What a reader does: open, let the pump cook, ask for diagnostics — the pull that
    // the server answers before the client asks.
    let sources: Vec<PathBuf> = all
        .iter()
        .filter(|path| path.to_string_lossy().ends_with(".cpp"))
        .take(5)
        .cloned()
        .collect();
    for path in &sources {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        session.did_open(path.clone(), &text);
        session.catch_up(path);
        let mut rounds = 0;
        while session.pending() > 0 && rounds < 200 {
            if session.advance(64).is_empty() {
                break;
            }
            rounds += 1;
        }
        let _ = session.cook(path);
        let _ = session.view(path);
        // **The pull, which is where the index caches are exercised** — the member lists, the guard verdicts, the
        // name queries. `CPPLS_PROBE_NO_DIAGNOSTICS=1` leaves it out so the two runs differ in exactly one thing.
        if std::env::var_os("CPPLS_PROBE_NO_DIAGNOSTICS").is_none() {
            let _ = session.diagnostics(path);
        }
    }
    park("five files", &session, sources.len());

    // **Phase 4: twenty**, which is where a session that keeps per-file readings starts to show it.
    let more: Vec<PathBuf> = all
        .iter()
        .filter(|path| path.to_string_lossy().ends_with(".cpp"))
        .skip(5)
        .take(15)
        .cloned()
        .collect();
    for path in &more {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        session.did_open(path.clone(), &text);
        session.catch_up(path);
        let mut rounds = 0;
        while session.pending() > 0 && rounds < 200 {
            if session.advance(64).is_empty() {
                break;
            }
            rounds += 1;
        }
        let _ = session.cook(path);
        let _ = session.view(path);
        // **The pull, which is where the index caches are exercised** — the member lists, the guard verdicts, the
        // name queries. `CPPLS_PROBE_NO_DIAGNOSTICS=1` leaves it out so the two runs differ in exactly one thing.
        if std::env::var_os("CPPLS_PROBE_NO_DIAGNOSTICS").is_none() {
            let _ = session.diagnostics(path);
        }
    }
    // **The same files again**, which is what the unit table is for: a reader who comes back to a file it already
    // cooked should not pay the walk twice. This is the measurement the budget is chosen from — a budget that keeps
    // every closure and a budget that keeps two are the same number until something re-asks.
    for path in sources.iter().chain(more.iter()) {
        let _ = session.cook(path);
        let _ = session.diagnostics(path);
    }
    println!("re-cooked {} file(s)", sources.len() + more.len());
    park(
        "twenty files, cooked twice",
        &session,
        sources.len() + more.len(),
    );
}
