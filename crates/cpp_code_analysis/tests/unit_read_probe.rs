//! **A probe, not a test**: what reading a whole translation unit as one program does to the index.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "C:\Users\zc\Desktop\github\clice"
//! cargo test --release -p cpp_code_analysis --test unit_read_probe -- --ignored --nocapture
//! ```
//!
//! # The question, and why it is a probe
//!
//! [`Session::read_the_unit`] reads a closure as **one program**: one rendering, one parse
//! ([`cpp_code_analysis::FileIndexer::index_unit_rendering`]), every fact filed under the file that wrote it, and
//! every file in the closure marked read so the per-file queue skips it. That is the shape `docs/architecture.md`
//! §5 step two needs — *one timeline per translation unit, not one walk per file* — and it is **not wired** into the
//! pump because of the numbers this probe prints.
//!
//! The previous attempt at wiring it changed three readings the wrong way and could not say why (the note on
//! `read_the_unit` records them: `declarations_in("std")` 2008 → 1959, and two `definition` counts). The guess here
//! is that the cause was the one `index_unit_rendering`'s crossing fence now quarantines — a file whose unclosed
//! scope used to swallow the rest of the program — and that what is left is cost.
//!
//! # What it prints
//!
//! ```text
//! before / after   the closure's facts, its cooked readings, and how many of a handful of names answer
//! per file         only the files that moved, because a total hides one file losing what another gained
//! the cost         the read itself, and the second read of the same program
//! ```
//!
//! A reading that goes **down** is the thing to look for: a unit parse is one parse of a whole program, so a file
//! whose mistake it cannot repair loses facts for every file after it — which is what the fence exists to bound.

use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\github\clice".to_string()),
    )
}

fn entry(root: &std::path::Path) -> PathBuf {
    std::env::var("CPPLS_UNIT_ENTRY")
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

/// The closure of `file` as paths — the files a unit read would file facts for.
fn closure_of(session: &Session, file: &std::path::Path) -> Vec<PathBuf> {
    session
        .index()
        .visible_files(file)
        .into_iter()
        .map(|(path, _)| PathBuf::from(path))
        .collect()
}

/// What the index says about one file: `(facts, cooked declarations, names answered)`.
fn shape_of(session: &Session, file: &std::path::Path, names: &[&str]) -> (usize, usize, usize) {
    let facts = session
        .index()
        .summary(file)
        .map_or(0, |summary| summary.declarations.len());
    let cooked = session
        .index()
        .cooked_declarations(file)
        .map_or(0, <[cpp_code_analysis::DeclFact]>::len);
    let answered = names
        .iter()
        .filter(|name| matches!(session.index().definitions(name, file), Known::Yes(_)))
        .count();
    (facts, cooked, answered)
}

#[test]
#[ignore = "a probe: opens a real project, reads a unit, and prints instead of asserting"]
fn what_a_unit_read_does_to_the_index() {
    let root = project();
    let file = entry(&root);
    let names = [
        "Selection",
        "SyntaxNode",
        "Decl",
        "SemanticToken",
        "Visitor",
        "clang::Decl",
    ];

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
        "settled in {:?} after {slices} slice(s); units {:?}",
        indexing.elapsed(),
        session.unit_stats()
    );

    // **The closure as the index sees it**, which is the set a unit read would give cooked facts to without any
    // request naming them.
    let closure = closure_of(&session, &file);
    let before: Vec<(PathBuf, usize, usize, usize)> = closure
        .iter()
        .map(|path| {
            let (facts, cooked, answered) = shape_of(&session, path, &names);
            (path.clone(), facts, cooked, answered)
        })
        .collect();
    let sum = |rows: &[(PathBuf, usize, usize, usize)], at: usize| -> usize {
        rows.iter().map(|row| match at {
            1 => row.1,
            2 => row.2,
            _ => row.3,
        })
        .sum()
    };
    println!(
        "before: {} file(s) in the closure, {} fact(s), {} cooked declaration(s), {} name answer(s)",
        closure.len(),
        sum(&before, 1),
        sum(&before, 2),
        sum(&before, 3)
    );

    // **The read itself**, and where its time goes.
    let started = Instant::now();
    let reading = session.read_the_unit(&file);
    let took = started.elapsed();
    let Some(reading) = reading else {
        panic!("the entry has a summary, so the unit can be walked");
    };
    println!(
        "read_the_unit: {took:?} | {} file(s) placed, {} token(s), {} error(s), {} unplaced, {} unbalanced",
        reading.files,
        reading.tokens,
        reading.errors,
        reading.unplaced,
        reading.unbalanced.len()
    );

    let after: Vec<(PathBuf, usize, usize, usize)> = closure
        .iter()
        .map(|path| {
            let (facts, cooked, answered) = shape_of(&session, path, &names);
            (path.clone(), facts, cooked, answered)
        })
        .collect();
    println!(
        "after:  {} fact(s), {} cooked declaration(s), {} name answer(s)",
        sum(&after, 1),
        sum(&after, 2),
        sum(&after, 3)
    );

    // **Per file, and only the ones that moved** — a total hides a file that lost what another gained.
    let moved: Vec<String> = before
        .iter()
        .zip(&after)
        .filter(|(left, right)| left != right)
        .map(|(left, right)| {
            format!(
                "  {} facts {}→{}, cooked {}→{}, names {}→{}",
                left.0.file_name().unwrap_or_default().to_string_lossy(),
                left.1,
                right.1,
                left.2,
                right.2,
                left.3,
                right.3
            )
        })
        .collect();
    println!("files that moved ({}):", moved.len());
    for line in moved.iter().take(24) {
        println!("{line}");
    }

    // **And what the second one costs**, which is the number that decides whether this can be scheduled: the render
    // and the parse are memoised on the program's bytes, so a project whose sources share their headers pays once.
    let started = Instant::now();
    let again = session.read_the_unit(&file).map(|reading| reading.files);
    println!("second read: {:?} for {again:?} file(s)", started.elapsed());

    let sizes = session.cache_sizes();
    println!(
        "cache: {} summar(ies), {} cooked file(s) / {} declaration(s)",
        sizes.summaries, sizes.cooked_files, sizes.cooked_declarations
    );
}
