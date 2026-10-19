//! **Where a cold start goes, split the way the server splits it** — read, insert, drain, cook.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example startup_probe -- <workspace root> <a file in it>
//! ```
//!
//! `checks_probe --index-only` answers *how long*; this answers *what for*. The pump in
//! `crates/cpp_ls/src/handlers/initialized/mod.rs` is four calls and this probe makes the same four, timing each from
//! the outside so that nothing has to go back into the library:
//!
//! ```text
//! prepare_a_wave    read the text, read or build the summary, and the unit walk   (&self: no lock needed)
//! commit_a_wave     put the summaries in the index and queue what they include    (&mut self)
//! prepare/commit_the_drain   the files whose reading may have changed now that their includes are in the index
//! claim/materials/render/commit   one cooking slice: a rendering and a parse of it
//! ```
//!
//! # Why the split is the whole point
//!
//! The four have four different fixes, and the coarse number hides which one is being paid: a summary served from the
//! disk cache and one parsed from source are the same "indexing" in a wall clock, and a **unit walk** is neither — it
//! is the translation-unit timeline, which walks a file's whole include closure and is the reason "index 1122 files"
//! once read as 79 % of a load. This probe prints them apart, per wave, so that the next reader starts from the split
//! rather than from the total.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// The pump's wave, and the same first value it starts from — see `THE_FIRST_WAVE` in the server's pump.
const FIRST_WAVE: usize = 8;
/// How many files one cooking slice takes — `COOK_SLICE` in the server.
const COOK: usize = 4;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(root), Some(seed)) = (args.first(), args.get(1)) else {
        eprintln!("usage: startup_probe <workspace root> <a real file in it> [--limit N]");
        std::process::exit(2);
    };
    let root = PathBuf::from(root);
    let seed = PathBuf::from(seed);
    let limit: usize = args
        .iter()
        .position(|argument| argument == "--limit")
        .and_then(|at| args.get(at + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX);

    let mut files: Vec<PathBuf> = Vec::new();
    walk(&root, &mut files, 0);
    files.retain(|path| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "cpp" | "cc" | "cxx" | "c++" | "h" | "hpp" | "hxx" | "hh" | "c" | "inl" | "ipp"
                )
            })
    });
    files.sort();
    files.dedup();
    files.truncate(limit);
    println!("{} file(s) under {}", files.len(), root.display());

    let opened = Instant::now();
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let after_open = opened.elapsed();
    println!("open          {:>9.1} ms", millis(after_open));

    // **The open first, then the project's files** — `checks_probe` documents why: opening a file is what puts it,
    // and what its `#include`s reach, into the index at all.
    if let Ok(text) = std::fs::read_to_string(&seed) {
        session.did_open(&seed, &text);
    }
    let queued = Instant::now();
    session.add_project_files(files.clone());
    println!(
        "add_project   {:>9.1} ms   ({} queued)",
        millis(queued.elapsed()),
        session.pending()
    );

    let mut prepare = Duration::ZERO;
    let mut commit = Duration::ZERO;
    let mut drain_prepare = Duration::ZERO;
    let mut drain_commit = Duration::ZERO;
    let mut materials_time = Duration::ZERO;
    let mut render = Duration::ZERO;
    let mut commit_cooked = Duration::ZERO;

    let mut waves = 0usize;
    let mut prepared_files = 0usize;
    let mut cooked_files = 0usize;
    let mut drain_files = 0usize;
    let mut busy_drains = 0usize;
    let mut busy_drain_prepare = Duration::ZERO;
    let mut reused = 0usize;
    let mut built = 0usize;
    let mut missing = 0usize;
    let wave = FIRST_WAVE;
    let mut last_report = Instant::now();
    let started = Instant::now();

    loop {
        let step = Instant::now();
        let prepared = session.prepare_a_wave(wave);
        let prepared_count = prepared.prepared.len();
        prepare += step.elapsed();
        prepared_files += prepared_count;

        let step = Instant::now();
        let (steps, drain) = session.commit_a_wave(prepared, wave);
        commit += step.elapsed();
        for done in &steps {
            match done.outcome {
                cpp_code_analysis::StepOutcome::Reused => reused += 1,
                cpp_code_analysis::StepOutcome::Built
                | cpp_code_analysis::StepOutcome::Unstored => built += 1,
                cpp_code_analysis::StepOutcome::Missing => missing += 1,
            }
        }

        let step = Instant::now();
        let busy = !drain.is_empty();
        let drain = session.prepare_the_drain(drain);
        let spent = step.elapsed();
        drain_prepare += spent;
        if busy {
            busy_drains += 1;
            busy_drain_prepare += spent;
        }

        let step = Instant::now();
        drain_files += session.commit_the_drain(drain);
        drain_commit += step.elapsed();

        // **One cooking slice**, the same four calls the pump makes.
        let claimed = session.claim_cooking(COOK);
        let claimed_count = claimed.len();
        let step = Instant::now();
        let materials = session.materials_for(claimed);
        materials_time += step.elapsed();

        let step = Instant::now();
        let rendered = session.render_them(materials);
        render += step.elapsed();

        let step = Instant::now();
        cooked_files += session.commit_them(rendered).len();
        commit_cooked += step.elapsed();

        waves += 1;

        if prepared_count == 0 && claimed_count == 0 && session.pending_work() == 0 {
            break;
        }
        if last_report.elapsed() > Duration::from_secs(10) {
            println!(
                "  … {:>6.1} s  waves {waves:>5}  prepared {prepared_files:>5}  pending {:>5}  cooking {:>4}  \
                 index {}",
                started.elapsed().as_secs_f64(),
                session.pending(),
                session.pending_cooking(),
                session.index().len()
            );
            last_report = Instant::now();
        }
        if waves > 1_000_000 {
            break;
        }
    }

    let total = started.elapsed();
    println!();
    println!(
        "indexed {prepared_files} file(s) in {:.1} s over {waves} wave(s); index holds {}",
        total.as_secs_f64(),
        session.index().len()
    );
    println!(
        "  summaries        {reused:>6} reused from disk, {built} built, {missing} missing"
    );
    let units = session.unit_stats();
    println!(
        "  timelines        {:>6} walked, {} from disk, {} from memory",
        units.walked, units.from_disk, units.from_memory
    );
    println!(
        "  prepare_a_wave   {:>8.1} s  ({:.1}%)   read + summary + unit walk",
        prepare.as_secs_f64(),
        percent(prepare, total)
    );
    println!(
        "  commit_a_wave    {:>8.1} s  ({:.1}%)   insert + queue",
        commit.as_secs_f64(),
        percent(commit, total)
    );
    println!(
        "  drain prepare    {:>8.1} s  ({:.1}%)   {busy_drains} non-empty drain(s), {:.1} s of them",
        drain_prepare.as_secs_f64(),
        percent(drain_prepare, total),
        busy_drain_prepare.as_secs_f64()
    );
    println!("  re-read files    {drain_files:>8} committed");
    println!(
        "  drain commit     {:>8.1} s  ({:.1}%)",
        drain_commit.as_secs_f64(),
        percent(drain_commit, total)
    );
    println!(
        "  cook materials   {:>8.1} s  ({:.1}%)",
        materials_time.as_secs_f64(),
        percent(materials_time, total)
    );
    println!(
        "  cook render      {:>8.1} s  ({:.1}%)   {cooked_files} file(s)",
        render.as_secs_f64(),
        percent(render, total)
    );
    println!(
        "  cook commit      {:>8.1} s  ({:.1}%)",
        commit_cooked.as_secs_f64(),
        percent(commit_cooked, total)
    );
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn percent(part: Duration, whole: Duration) -> f64 {
    if whole.is_zero() {
        0.0
    } else {
        part.as_secs_f64() / whole.as_secs_f64() * 100.0
    }
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>, depth: usize) {
    if depth > 16 || files.len() > 20_000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "build" || name == "Build" || name == "out" {
                continue;
            }
            walk(&path, files, depth + 1);
        } else if kind.is_file() {
            files.push(path);
        }
    }
}
