//! What a documentation comment is *attached to*.
//!
//! The grammar emits a comment into whatever node was open when it was found, which makes it a
//! **sibling** of the declaration it documents rather than a child. That is the right shape for a
//! lossless tree — a comment between two members belongs to the class body, not to either member — but
//! it means the relationship has to be read forwards from the comment, and nothing in the tree says
//! which way that is. These tests are that contract.
//!
//! Two questions, deliberately separate:
//!
//! * [`CppDocComment::get_owner`] — "what follows?", which always has an answer when something does.
//! * [`CppDocComment::get_documented_declaration`] — "what does this document?", which is the narrower
//!   question a hover or a signature help asks and which has no answer for a comment that documents
//!   nothing.

use cpp_parser::{
    CppAstNode, CppDeclaration, CppDocComment, CppParser, CppSyntaxKind, CppSyntaxTree, ParserConfig,
};

fn parse(source: &str) -> CppSyntaxTree {
    CppParser::parse(source, ParserConfig::default())
}

/// Every documentation comment in the tree, outermost first.
fn comments(tree: &CppSyntaxTree) -> Vec<CppDocComment> {
    tree.get_red_root()
        .descendants()
        .filter_map(CppDocComment::cast)
        .collect()
}

/// The single comment in a one-comment source.
fn the_comment(source: &str) -> CppDocComment {
    let tree = parse(source);
    let mut found = comments(&tree);
    assert_eq!(
        found.len(),
        1,
        "{source:?} should hold exactly one comment: {:#?}",
        tree.get_red_root()
    );
    found.remove(0)
}

/// The name of the declaration a comment documents.
///
/// The accessor absorbs the shape differences between declaration forms — a `using` alias keeps its
/// name in a child node rather than in a declarator — so this is the whole query.
fn documented_name(comment: &CppDocComment) -> Option<String> {
    comment.get_documented_name()
}

// ============================================================================
// The forward relationship
// ============================================================================

#[test]
fn a_comment_documents_the_declaration_after_it() {
    assert_eq!(
        documented_name(&the_comment("/// Doc.\nint x;\n")),
        Some("x".to_string())
    );
}

/// The comment is a *sibling*, so `get_owner` answers with the next node and not with a parent.
///
/// This is the property the whole design rests on: if the comment had been made a child of its
/// declaration, the class-body case below would put it inside the wrong member.
#[test]
fn the_owner_is_the_next_node_not_the_parent() {
    let comment = the_comment("/// Doc.\nint x;\n");
    let owner = comment.get_owner().expect("a following node");
    assert_eq!(CppSyntaxKind::from(owner.kind()), CppSyntaxKind::Declaration);

    assert_eq!(
        comment
            .get_parent::<cpp_parser::CppTranslationUnit>()
            .map(|unit| unit.syntax().text().to_string()),
        Some("/// Doc.\nint x;\n".to_string()),
        "the parent is the translation unit, not the declaration"
    );
}

/// A class definition is a declaration too, and a comment in front of one documents the class.
#[test]
fn a_comment_documents_a_class_definition() {
    assert_eq!(
        documented_name(&the_comment("/// A grid.\nclass Grid {\n    int n;\n};\n")),
        Some("Grid".to_string())
    );
}

/// A function, an alias, a variable: the same relationship, whatever the declaration form.
#[test]
fn the_documented_declaration_is_found_for_every_form() {
    let cases = [
        ("/// Doc.\nint f();\n", "f"),
        ("/// Doc.\nint x = 1;\n", "x"),
        ("/// Doc.\nenum class Color { Red };\n", "Color"),
        ("/// Doc.\nvoid f() { }\n", "f"),
    ];

    for (source, expected) in cases {
        assert_eq!(
            documented_name(&the_comment(source)),
            Some(expected.to_string()),
            "{source:?}"
        );
    }
}

/// A `using` alias is its own node kind rather than a `Declaration`, because its shape is not
/// `specifiers declarators`. It still declares a name, so the relationship has to survive it.
#[test]
fn a_using_alias_is_documented_too() {
    assert_eq!(
        documented_name(&the_comment("/// Doc.\nusing Point = shapes::Point;\n")),
        Some("Point".to_string())
    );
}

// ============================================================================
// What a comment does *not* document
// ============================================================================

/// Nothing follows, so there is nothing to document — and `get_owner` says so rather than inventing
/// a relationship with the file.
#[test]
fn a_comment_at_the_end_of_a_file_documents_nothing() {
    let comment = the_comment("int x;\n/// Trailing note.\n");

    assert!(
        comment.get_owner().is_none(),
        "nothing follows the comment"
    );
    assert!(comment.get_documented_declaration().is_none());
}

/// A trailing `///<` documents the member *before* it, which this forward-only relationship cannot
/// see. Reporting the following declaration instead would attach the comment to the wrong member, so
/// the answer is `None` — and it has to be an explicit test, because the marker is the one thing that
/// reverses the direction every other case relies on.
#[test]
fn a_trailing_comment_documents_nothing_forwards() {
    let tree = parse("struct S {\n    int a; ///< the a\n    int b;\n};\n");
    let comment = comments(&tree)
        .into_iter()
        .find(|comment| comment.get_comment_text().contains("the a"))
        .expect("the trailing comment");

    assert!(comment.is_trailing(), "the `///<` marker is recognised");
    assert_eq!(
        documented_name(&comment),
        None,
        "a trailing comment documents the member before it, so the forward walk must decline"
    );

    // The declaration it really documents is the one before it, and it is reachable — just not
    // through this accessor.
    let previous = comment
        .get_owner()
        .and_then(|owner| owner.prev_sibling())
        .and_then(CppDeclaration::cast)
        .and_then(|declaration| declaration.get_name_text());
    assert_eq!(
        previous,
        Some("a".to_string()),
        "the member it documents is its previous sibling"
    );
}

/// A comment written the ordinary way is not trailing, whatever else it says.
#[test]
fn only_the_trailing_marker_reverses_the_direction() {
    for source in ["/// Doc.\nint x;\n", "//! Doc.\nint x;\n", "/** Doc. */\nint x;\n"] {
        let comment = the_comment(source);
        assert!(!comment.is_trailing(), "{source:?}");
        assert_eq!(documented_name(&comment), Some("x".to_string()), "{source:?}");
    }
}

/// A preprocessor directive between the comment and the code does not break the relationship: the
/// comment is still about the declaration.
#[test]
fn a_directive_between_the_comment_and_the_declaration_is_stepped_over() {
    assert_eq!(
        documented_name(&the_comment("/// Doc.\n#define N 3\nint x;\n")),
        Some("x".to_string())
    );
}

/// Two comment groups split by a blank line: the second one documents the declaration, and the first
/// documents the second — not the declaration, because they are separate documents.
#[test]
fn separate_comment_groups_do_not_merge() {
    let tree = parse("/// First.\n\n/// Second.\nint x;\n");
    let comments = comments(&tree);
    assert_eq!(comments.len(), 2, "a blank line ends a group");

    assert_eq!(
        documented_name(&comments[1]),
        Some("x".to_string()),
        "the later group documents the declaration"
    );
    assert_eq!(
        documented_name(&comments[0]),
        None,
        "the earlier group documents the later comment, which is not a declaration"
    );
    assert_eq!(
        comments[0].get_owner().map(|node| CppSyntaxKind::from(node.kind())),
        Some(CppSyntaxKind::DocComment),
        "but it does have a following node"
    );
}

/// A comment inside a class body documents the member after it, and a comment between members is a
/// child of the body rather than of either member.
#[test]
fn a_comment_in_a_class_body_documents_the_member_after_it() {
    let tree = parse("struct S {\n    /// The n.\n    int n;\n    int m;\n};\n");
    let comment = comments(&tree)
        .into_iter()
        .next()
        .expect("the comment");

    assert_eq!(documented_name(&comment), Some("n".to_string()));
    assert!(
        comment.get_parent::<cpp_parser::CppClassBody>().is_some(),
        "the comment belongs to the class body, like the members do"
    );
}

/// **A comment after a member is nested in that member's declaration — and the walk still finds the
/// member it describes.**
///
/// The nesting is a wart: the declaration rule consumes its `;` and the trivia after it before it
/// closes its own node, so the next member's doc comment is emitted inside the declaration above it
/// rather than beside the members. It is pinned here because the tree shape is what it is, and because
/// the *outward* step of [`cpp_parser::CppDocComment::get_owner`] is what makes the relationship
/// survive it: from the last child of a declaration, the search continues with the declaration's own
/// following sibling, which is the member the comment describes.
///
/// The fix for the nesting belongs in `expect_semicolon`/`emit_trivia_after_current_token` — the
/// declaration has to be closed before the layout that follows its `;` is emitted. When that lands,
/// this test's first assertion fails and the expectation should be upgraded to "the comment is a child
/// of the class body".
#[test]
fn a_comment_after_a_member_is_nested_but_still_finds_its_declaration() {
    let tree = parse("struct S {\n    int x;\n    /// The y.\n    int y;\n};\n");

    let comment = comments(&tree)
        .into_iter()
        .find(|comment| comment.get_comment_text().contains("The y"))
        .expect("the comment");

    assert_eq!(
        comment
            .get_parent::<CppDeclaration>()
            .and_then(|declaration| declaration.get_name_text()),
        Some("x".to_string()),
        "the comment is emitted inside the member before it, not into the class body"
    );
    assert_eq!(
        documented_name(&comment),
        Some("y".to_string()),
        "and yet it documents the member after it, because the walk steps outward"
    );
}

/// A comment in front of a namespace documents the namespace, which is not a `Declaration` — so the
/// narrow question has no answer while the broad one does.
#[test]
fn a_comment_before_a_namespace_has_an_owner_but_no_declaration() {
    let comment = the_comment("/// Doc.\nnamespace ns { }\n");

    assert_eq!(
        comment.get_owner().map(|node| CppSyntaxKind::from(node.kind())),
        Some(CppSyntaxKind::NamespaceDecl)
    );
    assert!(
        comment.get_documented_declaration().is_none(),
        "a namespace is not a declaration node, so it is not the documented declaration"
    );
}

/// A plain comment has the same relationship as a doc comment. It documents nothing to a *renderer*,
/// but the tree does not encode the difference, and a consumer asking about structure should get the
/// structural answer.
#[test]
fn a_plain_comment_has_the_same_relationship() {
    let comment = the_comment("// plain\nint x;\n");

    assert_eq!(documented_name(&comment), Some("x".to_string()));
    assert!(
        !comment.is_documentation(),
        "but it is not documentation, which is a separate question"
    );
}

// ============================================================================
// The relationship survives the whole file
// ============================================================================

/// Every doc comment in a realistic file finds the declaration it describes, in source order.
///
/// The source deliberately stops at the last documented declaration: a comment at the end of a file
/// has nothing after it, which is [`a_comment_at_the_end_of_a_file_documents_nothing`]'s subject.
#[test]
fn every_documented_declaration_in_a_file_is_found() {
    let source = "\
/// A point.
struct Point {
    /// The x.
    int x;
    /// The y.
    int y;
};

/// Distance.
double dist(const Point& a, const Point& b);
";

    let tree = parse(source);
    let names: Vec<Option<String>> = comments(&tree)
        .iter()
        .map(documented_name)
        .collect();

    assert_eq!(
        names,
        vec![
            Some("Point".to_string()),
            Some("x".to_string()),
            Some("y".to_string()),
            Some("dist".to_string()),
        ],
        "every comment that documents something finds it, in source order"
    );
}
