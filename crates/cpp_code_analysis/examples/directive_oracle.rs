//! **Do the directives alone agree with the parsed summary?** — the oracle for indexing without a parse.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example directive_oracle -- <workspace root> <a file in it>
//! ```
//!
//! # The question
//!
//! A cold start parses each file *before* it knows the macro environment the file is read under, and repairs the
//! reading with a second pass. The way out (`docs/exact-index-design.md` §2.2) is to walk each translation unit's
//! timeline **first** — from what a file *directs*, which needs no parse — and then parse every file once with the
//! environment that walk gives it.
//!
//! That is only sound if the directive reading and the parsed summary **agree about everything a timeline walk
//! reads**: `macros`, `includes`, and `guards` (checked by grepping `walk_one_file` for the fields it touches —
//! those three and nothing else). This probe compares them for every file of a real project and prints where they
//! differ.
//!
//! # What a mismatch would mean
//!
//! Not "the plan is wrong" but "the directive summary is missing something the parse adds": a `#define` the parser
//! recovered from, an include that resolved differently, a guard the token rule reads differently from the tree
//! rule. Each is a specific defect with a fix, and the probe prints the file and the field.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(root), Some(seed)) = (args.first(), args.get(1)) else {
        eprintln!("usage: directive_oracle <workspace root> <a real file in it>");
        std::process::exit(2);
    };
    let root = PathBuf::from(root);
    let seed = PathBuf::from(seed);

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
    files.sort();
    files.dedup();
    println!("{} file(s) under {}", files.len(), root.display());

    let opened = Instant::now();
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    if let Ok(text) = std::fs::read_to_string(&seed) {
        session.did_open(&seed, &text);
    }
    session.add_project_files(files);
    println!("open + queue   {:>8.0} ms", opened.elapsed().as_secs_f64() * 1e3);

    let indexed = Instant::now();
    session.index_everything();
    println!(
        "indexed {} file(s) in {:.1} s",
        session.index().len(),
        indexed.elapsed().as_secs_f64()
    );

    let paths: Vec<PathBuf> = session
        .index()
        .summaries()
        .map(|summary| summary.path.clone())
        .collect();

    let started = Instant::now();
    let mut compared = 0usize;
    let mut unreadable = 0usize;
    let mut disagreeing: Vec<(PathBuf, String)> = Vec::new();
    let mut branch_metadata = 0usize;
    let mut bytes = 0usize;

    for path in &paths {
        let Some(parsed) = session.parsed_summary(path) else {
            continue;
        };
        let Some(directives) = session.directive_summary(path) else {
            unreadable += 1;
            continue;
        };
        compared += 1;
        bytes += std::fs::metadata(path).map(|it| it.len() as usize).unwrap_or(0);

        if directives.macros != parsed.macros {
            disagreeing.push((path.clone(), macros_differ(&directives.macros, &parsed.macros)));
        }
        if directives.includes != parsed.includes {
            disagreeing.push((path.clone(), includes_differ(&directives.includes, &parsed.includes)));
        }
        // **The two guard fields a timeline walk reads**, and the two it does not.
        //
        // `walk_one_file` reads `guards.visit_once` and nothing else out of `guards` (the rest of the struct is for
        // the *visibility* layer, which reads a summary that was built from a parse). `own_guard` is compared anyway
        // because it is the one field whose tree-based reading this replaces — an equal value here is the proof that
        // the token rule agrees.
        //
        // `regions` and `conditionals` are **not** compared: they depend on the *seed* — the index's macro table at
        // the moment the preprocessing ran — and this probe builds a directive summary after the whole project is
        // indexed while the parse built its own mid-index. Measured: the same counts, different branch metadata, in
        // exactly the files whose regions a later seed decides differently. The env-first driver builds them before
        // the parse, with the seed the parse would have had, so the question does not arise there.
        if directives.guards.visit_once != parsed.guards.visit_once {
            disagreeing.push((
                path.clone(),
                format!(
                    "visit_once: {} vs {}",
                    directives.guards.visit_once, parsed.guards.visit_once
                ),
            ));
        }
        if directives.guards.own_guard != parsed.guards.own_guard {
            disagreeing.push((
                path.clone(),
                format!(
                    "own_guard: {:?} vs {:?}",
                    directives.guards.own_guard, parsed.guards.own_guard
                ),
            ));
        }
        if directives.guards.regions != parsed.guards.regions
            || directives.guards.conditionals != parsed.guards.conditionals
        {
            branch_metadata += 1;
        }
    }

    let took = started.elapsed();
    println!(
        "\ndirective summaries: {compared} file(s), {:.1} MB, in {:.2} s ({:.2} ms per file, {:.0} MB/s)",
        bytes as f64 / 1e6,
        took.as_secs_f64(),
        took.as_secs_f64() * 1e3 / compared.max(1) as f64,
        bytes as f64 / 1e6 / took.as_secs_f64().max(1e-9)
    );
    println!("unreadable: {unreadable}");
    println!(
        "branch metadata (regions/conditionals) differs in {branch_metadata} file(s) — seed-dependent, not read by the walk"
    );
    println!(
        "{} disagreement(s) over {} file(s)",
        disagreeing.len(),
        compared
    );
    for (path, what) in disagreeing.iter().take(20) {
        println!("  {}: {what}", path.display());
    }
}

/// The first difference between two macro fact lists, named so that a failure says what to look at.
fn macros_differ(left: &[cpp_code_analysis::MacroFact], right: &[cpp_code_analysis::MacroFact]) -> String {
    if left.len() != right.len() {
        return format!("macros: {} vs {} fact(s)", left.len(), right.len());
    }
    for (index, (mine, theirs)) in left.iter().zip(right).enumerate() {
        if mine != theirs {
            // The whole fact when the interesting field is one the short form does not print: `bitmap.c` differs in
            // a field the name/offset/kind triple did not show.
            return format!("macros[{index}]: {mine:?} vs {theirs:?}");
        }
    }
    "macros: equal under comparison".to_string()
}

fn includes_differ(
    left: &[cpp_code_analysis::IncludeFact],
    right: &[cpp_code_analysis::IncludeFact],
) -> String {
    if left.len() != right.len() {
        let mine: Vec<&str> = left.iter().map(|fact| fact.spelling.as_str()).collect();
        let theirs: Vec<&str> = right.iter().map(|fact| fact.spelling.as_str()).collect();
        return format!("includes: {} vs {} fact(s): {mine:?} vs {theirs:?}", left.len(), right.len());
    }
    for (index, (mine, theirs)) in left.iter().zip(right).enumerate() {
        if mine != theirs {
            return format!(
                "includes[{index}]: `{}` -> {:?} vs `{}` -> {:?}",
                mine.spelling, mine.resolved, theirs.spelling, theirs.resolved
            );
        }
    }
    "includes: equal under comparison".to_string()
}

fn guards_differ(
    left: &cpp_code_analysis::SummaryGuards,
    right: &cpp_code_analysis::SummaryGuards,
) -> String {
    // Which *field* differs, not just the counts: a region list of the same length can differ in one range, and the
    // first version of this printed two identical-looking lines for every mismatch.
    let fields: [(&str, bool); 4] = [
        ("regions", left.regions == right.regions),
        ("conditionals", left.conditionals == right.conditionals),
        ("own_guard", left.own_guard == right.own_guard),
        ("visit_once", left.visit_once == right.visit_once),
    ];
    let differing: Vec<&str> = fields
        .iter()
        .filter(|(_, same)| !same)
        .map(|(name, _)| *name)
        .collect();
    format!(
        "guards differ in {differing:?} (regions {} vs {}, conditionals {} vs {}, own_guard {:?} vs {:?})",
        left.regions.len(),
        right.regions.len(),
        left.conditionals.len(),
        right.conditionals.len(),
        left.own_guard,
        right.own_guard
    )
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>, depth: usize) {
    if depth > 16 || files.len() > 20_000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "build" || name == "Build" || name == "out" {
                continue;
            }
            walk(&path, files, depth + 1);
        } else if kind.is_file() {
            files.push(path);
        }
    }
}
