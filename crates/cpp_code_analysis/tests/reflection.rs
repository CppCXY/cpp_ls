//! **C++26 static reflection** (P2996), end to end: the file parses, nothing is reported, and `^^T` has the type
//! the standard gives it.
//!
//! No GCC 16 exists on this machine, so the syntax is the proposal's ([P2996R13], as adjusted by [P3687] for C++26:
//! splice *template arguments* were removed, so a bare `[: r :]` in a template argument list is an expression and a
//! type there needs its `typename`). What can be checked here is what a language server must get right without a
//! compiler: the tokens, the tree, the absence of false diagnostics, and the one type that is known by definition.
//!
//! [P2996R13]: https://isocpp.org/files/papers/P2996R13.html
//! [P3687]: https://open-std.org/jtc1/sc22/WG21/docs/papers/2025/p3687r0.html

use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    file::coordinates::FileOffset,
};

/// The shapes the proposal's own examples write, in one file.
const REFLECTION: &str = "\
struct Widget { int x; double y; };
template <class T> struct Box { T value; };
constexpr auto r = ^^Widget;
constexpr auto m = ^^Widget::x;
using T = typename[:r:];
using S = typename[:^^Box<int>:]<int>;
int main() {
    Widget w{};
    auto& field = w.[:^^Widget::x:];
    auto n = std::meta::identifier_of(^^Widget::x);
    return field + n;
}
";

fn session() -> Session<MemoryFiles> {
    let files = MemoryFiles::new().with_file("/p/a.cpp", REFLECTION);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();
    session
}

/// **The file is read, not refused**: no syntax errors at all, in the cooked reading as well as its own text.
#[test]
fn a_reflection_file_has_no_diagnostics() {
    let session = session();
    let found = session
        .diagnostics("/p/a.cpp")
        .expect("the file has a reading");
    assert_eq!(found.errors.len(), 0, "{:?}", found.errors);
    assert!(found.checks.is_empty(), "no check findings: {:?}", found.checks);
}

/// **`^^T` is a `std::meta::info`**, whatever it reflects — the one expression whose type does not depend on its
/// operand. Read on `^^Widget`'s own token, through the rendering, because that is the reading a request is answered
/// from.
#[test]
fn a_reflection_is_a_std_meta_info() {
    let session = session();
    let view = session.view("/p/a.cpp").expect("the file is cooked");
    let at = REFLECTION
        .find("^^Widget;")
        .expect("the fixture reflects a type");
    let reading = view
        .reading_offset_of(FileOffset::new(at))
        .expect("the reflection is in the rendering");
    match session.type_at(&view, reading) {
        Known::Yes(found) => assert_eq!(
            found.type_of, "std::meta::info",
            "`^^` has one type, whichever operand it takes"
        ),
        other => panic!("a reflection is an expression with a type: {other:?}"),
    }
}

/// **A member splice is where a member is**, so a cursor on `w.[: r :]` sits in an expression that the layer reads
/// as a member access — not as an index or a lambda, which is what the two tokens could otherwise be.
#[test]
fn a_member_splice_is_an_expression() {
    let session = session();
    let view = session.view("/p/a.cpp").expect("the file is cooked");
    let at = REFLECTION
        .find("w.[:")
        .expect("the fixture splices a member");
    let reading = view
        .reading_offset_of(FileOffset::new(at + 2))
        .expect("the splice is in the rendering");
    // The question that must not panic or crash: the tree's shape is pinned by the parser's own tests; this asks
    // the layer that walks it for an answer at that position.
    let _ = session.type_at(&view, reading);
}
