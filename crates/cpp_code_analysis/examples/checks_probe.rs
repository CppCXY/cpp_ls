//! **Do the semantic checks stay silent on a corpus that is known good?**
//!
//! The module documentation of `sema::check` names this as the measurement that keeps the layer honest:
//!
//! > The failure mode this layer has to avoid is not missing a problem, it is **inventing one**, because an editor
//! > that underlines correct code is one the user turns off. The measurement that keeps it honest is a corpus that
//! > is *known good*: the standard library.
//!
//! So this runs every check over every file it is given and prints a finding per line, with its check's name. A
//! clean corpus prints a count of zero, and anything else is a defect in a check rather than in the corpus.
//!
//! ```text
//! usage: checks_probe <include-dir> [--limit N]
//! ```
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(dir), Some(root)) = (args.first(), args.get(1)) else {
        eprintln!("usage: checks_probe <include-dir> <root file> [--limit N]");
        std::process::exit(2);
    };
    let limit: usize = args
        .iter()
        .position(|arg| arg == "--limit")
        .and_then(|at| args.get(at + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX);

    let mut session = cpp_code_analysis::Session::open(
        std::path::PathBuf::from(dir),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(std::path::Path::new(".")),
    );
    // **The ask before the pump, and the pump again** — the contract `tests/completion.rs` states once for the
    // workspace, and the defect this probe shared with `one_check`: `Session::diagnostics` answers from the **cooked
    // reading**, a rendering is built because something asked for one, and without the ask every file answered
    // `None`. A probe that silently checks nothing is worse than no probe — it prints a count of zero, which is the
    // *good* answer, for a corpus nothing was ever checked on.
    let wanted: Vec<std::path::PathBuf> = session
        .index()
        .summaries()
        .map(|summary| summary.path.clone())
        .collect();

    // **And the files have to be the project's before anything can be cooked.** A rendering is built against a
    // file's **closure** — what its `#include`s resolve to — so a session that was never told which files are the
    // project's has no closure to walk, `diagnostics` answers `None` for every path, and the probe prints a count of
    // zero: the *good* answer, for a corpus nothing was ever checked on. Measured, in `one_check`, whose defect this
    // was too — with the ask but no project files, `syntax-error` reported zero on a file that does not parse.
    // **And the file itself has to be opened**, which is what puts it — and the files its `#include`s reach — into
    // the index at all: `summaries()` is empty without it, so the loop below had nothing to ask about. Measured, and
    // it is the same shape `one_check` needed.
    let root_path = std::path::PathBuf::from(root);
    session.did_open(&root_path, &std::fs::read_to_string(&root_path).unwrap_or_default());
    session.add_project_files(wanted.clone());
    for path in &wanted {
        session.want_cooked_reading(path);
    }
    session.index_everything();

    let mut checked = 0usize;
    let mut by_check: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut shown = 0usize;

    let paths: Vec<std::path::PathBuf> = session
        .index()
        .summaries()
        .map(|summary| summary.path.clone())
        .take(limit)
        .collect();

    for path in &paths {
        let Some(found) = session.diagnostics(path) else {
            continue;
        };
        checked += 1;
        for finding in &found.checks {
            *by_check.entry(finding.code.name()).or_default() += 1;
            if shown < 40 {
                shown += 1;
                println!(
                    "  {}:{}  [{}]  {}",
                    path.file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    finding.range.start_offset,
                    finding.code.name(),
                    finding.message
                );
            }
        }
    }

    println!("\n--- checks over {checked} file(s) ---");
    if by_check.is_empty() {
        println!("    **no findings at all** — the corpus is clean");
    }
    for (check, count) in &by_check {
        println!("    {count:>5}  {check}");
    }
}




