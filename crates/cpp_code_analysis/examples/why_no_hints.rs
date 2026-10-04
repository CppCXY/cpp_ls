//! **Why a file's inlay hints are not drawn** — the tally, on whatever file is named.
//!
//! ```text
//! why_no_hints <project-root> <file> [<line> <column>]
//! ```
//!
//! A hint that is not drawn has four causes that look identical in an editor, and each needs a different fix:
//!
//! ```text
//! [0] calls in range          how many the walk found at all — zero means the range, not the resolution
//! [1] callee did not resolve  the declaration was not found, or could not be placed
//! [2] no parameter list       the declaration was found and has none this layer can read
//! [3] **the declaring file could not be read** — `Session::view` answered `None`, which it does when the file is
//!     not in the VFS: every call into that file loses its hints at once, and this is the one that has no other
//!     symptom
//! [4] no arguments            the call's argument list could not be read
//! [5] outside the range       the argument is not where the client asked about
//! ```
//!
//! This exists because every one of those has been *guessed at* in turn while chasing one report. The tally names
//! the one that is happening.

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("a project root");
    let file = std::path::PathBuf::from(args.next().expect("a file"));

    let mut session = Session::open(
        std::path::PathBuf::from(&root),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let text = std::fs::read_to_string(&file).expect("the file");
    session.add_project_files([file.clone()]);
    session.did_open(&file, &text);
    session.index_everything();
    for _ in 0..100_000 {
        if session.advance(1).is_empty() {
            break;
        }
    }

    // **Both readings, because the answer differs**: the handler asks for the file's own tokens, and a probe that
    // asked for the rendering would be measuring a different question.
    for (label, view) in [
        (
            "the file's own tokens",
            session.view_of_the_file(&file).expect("the file is held"),
        ),
        ("the rendering", session.view(&file).expect("the file is held")),
    ] {
        let whole = cpp_parser::SourceRange::new(0, view.source.len());
        let (hints, why) = session.inlay_hints_saying_why(&view, whole);
        println!("--- {label} ({} bytes) ---", view.source.len());
        println!(
            "  {} call(s) in range, {} hint(s)",
            why[0],
            hints.len()
        );
        println!(
            "  skipped: {} callee unresolved, {} no parameter list, {} **declaring file unreadable**, \
             {} no arguments, {} outside the range",
            why[1], why[2], why[3], why[4], why[5]
        );
        for hint in hints.iter().take(8) {
            println!("    {}: at offset {}", hint.name, hint.offset);
        }
    }

    // **And whether the files those calls name can be read at all**, which is what `[3]` counts.
    for header in ["vector", "string", "format", "iostream", "cstdio", "stdio.h", "ostream", "xstring"] {
        let held = session.files().held(header).is_some();
        let viewed = session.view(header).is_some();
        println!("  `{header}`: held {held}, viewable {viewed}");
    }
}
