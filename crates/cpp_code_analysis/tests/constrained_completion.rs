//! **What a concept requires of its parameter** — the member list a constrained template parameter has.
//!
//! `auto f(A auto a) { a. }` has no class to list members of: `a` is a template parameter, and the only statement of
//! what it can do is the concept's own `requires` body. These tests pin that the completion reads it — and that a
//! parameter with no constraint still offers nothing, which is the difference between "the concept says so" and a
//! guess.

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{
    CompilerConfig, ItemKind, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

const SOURCE: &str = "\
template <class T>
concept B = requires(T b) {
    { b.bar() };
};

template <class T>
concept A = requires(T a) {
    { a.foo() } -> B;
    a.count;
};

template <A T>
auto by_head(T t) {
    t.
}

auto by_auto(A auto a) {
    a.
}

template <class U>
auto unconstrained(U u) {
    u.
}
";

fn session_of(source: &str) -> Session<MemoryFiles> {
    let memory = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    // **A completion is answered from a rendering**, and a rendering is built because something asked — the same
    // three steps every test in `tests/completion.rs` states once in its helper.
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();
    session
}

/// The labels the completion offers after the `.` of `needle`.
fn offered_after(needle: &str) -> Vec<(String, ItemKind)> {
    let session = session_of(SOURCE);
    let view = session.view("/p/a.cpp").expect("the file has a reading");
    let at = view
        .written_text()
        .expect("the rendering has the file's text")
        .find(needle)
        .unwrap_or_else(|| panic!("the fixture writes {needle:?}"))
        + needle.len();
    let offered = session.completions(&view, FileOffset::new(at));
    offered
        .items
        .into_iter()
        .map(|item| (item.label, item.kind))
        .collect()
}

/// **`A auto a` gives the members `A` requires** — the abbreviated function template's parameter is a constrained
/// template parameter, and `a.foo()` in the concept is the whole of what says `a` has a `foo`.
#[test]
fn a_constrained_auto_parameter_offers_what_the_concept_requires() {
    assert_eq!(
        offered_after("auto by_auto(A auto a) {\n    a."),
        [
            ("foo".to_string(), ItemKind::Method),
            ("count".to_string(), ItemKind::Field),
        ]
    );
}

/// **`template <A T>` is the same question one level up** — the parameter's type is a template parameter whose
/// constraint is written in the head, and the answer must not depend on the spelling a file happens to use.
#[test]
fn a_constrained_template_parameter_offers_the_same() {
    assert_eq!(
        offered_after("auto by_head(T t) {\n    t."),
        [
            ("foo".to_string(), ItemKind::Method),
            ("count".to_string(), ItemKind::Field),
        ]
    );
}

/// **An unconstrained parameter offers nothing**, and that is a fact rather than a gap: no concept has said what its
/// type has, so a list would be invented.
#[test]
fn an_unconstrained_parameter_offers_nothing() {
    assert!(offered_after("auto unconstrained(U u) {\n    u.").is_empty());
}
