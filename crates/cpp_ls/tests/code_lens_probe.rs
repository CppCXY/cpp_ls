//! **What the code lens counts, what Find References counts, and what the rename rule counted** — measured on the
//! report's own files.
//!
//! The lens asks `references::places_of` and nothing else (see `handlers/code_lens.rs`), so its number is that
//! function's length; `name_level_offsets` is the rule it used to ask, kept here to show the two apart.
//!
//! ```text
//! $env:CPPLS_CODESTYLE_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_ls --test code_lens_probe -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use cpp_code_analysis::{
    DiskFiles, Known, OpenDocuments, ReferenceBudget, ReferenceKind, Session, SessionFiles,
    WatchFilter,
};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_CODESTYLE_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    named.join("CodeFormatLib").is_dir().then_some(named)
}

fn open(root: &Path, file: &Path, text: &str) -> Session<DiskFiles> {
    let mut session = Session::open(
        root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(root),
    );
    session.add_project_files([file.to_path_buf()]);
    session.did_open(file, text);
    session.catch_up(file);
    let _ = session.cook(file);
    session.want_cooked_reading(file);
    for _ in 0..12 {
        session.index_everything();
        if session.pending_work() == 0 {
            break;
        }
    }
    session
}

fn counted(found: &Known<cpp_code_analysis::SymbolReferences>) -> (usize, usize, usize) {
    match found {
        Known::Yes(found) => {
            let mut uses = 0usize;
            let mut declarations = 0usize;
            let mut definitions = 0usize;
            for file in &found.files {
                for reference in &file.references {
                    match reference.kind {
                        ReferenceKind::Use { .. } => uses += 1,
                        ReferenceKind::Declaration => declarations += 1,
                        ReferenceKind::Definition => definitions += 1,
                        _ => {}
                    }
                }
            }
            (uses, declarations, definitions)
        }
        _ => (0, 0, 0),
    }
}

fn compare(root: &Path, relative: &str) {
    let file = root.join(relative);
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("[probe] no {relative}");
        return;
    };
    let session = open(root, &file, &text);
    let plain = cpp_code_analysis::FileView::parse_with(
        session.files().held(&file).expect("held"),
        &cpp_parser::MacroEnvironment::default(),
    );

    eprintln!("[probe] ==== {relative} ====");
    let summary = session.index().summary(&file).expect("a summary");
    let functions: Vec<&cpp_code_analysis::DeclFact> = summary
        .declarations
        .iter()
        .filter(|fact| fact.kind == cpp_code_analysis::DeclKind::Function && !fact.local)
        .collect();

    // **A warm-up pass first**, so what is timed is a query over an index that has already answered once: the first
    // call of each reads files a project has not touched yet, and that is a startup cost rather than a per-file one.
    for fact in &functions {
        let _ = cpp_ls::handlers::references::places_of(&session, fact, file.clone(), false);
        let _ = cpp_ls::handlers::references::name_level_offsets(
            &session,
            &plain,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(fact.name_range.start_offset),
            false,
        );
    }

    let mut lenses = 0usize;
    let mut both = std::time::Duration::ZERO;
    let mut by_the_rename_rule = std::time::Duration::ZERO;
    for fact in &functions {
        let line = text[..fact.name_range.start_offset].matches('\n').count() + 1;

        // **The lens's own number**: `places_of` with the rule the lens passes.
        let started = std::time::Instant::now();
        let lens = match cpp_ls::handlers::references::places_of(
            &session,
            fact,
            file.clone(),
            false,
        ) {
            Known::Yes(places) => places.len(),
            Known::No | Known::Unknown(_) => {
                eprintln!(
                    "[probe] line {line:>4} name={:<24} lens says nothing (the query cannot name it)",
                    fact.name
                );
                continue;
            }
        };
        both += started.elapsed();
        lenses += 1;

        let (uses, declarations, definitions) =
            counted(&session.symbol_references_of(fact, file.clone(), ReferenceBudget::default()));
        // **The rename rule**, which is what the lens asked before: same declaration, same view, same rule as
        // `textDocument/references`' macro path.
        let started = std::time::Instant::now();
        let rename = cpp_ls::handlers::references::name_level_offsets(
            &session,
            &plain,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(fact.name_range.start_offset),
            false,
        )
        .len();
        by_the_rename_rule += started.elapsed();
        eprintln!(
            "[probe] line {line:>4} name={:<24} lens={lens:<4} find=[{uses} use, {declarations} decl, \
             {definitions} def]  rename-rule={rename}",
            fact.name
        );
    }
    eprintln!(
        "[probe] {lenses} lens(es) for the file: places_of {both:.2?}, the rename rule {by_the_rename_rule:.2?}"
    );
}

#[test]
#[ignore = "a probe: needs the external project"]
fn what_the_lens_counts_against_what_find_references_counts() {
    let Some(root) = project() else {
        eprintln!("[probe] the project is not here");
        return;
    };
    compare(&root, "LuaParser/include/LuaParser/Parse/LuaParseError.h");
    compare(&root, "LuaParser/src/Parse/Mark.cpp");
    compare(
        &root,
        "CodeFormatLib/include/CodeFormatLib/Diagnostic/DiagnosticBuilder.h",
    );
}
