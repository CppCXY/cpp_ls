//! **What the rendering of a `_STD_BEGIN` file actually says** — the fixture behind
//! `session::tests::a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session`, asked of the rendering
//! rather than of the raw text.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test rendering_scope -- --nocapture
//! ```
//!
//! The session test asserts that `std::Widget` is found and that the summary records the body that opened the scope.
//! Before that can be true of a *rendering* reading, the rendering has to contain `namespace std {` — this prints
//! whether it does, and what the facts built from it say.

use std::path::PathBuf;

use cpp_code_analysis::{CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

#[test]
fn a_scope_opened_by_another_files_body_is_in_the_rendering() {
    let files = MemoryFiles::new()
        .with_file(
            "/p/open.h",
            "#pragma once\n#define _STD_BEGIN namespace std {\n#define _STD_END }\n",
        )
        .with_file(
            "/p/widget.h",
            "#include \"open.h\"\n_STD_BEGIN\nstruct Widget {\n    int size;\n};\n_STD_END\n",
        )
        .with_file("/p/main.cpp", "#include \"widget.h\"\nint f() { std::Widget w; return w.size; }\n");
    let mut session = Session::with_config(
        "/p",
        SessionFiles::new(OpenDocuments::new(), files),
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );

    session.add_project_files([
        PathBuf::from("/p/open.h"),
        PathBuf::from("/p/widget.h"),
        PathBuf::from("/p/main.cpp"),
    ]);
    session.index_everything();
    session.cook("/p/widget.h").expect("widget.h cooks");

    let rendered = session
        .rendering_of("/p/widget.h")
        .expect("widget.h has a rendering");
    println!("--- rendering of widget.h ---\n{}", rendered.text);

    let summary = session
        .index()
        .summary(std::path::Path::new("/p/widget.h"))
        .expect("indexed");
    let widget = summary
        .declarations
        .iter()
        .find(|fact| fact.name == "Widget")
        .expect("Widget is a fact");
    println!(
        "Widget: scope {:?}, guard {:?}",
        widget.scope, widget.guard
    );
    println!(
        "macro_readings: {}",
        summary.macro_readings.len()
    );
    // **And the same file read the way the index is supposed to read it**: the rendering, through the indexer. The
    // question this answers is whether the scope survives *that* path — the session test's failure cannot be blamed on
    // the commit if the summary built here already has no scope.
    let empty = cpp_code_analysis::MemoryFiles::new();
    let config = CompilerConfig::default();
    let indexer = cpp_code_analysis::FileIndexer::new(&empty, &config);
    let read = indexer.index_rendering(
        std::path::Path::new("/p/widget.h"),
        &rendered,
        cpp_code_analysis::SummaryKey::new(0, 0),
    );
    let from_the_rendering = read
        .summary
        .declarations
        .iter()
        .find(|fact| fact.name == "Widget");
    println!(
        "from the rendering: Widget scope {:?}, {} declaration(s), {} include(s)",
        from_the_rendering.and_then(|fact| fact.scope.clone()),
        read.summary.declarations.len(),
        read.summary.includes.len()
    );    assert!(
        rendered.text.contains("namespace std"),
        "the body of _STD_BEGIN is expanded: {}",
        rendered.text
    );
}
