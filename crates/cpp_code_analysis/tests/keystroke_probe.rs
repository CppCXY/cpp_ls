//! **A probe, not a test**: what one keystroke costs a real project, and whether the file is read as the program.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "C:\Users\zc\Desktop\github\clice"
//! $env:CPPLS_KEYSTROKE_FILE = "src/AST/Selection.cpp"
//! cargo test --release -p cpp_code_analysis --test keystroke_probe -- --ignored --nocapture
//! ```
//!
//! The project named above is the one this machine has: a C++ language server of its own, ~200 sources and a
//! self-contained `include/` tree. It is not the checkout the earlier figures came from
//! (`E:\EmmyLuaCodeStyle`), and the numbers are therefore not comparable to those — what they answer is the
//! *shape*: is the file read as the program, and what does that cost per keystroke.
//!
//! # The two questions
//!
//! ```text
//! how long does it take      the number a reader feels, per keystroke, after the project is settled
//! is it read as the program  `Step::with_the_environment` — a file read as *text* while a reader is typing in it
//!                            is the window `docs/architecture.md` §5's four patch layers exist for
//! ```
//!
//! # Why two halves
//!
//! A **body** edit moves no directive, so the file's timeline is still the one in force; a **directive** edit moves
//! the layout, which drops it. The two are the same keystroke to a reader and completely different work to the
//! analysis, and the second is the one that has to be walked again — so both are measured, and the difference
//! between the two columns is the price of the architecture's order (`directives → unit → parse`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\github\clice".to_string()),
    )
}

fn entry(root: &std::path::Path) -> PathBuf {
    std::env::var("CPPLS_KEYSTROKE_FILE")
        .map(PathBuf::from)
        .map(|file| {
            if file.is_absolute() {
                file
            } else {
                root.join(file)
            }
        })
        .unwrap_or_else(|_| root.join("src/AST/Selection.cpp"))
}

/// `min / median / max` of a run of measurements, in milliseconds.
fn summary(times: &[Duration]) -> String {
    let mut millis: Vec<u128> = times.iter().map(Duration::as_millis).collect();
    if millis.is_empty() {
        return "no steps".to_string();
    }
    millis.sort_unstable();
    format!(
        "min {} / median {} / max {} ms",
        millis[0],
        millis[millis.len() / 2],
        millis[millis.len() - 1]
    )
}

#[test]
#[ignore = "a probe: opens a real project, settles its index, and prints instead of asserting"]
fn what_a_keystroke_costs() {
    let root = project();
    let file = entry(&root);
    let original = std::fs::read_to_string(&file).expect("the file reads");

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);

    let indexing = Instant::now();
    session.index_everything();
    let mut slices = 0;
    while session.pending_work() > 0 && slices < 10_000 {
        session.advance(64);
        slices += 1;
    }
    println!(
        "settled in {:?} after {slices} extra slice(s); units {:?}",
        indexing.elapsed(),
        session.unit_stats()
    );

    // **What a reader does**: open the file, then type. The open is its own step (a summary and a timeline dropped,
    // the file queued at `Priority::Open`), and it is measured separately because it is not a keystroke.
    session.did_open(&file, &original);
    let opened = Instant::now();
    let steps = session.advance(1);
    let open_step = steps.iter().find(|step| step.path == file);
    println!(
        "didOpen + re-read: {:?} | environment {} | units {:?}",
        opened.elapsed(),
        open_step.is_some_and(|step| step.with_the_environment),
        session.unit_stats()
    );

    // **Body edits**, one at a time: the timeline survives, so this is the cheap half.
    let mut body_times = Vec::new();
    let mut body_rendered = 0usize;
    let mut text = original.clone();
    for round in 0..10 {
        text.push_str(&format!("// typed {round}\n"));
        session.did_change(&file, &text);
        let started = Instant::now();
        let steps = session.advance(1);
        body_times.push(started.elapsed());
        let step = steps.iter().find(|step| step.path == file);
        if step.is_some_and(|step| step.with_the_environment) {
            body_rendered += 1;
        }
    }
    println!("body keystrokes,   10x: {}", summary(&body_times));
    println!(
        "{body_rendered} of 10 read as the program; units {:?}",
        session.unit_stats()
    );

    // **Directive edits**: the timeline is dropped every time, so this is the half that has to walk again — and the
    // state a reader is in after any edit to a `#include` line or a `#define`.
    let mut directive_times = Vec::new();
    let mut directive_rendered = 0usize;
    for round in 0..10 {
        let edited = format!("{text}#define PAD_{round} 1\n");
        session.did_change(&file, &edited);
        let started = Instant::now();
        let steps = session.advance(1);
        directive_times.push(started.elapsed());
        if steps
            .iter()
            .find(|step| step.path == file)
            .is_some_and(|step| step.with_the_environment)
        {
            directive_rendered += 1;
        }
        text = edited;
    }
    println!("directive keystrokes, 10x: {}", summary(&directive_times));
    println!(
        "{directive_rendered} of 10 read as the program; units {:?}",
        session.unit_stats()
    );

    let sizes = session.cache_sizes();
    println!(
        "cache afterwards: {} summar(ies), {} unit(s), {} fact(s)",
        sizes.summaries, sizes.units, sizes.facts
    );
}
