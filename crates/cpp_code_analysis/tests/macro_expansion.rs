//! **A macro's expansion, as the hover shows it**: the substitution a compiler makes — `#` and `##` applied, and
//! every macro named in the result expanded again — and then formatted.
//!
//! The hover used to show a *textual* substitution: the body with the arguments put in, `##` left as two hashes in
//! the text, and the result's own macro names untouched. Measured on a real project, `kvi_ensure_space`'s card showed
//! `kvi_resize((v), (v).capacity)` for an argument that was itself a macro — a reader comparing it with a compiler's
//! answer was looking at two different things.

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    file::coordinates::FileOffset,
};

/// A session over one file, cooked, with the offset of the *use* the fixture is about.
fn expanded(source: &str, at: &str) -> String {
    let memory = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();
    let view = session.view("/p/a.cpp").expect("the file is cooked");
    let offset = source.find(at).expect("the fixture uses the marker") + 1;
    session
        .the_expansion_of_a_macro(&view, FileOffset::new(offset))
        .unwrap_or_else(|| panic!("`{at}` at {offset} has no expansion"))
}

/// **`##` pastes the two tokens into one**, which is the operator that makes the result a different *identifier*
/// rather than a different arrangement of tokens.
#[test]
fn a_macro_expansion_applies_the_paste_operator() {
    let source = "\
#define PASTE(a, b) a ## b
int PASTE(foo, bar) = 1;
";
    let expansion = expanded(source, "PASTE(foo");
    assert_eq!(expansion.trim(), "foobar", "`a ## b` is one token");
}

/// **The macros the body names are expanded too**, recursively — what a compiler does and what a one-pass
/// substitution cannot show.
#[test]
fn a_macro_expansion_expands_the_macros_in_its_body() {
    let source = "\
#define ORIGIN 3
#define WRAPPED (ORIGIN + 1)
int x = WRAPPED;
";
    let expansion = expanded(source, "WRAPPED;");
    assert!(
        expansion.contains('3'),
        "`ORIGIN` is a macro and its value is what the compiler sees: {expansion}"
    );
    assert!(
        !expansion.contains("ORIGIN"),
        "the name is substituted: {expansion}"
    );
}

/// **`#` stringizes its argument.**
#[test]
fn a_macro_expansion_applies_the_stringize_operator() {
    let source = "\
#define STR(x) #x
const char *s = STR(hello);
";
    let expansion = expanded(source, "STR(hello)");
    assert!(
        expansion.contains("\"hello\""),
        "`#x` is a string literal: {expansion}"
    );
}

/// **The expansion is formatted**, which is what makes it readable as code rather than as the body's own spacing.
#[test]
fn a_macro_expansion_is_formatted() {
    let source = "\
#define ADD(a,b) ((a)+(b))
int n = ADD(1,   2);
";
    let expansion = expanded(source, "ADD(1");
    assert_eq!(
        expansion.trim(),
        "((1) + (2))",
        "the formatter puts the operators where a reader expects them"
    );
}
