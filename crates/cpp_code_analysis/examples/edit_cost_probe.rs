//! **What one keystroke costs** — the queue a `did_change` fills, and how long it takes to drain.
//!
//! ```text
//! edit_cost_probe <project-root> <file> [<line> <column> <what to insert>]
//! ```
//!
//! # Why this is measured rather than reasoned about
//!
//! An edit that leaves every directive where it was is *supposed* to cost one file: the summary and the cooked
//! reading of the edited file are dropped, it goes back on the queue at `Priority::Open`, and nothing else moves —
//! that is what [`Session::buffer_changed`] says in its own words. Whether that is what happens is a different
//! question, and the symptom that raises it is not "it is slow" but "the inlay hints never come back": the hint
//! handler answers **nothing at all** while `Session::pending() > 0`, so a queue that does not drain is a feature
//! that does not work, not a feature that is late.
//!
//! So this prints, for one character inserted:
//!
//! ```text
//! before      the queue's state once the workspace has settled
//! the edit    what was queued by it, and how many files are pending right after
//! the drain   how many steps, how long, and whether the queue ever came back to empty
//! ```
//!
//! A drain that does not finish is the finding; a drain that finishes in milliseconds is the other one, and it
//! moves the search to whatever keeps the queue non-empty between edits.

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("a project root");
    let file = std::path::PathBuf::from(args.next().expect("a file"));
    let line: usize = args.next().and_then(|value| value.parse().ok()).unwrap_or(0);
    let column: usize = args.next().and_then(|value| value.parse().ok()).unwrap_or(0);
    let insert = args.next().unwrap_or_else(|| " ".to_string());

    let mut session = Session::open(
        std::path::PathBuf::from(&root),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let text = std::fs::read_to_string(&file).expect("the file");
    session.add_project_files([file.clone()]);
    session.did_open(&file, &text);
    session.index_everything();

    // **Drained to idle before the edit**, because what the edit costs is the difference from a settled session and
    // a probe that measured it mid-drain would be adding the two numbers together.
    let (steps, took) = drain(&mut session);
    println!("cold index: {steps} step(s) in {:.2?}; pending now {}", took, session.pending());

    // The edit: one character at the position given, which is what a keystroke is.
    let at = line_column_to_offset(&text, line, column);
    let mut edited = String::with_capacity(text.len() + insert.len());
    edited.push_str(&text[..at]);
    edited.push_str(&insert);
    edited.push_str(&text[at..]);

    let before = session.pending();
    let changed = Instant::now();
    session.did_change(&file, &edited);
    let build = changed.elapsed();
    println!(
        "the edit: `{insert}` at {line}:{column} (offset {at}); pending was {before}, is now {}; the call itself took {build:.2?}",
        session.pending()
    );

    // **The hint question asked the moment an edit lands**, which is the state a typist is always in.
    //
    // The handler in `cpp_ls` answers **nothing at all** while `Session::pending() > 0`, on the reasoning that a
    // callee in a file nobody has read yet resolves to nothing. The queue is non-empty for as long as the drain
    // takes, and a person typing is inside that window at every keystroke — so the gate is the difference between
    // "the hints are late" and "the hints are gone", and this asks whether the answer was available anyway.
    let view_now = session.view(&file).expect("the file is held");
    let whole = cpp_parser::SourceRange::new(0, view_now.source.len());
    let before_drain = session.inlay_hints(&view_now, whole);
    println!(
        "asked with {} pending, on the {} reading: {} hint(s)",
        session.pending(),
        if view_now.written_text().is_some() {
            "rendered"
        } else {
            "file's own tokens"
        },
        before_drain.len()
    );

    // **The drain, and whether it ends.** `advance` returning nothing is the queue's own answer that it is empty.
    let (steps, took) = drain(&mut session);
    println!(
        "the drain: {steps} step(s) in {took:.2?}; pending now {} — {}",
        session.pending(),
        if session.pending() == 0 {
            "**the queue came back to empty**, so a hint asked now would be answered"
        } else {
            "**THE QUEUE DID NOT DRAIN**, and every inlay hint is answered with nothing until it does"
        }
    );

    // …and the same question once the session has settled, which is the answer the gate is waiting for.
    let view_later = session.view(&file).expect("the file is held");
    let whole = cpp_parser::SourceRange::new(0, view_later.source.len());
    let after = session.inlay_hints(&view_later, whole);
    println!(
        "asked with {} pending, on the {} reading: {} hint(s)",
        session.pending(),
        if view_later.written_text().is_some() {
            "rendered"
        } else {
            "file's own tokens"
        },
        after.len()
    );

    // **The two causes separated.** The ask above had *both* a non-empty queue and the file's own tokens, and the
    // settled ask had neither — so it cannot say which one the hints need. `view_of_the_file` returns the file's own
    // tokens **even when a rendering is cached**, which is the isolation: same index, same instant, only the reading
    // differs.
    let plain = session.view_of_the_file(&file).expect("the file is held");
    let whole = cpp_parser::SourceRange::new(0, plain.source.len());
    let on_the_plain_reading = session.inlay_hints(&plain, whole);
    println!(
        "asked with {} pending on the file's own tokens, index complete: {} hint(s) — {}",
        session.pending(),
        on_the_plain_reading.len(),
        if on_the_plain_reading.len() == after.len() {
            "**the reading does not matter; the queue's gate is what suppresses them**"
        } else {
            "**THE READING IS WHAT MATTERS: the same question about the same file yields hints on one reading and \
             not the other**"
        }
    );
}

/// Steps until the queue reports nothing left, with the time and the count it took.
fn drain(session: &mut Session<DiskFiles>) -> (usize, std::time::Duration) {
    let started = Instant::now();
    let mut steps = 0usize;
    // Bounded rather than `loop`: a queue that never empties is the finding, and a probe that hung on it would
    // report nothing at all.
    while steps < 100_000 {
        if session.advance(1).is_empty() {
            break;
        }
        steps += 1;
    }
    (steps, started.elapsed())
}

/// The offset of a line and column, counted the way a client counts them: lines from zero, columns in bytes.
fn line_column_to_offset(text: &str, line: usize, column: usize) -> usize {
    let mut at = 0usize;
    for _ in 0..line {
        match text[at..].find('\n') {
            Some(next) => at += next + 1,
            None => return text.len(),
        }
    }
    (at + column).min(text.len())
}
