//! What a declaration says about itself: the comment above it, found **backwards** from the declaration.
//!
//! The parser's own tree points one way — a comment is a sibling of what it documents, so a comment knows its
//! declaration and a declaration knows nothing ([`cpp_parser::documentation_of`] is the reverse walk that fixes
//! that). What these tests cover is the layer above it: a **position** in a file asked for the documentation of
//! whatever is declared there, in the file being edited and in another one, and the cheap check that keeps a
//! question about a header from parsing the whole header.

use cpp_code_analysis::file::view::{documentation_at, might_be_documented};
use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

fn session_with(files: &[(&str, &str)]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    for (path, source) in files {
        memory = memory.with_file(*path, *source);
    }

    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    for (path, _) in files {
        session.load(path);
    }

    session
}

/// The documentation of the declaration at an offset in an open file, as the text a hover would render.
fn documentation_in(source: &str, offset: usize) -> Option<String> {
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    session
        .documentation(&view, std::path::Path::new("/p/a.cpp"), offset)
        .map(|comment| comment.get_comment_text())
}

// ============================================================================
// A position asks for the documentation of what is declared there
// ============================================================================

#[test]
fn a_declaration_reports_the_comment_above_it() {
    let source = "/// Doc.\nint x;\n";
    assert_eq!(
        documentation_in(source, source.find("int x").expect("the fixture")),
        Some("Doc.".to_string())
    );
}

/// **Every offset of the declaration answers the same thing.**
///
/// A cursor is not a parser: it lands on the type, on the name, on a `*`, on the `;`, on the whitespace between
/// them. All of those are inside the declaration, so all of them have to report the declaration's documentation —
/// and the walk over the ancestors is what makes that true, because a node inside a declarator has no
/// documentation of its own.
#[test]
fn every_offset_of_the_declaration_reports_it() {
    let source = "/// Doc.\nint f(Widget& w);\n";
    let start = source.find("int f").expect("the fixture");
    let end = source.find(';').expect("the fixture");

    for offset in start..=end {
        assert_eq!(
            documentation_in(source, offset),
            Some("Doc.".to_string()),
            "the declaration is documented at offset {offset}"
        );
    }
}

/// A class is a declaration, and a member of it is documented by its own comment rather than by the class's.
#[test]
fn a_member_is_not_documented_by_the_classs_comment() {
    let source = "/// The class.\nstruct S {\n    /// The n.\n    int n;\n};\n";

    assert_eq!(
        documentation_in(source, source.find("struct S").expect("the fixture")),
        Some("The class.".to_string())
    );
    assert_eq!(
        documentation_in(source, source.find("int n").expect("the fixture")),
        Some("The n.".to_string())
    );
}

/// A comment written after a member is nested inside that member's declaration (the tree shape the parser pins),
/// so the member below it is documented by a comment that is not its sibling. The user cannot see any of that:
/// both members report the comment above them.
#[test]
fn a_member_documented_by_a_nested_comment_is_found_too() {
    let source = "struct S {\n    int x;\n    /// The y.\n    int y;\n};\n";

    assert_eq!(
        documentation_in(source, source.find("int y").expect("the fixture")),
        Some("The y.".to_string())
    );
    assert_eq!(
        documentation_in(source, source.find("int x").expect("the fixture")),
        None,
        "the comment below `x` is not `x`'s documentation"
    );
}

/// A trailing `///<` documents what comes *before* it, and the parser declines to read it forwards. The
/// declaration under it must therefore report nothing rather than the comment of its neighbour.
#[test]
fn a_trailing_comment_documents_nothing_at_the_cursor() {
    let source = "struct S {\n    int a; ///< the a\n    int b;\n};\n";

    assert_eq!(
        documentation_in(source, source.find("int b").expect("the fixture")),
        None
    );
}

/// A plain `//` comment has the same *tree* relationship as a documentation comment, and a consumer decides what
/// to render. Hover asks [`cpp_parser::CppDocComment::is_documentation`] on top of this; the structural answer is
/// pinned here so that the two questions stay separate.
#[test]
fn a_plain_comment_is_found_but_is_not_documentation() {
    let source = "// A note.\nint x;\n";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let found = session
        .documentation(
            &view,
            std::path::Path::new("/p/a.cpp"),
            source.find("int x").expect("the fixture"),
        )
        .expect("the comment documents the declaration");

    assert_eq!(found.get_comment_text(), "A note.");
    assert!(
        !found.is_documentation(),
        "`//` is not one of the spellings the lexer calls documentation"
    );
}

#[test]
fn a_declaration_with_nothing_above_it_has_no_documentation() {
    assert_eq!(documentation_in("int x;\n", 0), None);
    assert_eq!(documentation_in("int x;\nint y;\n", 7), None);
}

// ============================================================================
// A declaration in another file
// ============================================================================

/// The header is parsed for the answer — nothing else in the session holds its tree — which is why the question
/// has to be worth a parse; see [`the_check_never_rejects_a_comment_the_parser_finds`].
#[test]
fn the_declaring_file_answers_too() {
    let header = "/// A widget.\nstruct Widget {\n    int size;\n};\n";
    let source = "#include \"b.h\"\nint f(Widget& w) { return w.size; }\n";
    let session = session_with(&[("/p/a.cpp", source), ("/p/b.h", header)]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let found = session
        .documentation(
            &view,
            std::path::Path::new("/p/b.h"),
            header.find("struct Widget").expect("the fixture"),
        )
        .expect("the header documents the class");

    assert_eq!(found.get_comment_text(), "A widget.");
}

/// A file the session cannot read is `None`: the declaration is still true and its documentation is simply not
/// available, which is the same answer as "it has none" and the only one that does not invent text.
#[test]
fn an_unreadable_file_has_no_documentation() {
    let session = session_with(&[("/p/a.cpp", "int x;\n")]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    assert!(
        session
            .documentation(&view, std::path::Path::new("/p/missing.h"), 0)
            .is_none()
    );
}

// ============================================================================
// The check that keeps a header from being parsed
// ============================================================================

/// **The check is only allowed to be too permissive.**
///
/// It exists so that a declaration in an unparsed file is not parsed to find out that nothing documents it, and a
/// `false` there would lose documentation silently. So: wherever the parser finds a comment, the check has to have
/// said `true`. The sources below are the shapes a comment can be separated from its declaration by.
#[test]
fn the_check_never_rejects_a_comment_the_parser_finds() {
    let cases = [
        ("/// Doc.\nint x;\n", "int x"),
        ("//! Doc.\nint x;\n", "int x"),
        ("/** Doc. */\nint x;\n", "int x"),
        ("/** Doc. */ int x;\n", "int x"),
        ("/* Doc.\n   More. */\nint x;\n", "int x"),
        ("/// Doc.\n#define N 3\nint x;\n", "int x"),
        ("/// Doc.\n#define N 3\n\n#define M 4\nint x;\n", "int x"),
        ("struct S {\n    int a;\n    /// Doc.\n    int b;\n};\n", "int b"),
        ("namespace ns {\n/// Doc.\nint x;\n}\n", "int x"),
    ];

    for (source, declaration) in cases {
        let offset = source.find(declaration).expect("the fixture declares it");
        let tree = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());
        let found = documentation_at(&tree.get_red_root(), offset);

        assert!(
            found.is_some(),
            "{source:?} documents `{declaration}` and the parser has to find it"
        );
        assert!(
            might_be_documented(source, offset),
            "{source:?} would be skipped by the check before the parse that finds its comment"
        );
    }
}

/// The other direction, which is only a cost: a file with no comment above the declaration is not parsed. The
/// check says so, and that is what it is for.
#[test]
fn a_declaration_with_code_above_it_is_not_worth_a_parse() {
    let source = "int a;\nint b;\n";
    assert!(!might_be_documented(source, source.find("int b").expect("the fixture")));

    let source = "struct S {\n    int a;\n    int b;\n};\n";
    assert!(!might_be_documented(source, source.find("int b").expect("the fixture")));

    // Nothing above it at all.
    assert!(!might_be_documented("int x;\n", 0));
}

/// A directive is not a comment, and it is not code either: the walk has to step over it, because the parser's
/// own rule does.
#[test]
fn a_directive_above_a_declaration_does_not_hide_a_comment() {
    let source = "/// Doc.\n#define N 3\nint x;\n";
    assert!(might_be_documented(
        source,
        source.find("int x").expect("the fixture")
    ));
}
