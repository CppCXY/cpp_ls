//! The Doxygen comment layer, seen through the C++ tree.
//!
//! The doc parser has no tree of its own: its nodes are ordinary children of the C++ tree, produced
//! by the same event stream. These tests therefore drive it the way a consumer does — parse C++ and
//! look for `DocComment` nodes — rather than calling the grammar directly.
//!
//! Three properties are asserted over a corpus rather than case by case, because they are the ones a
//! change is most likely to break silently:
//!
//! * **Losslessness.** Replacing a comment's token with its doc tokens must be a rewrite, not a loss.
//! * **Nesting.** A comment must not swallow the declaration it documents. That failure is invisible
//!   in the token text and only shows up in the tree's shape.
//! * **The event stream stays balanced.** The doc layer emits into the C++ stream, so an unpaired
//!   marker in either layer re-parents a subtree.

use cpp_parser::{
    CppParser, CppSyntaxKind, CppSyntaxTree, DocCommandKind, ParserConfig, command_kind,
};

fn parse(source: &str) -> CppSyntaxTree {
    CppParser::parse(source, ParserConfig::default())
}

/// Every `DocComment` node in the tree, outermost first.
fn comments(tree: &CppSyntaxTree) -> Vec<cpp_parser::CppSyntaxNode> {
    tree.get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocComment)
        .collect()
}

/// The source text a node covers.
///
/// Deliberately the node's *range* rather than the concatenation of its tokens: concatenating would
/// hide a comment's text being recorded outside the comment's own node, which is exactly the kind of
/// mis-nesting these tests exist to catch.
fn subtree_text(node: &cpp_parser::CppSyntaxNode) -> String {
    node.text().to_string()
}

/// The command names in a comment, in order.
fn command_names(node: &cpp_parser::CppSyntaxNode) -> Vec<String> {
    node.descendants()
        .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DocCommand)
        .filter_map(|command| {
            command
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| {
                    token.kind() == cpp_parser::CppTokenKind::DocCommandName.into()
                })
                .map(|token| token.text().to_string())
        })
        .collect()
}

/// The name argument of a command: the *last* `DocCommandArg` it owns.
///
/// The last, not the first: `@param[in] x` has two argument nodes, the direction and the name, and
/// the name is the one a cross-reference resolves.
fn argument_of(node: &cpp_parser::CppSyntaxNode, command: &str) -> Option<String> {
    node.descendants()
        .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DocCommand)
        .find(|child| {
            child
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .any(|token| token.text() == command)
        })
        .and_then(|child| {
            child
                .children()
                .filter(|grandchild| {
                    CppSyntaxKind::from(grandchild.kind()) == CppSyntaxKind::DocCommandArg
                })
                .last()
                .map(|arg| subtree_text(&arg))
        })
}

// ============================================================================
// Losslessness
// ============================================================================

const LOSS_CORPUS: &[&str] = &[
    "/// doc\nint x;\n",
    "// plain\nint x;\n",
    "/** block */\nint x;\n",
    "/// a\n/// b\n/// c\nint x;\n",
    "int /* inline */ x;\n",
    "int x; /// trailing\n",
    "/// @param x\nint f(int x);\n",
    "/// @code\n/// int y = 1;\n/// @endcode\nint x;\n",
    "/**\n * @brief x\n * @param[in] a\n */\nint f(int a);\n",
    "/// tail without newline",
    "///",
    "/**/",
    "//// banner\nint x;\n",
    "/// emoji \u{1F600} and \u{4e2d}\u{6587}\nint x;\n",
    "/* unterminated\nint x;\n",
];

/// A comment's text must survive being re-lexed by the doc layer.
///
/// This is the property that makes the whole two-layer design safe: the C++ lexer's one comment token
/// is *replaced* by the doc tokens, so if the doc lexer lost or duplicated a byte the tree would stop
/// reproducing the file — and nothing downstream would notice until a rename or a format rewrote the
/// comment wrongly.
///
/// Losslessness is asserted for every input, including ones the C++ lexer diagnoses. Round-tripping
/// and reporting cleanly are different properties, and an unterminated comment is the clearest case
/// where the first must hold while the second does not.
#[test]
fn comments_are_lossless() {
    for source in LOSS_CORPUS {
        let tree = parse(source);
        assert_eq!(
            tree.to_source_text(),
            *source,
            "{source:?} did not round-trip: {:#?}",
            tree.get_red_root()
        );
    }
}

/// Comments that are *valid* C++ parse without diagnostics.
#[test]
fn well_formed_comments_parse_cleanly() {
    for source in LOSS_CORPUS {
        // An unterminated comment is reported by the C++ lexer, and that is correct — see
        // `comments_are_lossless` for why it is still in the corpus.
        if source.contains("unterminated") {
            continue;
        }

        let tree = parse(source);
        assert!(
            tree.get_errors().is_empty(),
            "{source:?} should parse cleanly, got {:?}",
            tree.get_errors()
        );
    }
}

/// The text inside a comment node is exactly the comment's own text.
#[test]
fn a_comment_node_covers_exactly_its_comment() {
    let source = "int x;\n/// doc\n/// more\nint y;\n";
    let tree = parse(source);
    let comment = comments(&tree).first().cloned().expect("a comment");

    assert_eq!(
        subtree_text(&comment),
        "/// doc\n/// more",
        "the comment node must cover both lines and nothing else"
    );
}

// ============================================================================
// Grouping
// ============================================================================

/// Consecutive `///` lines are one document, and one node.
///
/// Splitting them would push the regrouping onto every consumer: "what does this declaration's
/// documentation say" would become a question about three nodes instead of one.
#[test]
fn adjacent_line_comments_form_one_comment() {
    let source = "/// one\n/// two\n/// three\nint x;\n";
    let tree = parse(source);

    let found = comments(&tree);
    assert_eq!(found.len(), 1, "expected one group, got {}", found.len());
    assert_eq!(subtree_text(&found[0]), "/// one\n/// two\n/// three");
}

/// A blank line ends the run. This is how Doxygen separates a file's documentation from a
/// declaration's, and it is the difference between documenting `x` and documenting the file.
#[test]
fn a_blank_line_ends_a_comment_group() {
    let source = "/// file header\n\n/// documents x\nint x;\n";
    let tree = parse(source);

    let found = comments(&tree);
    assert_eq!(
        found.len(),
        2,
        "a blank line should end the group: {:#?}",
        tree.get_red_root()
    );
    assert_eq!(subtree_text(&found[0]), "/// file header");
    assert_eq!(subtree_text(&found[1]), "/// documents x");
}

/// Comments on the same line are one group even without a newline between them.
#[test]
fn comments_on_one_line_are_one_group() {
    let source = "/* a */ /* b */\nint x;\n";
    let tree = parse(source);

    assert_eq!(comments(&tree).len(), 1, "{:#?}", tree.get_red_root());
}

/// A plain comment is still a comment node — it has to be, or its bytes would leave the tree — but
/// nothing inside it is documentation, so `@param` in it is not a command.
#[test]
fn a_plain_comment_is_not_parsed_for_commands() {
    let source = "// @param x not documentation\nint f(int x);\n";
    let tree = parse(source);

    let comment = comments(&tree).first().cloned().expect("a comment");
    assert!(
        command_names(&comment).is_empty(),
        "an ordinary `//` comment must not yield commands: {:#?}",
        comment
    );
    assert_eq!(subtree_text(&comment), "// @param x not documentation");
}

/// `////` is a banner, not documentation.
#[test]
fn a_banner_is_not_documentation() {
    let source = "//// section\n/// @param x real\nint f(int x);\n";
    let tree = parse(source);

    let found = comments(&tree);
    let with_commands: Vec<&cpp_parser::CppSyntaxNode> = found
        .iter()
        .filter(|node| !command_names(node).is_empty())
        .collect();

    assert_eq!(
        with_commands.len(),
        1,
        "only the `///` line documents: {found:#?}"
    );
    assert_eq!(command_names(with_commands[0]), vec!["param"]);
}

// ============================================================================
// Commands
// ============================================================================

#[test]
fn commands_are_parsed_with_their_arguments() {
    let source = "/// @param[in] x  the x\n/// @param[out] y the y\nint f(int x, int* y);\n";
    let tree = parse(source);

    let comment = comments(&tree).first().cloned().expect("a comment");
    assert_eq!(command_names(&comment), vec!["param", "param"]);
    assert_eq!(
        argument_of(&comment, "param"),
        Some("x".to_string()),
        "the argument is the name, not the name plus its description"
    );
}

#[test]
fn a_backslash_command_is_the_same_as_an_at_command() {
    let source = "/// \\brief Brief.\nint x;\n";
    let tree = parse(source);

    let comment = comments(&tree).first().cloned().expect("a comment");
    assert_eq!(command_names(&comment), vec!["brief"]);
}

/// The command name is matched case-insensitively, as Doxygen does. A file that writes `@Param`
/// documents its parameters, and losing that would be a silent hole in the documentation.
#[test]
fn command_names_are_case_insensitive() {
    assert_eq!(command_kind("param"), Some(DocCommandKind::Parameter));
    assert_eq!(command_kind("PARAM"), Some(DocCommandKind::Parameter));
    assert_eq!(command_kind("Param"), Some(DocCommandKind::Parameter));
    assert_eq!(command_kind("tparam"), Some(DocCommandKind::Parameter));
    assert_eq!(command_kind("returns"), Some(DocCommandKind::Description));
    assert_eq!(command_kind("throws"), Some(DocCommandKind::Exception));
    assert_eq!(command_kind("see"), Some(DocCommandKind::Reference));
    assert_eq!(command_kind("warning"), Some(DocCommandKind::Advisory));
    assert_eq!(command_kind("todo"), Some(DocCommandKind::Callout));
}

/// An unknown command is not an error: Doxygen has user-defined aliases, so a name the table does
/// not know is still a command, and its line is kept rather than dropped.
#[test]
fn an_unknown_command_is_still_a_command() {
    let source = "/// @myalias some text\nint x;\n";
    let tree = parse(source);

    assert!(
        tree.get_errors().is_empty(),
        "an alias must not be reported as broken: {:?}",
        tree.get_errors()
    );

    let comment = comments(&tree).first().cloned().expect("a comment");
    assert_eq!(command_names(&comment), vec!["myalias"]);
    assert!(
        subtree_text(&comment).contains("some text"),
        "an unknown command's line must be kept"
    );
    assert_eq!(command_kind("myalias"), None);
}

/// Text before the first command is the brief, whether or not `@brief` was written — Doxygen's own
/// rule, and the reason a doc comment that only writes `@param` still has a summary.
#[test]
fn leading_text_is_the_description() {
    let source = "/// Computes the area.\n/// @param r the radius.\ndouble area(double r);\n";
    let tree = parse(source);

    let comment = comments(&tree).first().cloned().expect("a comment");
    assert_eq!(command_names(&comment), vec!["param"]);
    assert!(
        subtree_text(&comment).contains("Computes the area."),
        "the leading text must be kept: {:#?}",
        comment
    );
}

/// A multi-line block comment's body continues across lines: `@brief one\n * two` is one brief.
#[test]
fn a_block_comment_body_continues_across_lines() {
    let source = "/**\n * @brief First line\n * second line\n */\nint x;\n";
    let tree = parse(source);

    let comment = comments(&tree).first().cloned().expect("a comment");
    let body = comment
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommandBody)
        .expect("a command body");

    assert!(
        subtree_text(&body).contains("second line"),
        "the block comment's second line belongs to the brief: {:#?}",
        body
    );
}

/// Inside `@code`, `@` and `//` are code. Reading a snippet as documentation turns
/// `std::vector<int>` into a reference to `std` and loses the snippet.
#[test]
fn a_code_block_is_not_parsed_as_documentation() {
    let source = "/// @code\n/// std::vector<int> v; // @notacommand\n/// @endcode\n/// @param x real\nint f(int x);\n";
    let tree = parse(source);

    let comment = comments(&tree).first().cloned().expect("a comment");
    assert_eq!(
        command_names(&comment),
        vec!["code", "param"],
        "only the real commands should be found: {:#?}",
        comment
    );

    let code = comment
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCodeBlock)
        .expect("a code block");
    assert!(
        subtree_text(&code).contains("std::vector<int>"),
        "the snippet must be kept verbatim: {:#?}",
        code
    );
}

// ============================================================================
// The tree's shape
// ============================================================================

/// A comment documents the declaration that follows it; it must not *contain* it.
///
/// This is the failure the whole marker discipline exists to prevent, and it is invisible in the
/// text: the tokens are all present and in order, but the tree claims the declaration is inside the
/// comment, so every later query about the declaration's scope is wrong.
#[test]
fn a_comment_does_not_swallow_the_declaration() {
    let source = "/// doc\n/// @param x the x\nint f(int x);\nint g;\n";
    let tree = parse(source);
    let root = tree.get_red_root();

    let comment = comments(&tree).first().cloned().expect("a comment");
    let comment_end = u32::from(comment.text_range().end());
    let declaration_start = source.find("int f").expect("the declaration") as u32;

    assert_eq!(
        comment_end,
        declaration_start - 1,
        "the comment ends after the newline that follows it, and no later: {:#?}",
        root
    );

    // The declarations are siblings of the comment, under the translation unit.
    let declarations: Vec<_> = root
        .children()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declaration)
        .collect();
    assert_eq!(
        declarations.len(),
        2,
        "both declarations must be top-level: {:#?}",
        root
    );
}

/// The doc layer emits into the C++ event stream, so an unpaired marker in it re-parents a subtree.
/// The audit reports unclosed nodes; this checks it stays empty for comments specifically.
#[test]
fn the_event_stream_stays_balanced_with_comments() {
    for source in LOSS_CORPUS {
        let (_, audit) = CppParser::parse_with_audit(source, ParserConfig::default());
        assert!(
            audit.is_balanced(),
            "{source:?} left the event stream unbalanced: {audit:?}"
        );
    }
}

/// Comments must still parse when they are the only thing in the file, or when a file ends in one.
/// A doc comment is written before the declaration it documents, so a file that is *only* a comment
/// is the state of a file someone has just started.
#[test]
fn a_comment_without_a_declaration_is_fine() {
    let cases = [
        "/// just a comment",
        "/// just a comment\n",
        "/// @param x nothing to document",
        "/** @brief nothing follows */",
        "// plain\n",
    ];

    for source in cases {
        let tree = parse(source);
        assert_eq!(tree.to_source_text(), *source, "{source:?} lost text");
        assert!(
            tree.get_errors().is_empty(),
            "{source:?} should parse cleanly: {:?}",
            tree.get_errors()
        );
        assert_eq!(comments(&tree).len(), 1, "{source:?}");
    }
}

/// Comments sit inside whatever they were found in, so a comment in a function body is a child of
/// the body and one between members is a child of the class body. That is what makes the tree usable
/// for "document this member".
#[test]
fn comments_attach_where_they_are_found() {
    let source = "class C {\npublic:\n    /// documents m\n    int m;\n};\n";
    let tree = parse(source);

    let comment = comments(&tree).first().cloned().expect("a comment");
    let parent = comment.parent().expect("a parent");

    assert_eq!(
        CppSyntaxKind::from(parent.kind()),
        CppSyntaxKind::ClassBody,
        "the comment belongs to the class body: {:#?}",
        tree.get_red_root()
    );
}

/// A trailing comment on the same line as a declaration stays in the tree and parses.
#[test]
fn a_trailing_comment_is_kept() {
    let source = "int x; ///< the x\nint y;\n";
    let tree = parse(source);

    assert_eq!(tree.to_source_text(), source);
    assert_eq!(comments(&tree).len(), 1);
}

/// The doc layer must survive whatever a user has typed. A comment is the most-edited part of a file
/// — it is where prose goes — so "does not panic and does not lose bytes" is the contract, not
/// "parses correctly".
///
/// The one exception is an unterminated `/*`, which the *C++* lexer reports and should: a block
/// comment that never closes has swallowed whatever came after it. That is a real problem with the
/// file, not a defect in this layer.
#[test]
fn half_typed_comments_do_not_break_anything() {
    let cases = [
        "/// @",
        "/// @param",
        "/// @param[",
        "/// @param[in",
        "/// @code",
        "/// @code\n/// int x =",
        "/** @brief",
        "/**\n *",
        "/// \\",
        "/// @@@@",
        "/// @ref",
        "/// @param x =",
        "///",
        "/// @endcode",
    ];

    for source in cases {
        let tree = parse(source);
        assert_eq!(
            tree.to_source_text(),
            source,
            "{source:?} lost text: {:#?}",
            tree.get_red_root()
        );

        // An unterminated `/*` is reported by the *C++* lexer, and should be: a block comment that
        // never closes has swallowed whatever came after it. That is a real problem with the file,
        // not a defect in this layer.
        if !source.starts_with("/*") || source.contains("*/") {
            assert!(
                tree.get_errors().is_empty(),
                "{source:?} is a comment, not an error: {:?}",
                tree.get_errors()
            );
        }

        let (_, audit) = CppParser::parse_with_audit(source, ParserConfig::default());
        assert!(audit.is_balanced(), "{source:?}: {audit:?}");
    }
}

/// Comments in the middle of real code, which is where the doc parser and the C++ parser have to
/// agree about who owns what.
#[test]
fn comments_between_tokens_do_not_disturb_the_cpp_grammar() {
    let cases = [
        "int /* c */ x;\n",
        "int x /* c */ ;\n",
        "void f(/* c */ int a);\n",
        "struct S { /// doc\n  int m; };\n",
        "int a; /// doc\n/// more\nint b;\n",
        "namespace n { /// doc\nint x; }\n",
        "#include <vector> /// doc\nint x;\n",
    ];

    for source in cases {
        let tree = parse(source);
        assert_eq!(tree.to_source_text(), *source, "{source:?} lost text");
        assert!(
            tree.get_errors().is_empty(),
            "{source:?} should parse cleanly: {:?}",
            tree.get_errors()
        );
    }
}

/// The realistic module unit, seen through the doc layer.
///
/// `tests/ast.rs` already parses this file for its declarations. This parses it for its
/// *documentation*, which is the harder half and the one that only works if the two layers agree
/// about who owns which bytes: the file documents a module, a namespace member, a class, a function
/// with parameters, a template with template parameters, and a method via `@copydoc` — with a
/// `@code` block spanning three `///` lines in the middle of it.
#[test]
fn a_realistic_module_unit_yields_its_documentation() {
    let source = include_str!("real_world.cpp");
    let tree = parse(source);

    assert_eq!(
        tree.get_errors(),
        [],
        "documentation must not turn valid code into a diagnostic: {:?}",
        tree.get_errors()
    );
    assert_eq!(tree.to_source_text(), source, "the tree must reproduce the file");

    let found = comments(&tree);
    assert!(
        found.len() >= 8,
        "expected a documentation block per documented entity, found {}: {:#?}",
        found.len(),
        tree.get_red_root()
    );

    // Collect every command in the file, with its arguments.
    let mut commands: Vec<(String, Vec<String>)> = Vec::new();
    for comment in &found {
        for command in comment
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommand)
        {
            let name = command
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| token.kind() == cpp_parser::CppTokenKind::DocCommandName.into())
                .map(|token| token.text().to_string())
                .unwrap_or_default();
            let args: Vec<String> = command
                .children()
                .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommandArg)
                .map(|arg| subtree_text(&arg))
                .collect();
            commands.push((name, args));
        }
    }

    // `@param x` and `@param y` of the constructor: the names, not the names plus their descriptions.
    assert!(
        commands.contains(&("param".to_string(), vec!["x".to_string()])),
        "the constructor's parameters must be documented by name: {commands:?}"
    );
    assert!(
        commands.contains(&("param".to_string(), vec!["y".to_string()])),
        "{commands:?}"
    );
    // `@tparam T` and `@tparam N`: the template's parameters, which `@param` does not cover.
    assert!(
        commands.contains(&("tparam".to_string(), vec!["T".to_string()])),
        "template parameters need their own command: {commands:?}"
    );
    // The cross-cutting ones.
    for name in ["file", "brief", "returns", "note", "warning", "code", "copydoc"] {
        assert!(
            commands.iter().any(|(command, _)| command == name),
            "`@{name}` should have been recognised: {commands:?}"
        );
    }

    // The `@code` block spans three `///` lines and must hold the whole snippet, not just one line of
    // it. `@endcode` is inside the node because it is a token of the block — it is what ends it — and
    // a consumer rendering the snippet strips the comment prefixes and the closing command itself.
    let block = found
        .iter()
        .flat_map(|comment| comment.descendants())
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCodeBlock)
        .expect("a code block");
    let block_text = subtree_text(&block);
    assert!(
        block_text.contains("Grid<double, 4> g;"),
        "the first line of the snippet must be inside the block: {block_text:?}"
    );
    assert!(
        block_text.contains("g.at(0) = 1.0;"),
        "so must the second — a block that ends at the first `///` line is the bug this covers: \
         {block_text:?}"
    );
    assert!(
        block_text.trim_end().ends_with("@endcode"),
        "the block ends at `@endcode`: {block_text:?}"
    );

    // `@param[in]` in the middle of a sentence, and `@brief` on a continuation line of a block
    // comment, are the two spellings most likely to be missed.
    assert!(
        commands
            .iter()
            .any(|(name, args)| name == "brief" && args.is_empty()),
        "{commands:?}"
    );
}
