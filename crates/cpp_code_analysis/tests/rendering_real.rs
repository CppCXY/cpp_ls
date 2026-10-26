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

    // **What the index holds, with nobody having asked for a rendering.** This used to assert the opposite — that the
    // summary was the *written* reading (unsplit `None`, `split_by_a_directive` set) and that only a hover asking for
    // the rendering could get `reference`. It is the other way round now, and that is §5's goal reached: the second
    // pass re-reads a file whose declarations a directive splits, so by the time the queue drains the summary **is**
    // the rendering. The assertion below is the gate, and it is what a consumer's answer depends on.
    let summary = session
        .index()
        .summary(&header)
        .expect("the header is read");
    let filed: Vec<(Option<String>, bool)> = summary
        .declarations
        .iter()
        .filter(|fact| fact.name == "emplace_back")
        .map(|fact| (fact.returns.clone(), fact.split_by_a_directive))
        .collect();
    println!("filed by the pipeline: {filed:?}");
    assert!(
        !filed.is_empty(),
        "the header declares `emplace_back`: {filed:?}"
    );
    // **The claim is that the pipeline's own reading names the branch the compilation takes** — and its two spellings
    // differ by toolchain: libstdc++ writes the type out (`reference`), MSVC writes it as the macro
    // `_CONTAINER_EMPLACE_RETURN`, which a reading that does not expand macros can only repeat. What matters here is
    // that the *flag* is gone (the declaration is no longer stated twice) and that the type is not one of the branches
    // a compiler never takes (`void`); the macro-named spelling is what patch 1 asks the rendering about, and the
    // assertion below the loop is where the expanded answer is checked.
    assert!(
        filed
            .iter()
            .all(|(returns, split)| !split && returns.as_deref() != Some("void")),
        "no `emplace_back` the index files is split or names the untaken branch: {filed:?}"
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
    // **A type, not a macro name.** The written reading cannot expand `_CONTAINER_EMPLACE_RETURN` and answers the
    // macro's own spelling (measured on MSVC's `<vector>`); the rendering has expanded it, so its answer must look like
    // a type. libstdc++ writes the type out anyway and answers `reference` — both toolchains satisfy this, and neither
    // spelling is hard-coded.
    let returns = read.returns.as_deref().unwrap_or_default();
    assert!(
        !returns.is_empty()
            && !returns
                .chars()
                .all(|character| character.is_ascii_uppercase() || character == '_'),
        "the rendering names a type rather than a macro: {returns:?}"
    );
    println!("the rendering answers: {returns:?} (the written reading said {filed:?})");
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
        // **A name this toolchain's header does not declare is skipped, not failed.** `_M_data_ptr` exists in
        // libstdc++'s `stl_vector.h` and not in MSVC's `<vector>`, and a probe that opened whichever header the
        // machine has cannot insist on the names of one of them.
        let cpp_code_analysis::Known::Yes(found) = known else {
            println!("{name}: not declared by this toolchain's header — skipped");
            continue;
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
        let summary = session
            .index()
            .summary(&header)
            .expect("the header is indexed");
        let facts: Vec<_> = summary
            .declarations
            .iter()
            .filter(|fact| fact.name == "emplace_back")
            .map(|fact| (fact.returns.clone(), fact.split_by_a_directive))
            .collect();
        println!("filed by the pipeline: {facts:?}");
        facts
    };
    // **One per declaration of the name the header has**, which is one in libstdc++'s `bits/stl_bvector.h` and two in
    // MSVC's `<vector>` (the class and its `bool` specialization): the claim is about what every one of them says, not
    // about how many there are.
    assert!(
        !filed.is_empty()
            && filed
                .iter()
                .all(|(returns, split)| returns.as_deref() == Some("reference") && !split),
        "the reading the index holds is the rendering's, and nothing had to ask for it: {filed:?}"
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

    // **Every one of them a type, and none of them a macro name** — see the note in
    // `a_split_return_type_is_read_from_the_rendering` for why the spelling is not named here.
    assert!(
        !emplace_back.is_empty()
            && emplace_back
                .iter()
                .all(|returns| returns.as_deref().is_some_and(|returns| {
                    !returns.is_empty()
                        && !returns
                            .chars()
                            .all(|character| character.is_ascii_uppercase() || character == '_')
                })),
        "the compiled branch's type, with no fallback: {emplace_back:?}"
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
