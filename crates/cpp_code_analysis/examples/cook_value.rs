//! **What the rendering buys a file that already has its closure's macro bodies** — the measurement
//! `docs/incremental-edits.md` §5.2 step 4 is waiting on.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example cook_value -- <root file>
//! ```
//!
//! # The decision this is for
//!
//! The plan is to stop rendering the file a person is typing into, and answer from the **raw** reading instead —
//! which is only a fair trade if the raw reading, *with the macro bodies of its own closure in hand*, already says
//! what the rendering says. The number the document quotes for that is old and was measured on one header
//! (`cook(<string>)`: 1000 declarations, 1 of which the raw reading did not already have). This is the same question
//! asked of a whole closure, per file, as a set difference rather than a count:
//!
//! ```text
//!   only in the cooked reading   what a keystroke would LOSE by not rendering the file
//!   only in the raw reading      what the rendering drops — `#if` branches nobody takes, declarations inside
//!                                macro bodies, and anything the expansion erases
//! ```
//!
//! # What it does not measure
//!
//! * **Diagnostics.** `Session::diagnostics` answers from the cooked reading when one is held and from the file's
//!   own text otherwise, and the two differ exactly where a branch nobody takes has an error in it. That is a real
//!   capability and this probe says nothing about it.
//! * **The scope of a declaration**, only its qualified name. A reading that puts `Widget` in the wrong namespace
//!   but still calls it `Widget` is invisible here — `cook_vs_compiler` is the probe for that.
//!
//! `cooked-index.rs` in the crate root is the same comparison with **no closure at all** (level 0), which is the
//! plumbing question; this is the quality question, which needs the environment to be real.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let Some(entry) = std::env::args().nth(1) else {
        eprintln!("usage: cook_value <root file>");
        std::process::exit(2);
    };
    let root = std::fs::canonicalize(Path::new(&entry)).unwrap_or_else(|_| PathBuf::from(&entry));
    let home = root.parent().map(Path::to_path_buf).unwrap_or_default();
    let text = std::fs::read_to_string(&root).expect("the root file reads");

    let mut session = Session::open(
        home.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&home),
    );
    session.did_open(&root, &text);
    session.want_cooked_reading(&root);
    session.index_everything();

    // **And then type into it**, because that is the case step 4 is about and it is not the case the first index
    // measures: a body edit leaves the file's timeline in hand (`RootKey` keys it on the preamble), so this is the
    // moment where the raw reading can be built **with** the closure's macro bodies — see `Session::advance`. A cold
    // reading is built without them, which is why a header's two readings differ far more than a translation unit's:
    // the numbers below are the *sum* of the environment and the rendering, and only the second is step 4's subject.
    let typed = format!(
        "{text}\nnamespace app {{ int extra() {{ return 1; }} }}\n"
    );
    session.did_change(&root, &typed);
    session.want_cooked_reading(&root);
    session.index_everything();

    let mut rows: Vec<(PathBuf, usize, usize, usize, usize)> = Vec::new();
    for summary in session.index().summaries() {
        let path = summary.path.clone();
        let raw: std::collections::BTreeSet<String> = summary
            .declarations
            .iter()
            .map(cpp_code_analysis::DeclFact::qualified_name)
            .collect();
        let Some(cooked) = session.index().cooked_declarations(&path) else {
            continue;
        };
        let cooked: std::collections::BTreeSet<String> = cooked
            .iter()
            .map(cpp_code_analysis::DeclFact::qualified_name)
            .collect();

        let only_cooked = cooked.difference(&raw).count();
        let only_raw = raw.difference(&cooked).count();
        let both = raw.intersection(&cooked).count();
        rows.push((path, raw.len(), cooked.len(), only_cooked, only_raw));
        let _ = both;
    }

    if rows.is_empty() {
        println!("nothing was cooked — is {} inside a project?", root.display());
        return;
    }

    let total_raw: usize = rows.iter().map(|row| row.1).sum();
    let total_cooked: usize = rows.iter().map(|row| row.2).sum();
    let total_only_cooked: usize = rows.iter().map(|row| row.3).sum();
    let total_only_raw: usize = rows.iter().map(|row| row.4).sum();

    rows.sort_by_key(|row| std::cmp::Reverse(row.3));
    println!("{} file(s) cooked\n", rows.len());
    println!("  {:<9} {:>8} {:>8} {:>12} {:>10}", "file", "raw", "cooked", "only cooked", "only raw");
    for (path, raw, cooked, only_cooked, only_raw) in rows.iter().take(15) {
        let name = path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
        let mark = if *path == root { " ← the file being edited" } else { "" };
        println!("  {name:<9} {raw:>8} {cooked:>8} {only_cooked:>12} {only_raw:>10}{mark}");
    }

    println!("\ntotal raw {total_raw} | cooked {total_cooked}");
    println!("only in the cooked reading {total_only_cooked}  ← what skipping the render would lose");
    println!("only in the raw reading    {total_only_raw}");
    println!(
        "\nthe edited file itself: {} raw declarations, {} of which the cooked reading does not have",
        rows.iter().find(|row| row.0 == root).map(|row| row.1).unwrap_or(0),
        rows.iter().find(|row| row.0 == root).map(|row| row.3).unwrap_or(0),
    );
}
