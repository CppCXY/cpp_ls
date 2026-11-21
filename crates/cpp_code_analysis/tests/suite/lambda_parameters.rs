//! **A generic lambda's parameter type, deduced from the call site** — the report of 2026-11-18.
//!
//! `foo.call([](auto &&x) {})` declares no type for `x`: the closure is a template, and its parameter has one only at
//! the call that receives it. The four answers a reader expects are these:
//!
//! ```text
//! foo.call([](auto &&a) {});   a: Foo&          a non-const object prefers the non-const `call`, and `*this` is one
//! cfoo.call([](auto &&b) {});  b: const Foo&    a const object reaches only the const overload, where `*this` is const
//! bar.call([](auto &&c) {});   (nothing)        both `Bar::call`s are constrained, so which one runs is not decidable
//! cbar.call([](auto &&d) {});  d: const Bar&    one candidate is reachable at all, so its constraint is not the choice
//! ```
//!
//! The deduction is `Session::a_generic_lambdas_parameter_type`; this file is the four rules it documents, measured.

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

const SOURCE: &str = "\
struct Foo {
    template <typename Fn>
    void call(Fn &&fn) {
        fn(*this);
    }

    template <typename Fn>
    void call(Fn &&fn) const {
        fn(*this);
    }
};

struct Bar {
    template <typename Fn>
    void call(Fn &&fn)
        requires requires { fn(*this); }
    {
        fn(*this);
    }

    template <typename Fn>
    void call(Fn &&fn) const
        requires requires { fn(*this); }
    {
        fn(*this);
    }
};

void run() {
    Foo foo;
    foo.call([](auto &&a) {});

    const Foo cfoo;
    cfoo.call([](auto &&b) {});

    Bar bar;
    bar.call([](auto &&c) {});

    const Bar cbar;
    cbar.call([](auto &&d) {});

    // **Not a call argument at all**, so there is no call site to read a type from.
    auto alone = [](auto e) {};
    alone(1);
}
";

fn session_with(source: &str) -> Session<MemoryFiles> {
    let memory = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    session
}

/// The parameter each hint is attached to — the identifier just before the hint's offset.
fn parameter_of(source: &str, offset: usize) -> String {
    let before = &source[..offset.min(source.len())];
    before
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .next_back()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_generic_lambdas_parameter_gets_the_type_the_call_site_gives_it() {
    let session = session_with(SOURCE);
    let view = session
        .view_of_the_file("/p/a.cpp")
        .expect("the file is readable");
    let range = cpp_parser::SourceRange::new(0, SOURCE.len());
    let found: std::collections::HashMap<String, String> = session
        .type_hints(&view, range)
        .into_iter()
        .filter(|hint| hint.kind == cpp_code_analysis::TypeHintKind::LambdaParameter)
        .map(|hint| (parameter_of(SOURCE, hint.offset), hint.label))
        .collect();

    assert_eq!(
        found.get("a").map(String::as_str),
        Some("Foo&"),
        "a non-const object: the non-const `call`, and `*this` is an lvalue: {found:?}"
    );
    assert_eq!(
        found.get("b").map(String::as_str),
        Some("const Foo&"),
        "a const object reaches only the const overload, where `*this` is `const Foo`: {found:?}"
    );
    assert_eq!(
        found.get("c"),
        None,
        "two constrained candidates: which one runs is not decidable, so nothing is claimed: {found:?}"
    );
    assert_eq!(
        found.get("d").map(String::as_str),
        Some("const Bar&"),
        "one candidate is reachable, so its constraint is not what chose it: {found:?}"
    );
    assert_eq!(
        found.get("e"),
        None,
        "a lambda that is not a call argument has no call site to read: {found:?}"
    );
}
