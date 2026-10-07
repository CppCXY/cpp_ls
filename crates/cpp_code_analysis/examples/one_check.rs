//! **Every finding of one check, with the declaration behind it** — for working through a corpus report one shape
//! at a time.
//!
//! `checks_probe` prints a line per finding; this prints the finding **and the fact it came from**, because the
//! question about a corpus finding is always "what did the reading think this declaration was". Twice now the
//! answer has been a reading defect rather than a check defect (`_CharT _Fill[]`, and the calling-convention macro
//! that made a function a variable), and both are invisible from the message alone.
//!
//! ```text
//! usage: one_check <include-dir> <root file> <check name> [--limit N]
//! ```
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(dir), Some(root), Some(check)) = (args.first(), args.get(1), args.get(2)) else {
        eprintln!("usage: one_check <include-dir> <root file> <check name> [--limit N]");
        std::process::exit(2);
    };
    let limit: usize = args
        .iter()
        .position(|arg| arg == "--limit")
        .and_then(|at| args.get(at + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX);

    let root_path = std::path::PathBuf::from(dir).join(root);
    let mut session = cpp_code_analysis::Session::open(
        std::path::PathBuf::from(dir),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(std::path::Path::new(".")),
    );
    session.did_open(&root_path, &std::fs::read_to_string(&root_path).unwrap_or_default());
    session.index_everything();

    let paths: Vec<std::path::PathBuf> = session
        .index()
        .summaries()
        .map(|summary| summary.path.clone())
        .take(limit)
        .collect();

    let mut total = 0usize;
    for path in &paths {
        let Some(found) = session.diagnostics(path) else {
            continue;
        };
        for finding in found.checks.iter().filter(|finding| finding.check == check) {
            total += 1;
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            println!("  {name}:{}  {}", finding.range.start_offset, finding.message);

            // **The facts that mention this name**, which is where a reading defect shows: the check was handed a
            // fact, and what that fact says about kind, type and parameters is the whole of what it could know.
            if let Some(summary) = session.index().summary(path) {
                for fact in summary.declarations.iter().filter(|fact| fact.name == finding.name) {
                    println!(
                        "      fact: kind={:?} type={:?} returns={:?} params={:?} range={:?}..{:?}",
                        fact.kind,
                        fact.type_of,
                        fact.returns,
                        fact.parameters.len(),
                        fact.range.start_offset,
                        fact.range.end_offset()
                    );
                }
            }

            // And the line of the file it is about, which is what a person reads to judge it.
            if let Some(text) = std::fs::read_to_string(path).ok() {
                let line = text[..finding.range.start_offset.min(text.len())].matches('\n').count() + 1;
                if let Some(source) = text.lines().nth(line.saturating_sub(1)) {
                    println!("      source: {}", source.trim().chars().take(100).collect::<String>());
                }
            }
        }
    }

    println!("\n  {total} finding(s) from `{check}` over {} file(s)", paths.len());
}
