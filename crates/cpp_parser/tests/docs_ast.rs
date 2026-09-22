//! The typed AST layer over documentation comments.
//!
//! `CppDocComment` and the wrappers under it exist because a comment's shape is not obvious from its
//! text: a command's name is one token of a node that also owns its arguments and its body, a
//! `@param[in] x` is two argument nodes, and a comment's `///` markers are inside the tree because
//! the file has to round-trip. Each test here pins one of those, through the accessors a consumer
//! actually calls, on sources whose shape is pinned by `doc_shape.rs`.
//!
//! The tests deliberately drive the layer the way a consumer does — parse C++, find the `DocComment`
//! node, cast it — rather than building syntax nodes by hand, so a change to how the doc layer is
//! grafted into the tree fails here rather than in a downstream editor.

use cpp_parser::{
    CppAstNode, CppDocCodeBlock, CppDocCommand, CppDocCommandArg, CppDocCommandBody, CppDocComment,
    CppDocCommentBody, CppDocInline, CppDocItem, CppParser, CppSyntaxKind, CppSyntaxNode,
    CppSyntaxTree, DocCommandKind, ParserConfig,
};

fn parse(source: &str) -> CppSyntaxTree {
    CppParser::parse(source, ParserConfig::default())
}

/// The first node of `kind` in `source`, untyped.
fn find(source: &str, kind: CppSyntaxKind) -> CppSyntaxNode {
    parse(source)
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == kind)
        .unwrap_or_else(|| panic!("{source:?} contains no {kind:?}"))
}

/// The typed first comment of `source`.
fn first_comment(source: &str) -> CppDocComment {
    CppDocComment::cast(find(source, CppSyntaxKind::DocComment))
        .expect("a node of kind DocComment casts to CppDocComment")
}

/// The typed first command of `source`.
fn first_command(source: &str) -> CppDocCommand {
    first_comment(source)
        .get_commands()
        .next()
        .unwrap_or_else(|| panic!("{source:?} contains no command"))
}

// ============================================================================
// Casting
// ============================================================================

/// Every wrapper accepts its own kind and refuses the others.
///
/// The refusal half is not decoration: `cast` is what `CppAstChildren` calls on every step of an
/// iteration, so a wrapper that accepted a kind it does not model would silently produce typed
/// children of the wrong type.
#[test]
fn every_wrapper_casts_its_own_kind_and_refuses_the_others() {
    let source = "/// @param[in] x the x\nint f(int x);\n";
    let doc_comment = find(source, CppSyntaxKind::DocComment);

    assert!(CppDocComment::can_cast(CppSyntaxKind::DocComment));
    assert!(CppDocComment::cast(doc_comment.clone()).is_some());

    assert!(CppDocCommentBody::can_cast(CppSyntaxKind::DocCommentBody));
    assert!(CppDocCommentBody::cast(find(source, CppSyntaxKind::DocCommentBody)).is_some());

    assert!(CppDocCommand::can_cast(CppSyntaxKind::DocCommand));
    assert!(CppDocCommand::cast(find(source, CppSyntaxKind::DocCommand)).is_some());

    assert!(CppDocCommandArg::can_cast(CppSyntaxKind::DocCommandArg));
    assert!(CppDocCommandArg::cast(find(source, CppSyntaxKind::DocCommandArg)).is_some());

    // A node of a different kind is refused rather than wrapped.
    assert!(CppDocCommand::cast(doc_comment.clone()).is_none());
    assert!(!CppDocCommand::can_cast(CppSyntaxKind::DocComment));
    assert!(CppDocCommentBody::cast(doc_comment.clone()).is_none());
    assert!(CppDocCommandArg::cast(doc_comment.clone()).is_none());

    // `DocInline` and `DocCodeBlock` are not in this tree at all — the grammar produces the first
    // nowhere yet and the second only for `@code` — so their wrappers are pinned by kind alone.
    assert!(CppDocInline::can_cast(CppSyntaxKind::DocInline));
    assert!(CppDocInline::cast(doc_comment.clone()).is_none());
    assert!(CppDocCodeBlock::can_cast(CppSyntaxKind::DocCodeBlock));
    assert!(CppDocCodeBlock::cast(doc_comment).is_none());
}

// ============================================================================
// Commands and their names
// ============================================================================

/// A command's name is its name token, not the text of the node that owns it.
///
/// The node deliberately contains its argument and its body, so `text()` is `param x the x`; the
/// accessor exists so that matching a command against the command table is not a text search.
#[test]
fn a_command_is_named_by_its_name_token_not_by_its_text() {
    let command = first_command("/// @param x the x\nint f(int x);\n");

    assert_eq!(command.get_name().as_deref(), Some("param"));
    assert_eq!(
        command.get_name_token().expect("a name token").get_text(),
        "param",
        "the introducer is not part of the name"
    );

    let text = command.syntax().text().to_string();
    assert!(
        text.starts_with("param"),
        "the command node starts at its name: {text:?}"
    );
    assert_ne!(
        text, "param",
        "the node owns its argument and body, so its text is more than the name: {text:?}"
    );
}

/// A command with nothing after it reports no argument and no body rather than empty ones.
///
/// Half-written documentation is the normal state of a file being edited, and "how many parameters
/// does this document?" must not count a parameter that has not been typed yet.
#[test]
fn a_command_with_nothing_after_it_has_no_argument_and_no_body() {
    let bare = first_command("/// @param\nint f(int x);\n");

    assert!(bare.get_argument().is_none());
    assert_eq!(bare.get_argument_text(), None);
    assert!(bare.get_body().is_none());
    assert_eq!(bare.get_body_text(), None);
    assert_eq!(bare.get_kind(), Some(DocCommandKind::Parameter));
}

/// `@param[in] x desc` has two argument nodes, and the *name* is the last one.
///
/// The direction comes first because that is how Doxygen writes it, so "the argument" — the thing a
/// cross-reference resolves — is the last node, not the first.
#[test]
fn a_directional_parameter_has_two_arguments_and_the_name_is_the_last() {
    let command = first_command("/// @param[in] x the x\nint f(int x);\n");

    let arguments: Vec<String> = command.get_args().map(|arg| arg.get_text()).collect();
    assert_eq!(
        arguments,
        ["[in]", "x"],
        "the direction is an argument node of its own, before the name"
    );

    assert_eq!(command.get_argument_text().as_deref(), Some("x"));
    assert_eq!(
        command.get_argument().expect("a name argument").get_text(),
        "x"
    );
}

/// A bracketed argument reports its direction; a name argument reports none.
#[test]
fn a_direction_is_read_from_its_brackets() {
    let command = first_command("/// @param[in,out] x the x\nint f(int x);\n");
    let direction = command.get_args().next().expect("a direction argument");

    assert!(direction.is_direction());
    assert_eq!(direction.get_direction().as_deref(), Some("in,out"));

    let name = command.get_argument().expect("a name argument");
    assert!(!name.is_direction());
    assert_eq!(name.get_direction(), None);

    // An empty bracket is not a direction: `is_direction` is true exactly when there is something to
    // report, so the two accessors cannot disagree.
    let empty = first_command("/// @param[] x\nint f(int x);\n");
    let argument = empty.get_args().next().expect("an argument");
    assert_eq!(argument.get_text(), "[]");
    assert!(!argument.is_direction());
    assert_eq!(argument.get_direction(), None);
}

/// A description that follows an argument on the same line is the body, not part of the argument.
#[test]
fn the_body_is_the_description_and_not_the_argument() {
    assert_eq!(
        first_command("/// @param x the x\nint f(int x);\n")
            .get_body_text()
            .as_deref(),
        Some("the x")
    );

    // The grammar accepts one space or two after the argument, and neither belongs to the body.
    assert_eq!(
        first_command("/// @param x  the x\nint f(int x);\n")
            .get_body_text()
            .as_deref(),
        Some("the x")
    );

    // A command with no argument at all has the whole line as its body: the split is the grammar's
    // job, and it made none here.
    assert_eq!(
        first_command("/// @brief B\nint x;\n")
            .get_body_text()
            .as_deref(),
        Some("B")
    );
}

/// A command body node reports its own text, trimmed.
#[test]
fn a_body_node_reports_its_own_text() {
    let body = CppDocCommandBody::cast(find(
        "/** @brief B */\nint x;\n",
        CppSyntaxKind::DocCommandBody,
    ))
    .expect("a body node");
    assert_eq!(body.get_text(), "B");
    assert_eq!(
        body.syntax().text().to_string(),
        body.get_text(),
        "the grammar already keeps the layout around a body outside it"
    );

    // Prose before the first command is a body directly under the comment, with no command over it.
    let source = "/// Some prose.\nint x;\n";
    assert_eq!(first_comment(source).get_commands().count(), 0);
    assert_eq!(
        CppDocCommandBody::cast(find(source, CppSyntaxKind::DocCommandBody))
            .expect("a body node")
            .get_text(),
        "Some prose."
    );
}

/// Commands come back in source order, however many `///` lines they are spread over.
#[test]
fn commands_come_back_in_source_order() {
    let comment =
        first_comment("/// @brief B\n/// @param x the x\n/// @returns R\nint f(int x);\n");

    let names: Vec<String> = comment
        .get_commands()
        .filter_map(|c| c.get_name())
        .collect();
    assert_eq!(names, ["brief", "param", "returns"]);

    // The commands are nested inside the comment's body rather than beside the comment, which is why
    // the accessor walks descendants: a direct-children version would find none of them.
    let body = comment.get_body().expect("a body");
    assert!(
        body.syntax()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommand),
        "the commands live inside the body"
    );
}

/// A command is found by name, and by what its name means.
#[test]
fn a_command_is_found_by_name_and_by_kind() {
    let source =
        "/// @brief B\n/// @param x the x\n/// @param y the y\n/// @note N\nint f(int x, int y);\n";
    let comment = first_comment(source);

    assert_eq!(
        comment
            .get_command("param")
            .expect("a param command")
            .get_argument_text()
            .as_deref(),
        Some("x"),
        "the first command of that name wins"
    );
    assert!(comment.get_command("nope").is_none());
    assert!(
        comment.get_command("Param").is_none(),
        "the tree records `param`, so a lookup by text is case-sensitive"
    );

    let parameters: Vec<String> = comment
        .get_commands_of_kind(DocCommandKind::Parameter)
        .filter_map(|command| command.get_argument_text())
        .collect();
    assert_eq!(parameters, ["x", "y"]);

    assert_eq!(
        comment.get_command("brief").expect("a brief").get_kind(),
        Some(DocCommandKind::Description)
    );
    assert_eq!(
        comment.get_command("note").expect("a note").get_kind(),
        Some(DocCommandKind::Advisory)
    );
    assert!(
        comment
            .get_command("brief")
            .expect("a brief")
            .is_description()
    );
    assert!(!comment.get_command("note").expect("a note").is_callout());

    // The *kind* is case-insensitive even though the name lookup is not: Doxygen treats `@Param` and
    // `@param` as one command, and that is the command table's rule rather than the tree's.
    let shouted = first_command("/// @Param x the x\nint f(int x);\n");
    assert_eq!(shouted.get_name().as_deref(), Some("Param"));
    assert_eq!(shouted.get_kind(), Some(DocCommandKind::Parameter));

    // A name the table does not know is still a command; it is simply one with no kind.
    let alias = first_command("/// @whatever text\nint x;\n");
    assert_eq!(alias.get_kind(), None);
    assert_eq!(alias.get_body_text().as_deref(), Some("text"));
}

/// A callout is recognised as one, and asking for a kind nothing has yields an empty iterator.
#[test]
fn callouts_and_empty_iterators() {
    let comment = first_comment("/// @brief B\n/// @todo T\nint x;\n");

    assert!(comment.get_command("todo").expect("a todo").is_callout());
    assert_eq!(
        comment
            .get_commands_of_kind(DocCommandKind::Parameter)
            .count(),
        0
    );
    assert!(comment.get_command("param").is_none());
}

// ============================================================================
// The comment itself
// ============================================================================

/// The comment has one body child, and the body is what covers the comment's bytes.
#[test]
fn the_body_is_the_comment_content() {
    let comment = first_comment("/// @brief B\nint x;\n");
    let body = comment.get_body().expect("a body");

    assert!(CppDocCommentBody::can_cast(CppSyntaxKind::DocCommentBody));
    assert_eq!(body.get_text(), "/// @brief B");

    // The comment node has no direct tokens of its own: its opening `///` is inside the body, which
    // is what makes the body the thing a content walk starts from.
    assert!(
        comment
            .syntax()
            .children_with_tokens()
            .all(|element| element.as_token().is_none()),
        "the comment holds its tokens in the body, not directly"
    );
}

/// The openers that document are documentation; the ones that only look like it are not.
#[test]
fn documentation_openers_are_recognised() {
    for source in [
        "/// doc\nint x;\n",
        "//! doc\nint x;\n",
        "/** doc */\nint x;\n",
        "/*! doc */\nint x;\n",
    ] {
        assert!(
            first_comment(source).is_documentation(),
            "{source:?} documents"
        );
    }

    for source in [
        "// plain\nint x;\n",
        "//// banner\nint x;\n",
        "/**/\nint x;\n",
        "/*!*/\nint x;\n",
    ] {
        assert!(
            !first_comment(source).is_documentation(),
            "{source:?} is not documentation"
        );
    }
}

/// A comment that is not documentation has text and a body, and no commands at all.
#[test]
fn a_plain_comment_has_no_commands() {
    let comment = first_comment("// @param x not documentation\nint x;\n");

    assert!(!comment.is_documentation());
    assert!(comment.get_body().is_some());
    assert_eq!(
        comment.get_commands().count(),
        0,
        "`@param` in a `//` comment is prose, not a command"
    );
    assert!(comment.get_command("param").is_none());
    assert_eq!(comment.get_comment_text(), "@param x not documentation");
}

/// The comment's text is what a human reads: delimiters and line markers stripped, lines joined.
#[test]
fn comment_text_strips_delimiters_and_line_markers() {
    assert_eq!(
        first_comment("/// @brief B\nint x;\n").get_comment_text(),
        "@brief B"
    );
    assert_eq!(
        first_comment("/// @brief B\n/// @param x the x\nint f(int x);\n").get_comment_text(),
        "@brief B\n@param x the x",
        "one line per source line, in order"
    );
    assert_eq!(
        first_comment("/** @brief B */\nint x;\n").get_comment_text(),
        "@brief B",
        "the opener and the closer go, and the layout they sat in does not become a blank"
    );
    assert_eq!(
        first_comment("/**\n * @brief First\n * second\n */\nint x;\n").get_comment_text(),
        "@brief First\nsecond",
        "the `*` continuation marker goes and a continued line joins the one above it"
    );
    assert_eq!(
        first_comment("// plain\nint x;\n").get_comment_text(),
        "plain"
    );
    assert_eq!(
        first_comment("/**/\nint x;\n").get_comment_text(),
        "",
        "an empty comment renders as empty rather than as its delimiters"
    );
}

// ============================================================================
// Code blocks
// ============================================================================

/// A code block's text is the code, with the comment's line markers removed.
#[test]
fn code_text_strips_the_comment_prefixes() {
    let source = "/// @code\n/// int y = 1;\n/// int z = 2;\n/// @endcode\nint x;\n";
    let command = first_command(source);
    let block = command.get_code_block().expect("a code block");

    assert_eq!(
        block.get_code_text(),
        "int y = 1;\nint z = 2;",
        "the tree keeps every `///` for losslessness; this accessor is what removes them"
    );

    // The command's accessor is the same rule, so a caller that has the command does not need the
    // block node to get the code.
    assert_eq!(command.get_code_text(), Some(block.get_code_text()));
}

/// The code is returned as it was written: indentation and punctuation are not the comment's.
#[test]
fn code_text_keeps_the_code_as_written() {
    // Indentation past the marker's single space is the code's own.
    assert_eq!(
        first_command("/// @code\n///     indented();\n/// @endcode\nint x;\n")
            .get_code_text()
            .as_deref(),
        Some("    indented();")
    );

    // In a `///` comment a leading `*` is code — this is a pointer dereference — so it survives.
    assert_eq!(
        first_command("/// @code\n/// *p = 1;\n/// @endcode\nint x;\n")
            .get_code_text()
            .as_deref(),
        Some("*p = 1;")
    );

    // In a block comment the same character is the `*` line marker, so it goes.
    assert_eq!(
        first_command("/**\n * @code\n * int y = 1;\n * @endcode\n */\nint x;\n")
            .get_code_text()
            .as_deref(),
        Some("int y = 1;")
    );
}

/// A block that was never closed still reports its code.
#[test]
fn an_unterminated_code_block_still_reports_its_code() {
    let command = first_command("/// @code\n/// int y = 1;\nint x;\n");

    assert!(command.get_code_block().is_some());
    assert_eq!(command.get_code_text().as_deref(), Some("int y = 1;"));

    // A command that opens no block reports no code rather than an empty snippet.
    let brief = first_command("/// @brief B\nint x;\n");
    assert!(brief.get_code_block().is_none());
    assert_eq!(brief.get_code_text(), None);
}

// ============================================================================
// The sum type
// ============================================================================

/// The sum type covers the items a comment contains, in order, and only those.
#[test]
fn the_sum_type_covers_the_items_of_a_comment() {
    let source = "/// @param x the x\n/// @code\n/// int y = 1;\n/// @endcode\nint f(int x);\n";
    let items: Vec<CppDocItem> = first_comment(source).get_items().collect();

    let shape: Vec<&str> = items
        .iter()
        .map(|item| match item {
            CppDocItem::Command(_) => "command",
            CppDocItem::Body(_) => "body",
            CppDocItem::Inline(_) => "inline",
            CppDocItem::CodeBlock(_) => "code",
        })
        .collect();
    assert_eq!(shape, ["command", "body", "command", "code"]);

    assert!(
        matches!(items.first(), Some(CppDocItem::Command(command)) if command.get_name().as_deref() == Some("param")),
        "the first item is the command that comes first in the source"
    );

    // The sum type is over the items, not over the nodes that contain them.
    assert!(CppDocItem::can_cast(CppSyntaxKind::DocCommand));
    assert!(!CppDocItem::can_cast(CppSyntaxKind::DocComment));
    assert!(!CppDocItem::can_cast(CppSyntaxKind::DocCommandArg));
    assert!(CppDocItem::cast(find(source, CppSyntaxKind::DocComment)).is_none());
}
