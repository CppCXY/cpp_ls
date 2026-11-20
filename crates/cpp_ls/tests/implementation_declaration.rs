//! **"Go to implementation" is a question about a declaration**, and the declaration is usually in a header.
//!
//! Reported from an editor: a function **declared in a C header** does not go to its implementation. The body is in a
//! `.c` file the header does not include, so the query runs in the direction the reader asks — *this declaration, where
//! is its code* — and the answer is a scan over the summaries for the same name whose declaration **defines** it.
//!
//! That last word was decided from the *text* ("a definition ends with `}`"), which is wrong for C: measured on Lua
//! 5.5.0, `lstate.c`'s `lua_newstate` is `definition: true` with the range `8508..8560`, whose text is
//! `*lua_newstate (lua_Alloc f, void *ud, unsigned seed)` — the range covers the declarator, not the body. Every C
//! implementation was therefore skipped. The fact carries the flag; these fixtures pin that it is what is read.

use cpp_code_analysis::file::coordinates::ReadingOffset;
use cpp_code_analysis::{Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use cpp_ls::handlers::implementation::definitions_of;

/// A session over the given files, everything indexed, and the view of `asked`.
fn session_over(
    files: &[(&str, &str)],
    asked: &str,
) -> (Session<MemoryFiles>, cpp_code_analysis::FileView) {
    let mut memory = MemoryFiles::new();
    for (path, text) in files {
        memory = memory.with_file(*path, *text);
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        cpp_code_analysis::CompilerConfig::default(),
    );
    session.add_project_files(files.iter().map(|(path, _)| std::path::PathBuf::from(path)));
    session.index_everything();
    session.load(std::path::Path::new(asked));
    let held = session
        .files()
        .held(std::path::Path::new(asked))
        .expect("the asked file is readable")
        .clone();
    let view =
        cpp_code_analysis::FileView::parse_with(&held, &cpp_parser::MacroEnvironment::default());
    (session, view)
}

/// **A C header's function declaration answers the `.c` file that defines it.**
#[test]
fn a_c_header_declaration_answers_its_source() {
    let header = "\
#ifndef A_H
#define A_H
int count(int x);
int other(int y);
#endif
";
    let source = "\
#include \"a.h\"

int count(int x) {
    return x + 1;
}

int other(int y) {
    return y;
}
";
    let (session, view) = session_over(&[("/p/a.h", header), ("/p/a.c", source)], "/p/a.h");
    let at = header.find("count").expect("the declaration") + 1;
    let found = definitions_of(&session, &view, at);
    assert_eq!(found.len(), 1, "one implementation: {found:?}");
    assert!(
        found[0].uri.as_str().ends_with("a.c"),
        "the answer is the source file, not the header: {found:?}"
    );
    assert_eq!(
        found[0].range.start.line, 2,
        "at `int count(int x) {{` — the third line of the source: {found:?}"
    );
}

/// **And the C++ shape still answers**, which is the one the text test was written for: a member declared in a class
/// and defined out of line in a `.cpp`.
#[test]
fn a_cxx_member_declaration_answers_its_out_of_line_definition() {
    let header = "\
class Widget {
public:
    void run();
    int size();
};
";
    let source = "\
#include \"widget.h\"

void Widget::run() {
}

int Widget::size() {
    return 1;
}
";
    let (session, view) = session_over(
        &[("/p/widget.h", header), ("/p/widget.cpp", source)],
        "/p/widget.h",
    );
    let at = header.find("run").expect("the member") + 1;
    let found = definitions_of(&session, &view, at);
    assert_eq!(found.len(), 1, "one definition: {found:?}");
    assert!(
        found[0].uri.as_str().ends_with("widget.cpp"),
        "the answer is the source: {found:?}"
    );
    assert_eq!(
        found[0].range.start.line, 2,
        "at `void Widget::run() {{`: {found:?}"
    );
}

/// **A declaration with no body anywhere answers nothing**, rather than the declaration itself: the feature is "where
/// is the code", and a question with no answer is `null`.
#[test]
fn a_declaration_nothing_defines_answers_nothing() {
    let header = "int missing(int x);\n";
    let source = "#include \"a.h\"\n\nint present(int x) { return x; }\n";
    let (session, view) = session_over(&[("/p/a.h", header), ("/p/a.c", source)], "/p/a.h");
    let at = header.find("missing").expect("the declaration") + 1;
    // The name resolves — the declaration is a fact — and nothing defines it.
    assert!(
        matches!(
            session.definitions(&view, ReadingOffset::new(at)),
            Known::Yes(_)
        ),
        "the declaration is indexed"
    );
    assert!(definitions_of(&session, &view, at).is_empty());
}
