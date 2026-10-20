//! **Does the standard library cook without errors?** — the question, answered by counting.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example cook_errors -- <dir> [<entry.cpp>]
//! ```
//!
//! # Why a probe of its own
//!
//! `std_closure` answers how much of the closure was *reached* (and what did not resolve);
//! `diagnostics_cost` answers what one `textDocument/diagnostic` costs on **one** file. Neither answers this: cook
//! every file the index holds and count what the parse of the **cooked reading** reported, per file and in total.
//!
//! The distinction matters because the two failure modes look alike from outside and are different things:
//!
//! ```text
//!   an unresolved `#include`      a file the walk could not reach at all — `std_closure` counts these
//!   a parse error in a rendering  a file that *was* read and expanded, and whose result the grammar refused
//! ```
//!
//! The second is the one this exists for. A macro-heavy header can be reached, expanded, and still come out as
//! something the parser reports on — and "the standard library parses" is a claim about that count, not about the
//! first.
//!
//! # What it prints
//!
//! ```text
//!   files cooked            how many of the index's files answered a view (a view is the cooked reading)
//!   parse errors            the total, and the files with any, worst first
//!   the first error         the message and range of each of the worst few, because "3 errors" and "3 errors of the
//!                           same kind in one construct" are different reports
//! ```

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: cook_errors <dir> [<entry.cpp>]");
        std::process::exit(2);
    };
    let home = PathBuf::from(&dir);
    let entry = std::env::args().nth(2).map(PathBuf::from);

    let mut session = Session::open(
        home.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&home),
    );
    if let Some(entry) = &entry {
        let text = std::fs::read_to_string(entry).unwrap_or_default();
        session.did_open(entry, &text);
    }

    // **The index first, then the asks, then the pump again.** A rendering is built for a file that was *asked* for,
    // and the ask names a path the index has to know: asking before anything was read collects an empty list, which
    // is what the first version of this probe measured (it printed `0 file(s) in the index`).
    session.index_everything();
    let wanted: Vec<PathBuf> = session
        .index()
        .summaries()
        .map(|summary| summary.path.clone())
        .collect();
    for path in &wanted {
        session.want_cooked_reading(path);
    }
    let started = std::time::Instant::now();
    session.index_everything();
    println!(
        "{} file(s) in the index, cooked in {:?}",
        wanted.len(),
        started.elapsed()
    );

    let mut cooked = 0usize;
    let mut total = 0usize;
    let mut worst: Vec<(PathBuf, Vec<String>)> = Vec::new();

    for path in &wanted {
        let Some(view) = session.view(path) else {
            continue;
        };
        cooked += 1;
        let errors = view.errors();
        if errors.is_empty() {
            continue;
        }
        total += errors.len();
        // **The rendering is dumped**, because one error in a hundred-thousand-byte header cannot be chased by
        // guessing: a fragment does not reproduce it (five attempts, each more faithful than the last), which means
        // the trigger is the parser's state from an *earlier* construct. With the whole rendering on disk the bisect
        // is mechanical: truncate it, parse, keep the half that still fails.
        if let Ok(dumped) = std::env::var("CPPLS_DUMP_RENDERING") {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let _ = std::fs::write(std::path::Path::new(&dumped).join(name), &*view.source);
        }
        let described: Vec<String> = errors
            .iter()
            .take(3)
            .map(|error| {
                let range = cpp_parser::source_range(error.range);
                // **A window, not the line.** A rendering is one long text with the directives resolved away, so the
                // line an offset is on can be thousands of characters — and what a parser tripped over is decided by
                // the few tokens around it.
                let from = range.start_offset.saturating_sub(200);
                let to = (range.end_offset() + 120).min(view.source.len());
                let window = view
                    .source
                    .get(from..to)
                    .unwrap_or_default()
                    .replace('\n', "\\n");
                // **The error's own bytes marked**, because "which `]`" is the whole question when the answer is one
                // character long and the construct around it holds three of them.
                let marked = match range.start_offset.checked_sub(from) {
                    Some(at) if at <= window.len() => {
                        let mut marked = window.clone();
                        marked.insert_str(at, "  <<<HERE>>>  ");
                        marked
                    }
                    _ => window,
                };
                format!(
                    "{}..{}  {:?}\n      …{marked}…",
                    range.start_offset,
                    range.end_offset(),
                    error.message,
                )
            })
            .collect();
        worst.push((path.clone(), described));
    }

    worst.sort_by_key(|(_, errors)| std::cmp::Reverse(errors.len()));

    println!("\n--- the reading a compiler is handed ---");
    println!("  files cooked            {cooked} of {}", wanted.len());
    println!("  files with parse errors {}", worst.len());
    println!("  parse errors in total   {total}");

    for (path, errors) in worst.iter().take(10) {
        println!("\n  {}  ({} shown)", path.display(), errors.len());
        for line in errors {
            println!("    {line}");
        }
    }

    // **What the *client* would be shown**, which is not always the same list: `Session::diagnostics` publishes one
    // message for a file that declares itself a fragment (an `#error` line and a rendering that begins with a brace)
    // instead of the cascade the grammar reports for it. A probe that stopped at the parse errors could not tell
    // those apart and would count `bits/text_encoding-data.h` as 895 findings forever.
    if !worst.is_empty() {
        println!("\n--- what a client is shown ---");
        for (path, _) in worst.iter().take(10) {
            match session.diagnostics(path) {
                Some(found) => println!(
                    "  {}  {} error(s){}",
                    path.display(),
                    found.errors.len(),
                    found
                        .errors
                        .first()
                        .map(|error| format!(" — first: {:?}", error.message))
                        .unwrap_or_default()
                ),
                None => println!("  {}  (no reading)", path.display()),
            }
        }
    }

    // **Every file the client would see a squiggle in, with the text around it.** The list above is only the files
    // the parse errors of *this* pass touched; a file cooked during the pass but asked for afterwards is not in it,
    // and that is exactly where a construct to fix can hide.
    println!("\n--- every file with a published error ---");
    for path in &wanted {
        let Some(found) = session.diagnostics(path) else {
            continue;
        };
        if found.errors.is_empty() {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap_or_default();
        println!("\n  {}  {} error(s)", path.display(), found.errors.len());
        for error in found.errors.iter().take(2) {
            let from = error.start.saturating_sub(160);
            let to = (error.end + 160).min(text.len());
            let window = text
                .get(from..to)
                .unwrap_or_default()
                .replace('\n', "\\n");
            println!("    {}  {:?}\n      …{window}…", error.start, error.message);
        }
    }
}
