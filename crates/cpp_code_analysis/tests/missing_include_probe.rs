//! **`missing-include` on a real project** — how many it reports, and what it costs.
//!
//! The rule exists to be turned into a fix, and a rule of this kind lives or dies by its false-positive rate: five
//! squiggles a reader disagrees with and the channel is off for good. So the measurement is the feature's gate, and it
//! is run on a real project rather than a fixture:
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_code_analysis --test missing_include_probe -- --ignored --nocapture
//! ```
//!
//! What it prints: the findings per file (name, message), the total, and the time the pull took with the rule on.

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_REAL_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    named.is_dir().then_some(named)
}

#[test]
#[ignore = "indexes a real project; run with --ignored"]
fn what_missing_include_says_about_a_real_project() {
    let Some(root) = project() else {
        eprintln!("no project: set CPPLS_REAL_PROJECT");
        return;
    };
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    // **The project scan is the session's, and it lists the files the build describes.**
    let files = session.project_files().to_vec();
    println!("the project lists {} file(s)", files.len());
    let started = std::time::Instant::now();
    session.index_everything();
    println!("indexed in {:?}", started.elapsed());

    let mut files = 0usize;
    let mut total = 0usize;
    let mut slowest = std::time::Duration::ZERO;
    let mut slowest_file = String::new();
    let mut shown = 0usize;
    let project_files = session.project_files().to_vec();
    for path in project_files {
        // Only translation units: a header is checked too, but the reader's report is about `.cpp`s and they are the
        // expensive ones.
        let is_source = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "cpp" | "cc" | "cxx" | "c"));
        if !is_source {
            continue;
        }
        // **A pull needs a cooked reading** — the checks run over the rendering, so the file is cooked first, and
        // the cook is *outside* the timer: the number this probe reports is the pull, not the parse.
        let _ = session.cook(&path);
        let started = std::time::Instant::now();
        let Some(diagnostics) = session.diagnostics(&path) else {
            continue;
        };
        let elapsed = started.elapsed();
        if elapsed > slowest {
            slowest = elapsed;
            slowest_file = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
        }
        files += 1;
        let found: Vec<String> = diagnostics
            .checks
            .iter()
            .filter(|finding| {
                finding.code == cpp_code_analysis::sema::check::DiagnosticCode::MissingInclude
            })
            .map(|finding| format!("{}: {}", finding.name, finding.message))
            .collect();
        total += found.len();
        if !found.is_empty() && shown < 25 {
            shown += 1;
            println!(
                "{:<28} {}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                found.join(" | ")
            );
        }
    }
    println!(
        "{files} source file(s), {total} missing-include finding(s); slowest pull {slowest:?} ({slowest_file})"
    );
}
