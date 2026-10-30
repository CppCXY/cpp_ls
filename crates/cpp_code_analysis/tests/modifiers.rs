//! **The bits a declaration is written with** — [`DeclModifiers`], recorded when the summary is built.
//!
//! Every assertion here used to be a text search at a check site (`written.contains("override")`), which is why the
//! answers moved to the fact: `override` and `final` are contextual identifiers, `[[maybe_unused]]` is an attribute,
//! and `virtual`/`constexpr` are keywords in the specifier sequence. One read at index time, one bit per question.

use cpp_code_analysis::{
    CompilerConfig, DeclModifiers, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// The summary's facts, for a fixture in one memory file.
fn facts_of(source: &str) -> Vec<cpp_code_analysis::DeclFact> {
    let path = std::path::Path::new("/p/a.cpp");
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([path.to_path_buf()]);
    session.want_cooked_reading(path);
    session.index_everything();
    session
        .parsed_summary(path)
        .expect("a summary")
        .declarations
}

/// The modifiers of the fact whose name and scope are given.
fn modifiers_of(source: &str, name: &str, scope: Option<&str>) -> DeclModifiers {
    facts_of(source)
        .into_iter()
        .find(|fact| fact.name == name && fact.scope.as_deref() == scope)
        .unwrap_or_else(|| panic!("`{name}` must be a fact in {scope:?}"))
        .modifiers
}

#[test]
fn the_virt_specifiers_are_bits() {
    let source = "\
struct Base { virtual void f(); };
struct Derived : Base {
    void f() override;
    void g() final;
};
";
    assert!(
        modifiers_of(source, "Base", None).is_empty(),
        "a class declares neither `override` nor `final`"
    );
    assert!(
        modifiers_of(source, "f", Some("Base")).contains(DeclModifiers::VIRTUAL),
        "`virtual` is a specifier of the base's member"
    );
    assert!(
        modifiers_of(source, "f", Some("Derived")).contains(DeclModifiers::OVERRIDE),
        "`override` is a contextual identifier after the parameter list"
    );
    assert!(
        modifiers_of(source, "g", Some("Derived")).contains(DeclModifiers::FINAL),
        "`final` is the other one"
    );
}

#[test]
fn the_specifier_keywords_are_bits() {
    let source = "\
consteval int immediate() { return 1; }
constexpr int constant() { return 2; }
constinit int at_start = 3;
static void once() {}
inline void often() {}
void log(const char* format, ...);
";
    assert!(modifiers_of(source, "immediate", None).contains(DeclModifiers::CONSTEVAL));
    assert!(modifiers_of(source, "constant", None).contains(DeclModifiers::CONSTEXPR));
    assert!(modifiers_of(source, "at_start", None).contains(DeclModifiers::CONSTINIT));
    assert!(modifiers_of(source, "once", None).contains(DeclModifiers::STATIC));
    assert!(modifiers_of(source, "often", None).contains(DeclModifiers::INLINE));
    assert!(
        modifiers_of(source, "log", None).contains(DeclModifiers::VARIADIC),
        "a parameter list that ends in `...` is variadic"
    );
}

#[test]
fn an_attribute_is_found_before_the_declaration() {
    let source = "\
void f() {
    [[maybe_unused]] int local = 3;
    int plain = 4;
}
";
    assert!(
        modifiers_of(source, "local", None).contains(DeclModifiers::MAYBE_UNUSED),
        "`[[maybe_unused]]` is a sibling of the declaration and is found by its prefix"
    );
    assert!(
        !modifiers_of(source, "plain", None).contains(DeclModifiers::MAYBE_UNUSED),
        "a previous declaration's attribute does not excuse the next one"
    );
}
