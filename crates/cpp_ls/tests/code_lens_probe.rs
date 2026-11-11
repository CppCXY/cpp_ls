//! **What a code lens counts and what Find References counts, for the same declaration** — measured on the report's
//! own file.
//!
//! ```text
//! $env:CPPLS_CODESTYLE_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_ls --test code_lens_probe -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use cpp_code_analysis::file::coordinates::ReadingOffset;
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

fn counted(found: &Known<cpp_code_analysis::SymbolReferences>) -> String {
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
            format!("{uses} use, {declarations} decl, {definitions} def")
        }
        Known::No => "no".to_string(),
        Known::Unknown(reason) => format!("unknown({reason:?})"),
    }
}

fn described(defined: &Known<cpp_code_analysis::ProjectDefinitions>) -> String {
    match defined {
        Known::Yes(found) => found
            .found
            .iter()
            .map(|definition| {
                format!(
                    "{}@{}({})",
                    definition.fact.name,
                    definition.fact.name_range.start_offset,
                    definition.fact.scope.as_deref().unwrap_or("-")
                )
            })
            .collect::<Vec<_>>()
            .join(", "),
        Known::No => "no".to_string(),
        Known::Unknown(reason) => format!("unknown({reason:?})"),
    }
}

fn compare(root: &Path, relative: &str, needles: &[(&str, usize)]) {
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
    for fact in &summary.declarations {
        if fact.kind != cpp_code_analysis::DeclKind::Function || fact.local {
            continue;
        }
        let at = fact.name_range.start_offset;
        let lens = cpp_ls::handlers::references::name_level_offsets(
            &session,
            &plain,
            ReadingOffset::new(at),
            false,
        )
        .len();
        let defined = session.definitions(&plain, ReadingOffset::new(at));
        let by_symbol = session.symbol_references_of(fact, file.clone(), ReferenceBudget::default());
        let line = text[..at].matches('\n').count() + 1;
        eprintln!(
            "[probe] line {line:>4} name={:<22} lens={lens:<4} find=[{}] definitions={}",
            fact.name,
            counted(&by_symbol),
            described(&defined)
        );
    }

    for (needle, skip) in needles {
        let Some(at) = text.find(needle).map(|at| at + skip) else {
            continue;
        };
        let defined = session.definitions(&plain, ReadingOffset::new(at));
        let by_symbol =
            session.symbol_references(&plain, ReadingOffset::new(at), ReferenceBudget::default());
        let lens = cpp_ls::handlers::references::name_level_offsets(
            &session,
            &plain,
            ReadingOffset::new(at),
            false,
        )
        .len();
        let line = text[..at].matches('\n').count() + 1;
        eprintln!(
            "[probe] needle {needle:?} line {line}: definitions={} find=[{}] lens-term={lens}",
            described(&defined),
            counted(&by_symbol)
        );
    }
}

#[test]
#[ignore = "a probe: needs the external project"]
fn what_the_lens_counts_against_what_find_references_counts() {
    let Some(root) = project() else {
        eprintln!("[probe] the project is not here");
        return;
    };
    compare(
        &root,
        "LuaParser/include/LuaParser/Parse/LuaParseError.h",
        &[("class LuaParseError", "class ".len())],
    );
    compare(
        &root,
        "LuaParser/src/Parse/Mark.cpp",
        &[
            ("CompleteMarker::Marker(std", 0),
            ("CompleteMarker::CompleteMarker(std", "CompleteMarker::".len()),
            ("events[Pos].U", 0),
        ],
    );
}
