//! **What a file's diagnostics cost**, per file, over a sample of a real project.
//!
//! `diagnose_everything` said 445 ms/file over 1191 files; this prints the distribution, which is the number a fix has to
//! move. The checks are timed as a whole here — splitting them per rule needs the check registry to run one rule at a time,
//! which is the next probe (see `docs/checks.md`).
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example check_costs -- E:\EmmyLuaCodeStyle --limit 50
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = PathBuf::from(args.first().expect("a project root"));
    let limit: usize = args
        .iter()
        .position(|argument| argument == "--limit")
        .and_then(|at| args.get(at + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(50);

    let mut files: Vec<PathBuf> = Vec::new();
    walk(&root, &mut files, 0);
    files.retain(|path| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "cpp" | "cc" | "cxx" | "h" | "hpp" | "hxx" | "hh" | "c" | "inl" | "ipp"
                )
            })
    });
    files.truncate(limit);
    println!("{} file(s) under {}", files.len(), root.display());

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(files.clone());
    let indexed = Instant::now();
    session.index_everything();
    println!("indexed in {:.1} s", indexed.elapsed().as_secs_f64());

    // Every rendering first, then one drain — the usage `diagnose_everything` documents.
    session.want_everything_cooked();
    let mut waves = 0usize;
    while session.pending() > 0 && waves < 100_000 {
        if session.advance(64).is_empty() {
            break;
        }
        waves += 1;
    }
    loop {
        let claimed = session.claim_cooking(64);
        if claimed.is_empty() {
            break;
        }
        let materials = session.materials_for(claimed);
        let rendered = session.render_them(materials);
        session.commit_them(rendered);
    }

    let mut took: Vec<(u128, String)> = Vec::new();
    let start = Instant::now();
    for file in &files {
        let at = Instant::now();
        let _ = session.diagnostics(file);
        took.push((
            at.elapsed().as_micros(),
            file.file_name().and_then(|name| name.to_str()).unwrap_or("?").to_string(),
        ));
    }
    let total = start.elapsed();

    took.sort_by(|left, right| right.0.cmp(&left.0));
    println!(
        "\ntotal {:.2} s over {} file(s) — {:.1} ms/file",
        total.as_secs_f64(),
        files.len(),
        total.as_secs_f64() * 1000.0 / files.len() as f64
    );
    println!("\nslowest:");
    for (micros, name) in took.iter().take(10) {
        println!("  {name:<40} {:>8.1} ms", *micros as f64 / 1000.0);
    }
    let median = took.get(took.len() / 2).map(|(micros, _)| *micros).unwrap_or(0);
    let fastest = took.last().map(|(micros, _)| *micros).unwrap_or(0);
    println!(
        "\nmedian {:.1} ms   fastest {:.1} ms   slowest {:.1} ms",
        median as f64 / 1000.0,
        fastest as f64 / 1000.0,
        took.first().map(|(micros, _)| *micros as f64 / 1000.0).unwrap_or(0.0)
    );
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 24 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
            if matches!(name, ".git" | "build" | "out" | "node_modules" | ".vs" | "CMakeFiles") {
                continue;
            }
            walk(&path, out, depth + 1);
        } else {
            out.push(path);
        }
    }
}