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
/// **A declaration in a branch this compilation does not take is not an answer.**
///
/// libstdc++ declares `insert` and `_M_data_ptr` twice — `void insert(...)`/`value_type* _M_data_ptr(...)` under the
/// `#else` of a `#if __cplusplus >= 201103L`, and the `iterator`/`pointer_traits<_Ptr>::element_type*` versions under
/// the taken branch. Both were answers, so a hover or a completion could name a declaration a compiler never reads.
/// The lookup now asks `visibility_at` for each candidate's own guard and drops the ones whose branch is not taken
/// (`Unknown` keeps them: a guard this layer cannot decide must not become "the name is not declared").
#[test]
#[ignore = "opens a real project; run with --ignored"]
fn a_dead_branch_is_not_an_answer() {
    let project = project();
    let header = std::path::PathBuf::from(
        std::env::var("CPPLS_STL_VECTOR").unwrap_or_else(|_| {
            r"C:\Users\zc\Desktop\mingw64\lib\gcc\x86_64-w64-mingw32\15.1.0\include\c++\bits\stl_vector.h"
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

    for name in ["insert", "_M_data_ptr"] {
        let started = Instant::now();
        let known = session.index().definitions(name, &header);
        let took = started.elapsed();
        let cpp_code_analysis::Known::Yes(found) = known else {
            panic!("`{name}` must be declared somewhere: {known:?}");
        };
        let returns: Vec<Option<&str>> = found
            .found
            .iter()
            .map(|declaration| declaration.fact.returns.as_deref())
            .collect();
        println!("{name}: {returns:?} in {took:?}");
        assert!(
            !returns.contains(&Some("void")),
            "the `#else` branch's declaration is not an answer: {returns:?}"
        );
        assert!(
            !returns.contains(&Some("value_type *")),
            "and neither is the other one: {returns:?}"
        );
        assert!(
            !returns.is_empty(),
            "the taken branch is still an answer: {returns:?}"
        );
    }
}
/// **The main path of `docs/architecture.md` §5 step one**: one file, lexed once, rendered, and the *rendering*
/// parsed — every fact's range mapped back into the file.
///
/// The assertions are the ones the old path could not make:
///
/// * `emplace_back` returns `reference` **without any fallback**, because the `#else` branch does not exist in the
///   text the parser read;
/// * no fact carries `split_by_a_directive` — there is no "the file states two types" case left;
/// * `insert` has no `void` — the `#else` branch's declaration is not in the rendering;
/// * every range is inside the file it was mapped into.
#[test]
#[ignore = "opens a real project; run with --ignored"]
fn the_rendering_is_the_reading() {
    let project = project();
    let header = std::path::PathBuf::from(
        std::env::var("CPPLS_STL_HEADER").unwrap_or_else(|_| {
            r"C:\Users\zc\Desktop\mingw64\lib\gcc\x86_64-w64-mingw32\15.1.0\include\c++\bits\stl_bvector.h"
                .to_string()
        }),
    );
    let text = std::fs::read_to_string(&header).expect("the header reads");

    let mut session = Session::open(
        project.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project),
    );
    session.add_project_files([header.clone()]);
    session.index_everything();

    // **And then the pump, the way a server drives it.** `index_everything` returns while the second pass is still
    // owed, and that is deliberate — folding the pass into it took a real project's cold start from 12.6 s to
    // 135.6 s (see its note). The work is the pump's, a slice at a time, and what it buys is this: the summary the
    // index holds for a file is the reading a compiler would take of it.
    let mut slices = 0;
    while session.pending_work() > 0 && slices < 4_000 {
        session.advance(64);
        slices += 1;
    }
    println!(
        "the pump took {slices} slice(s); owed afterwards: {}",
        session.pending_work()
    );

    // **And what a plain `index_everything` filed, with nothing asked of it.** This is the gate the whole migration
    // is judged by: the answer a consumer gets must not depend on whether somebody happened to request a rendering.
    // The re-read pass is what makes this true — a file whose own text splits a declaration across `#if` branches is
    // re-read through its timeline exactly because the fact says it might read differently.
    let filed = {
        let summary = session.index().summary(&header).expect("the header is indexed");
        let facts: Vec<_> = summary
            .declarations
            .iter()
            .filter(|fact| fact.name == "emplace_back")
            .map(|fact| (fact.returns.clone(), fact.split_by_a_directive))
            .collect();
        println!("filed by the pipeline: {facts:?}");
        facts
    };
    assert_eq!(
        filed,
        vec![(Some("reference".to_string()), false)],
        "the reading the index holds is the rendering's, and nothing had to ask for it"
    );

    let started = Instant::now();
    let summary = session
        .index_the_rendering(&header)
        .expect("the rendering is indexed");
    let took = started.elapsed();

    let find = |name: &str| {
        summary
            .declarations
            .iter()
            .filter(|fact| fact.name == name)
            .map(|fact| fact.returns.clone())
            .collect::<Vec<_>>()
    };
    let emplace_back = find("emplace_back");
    let insert = find("insert");
    println!(
        "{} facts in {took:?}: emplace_back {emplace_back:?}, insert {insert:?}",
        summary.declarations.len()
    );

    assert_eq!(
        emplace_back,
        vec![Some("reference".to_string())],
        "the compiled branch's type, with no fallback"
    );
    assert!(
        !insert.contains(&Some("void".to_string())),
        "the `#else` branch is not in the rendering: {insert:?}"
    );
    assert!(
        summary
            .declarations
            .iter()
            .all(|fact| !fact.split_by_a_directive),
        "a rendering has no directive left to split a declaration"
    );
    let length = text.len();
    assert!(
        summary
            .declarations
            .iter()
            .all(|fact| fact.range.end_offset() <= length && fact.name_range.end_offset() <= length),
        "every range is a place in the file"
    );
}