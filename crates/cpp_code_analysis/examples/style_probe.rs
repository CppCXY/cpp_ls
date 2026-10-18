//! **A census of the style rules over a real project** — counts by code, and a few examples each.
//!
//! The checks are tested on fixtures, and a fixture is a claim about a shape that somebody thought of. A census answers the
//! other question: how often each rule fires on code nobody wrote for it, and whether the first few findings are true. It is
//! the same method the diagnostic census used (see `docs/cold-start-cpu.md`), applied to the rules that were added since.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example style_probe -- E:\EmmyLuaCodeStyle --limit 3
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cpp_code_analysis::{
    DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = PathBuf::from(args.first().expect("a project root"));
    let limit: usize = args
        .iter()
        .position(|argument| argument == "--limit")
        .and_then(|at| args.get(at + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);

    let mut files: Vec<PathBuf> = Vec::new();
    walk(&root, &mut files, 0);
    files.retain(|path| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "cpp" | "cc" | "cxx" | "c++" | "h" | "hpp" | "hxx" | "hh" | "c" | "inl" | "ipp"
                )
            })
    });
    println!("{} file(s) under {}", files.len(), root.display());

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    // **The open first, and the project's files after it** — the contract `checks_probe` documents: opening a file is what
    // puts it, and the files its `#include`s reach, into the index at all, and a capture before the open is an empty list.
    if let Some(first) = files.first()
        && let Ok(text) = std::fs::read_to_string(first)
    {
        session.did_open(first, &text);
    }
    session.add_project_files(files.clone());
    session.index_everything();

    let (mut viewed, mut answered) = (0usize, 0usize);
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut examples: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for file in &files {
        // **A file is diagnosed from its *rendering***, and a rendering is built because something asked for one — see
        // `tests/completion.rs`'s note. Without the ask, `diagnostics` answers `None` for every file, which is how the first
        // run of this probe printed an empty census over 1216 files.
        session.want_cooked_reading(file);
        if session.view(file).is_none() {
            // The ask queues the work; the pump is what does it. One step per file is enough — the pump is incremental by
            // design, and a probe that drained it eagerly would be measuring the index rather than the rules.
            session.advance(1);
        }
        if session.view(file).is_some() {
            viewed += 1;
        }
        let Some(found) = session.diagnostics(file) else {
            continue;
        };
        answered += 1;
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for finding in &found.checks {
            *counts.entry(finding.code.name()).or_default() += 1;
            let kept = examples.entry(finding.code.name()).or_default();
            if kept.len() < limit {
                let start = usize::from(finding.range.start_offset);
                let end = usize::from(finding.range.end_offset()).min(text.len());
                let line = text
                    .get(start..end)
                    .unwrap_or_default()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                kept.push(format!(
                    "{}: `{}`",
                    file.file_name().and_then(|name| name.to_str()).unwrap_or("?"),
                    line
                ));
            }
        }
    }

    println!("\nviewed {viewed} of {}, answered {answered}", files.len());
    let mut ordered: Vec<_> = counts.into_iter().collect();
    ordered.sort_by(|left, right| right.1.cmp(&left.1));
    println!("\n{:<28} {:>6}", "code", "count");
    for (code, count) in &ordered {
        println!("{code:<28} {count:>6}");
    }
    for (code, found) in &examples {
        println!("\n{code} — examples:");
        for example in found {
            println!("  {example}");
        }
    }
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
            // The usual build and dependency directories: not the project's own code, and slow to walk.
            if matches!(name, ".git" | "build" | "out" | "node_modules" | ".vs" | "CMakeFiles") {
                continue;
            }
            walk(&path, out, depth + 1);
        } else {
            out.push(path);
        }
    }
}