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
    let Some(dir) = args.first() else {
        eprintln!("usage: checks_probe <include-dir> [--limit N]");
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
            *by_check.entry(finding.check).or_default() += 1;
            if shown < 40 {
                shown += 1;
                println!(
                    "  {}:{}  [{}]  {}",
                    path.file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    finding.range.start_offset,
                    finding.check,
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
