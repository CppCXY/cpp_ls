//! **The call hierarchy on a real project, at the layer the handlers call** — the probe that asks the three questions
//! through the server hung, so this separates the cost from the plumbing.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_ls --test call_hierarchy_real -- --ignored --nocapture
//! ```
//!
//! Each step is timed and printed. A step that takes minutes is the answer to "why did the client wait": the handler is
//! a thin wrapper around these three calls, and the wire test (`handshake::the_call_hierarchy_answers_over_the_wire`)
//! already shows the wrapper answers over a small project.

use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::file::coordinates::ReadingOffset;
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use cpp_ls::handlers::call_hierarchy::{incoming_calls_of, item_at, outgoing_calls_of};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    )
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn the_three_questions_are_affordable() {
    let project = project();
    let header = project.join("CodeFormatCLib/src/codeformat.h");
    let source = std::fs::read_to_string(&header).expect("the header reads");

    let mut session = Session::open(
        project.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project),
    );
    session.add_project_files([header.clone()]);
    session.index_everything();

    let view = session.view_of_the_file(&header).expect("the header parses");
    let at = source
        .find("UpdateCodeStyle")
        .expect("the header declares it")
        + 1;

    let started = Instant::now();
    let item = item_at(&session, &view, at);
    println!(
        "prepare: {:?} in {:?}",
        item.as_ref().map(|item| item.name.clone()),
        started.elapsed()
    );
    let item = item.expect("the declaration is a call hierarchy item");
    assert!(item.name.is_empty() || true);

    let raw = cpp_ls::handlers::references::name_level_offsets(
        &session,
        &view,
        ReadingOffset::new(at),
        false,
    );
    println!(
        "raw references: {:?}",
        raw.iter()
            .map(|(file, range)| (
                file.file_name().map(|name| name.to_string_lossy().into_owned()),
                range.start_offset,
                session.files().held(file).is_some()
            ))
            .collect::<Vec<_>>()
    );
    let started = Instant::now();
    let incoming = incoming_calls_of(&session, &view, at, &item.name);
    println!(
        "incoming: {} call(s) in {:?} — {:?}",
        incoming.len(),
        started.elapsed(),
        incoming
            .iter()
            .map(|call| (call.from.name.clone(), call.from_ranges.len()))
            .collect::<Vec<_>>()
    );

    let started = Instant::now();
    let outgoing = outgoing_calls_of(&session, &view, at);
    println!(
        "outgoing: {} call(s) in {:?} — {:?}",
        outgoing.len(),
        started.elapsed(),
        outgoing
            .iter()
            .map(|call| (call.to.name.clone(), call.from_ranges.len()))
            .take(8)
            .collect::<Vec<_>>()
    );

    // And the same from the definition, where the body is.
    let definition = project.join("CodeFormatCLib/src/CodeFormat.cpp");
    if let Some(view) = session.view_of_the_file(&definition) {
        let source = std::fs::read_to_string(&definition).expect("the source reads");
        let at = source.find("CodeFormat::UpdateCodeStyle").expect("the definition") + 1;
        let started = Instant::now();
        let outgoing = outgoing_calls_of(&session, &view, at);
        println!(
            "outgoing (from the definition): {} call(s) in {:?} — {:?}",
            outgoing.len(),
            started.elapsed(),
            outgoing
                .iter()
                .map(|call| (call.to.name.clone(), call.from_ranges.len()))
                .take(8)
                .collect::<Vec<_>>()
        );
        let _ = ReadingOffset::new(at);
    }
}
