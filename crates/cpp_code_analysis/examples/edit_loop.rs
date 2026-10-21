//! **What one keystroke costs on a real project, and which phase pays for it.**
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example edit_loop -- <root> [<file>] [<keystrokes>]
//! ```
//!
//! The server's edit path is not one call, and its cost is not one number. A keystroke is
//!
//! ```text
//! did_change      the buffer is replaced, the summary dropped, the file queued
//! pump slice      prepare_a_wave (parse, under the read lock)
//!                 commit_a_wave  (insert + queue includes, under the write lock)
//!                 materials_for / render_them      (a cook's expensive half, under the read lock)
//!                 prepare_the_drain                (the second pass, under the read lock)
//!                 commit_the_drain / commit_them   (two inserts, under the write lock)
//! request         view + a completion, which is what a user actually waits for
//! ```
//!
//! This probe runs that loop against a real project, one character at a time, and prints each phase's
//! cost per keystroke. The point is the split: "editing is slow" is a report, and "the second pass is
//! 140 ms of a 160 ms keystroke" is a thing to fix.
//!
//! # What is measured, and why a *character*
//!
//! The character is inserted into a function body, which is the edit a reader makes most: it changes
//! no directive, so the file's translation unit survives, and the whole of the cost is the file being
//! re-read and re-cooked. An edit that moves a `#define` is the other case (the environment and every
//! dependent's reading are dropped) and `--directive` asks for that one.

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Every phase of the server's pump slice, with the cost of each.
#[derive(Default, Clone, Copy)]
struct Slice {
    prepared_files: usize,
    committed: usize,
    claimed: usize,
    built: usize,
    reused: usize,
    with_the_environment: usize,
    prepared: Duration,
    commit: Duration,
    materials: Duration,
    render: Duration,
    drain: Duration,
    commit_drain: Duration,
    commit_cooked: Duration,
    waves: usize,
}

impl Slice {
    fn add(&mut self, other: &Slice) {
        self.prepared_files += other.prepared_files;
        self.committed += other.committed;
        self.claimed += other.claimed;
        self.built += other.built;
        self.reused += other.reused;
        self.with_the_environment += other.with_the_environment;
        self.prepared += other.prepared;
        self.commit += other.commit;
        self.materials += other.materials;
        self.render += other.render;
        self.drain += other.drain;
        self.commit_drain += other.commit_drain;
        self.commit_cooked += other.commit_cooked;
        self.waves += other.waves;
    }

    fn total(&self) -> Duration {
        self.prepared
            + self.commit
            + self.materials
            + self.render
            + self.drain
            + self.commit_drain
            + self.commit_cooked
    }
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("a project root"));
    let file = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("main.cpp"));
    let keys: usize = std::env::args()
        .nth(3)
        .and_then(|count| count.parse().ok())
        .unwrap_or(10);

    let on_disk = std::fs::read_to_string(&file).expect("the file to edit");
    let documents = OpenDocuments::new();
    let files = SessionFiles::new(documents.clone(), DiskFiles);

    let started = Instant::now();
    let mut session = Session::open(root.clone(), files.clone(), WatchFilter::new(&root));
    let opened = started.elapsed();
    println!("project: {}", root.display());
    println!(
        "  opened in {opened:?} — {} project files, {} system include directories",
        session.project_files().len(),
        session
            .toolchain()
            .map(|toolchain| toolchain.system_include_paths.len())
            .unwrap_or(0)
    );

    // The file is opened and the project indexed, so that every later keystroke is measured against a
    // session in the state the server reaches after its first pass.
    session.did_open(&file, &on_disk);
    let started = Instant::now();
    let read = session.index_everything();
    let indexed = started.elapsed();
    let stats = session.stats();
    let units = session.unit_stats();
    println!(
        "  indexed {read} files in {indexed:?} — {} parsed, {} from disk, {} not stored | units: \
         {} memory, {} disk, {} walked",
        stats.rebuilt, stats.reused, stats.unstored, units.from_memory, units.from_disk, units.walked
    );
    println!(
        "  index: {} files, {} declarations | cooking backlog {} | cache {:?}",
        session.index().len(),
        session
            .index()
            .summaries()
            .map(|summary| summary.declarations.len())
            .sum::<usize>(),
        session.pending_cooking(),
        session.cache_sizes()
    );
    println!(
        "  second-pass backlog after `index_everything`: {} file(s)",
        session.re_read_backlog()
    );

    // **The tail of the start-up, drained before the keystrokes are measured.** `index_everything` stops when the
    // index queue is empty and does not ask what the second pass still owes, so the backlog it leaves is charged to
    // whatever runs next — a keystroke. This makes both numbers available: the tail's own cost, and the steady state
    // that editing reaches once it is paid.
    let mut slices = 0usize;
    let started = Instant::now();
    while session.re_read_backlog() > 0 && slices < 20_000 {
        session.advance(64);
        slices += 1;
    }
    println!(
        "  the tail: {slices} slice(s) in {:?}, {} file(s) left",
        started.elapsed(),
        session.re_read_backlog()
    );

    // The cursor: inside a function body, which is where a reader types. A file with no `{` at all
    // gets its end, which is still an edit that moves no directive.
    let text = session.text(&file).unwrap_or(on_disk.clone());
    let cursor = cursor_in_a_body(&text).unwrap_or(text.len());

    let mut slices = Slice::default();
    let mut did_change = Duration::ZERO;
    let mut view_time = Duration::ZERO;
    let mut ask_time = Duration::ZERO;
    let mut diagnose_time = Duration::ZERO;
    let mut rows = Vec::new();

    println!("\nkeystrokes (one character inserted at offset {cursor}):");
    println!(
        "  {:>3}  {:>8}  {:>8}  {:>8}  {:>8}  {:>8}  {:>8}  {:>7}  {:>8}  {:>8}",
        "key", "did_chg", "pump", "  parse", "  cook", " drain", "  write", "steps", " request", "  diag"
    );

    let mut buffer = text.clone();
    for key in 0..keys {
        // **A character in the body**: the inserted byte is the same one a reader types, and it changes
        // no directive, so the file's unit survives and the cost is the file again.
        let at = cursor.min(buffer.len());
        buffer.insert(at, if key % 2 == 0 { ' ' } else { 'x' });

        let started = Instant::now();
        session.did_change(&file, &buffer);
        let change = started.elapsed();

        let slice = pump(&mut session);
        slices.add(&slice);
        did_change += change;

        let started = Instant::now();
        let view = session.view(&file);
        let took_view = started.elapsed();

        let started = Instant::now();
        let answered = view
            .as_ref()
            .map(|view| {
                session
                    .completions(view, FileOffset::new(at.min(view.source.len())))
                    .items
                    .len()
            })
            .unwrap_or(0);
        let took_ask = started.elapsed();
        view_time += took_view;
        ask_time += took_ask;

        // **The diagnostics pull**, which a client asks for after every keystroke — and the one request that runs the
        // check layer, so it is the half of the loop that is not the index.
        let started = Instant::now();
        let found = session.diagnostics(&file).map(|diagnostics| {
            diagnostics.errors.len() + diagnostics.checks.len() + diagnostics.notes.len()
        });
        let took_diagnose = started.elapsed();
        diagnose_time += took_diagnose;

        println!(
            "  {:>3}  {:>7}ms  {:>7}ms  {:>7}ms  {:>7}ms  {:>7}ms  {:>7}ms  {:>7}  {:>6}ms/{:<4}  \
             {:>6}ms/{:<2}  built {} reused {} env {}",
            key + 1,
            change.as_millis(),
            slice.total().as_millis(),
            (slice.prepared + slice.commit).as_millis(),
            (slice.materials + slice.render).as_millis(),
            (slice.drain + slice.commit_drain + slice.commit_cooked).as_millis(),
            slice.write_hold().as_millis(),
            slice.waves,
            took_ask.as_millis(),
            answered,
            took_diagnose.as_millis(),
            found.unwrap_or(0),
            slice.built,
            slice.reused,
            slice.with_the_environment,
        );
        rows.push((change, slice));
    }

    let counted = keys.max(1) as u32;
    println!("\nmean over {keys} keystroke(s):");
    println!(
        "  did_change  {:>7.1} ms",
        did_change.as_secs_f64() * 1000.0 / f64::from(counted)
    );
    println!(
        "  pump        {:>7.1} ms   ({} slices/keystroke)",
        slices.total().as_secs_f64() * 1000.0 / f64::from(counted),
        slices.waves as f64 / f64::from(counted)
    );
    for (name, part) in [
        ("parse (wave prepare)", slices.prepared),
        ("wave commit", slices.commit),
        ("cook materials", slices.materials),
        ("cook render", slices.render),
        ("second pass (drain)", slices.drain),
        ("drain commit", slices.commit_drain),
        ("cook commit", slices.commit_cooked),
    ] {
        println!(
            "    {name:<22} {:>7.1} ms",
            part.as_secs_f64() * 1000.0 / f64::from(counted)
        );
    }
    println!(
        "  write hold  {:>7.1} ms",
        slices.write_hold().as_secs_f64() * 1000.0 / f64::from(counted)
    );
    println!(
        "  request     {:>7.1} ms   (view {:.1} ms + ask {:.1} ms + diagnostics {:.1} ms)",
        (view_time + ask_time + diagnose_time).as_secs_f64() * 1000.0 / f64::from(counted),
        view_time.as_secs_f64() * 1000.0 / f64::from(counted),
        ask_time.as_secs_f64() * 1000.0 / f64::from(counted),
        diagnose_time.as_secs_f64() * 1000.0 / f64::from(counted)
    );
    println!(
        "  work        {:>7.1} files parsed/keystroke, {} committed, {} claimed to cook",
        slices.prepared_files as f64 / f64::from(counted),
        slices.committed as f64 / f64::from(counted),
        slices.claimed as f64 / f64::from(counted)
    );

    // The distribution, because a mean hides the one keystroke that took a second.
    let mut totals: Vec<u128> = rows
        .iter()
        .map(|(change, slice)| (*change + slice.total()).as_millis())
        .collect();
    totals.sort_unstable();
    if !totals.is_empty() {
        println!(
            "  per keystroke total: min {} ms, median {} ms, max {} ms",
            totals[0],
            totals[totals.len() / 2],
            totals[totals.len() - 1]
        );
    }
}

impl Slice {
    /// The part of a slice the write lock was held for.
    fn write_hold(&self) -> Duration {
        self.commit + self.commit_drain + self.commit_cooked
    }
}

/// One pump slice — the server's own `index_in_background` loop body, in order.
fn pump<F: cpp_code_analysis::FileProvider + Clone>(session: &mut Session<F>) -> Slice {
    let mut slice = Slice::default();
    // A keystroke is one file of work; the loop is here because a slice can queue more (an include the
    // edit added), and because the server loops the same way.
    for _ in 0..64 {
        if session.pending_work() == 0 {
            break;
        }
        slice.waves += 1;

        let started = Instant::now();
        let prepared = session.prepare_a_wave(8);
        slice.prepared += started.elapsed();
        slice.prepared_files += prepared.prepared.len();

        let started = Instant::now();
        let (done, drain) = session.commit_a_wave(prepared, 8);
        slice.commit += started.elapsed();
        slice.committed += done.len();
        for step in &done {
            match step.outcome {
                cpp_code_analysis::index::worklist::StepOutcome::Built
                | cpp_code_analysis::index::worklist::StepOutcome::Unstored => slice.built += 1,
                cpp_code_analysis::index::worklist::StepOutcome::Reused => slice.reused += 1,
                cpp_code_analysis::index::worklist::StepOutcome::Missing => {}
            }
            if step.with_the_environment {
                slice.with_the_environment += 1;
            }
        }

        let claimed = session.claim_cooking(4);
        slice.claimed += claimed.len();

        let started = Instant::now();
        let materials = session.materials_for(claimed);
        slice.materials += started.elapsed();
        let started = Instant::now();
        let rendered = session.render_them(materials);
        slice.render += started.elapsed();

        let started = Instant::now();
        let plan = session.prepare_the_drain(drain);
        slice.drain += started.elapsed();

        let started = Instant::now();
        session.commit_the_drain(plan);
        slice.commit_drain += started.elapsed();

        let started = Instant::now();
        session.commit_them(rendered);
        slice.commit_cooked += started.elapsed();
    }
    slice
}

/// **The offset a reader is typing at**: inside the innermost `{ … }` that is not the file's own.
fn cursor_in_a_body(text: &str) -> Option<usize> {
    // The last `{` before a `return` is the body of a function near the end of the file, which is
    // where an edit is cheapest to describe and typical to make.
    let at = text.rfind("return")?;
    let open = text[..at].rfind('{')?;
    Some(open + 1)
}

#[allow(dead_code)]
fn short(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}
