//! **The reading where the preprocessor is resolved**, asked for one declaration — the answer a fact cannot give.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_code_analysis --test rendering_real -- --ignored --nocapture
//! ```
//!
//! `bits/stl_bvector.h` writes `std::vector<bool>::emplace_back` as
//!
//! ```text
//! #if __cplusplus > 201402L
//!     _GLIBCXX20_CONSTEXPR
//!     reference
//! #else
//!     void
//! #endif
//!     emplace_back(_Args&&... __args)
//! ```
//!
//! The written reading answers the `#else` branch's `void` (or, since `DeclFact::split_by_a_directive` exists, no
//! type at all); the rendering deletes the branch that is not compiled and answers `reference`. This test is the
//! proof that the second answer is reachable by a consumer, which is what a hover on `_configs.emplace_back(` needs.

use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    )
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn a_split_return_type_is_read_from_the_rendering() {
    let project = project();
    // The header this project's `<vector>` resolves to — the same one the report's popup showed.
    let header = PathBuf::from(
        std::env::var("CPPLS_STL_HEADER").unwrap_or_else(|_| {
            r"C:\Users\zc\Desktop\mingw64\lib\gcc\x86_64-w64-mingw32\15.1.0\include\c++\bits\stl_bvector.h"
                .to_string()
        }),
    );

    let mut session = Session::open(
        project.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project),
    );
    session.add_project_files([header.clone()]);
    session.index_everything();

    // What the index holds: no type, and the flag that says why.
    let summary = session
        .index()
        .summary(&header)
        .expect("the header is read");
    let written = summary
        .declarations
        .iter()
        .find(|fact| fact.name == "emplace_back")
        .expect("the header declares it");
    println!(
        "written: returns {:?}, split_by_a_directive {}",
        written.returns, written.split_by_a_directive
    );
    assert!(
        written.split_by_a_directive,
        "the declaration is split by a directive"
    );
    assert!(
        written.returns.is_none(),
        "and the written reading names no branch: {:?}",
        written.returns
    );

    // **Twice**, because the two halves cost different things: the first call also cooks (a unit walk and a render,
    // which the session then caches), the second pays only the summary of the rendered text.
    for round in 1..=2 {
        let started = Instant::now();
        let read = session
            .fact_from_the_rendering(&header, "emplace_back", Some("std::vector"))
            .expect("the rendering declares it");
        println!(
            "rendering ({round}): returns {:?} in {:?}",
            read.returns,
            started.elapsed()
        );
    }
    let read = session
        .fact_from_the_rendering(&header, "emplace_back", Some("std::vector"))
        .expect("the rendering declares it");
    assert_eq!(
        read.returns.as_deref(),
        Some("reference"),
        "the compiled branch's type"
    );
}
