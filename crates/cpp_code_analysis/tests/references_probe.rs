//! **Why a reference count is zero** — the counters of one query, on the file the report came from.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test references_probe -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use cpp_code_analysis::{
    DiskFiles, Known, OpenDocuments, ReferenceBudget, ReferenceKind, Session, SessionFiles,
    WatchFilter,
};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_CODESTYLE_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\github\EmmyLuaCodeStyle"));
    named
        .join("CodeFormatCore")
        .join("src")
        .is_dir()
        .then_some(named)
}

#[test]
#[ignore = "a probe: needs the external project"]
fn what_one_reference_query_actually_looked_at() {
    let Some(root) = project() else {
        eprintln!("[probe] the project is not here");
        return;
    };
    let header = root.join("CodeFormatCore/include/CodeFormatCore/Diagnostic/DiagnosticBuilder.h");
    let source = std::fs::read_to_string(&header).expect("the header");
    // `FormatState &GetState();` — the declaration the lens counted 0 for.
    let Some(at) = source.find("GetState()") else {
        eprintln!("[probe] `GetState` is not in the header");
        return;
    };

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([header.clone()]);
    session.did_open(&header, &source);
    session.catch_up(&header);
    let _ = session.cook(&header);
    session.want_cooked_reading(&header);
    for _ in 0..12 {
        session.index_everything();
        if session.pending_work() == 0 {
            break;
        }
    }

    // **The fact, which is what the lens holds** — the file offset in a summary is not a rendering offset, and asking
    // with one is the defect being measured.
    let fact = session
        .index()
        .summary(&header)
        .and_then(|summary| {
            summary
                .declarations
                .iter()
                .find(|fact| {
                    fact.name_range.start_offset <= at && at < fact.name_range.end_offset()
                })
                .cloned()
        })
        .expect("the declaration");
    let symbol = cpp_code_analysis::SymbolToFind::of(&fact, header.clone());
    eprintln!(
        "[probe] fact.name={:?} symbol.name={:?} qualified={:?} last_segment={:?} declared_in={:?}",
        fact.name, symbol.name, symbol.qualified, symbol.last_segment, symbol.declared_in
    );
    if let Some(summary) = session.index().summary(&header) {
        eprintln!(
            "[probe] the header's own filter says {:?} for {:?} (filter {} bytes)",
            cpp_code_analysis::summary::use_filter_might_contain(
                &summary.use_filter,
                &symbol.last_segment
            ),
            symbol.last_segment,
            summary.use_filter.len()
        );
    }
    let started = std::time::Instant::now();
    let found = session.symbol_references_of(&fact, header.clone(), ReferenceBudget::default());
    let elapsed = started.elapsed();
    match &found {
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
            eprintln!(
                "[probe] GetState: {uses} use(s), {declarations} declaration(s), {definitions} definition(s); \
                 looked_at {} file(s), {} without the name, {} not looked at, {} unreadable; {:.2?}",
                found.looked_at,
                found.without_the_name,
                found.not_looked_at,
                found.unreadable.len(),
                elapsed,
            );
            for file in found.files.iter().take(6) {
                eprintln!(
                    "[probe]     {} : {} occurrence(s)",
                    file.file.display(),
                    file.references.len()
                );
            }
        }
        Known::Unknown(reason) => eprintln!("[probe] GetState: unknown — {reason:?}"),
        Known::No => eprintln!("[probe] GetState: no"),
    }
}
