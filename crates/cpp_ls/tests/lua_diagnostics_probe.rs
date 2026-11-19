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

/// **Every source of the project, as a pull would report it** — the usability picture, not one file's story.
///
/// The report that started this probe was about one file; the question a reader actually has is "how much of this
/// project does the analysis get wrong", and the answer is a list. Each `.c` is opened, cooked and diagnosed with the
/// project's own configuration (`compile_commands.json` at the root, when it is there), and only files with something
/// to say are printed.
#[test]
#[ignore = "a probe: opens every Lua source and prints its diagnostics"]
fn what_every_file_says() {
    let root = project();
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let files = project_files(&root);
    session.add_project_files(files.clone());
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 600 {
        session.advance(32);
        slices += 1;
    }
    println!("[scan] {} file(s), {slices} slice(s)", files.len());

    let mut with_errors = 0usize;
    let mut with_checks = 0usize;
    let mut total_errors = 0usize;
    let mut total_checks = 0usize;
    let mut sources: Vec<_> = files
        .iter()
        .filter(|path| path.extension().is_some_and(|it| it == "c"))
        .collect();
    sources.sort();
    for path in sources {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        session.did_open(path, &text);
        // **The server's own order**: a pull prepares the file, the pump cooks the closure, and the diagnostics are
        // then answered from the reading it filed. Cooking before the pump reads a file whose unit is still
        // provisional and answers `None` — which is what the first version of this scan measured.
        let mut waited = 0usize;
        while session.pending_work() > 0 && waited < 600 {
            session.advance(32);
            waited += 1;
        }
        if session.diagnostics(path).is_none() && session.cook(path).is_none() {
            println!("== {} : no reading", path.display());
            continue;
        }
        let Some(found) = session.diagnostics(path) else {
            println!("== {} : no reading", path.display());
            continue;
        };
        total_errors += found.errors.len();
        total_checks += found.checks.len();
        if found.errors.is_empty() && found.checks.is_empty() {
            continue;
        }
        if !found.errors.is_empty() {
            with_errors += 1;
        }
        if !found.checks.is_empty() {
            with_checks += 1;
        }
        println!(
            "== {} : {} error(s), {} check(s), reading {:?}, {} withheld",
            path.file_name().unwrap_or_default().to_string_lossy(),
            found.errors.len(),
            found.checks.len(),
            found.reading,
            found.checks_withheld
        );
        for error in &found.errors {
            let (line, column) = line_col(&text, error.start);
            let around = text
                .get(error.start..error.end.min(text.len()))
                .unwrap_or("");
            println!("   {line}:{column}  {}  [{around:?}]", error.message);
        }
        for finding in &found.checks {
            let (line, column) = line_col(&text, finding.range.start_offset);
            println!(
                "   {line}:{column}  check {}  {}",
                finding.code.name(),
                finding.message
            );
        }
    }
    println!(
        "[scan] {with_errors} file(s) with errors, {with_checks} with checks; {total_errors} error(s), {total_checks} check(s) in total"
    );
}
/// **What the rendering looks like where a file reports parse errors** — the half a span in the file cannot show.
///
/// `lvm.c` reports seven errors at one position (`setnilvalue(s2v(L->top.p++))`), and the question is whether the
/// *rendering* is malformed there (a macro expansion or a join rule) or whether the parser refused text that is
/// well-formed. The file's own text is printed beside the rendering so the two can be compared at the same place.
#[test]
#[ignore = "a probe: prints the rendering at every parse error of one file"]
fn what_the_rendering_looks_like_where_the_errors_are() {
    let root = project();
    let file = root.join("src/lvm.c");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("skipped: {} is not here", file.display());
        return;
    };
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(project_files(&root));
    session.did_open(&file, &text);
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 600 {
        session.advance(32);
        slices += 1;
    }
    let _ = session.cook(&file);
    let Some(found) = session.diagnostics(&file) else {
        println!("no reading");
        return;
    };
    let view = session.view(&file).expect("the cooked file has a view");
    let rendering = &view.source;
    println!(
        "{} error(s), rendering {} byte(s)",
        found.errors.len(),
        rendering.len()
    );
    for error in &found.errors {
        let (line, column) = line_col(&text, error.start);
        let written = text
            .get(error.start..error.end.min(text.len()))
            .unwrap_or("");
        println!(
            "--- {line}:{column} {} [written {written:?}]",
            error.message
        );
        let reading = view.reading_offset_of(
            cpp_code_analysis::file::coordinates::FileOffset::new(error.start),
        );
        match reading {
            Some(reading) => {
                let at = reading.get();
                let from = at.saturating_sub(120);
                let to = (at + 60).min(rendering.len());
                println!("    rendering[{from}..{to}]: {:?}", &rendering[from..to]);
            }
            None => println!("    the error is at a place the rendering does not contain"),
        }
    }
}

/// **An `#include` behind a condition, the region recorded for it, and whether the index can decide it.**
///
/// Lua 5.5.0's `lua.c` writes `<readline/readline.h>` under `#if defined(LUA_USE_READLINE)` and `<dlfcn.h>` under
/// `#if defined(LUA_USE_DLOPEN) && defined(LUA_READLINELIB)`; a Windows build reads neither, and the rule asked
/// `the_guard_is_taken` — which answered **true** ("keep the finding") while the environment was incomplete.
///
/// The output here is the whole chain: the region *is* the right `#if`, the verdict is the environment's answer, and
/// the toolchain's macro count is why. Measured with a `cl.exe` database on this machine: 88 macros, none of them the
/// compiler's **own dump**, so `the_compilation_is_known` is false and every condition is `Unknown` — the rule keeps
/// its finding, as documented. With a `gcc` database (a compiler the analysis can actually run) the same three
/// includes answer `Inactive` and the findings are gone; see `what_every_file_says`.
#[test]
#[ignore = "a probe: prints one include's guard, the index's verdict and the toolchain's macro count"]
fn why_the_readline_includes_are_reported() {
    let root = project();
    let file = root.join("src/lua.c");
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(project_files(&root));
    let text = std::fs::read_to_string(&file).expect("lua.c is in the project");
    session.did_open(&file, &text);
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 600 {
        session.advance(32);
        slices += 1;
    }
    let _ = session.cook(&file);
    println!(
        "[guard] toolchain present {} macro(s) {}",
        session.toolchain().is_some(),
        session
            .toolchain()
            .map(|tool| tool.macros().len())
            .unwrap_or(0)
    );
    let Some(summary) = session.index().summary(&file) else {
        println!("[guard] no summary");
        return;
    };
    for include in &summary.includes {
        if !include.spelling.contains("readline") && !include.spelling.contains("dlfcn") {
            continue;
        }
        if let cpp_code_analysis::FactGuard::Region(index) = include.guard
            && let Some(region) = summary.guards.regions.get(index as usize)
        {
            let start = region.start_offset;
            let end = region.end_offset().min(text.len());
            println!(
                "[guard] region {index} = {:?}",
                text[start..end].lines().next().unwrap_or_default()
            );
        }
        println!(
            "[guard] {:?} guard {:?} taken {} resolved {}",
            include.spelling,
            include.guard,
            session
                .index()
                .the_guard_is_taken(&file, include.guard, include.range.start_offset),
            include.resolved.is_some()
        );
    }
}
