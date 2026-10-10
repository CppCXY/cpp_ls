//! **What one `textDocument/diagnostic` costs, and which part of it.**
//!
//! ```text
//! cargo run --release --example diagnostics_cost -- <dir> [<file.cpp>]
//! ```
//!
//! `Session::diagnostics` answers from **two different readings** (`session.rs:2687-2769`) and they do not cost the
//! same:
//!
//! ```text
//! the index holds a cooked reading   the errors are already placed in the file, so this is a lookup
//! it does not                        the answer needs the file's own tree, which is a parse
//! ```
//!
//! …and whichever arm runs, the **checks** need that tree as well — `checks_about` takes a `&FileView`, and the
//! view is a parse of the file through the VFS. So the question this probe answers is not "how long do diagnostics
//! take" but **which of the three it is**: the parse, the module notes, or one of the five checks.
//!
//! That distinction decides the fix, and the two candidates are far apart:
//!
//! * if the **parse** dominates, the file's tree is being rebuilt per request and wants to be held (clangd keeps a
//!   full AST for the open file, three of them — `ASTRetentionPolicy`);
//! * if a **check** dominates, it is one function to write better, which is the shape the 302 ms check had.
//!
//! # What it prints
//!
//! 1. the **first** call, which is a cold VFS and a cold cooked reading;
//! 2. the **second**, which is the steady state a client actually lives in — a client re-asks on every edit;
//! 3. `CPPLS_TRACE_DIAGNOSTICS=1` adds one line per check, so the attribution is the crate's own numbers rather
//!    than a subtraction this file did.

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("a directory"));
    let file = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("main.cpp"));

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );

    println!(
        "{} include paths | toolchain {:?}",
        session.config().include_paths.len(),
        session
            .toolchain()
            .and_then(|toolchain| toolchain.version.clone())
    );

    let Some(source) = std::fs::read_to_string(&file).ok() else {
        panic!("{} does not read", file.display());
    };
    session.did_open(&file, &source);

    // The pump, to the end: a diagnostic asked for while the index is filling is a different question (see
    // `tests/latency.rs`), and this probe is about the cost of the answer once it exists.
    let indexing = Instant::now();
    session.index_everything();
    println!(
        "indexed {} file(s) in {} ms\n",
        session.index().len(),
        indexing.elapsed().as_millis()
    );

    println!("--- the same file, asked repeatedly ---");
    let mut previous: Option<Duration> = None;
    let mut findings = 0usize;

    for attempt in 0..6 {
        cpp_code_analysis::stages::check_trace::reset();

        let asking = Instant::now();
        let answer = session.diagnostics(&file);
        let took = asking.elapsed();

        let Some(answer) = answer else {
            println!("  #{attempt}: no diagnostics (the file is not readable)");
            return;
        };
        findings = answer.errors.len() + answer.checks.len();

        println!(
            "  #{attempt}: {:>7} ms | reading {} | {} parse error(s), {} check(s), {} module note(s){}",
            took.as_millis(),
            match answer.reading {
                cpp_code_analysis::DiagnosticReading::Cooked => "cooked",
                cpp_code_analysis::DiagnosticReading::Raw => "raw",
            },
            answer.errors.len(),
            answer.checks.len(),
            answer.notes.len(),
            match previous {
                Some(before) if took.as_millis() > before.as_millis() => "  ← slower than the last",
                _ => "",
            }
        );
        for line in cpp_code_analysis::stages::check_trace::lines() {
            println!("        {line}");
        }

        previous = Some(took);
    }

    // ---------------------------------------------------------------------------------------------
    // The two halves told apart: is it the tree, or is it the checks?
    // ---------------------------------------------------------------------------------------------
    //
    // `diagnostics` cannot be asked for one without the other, so the tree is timed where the session already
    // measures it — `Session::view` — and what is left after subtracting it is the checks and the answer's assembly.
    println!("\n--- the tree, and then the answer ---");

    let viewing = Instant::now();
    let view = session.view(&file);
    let parse = viewing.elapsed();
    println!("  Session::view (a parse)          {:>7} ms", parse.as_millis());
    println!("  the file has {} node(s)", match &view {
        Some(view) => view.root.descendants().count(),
        None => 0,
    });

    let answering = Instant::now();
    let _ = session.diagnostics(&file);
    let whole = answering.elapsed();
    println!(
        "  Session::diagnostics            {:>7} ms   ({} finding(s) in the last answer)",
        whole.as_millis(),
        findings
    );
    let _ = Path::new("");
}
