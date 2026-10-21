//! **Every finding the check layer publishes over a whole project** — a baseline that a performance change must not
//! move.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example diagnostics_audit -- <root> [--files N] [--time]
//! ```
//!
//! The output is one line per finding, in a stable order (`path:line:col code message`), so two runs can be diffed:
//! a change that makes the layer fast and moves one finding is a change that made the layer wrong. `--time` prints
//! the per-rule cost instead (the same numbers as `CPPLS_TIME_CHECKS=1`, summed over the project).
//!
//! # Why the whole project and not one file
//!
//! A check reads the index as well as the file, so the findings about one file depend on which other files were
//! cooked. A baseline taken file by file would therefore be a baseline of a different run each time; this cooks
//! everything it can before it asks, which is the state a session is in once the pump has settled.

use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(root) = args.first() else {
        eprintln!("usage: diagnostics_audit <root> [--files N] [--time]");
        std::process::exit(2);
    };
    let root = PathBuf::from(root);
    let limit: usize = args
        .iter()
        .position(|arg| arg == "--files")
        .and_then(|at| args.get(at + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX);
    let timing = args.iter().any(|arg| arg == "--time");
    let only: Option<String> = args
        .iter()
        .position(|arg| arg == "--match")
        .and_then(|at| args.get(at + 1))
        .map(|value| value.replace('\\', "/").to_lowercase());

    let mut session = cpp_code_analysis::Session::open(
        root.clone(),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(&root),
    );

    // **The project's own files**, not the standard library's and not a vendored tree: a check reads a file's
    // *reading*, and the files a reader edits are the ones this project owns. The closure is indexed either way, so
    // the knowledge a check reads is the same.
    session.add_project_files(session.project_files().to_vec());
    let started = std::time::Instant::now();
    session.index_everything();
    eprintln!("indexed in {:?}", started.elapsed());

    let wanted = root.display().to_string().replace('\\', "/").to_lowercase();
    let files: Vec<PathBuf> = session
        .index()
        .summaries()
        .map(|summary| summary.path.clone())
        .filter(|path| {
            let spelled = path.display().to_string().replace('\\', "/").to_lowercase();
            spelled.starts_with(&wanted)
                && !spelled.contains("/3rd/")
                && only
                    .as_ref()
                    .is_none_or(|needle| spelled.contains(needle.as_str()))
        })
        .filter(|path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("cpp" | "cc" | "cxx" | "c" | "h" | "hpp" | "hh" | "hxx" | "inl" | "ipp")
            )
        })
        .take(limit)
        .collect();
    eprintln!("{} project file(s) of {} indexed", files.len(), session.index().len());

    // **Everything cooked**, because `diagnostics` answers from a cooked reading: a file with none has no answer at
    // all, and a baseline that skipped those files would not be a baseline of the check layer.
    let started = std::time::Instant::now();
    let mut cooked = 0usize;
    while session.pending_work() > 0 {
        session.advance(64);
        let claimed = session.claim_cooking(64);
        if claimed.is_empty() {
            break;
        }
        let materials = session.materials_for(claimed);
        let rendered = session.render_them(materials);
        cooked += session.commit_them(rendered).len();
    }
    session.want_everything_cooked();
    while session.pending_work() > 0 {
        session.advance(64);
        let claimed = session.claim_cooking(64);
        if claimed.is_empty() {
            break;
        }
        let materials = session.materials_for(claimed);
        let rendered = session.render_them(materials);
        cooked += session.commit_them(rendered).len();
    }
    eprintln!("cooked {cooked} file(s) in {:?}", started.elapsed());

    let mut findings = Vec::new();
    let mut per_file: BTreeMap<String, u128> = BTreeMap::new();
    let started = std::time::Instant::now();
    for path in &files {
        let one = std::time::Instant::now();
        let Some(diagnostics) = session.diagnostics(path) else {
            findings.push(format!("{}: NO READING", path.display()));
            continue;
        };
        per_file.insert(
            path.display().to_string(),
            one.elapsed().as_millis(),
        );
        for finding in &diagnostics.checks {
            let line = line_of(session.text(path).unwrap_or_default().as_str(), finding.range.start_offset);
            findings.push(format!(
                "{}:{}:{} {} {}",
                path.display(),
                line.0,
                line.1,
                finding.code.name(),
                finding.message
            ));
        }
    }
    let took = started.elapsed();

    findings.sort();
    for line in &findings {
        println!("{line}");
    }

    if timing {
        eprintln!("\nper file, slowest first:");
        let mut rows: Vec<(String, u128)> = per_file.into_iter().collect();
        rows.sort_by_key(|(_, ms)| std::cmp::Reverse(*ms));
        for (path, ms) in rows.iter().take(20) {
            eprintln!("  {ms:>7} ms  {path}");
        }
        eprintln!(
            "  {:>7} ms  all {} file(s), {} finding(s)",
            took.as_millis(),
            files.len(),
            findings.len()
        );
    }
}

/// The 1-based `(line, column)` an offset is at, by counting newlines — the probe has no line index of its own.
fn line_of(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or_default().chars().count() + 1;
    (line, column)
}

