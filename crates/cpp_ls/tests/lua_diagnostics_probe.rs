//! **What the session says about a file the report calls "large-scale syntax errors"** — and from which reading.
//!
//! ```text
//! $env:CPPLS_LUA_PROJECT = "C:\Users\zc\Desktop\github\lua_rt\lua_src\lua-5.5.0"
//! cargo test --release -p cpp_ls --test lua_diagnostics_probe -- --ignored --nocapture
//! ```
//!
//! The report: opening Lua's own `ldebug.c` shows syntax errors at positions in **comments** (`** temporarily broken
//! while inserting…`, `** during a signal, under "reasonable"`). Two parses are clean — the file's own text and the
//! rendering the cook wrote (`cpp_dump --errors-only`: `errors=0` for both) — so the number the server publishes has
//! to come from somewhere else, and this probe prints every place it could come from:
//!
//! 1. `Session::diagnostics` — what a pull answers, with the reading it names;
//! 2. the index's cooked reading — its placed diagnostics and its `unplaced` count;
//! 3. the file's own tree — the parse the written reading would have had.

use std::path::{Path, PathBuf};

use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_LUA_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\github\lua_rt\lua_src\lua-5.5.0".to_string()),
    )
}

/// Every source the project has, so the include closure of the reported file is really indexed.
fn project_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.join("src")];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let entry = entry.path();
            if entry.is_dir() {
                pending.push(entry);
            } else if entry.extension().is_some_and(|it| it == "c" || it == "h") {
                files.push(entry);
            }
        }
    }
    files
}

fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut column = 1usize;
    for byte in text.bytes().take(offset.min(text.len())) {
        if byte == b'\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

#[test]
#[ignore = "a probe: opens the Lua sources and prints every diagnostics source"]
fn what_the_session_says_about_ldebug() {
    let root = project();
    let file = root.join("src/ldebug.c");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("skipped: {} is not here", file.display());
        return;
    };
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let files = project_files(&root);
    println!("{} project file(s)", files.len());
    session.add_project_files(files);
    session.did_open(&file, &text);
    // **The server's own order**: a pull arrives before the index is read, and `prepare_for_a_diagnostic` cooks the
    // file right there. Print what that cook produces, and what the *stored* reading says afterwards — a reading
    // filed from half a closure is one every later pull is served, because its key is the closure it was made from.
    println!(
        "[early] closure ready before the cook: {}",
        session.is_closure_ready(&file)
    );
    let early = session.cook(&file);
    println!("[early] cook -> {early:?}");
    if let Some(cooked) = session.index().cooked_reading(&file) {
        println!(
            "[early] cooked reading: {} declaration(s), {} placed diagnostic(s), {} unplaced",
            cooked.declarations.len(),
            cooked.diagnostics.len(),
            cooked.unplaced
        );
    }
    let started = std::time::Instant::now();
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 600 {
        session.advance(32);
        slices += 1;
    }
    println!(
        "pumped {slices} slice(s) in {:?}, {} pending, closure ready {}",
        started.elapsed(),
        session.pending_work(),
        session.is_closure_ready(&file)
    );

    // **What the composition holds now that the closure is ready**, and what a fresh render of it says — the two
    // halves that say whether the environment or the renderer is wrong.
    if let Some(materials) = session.cooking_materials(&file) {
        match &materials.configuration {
            Some(configuration) => println!(
                "[materials] unit {}, composed table {} name(s); getstr {}, cast {}, lua_assert {}",
                materials.unit.is_some(),
                configuration.table.len(),
                configuration.table.get("getstr").is_some(),
                configuration.table.get("cast").is_some(),
                configuration.table.get("lua_assert").is_some()
            ),
            None => println!(
                "[materials] unit {}, no composed table",
                materials.unit.is_some()
            ),
        }
        if let Some(rendered) = session.render_a_cooked(&materials) {
            let text = &rendered.indexed.rendered.text;
            println!(
                "[fresh] {} declaration(s), {} diagnostic(s); rendering {} byte(s), getstr {} time(s), cast {} time(s)",
                rendered.reading.declarations,
                rendered.reading.diagnostics,
                text.len(),
                text.matches("getstr").count(),
                text.matches("cast(").count()
            );
            if let Some(view) = session.view_of_the_file(&file) {
                let _ = view;
            }
        }
    }

    // **1. What a pull answers.**
    match session.diagnostics(&file) {
        Some(found) => {
            println!(
                "[diagnostics] reading {:?}, {} error(s), {} check(s), {} withheld, {} unplaced",
                found.reading,
                found.errors.len(),
                found.checks.len(),
                found.checks_withheld,
                found.unplaced
            );
            for error in found.errors.iter().take(40) {
                let (line, column) = line_col(&text, error.start);
                let around = text
                    .get(error.start..error.end.min(text.len()))
                    .unwrap_or("");
                println!(
                    "[diagnostics] {line}:{column} {}..{} [{}] {around:?}",
                    error.start, error.end, error.message
                );
            }
        }
        None => println!("[diagnostics] none"),
    }

    // **2. The index's cooked reading**, which is where the placed errors are filed.
    match session.index().cooked_reading(&file) {
        Some(cooked) => {
            println!(
                "[cooked] {} declaration(s), {} placed diagnostic(s), {} unplaced",
                cooked.declarations.len(),
                cooked.diagnostics.len(),
                cooked.unplaced
            );
            for error in cooked.diagnostics.iter().take(40) {
                let (line, column) = line_col(&text, error.range.start_offset);
                println!(
                    "[cooked] {line}:{column} {}..{} {}",
                    error.range.start_offset,
                    error.range.end_offset(),
                    error.message
                );
            }
        }
        None => println!("[cooked] the index holds no cooked reading for this file"),
    }

    // **3. The file's own parse** — the written reading, which the diagnostics no longer publish.
    match session.view_of_the_file(&file) {
        Some(view) => println!(
            "[written] {} error(s) in the file's own tree",
            view.tree.get_errors().len()
        ),
        None => println!("[written] no view"),
    }
}
