//! **Where does a checks run spend its time?**
//!
//! `checks_probe` takes minutes over the standard library, and the question is which half owns it: the **indexing**
//! (which walks and parses a closure) or the **checks** (which are documented as not parsing anything at all —
//! *"Nothing is parsed here … asking for diagnostics is not a reason for a parse to happen"*). The two have
//! different fixes, so they are measured apart.
//!
//! ```text
//! usage: checks_timing <include-dir> <root file> [--limit N]
//! ```
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = args.first() else {
        eprintln!("usage: checks_timing <include-dir> <root file> [--limit N]");
        std::process::exit(2);
    };
    let root = std::path::PathBuf::from(dir).join(
        args.get(1)
            .cloned()
            .unwrap_or_else(|| "memory".to_string()),
    );
    let limit: usize = args
        .iter()
        .position(|arg| arg == "--limit")
        .and_then(|at| args.get(at + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX);

    let opening = std::time::Instant::now();
    let mut session = cpp_code_analysis::Session::open(
        std::path::PathBuf::from(dir),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(std::path::Path::new(".")),
    );
    println!("  session opened in {} ms", opening.elapsed().as_millis());

    // **What a language server actually does**, rather than what the probe did: one file is read, and its closure
    // follows. `index_everything` reads the *whole workspace*, which for a probe rooted at an include directory is
    // the entire standard library.
    let indexing = std::time::Instant::now();
    session.did_open(&root, &std::fs::read_to_string(&root).unwrap_or_default());
    session.index_everything();
    let indexed = indexing.elapsed();
    println!(
        "  `{}` and its closure indexed in {} ms ({} file(s) in the index)",
        root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        indexed.as_millis(),
        session.index().len()
    );

    // And then the question the checks ask, per file, with the two halves timed apart.
    let paths: Vec<std::path::PathBuf> = session
        .index()
        .summaries()
        .map(|summary| summary.path.clone())
        .take(limit)
        .collect();

    let mut parse = std::time::Duration::ZERO;
    let mut check = std::time::Duration::ZERO;
    let mut findings = 0usize;
    for path in &paths {
        // The parse, which `checks_about` needs a `FileView` for.
        let started = std::time::Instant::now();
        let _view = session.view(path);
        parse += started.elapsed();

        // …and the checks themselves.
        let started = std::time::Instant::now();
        if let Some(found) = session.diagnostics(path) {
            findings += found.checks.len();
        }
        check += started.elapsed();
    }

    println!(
        "  over {} file(s): **{} ms building views, {} ms running checks**, {findings} finding(s)",
        paths.len(),
        parse.as_millis(),
        check.as_millis()
    );
    println!("  (a view is the parse `checks_about` takes; the checks read it and the summary)");
}
