//! The *shape* of a documentation comment's tree.
//!
//! The other doc tests ask whether individual accessors find the right thing. These ask a different
//! question: is the tree the same shape every time? A consumer that walks comments has to walk them
//! the same way for every file, so an inconsistency between two spellings of the same thing —
//! `/// @param x` and `/** @param x */` — is a defect even when both "work".
//!
//! # Why a rendered shape rather than assertions on accessors
//!
//! `get_name()` returning `Some("x")` says nothing about where `x` sits, whether it is wrapped, or
//! whether a sibling appeared. Rendering the whole subtree as one line makes all of that visible at
//! once, and makes a change to the shape show up as a reviewable diff rather than as a silent
//! re-parenting that every existing assertion still passes.
//!
//! # Reading the rendering
//!
//! `KIND[tokens](children)` — a node, the non-layout token text it directly contains, and then its
//! children, recursively. Whitespace and newlines are dropped: they are layout, they are asserted for
//! losslessness elsewhere, and including them would bury the structure in noise. A run of whitespace
//! *inside* a token's text is collapsed to one space and the ends are trimmed, for the same reason:
//! `@brief B` and `@brief  B` differ in their bytes and not in their shape.
//!
//! Two things follow from that and are worth knowing when reading an expectation:
//!
//! * The `DocComment` itself has no direct tokens. Its opening `///` is inside the body, which is what
//!   makes the body the thing a consumer walks for content.
//! * Where a node's text starts depends on the node. A `DocCommand` starts at the command name, so
//!   `@` is not in it; a `DocCommandBody` starts at whatever the command's arguments did not take,
//!   which for a description on the same line as its command is the whole description.

use cpp_parser::{CppKind, CppParser, CppSyntaxKind, CppSyntaxTree, CppTokenKind, ParserConfig};

fn parse(source: &str) -> CppSyntaxTree {
    CppParser::parse(source, ParserConfig::default())
}

/// The node kinds and token text of a subtree, as one line.
fn render(node: &cpp_parser::CppSyntaxNode, out: &mut String) {
    for child in node.children() {
        let kind = format!("{:?}", CppSyntaxKind::from(child.kind()));
        let tokens = direct_tokens(&child);

        out.push_str(&format!("{kind}[{tokens}]"));

        let has_children = child.children().next().is_some();
        if has_children {
            out.push('(');
            render(&child, out);
            out.push(')');
        }
        out.push(' ');
    }
    while out.ends_with(' ') {
        out.pop();
    }
}

/// The shape of the documentation attached to the first declaration, with the declaration itself
/// left out: these tests are about the comment's shape, not the C++ grammar's.
fn doc_shape(source: &str) -> String {
    let tree = parse(source);
    assert_eq!(
        tree.get_errors(),
        [],
        "the input must parse cleanly for its shape to mean anything: {:?}",
        tree.get_errors()
    );
    assert_eq!(
        tree.to_source_text(),
        source,
        "the shape must be of a tree that still reproduces the file"
    );

    let comment = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocComment)
        .expect("a documentation comment");

    // The comparison starts at the comment, so the helper has to render *it* rather than its
    // children — otherwise every expectation would be missing its outermost node.
    //
    // The comment's own opening token is *not* a direct token of it: it belongs to the body, which is
    // what makes the body the thing a consumer walks for content.
    let kind = format!("{:?}", CppSyntaxKind::from(comment.kind()));
    let mut inner = String::new();
    render(&comment, &mut inner);
    format!("{kind}({inner})")
}

/// The non-layout token text a node directly contains.
///
/// A comment node covers text that includes layout — `DocCommentBody` holds the `///` of every line —
/// so the text is flattened here: whitespace runs become one space, and the ends are trimmed. The shape
/// is then about *structure*: a consumer reads the same tree from `/// @brief B` and
/// `/** @brief B */`, and the rendering should say so.
fn direct_tokens(node: &cpp_parser::CppSyntaxNode) -> String {
    let raw: String = node
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            !matches!(
                token.kind(),
                CppKind::Token(CppTokenKind::Whitespace)
                    | CppKind::Token(CppTokenKind::Newline)
            )
        })
        .map(|token| token.text().to_string())
        .collect();

    let mut out = String::with_capacity(raw.len());
    for word in raw.split_whitespace() {
        if !out.is_empty() && !out.ends_with(['/', '@']) {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// The shape of a comment's *content*: the nodes inside its body, without the body node itself.
///
/// This is what a consumer walks for commands, and it is the thing the two spellings of a comment
/// agree on — the body's own first token is the comment's opener, which is the one difference that is
/// not structural.
fn doc_content_shape(source: &str) -> String {
    let tree = parse(source);
    assert_eq!(tree.to_source_text(), source);

    let body = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommentBody)
        .expect("a comment body");

    let mut inner = String::new();
    render(&body, &mut inner);
    inner
}

// ============================================================================
// The document as a whole
// ============================================================================

/// One comment, one body, one command per line — the shape every other case is a variation of.
#[test]
fn one_comment_is_one_body_of_commands() {
    assert_eq!(
        doc_shape("/// @param x the x\nint f(int x);\n"),
        "DocComment(DocCommentBody[///@](DocCommand[param](DocCommandArg[x] \
         DocCommandBody[the x])))"
    );
}

/// Several `///` lines are **one** body, not one body each.
///
/// This is the property a consumer depends on most: "give me this declaration's commands" has to be a
/// single walk, not a walk per line. It was one body per comment until the grouping was fixed, and
/// the shape looked plausible either way.
#[test]
fn several_lines_are_one_body() {
    assert_eq!(
        doc_shape("/// @brief B\n/// @param x the x\n/// @returns R\nint f(int x);\n"),
        "DocComment(DocCommentBody[///@///@///@](DocCommand[brief](DocCommandBody[B]) \
         DocCommand[param](DocCommandArg[x] DocCommandBody[the x]) \
         DocCommand[returns](DocCommandBody[R])))"
    );
}

/// A block comment and the equivalent line comments produce **the same content**.
///
/// The spelling differs; the structure must not. A consumer that handled only one of them would work
/// on half of a real codebase. The comment's own opener — `///` against `/**` — is the one difference
/// that is not structural, so the comparison starts inside the body.
#[test]
fn line_and_block_spellings_agree() {
    let line = doc_content_shape("/// @brief B\n/// @param x the x\nint f(int x);\n");
    let block = doc_content_shape("/** @brief B\n *  @param x the x\n */\nint f(int x);\n");

    assert_eq!(
        line, block,
        "the two spellings of the same documentation must not differ in shape"
    );
    assert_eq!(
        line,
        "DocCommand[brief](DocCommandBody[B]) DocCommand[param](DocCommandArg[x] \
         DocCommandBody[the x])"
    );
}

// ============================================================================
// Commands and their pieces
// ============================================================================

/// The argument is the name and nothing else; the description is a sibling body.
///
/// Both spellings — one space or two — have to give the same split, because Doxygen accepts either
/// and only the double space is *required*.
#[test]
fn the_argument_and_its_description_are_separate() {
    let one_space = doc_shape("/// @param x the x\nint f(int x);\n");
    let two_spaces = doc_shape("/// @param x  the x\nint f(int x);\n");

    assert_eq!(one_space, two_spaces, "one space or two must not matter");
    assert!(
        one_space.contains("DocCommandArg[x] DocCommandBody[the x]"),
        "the name and its description must be separate nodes: {one_space}"
    );
}

/// A command with no argument has no argument node, rather than an empty one.
#[test]
fn a_command_without_an_argument_has_no_argument_node() {
    assert_eq!(
        doc_shape("/// @brief B\nint x;\n"),
        "DocComment(DocCommentBody[///@](DocCommand[brief](DocCommandBody[B])))"
    );

    // A parameter command written with no name is the same: the node is absent, not empty. An empty
    // node would make "how many parameters does this document?" answer one too many.
    assert_eq!(
        doc_shape("/// @param\nint f(int x);\n"),
        "DocComment(DocCommentBody[///@](DocCommand[param]))"
    );
}

/// The `[in]` direction is an argument node of its own, *before* the name.
#[test]
fn a_direction_is_an_argument_before_the_name() {
    assert_eq!(
        doc_shape("/// @param[in] x the x\nint f(int x);\n"),
        "DocComment(DocCommentBody[///@](DocCommand[param](DocCommandArg[[in]] DocCommandArg[x] \
         DocCommandBody[the x])))"
    );
}

/// Prose before the first command is a body directly under the comment — the brief, in Doxygen's
/// terms — and it does not get a command node, because there is no command.
#[test]
fn leading_prose_is_a_body_without_a_command() {
    assert_eq!(
        doc_shape("/// Some prose.\nint x;\n"),
        "DocComment(DocCommentBody[///](DocCommandBody[Some prose.]))"
    );
}

/// An inline reference is one argument node holding the whole target, qualification and all.
#[test]
fn a_reference_keeps_its_qualification() {
    assert_eq!(
        doc_shape("/// @ref ns::Foo<T>\nint x;\n"),
        "DocComment(DocCommentBody[///@](DocCommand[ref](DocCommandArg[ns::Foo<T>])))"
    );
}

// ============================================================================
// The constructs that span comments
// ============================================================================

/// A code block is one node covering the whole snippet, and it does not get a command body.
#[test]
fn a_code_block_spans_its_comments() {
    assert_eq!(
        doc_shape("/// @code\n/// int y = 1;\n/// int z = 2;\n/// @endcode\nint x;\n"),
        "DocComment(DocCommentBody[///@](DocCommand[code] \
         DocCodeBlock[///int y = 1;///int z = 2;///@endcode]))"
    );
}

/// A code block that is never closed still produces exactly one block node.
///
/// Half-written documentation is the normal state of a file being edited, so the shape has to be
/// stable for it too — not an error, and not a node that grows to swallow the rest of the comment.
#[test]
fn an_unterminated_code_block_is_still_one_node() {
    assert_eq!(
        doc_shape("/// @code\n/// int y = 1;\nint x;\n"),
        "DocComment(DocCommentBody[///@](DocCommand[code] DocCodeBlock[///int y = 1;]))"
    );
}

/// Commands after a code block are parsed again — the block does not leak.
#[test]
fn a_closed_code_block_does_not_swallow_what_follows() {
    assert_eq!(
        doc_shape("/// @code\n/// int y = 1;\n/// @endcode\n/// @param x the x\nint f(int x);\n"),
        "DocComment(DocCommentBody[///@///@](DocCommand[code] \
         DocCodeBlock[///int y = 1;///@endcode] DocCommand[param](DocCommandArg[x] \
         DocCommandBody[the x])))"
    );
}

// ============================================================================
// Comments that are not documentation
// ============================================================================

/// A plain comment has a comment and a body and nothing else: it is in the tree for losslessness, and
/// no command is looked for inside it.
#[test]
fn a_plain_comment_has_no_commands() {
    assert_eq!(
        doc_shape("// plain\nint x;\n"),
        "DocComment(DocCommentBody[//plain])"
    );

    // Even when it looks like documentation. `//` does not document, so `@param` in it is prose: one
    // text run, with no command and no argument node anywhere in it.
    //
    // Asserted on the node's text rather than on the rendering: the rendering normalises layout away,
    // and whether `@param` is separated from `x` by a space is not its business.
    let tree = parse("// @param x not documentation\nint x;\n");
    let body = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommentBody)
        .expect("a comment body");
    assert!(
        body.text().to_string().contains("@param x not documentation"),
        "the whole comment is prose: {:?}",
        body.text().to_string()
    );

    let plain = doc_shape("// @param x not documentation\nint x;\n");
    assert!(
        !plain.contains("DocCommand"),
        "a plain comment has no commands: {plain}"
    );
}

/// `////` is a banner, not documentation.
#[test]
fn a_banner_is_a_plain_comment() {
    assert_eq!(
        doc_shape("//// section\nint x;\n"),
        "DocComment(DocCommentBody[////section])"
    );
}

/// A comment with no text at all is still one comment node with one body, not an empty tree.
#[test]
fn an_empty_comment_keeps_its_shape() {
    assert_eq!(
        doc_shape("///\nint x;\n"),
        "DocComment(DocCommentBody[///])"
    );
    assert_eq!(
        doc_shape("/**/\nint x;\n"),
        "DocComment(DocCommentBody[/**/])"
    );
}

// ============================================================================
// The block comment's own delimiters
// ============================================================================

/// The closing `*/` is a token of the comment's body, not part of the last command's text.
///
/// Leaving it inside the last *command* makes `/** @brief B */` report a brief of `B */`, and a
/// consumer rendering that text prints the comment's terminator. The body still covers it — the body
/// covers the whole comment — which is why the check below is on the command's body rather than on
/// the rendering, where `*/` legitimately appears.
#[test]
fn a_block_comment_closer_is_outside_the_body() {
    let shape = doc_shape("/** @brief B */\nint x;\n");

    assert_eq!(
        shape,
        "DocComment(DocCommentBody[/**@*/](DocCommand[brief](DocCommandBody[B])))"
    );

    // What the brief *says* is `B`, and the comment's terminator is not in it. The failure this guards
    // against is `B */`; the rendering above already pins the exact text, so the point here is that the
    // terminator is excluded by the *range* rather than merely by how the rendering flattens layout.
    let tree = parse("/** @brief B */\nint x;\n");
    let body = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommandBody)
        .expect("a brief body");
    let text = body.text().to_string();
    assert_eq!(
        text.trim(),
        "B",
        "the body is the brief's text and nothing else: {text:?}"
    );
    assert!(
        !text.contains("*/"),
        "the comment's terminator must not be inside the body: {text:?}"
    );
}

/// The same for a body that runs across the continuation lines of a block comment.
///
/// The check is on the *body node's* text rather than on the rendering: the comment's closer is a
/// token of the comment's own body and does appear in the rendering, so searching the whole shape for
/// `*/` would fail on a tree that is entirely correct.
#[test]
fn a_multiline_block_body_stops_at_the_closer() {
    let tree = parse("/**\n * @brief First\n * second\n */\nint x;\n");
    let body = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommandBody)
        .expect("a brief body");

    let text = body.text().to_string();
    assert!(
        text.contains("First") && text.contains("second"),
        "the continuation lines belong to the brief: {text:?}"
    );
    assert!(
        !text.contains("*/"),
        "a body must not end with the comment's closer: {text:?}"
    );

    // And the tree it came from still reproduces the file: the closer is *somewhere*, just not there.
    assert_eq!(tree.to_source_text(), "/**\n * @brief First\n * second\n */\nint x;\n");
}

// ============================================================================
// Stability across many inputs
// ============================================================================

/// Whatever the input, the tree has these invariants.
///
/// The per-case tests above pin *what* the shape is for inputs we chose. This one pins the properties
/// that must hold for every input, including the ones that only a fuzzer would write — so a change
/// that produces a new shape somewhere still fails if that shape is malformed.
#[test]
fn every_doc_tree_has_the_invariants_a_consumer_relies_on() {
    let corpus = [
        "///\nint x;\n",
        "/// @\nint x;\n",
        "/// @param\nint f(int x);\n",
        "/// @param x\nint f(int x);\n",
        "/// @param x the x\nint f(int x);\n",
        "/// @param[in] x  the x\nint f(int x);\n",
        "/// @param[] x\nint f(int x);\n",
        "/// @code\nint x;\n",
        "/// @code\n/// y\n/// @endcode\nint x;\n",
        "/// @endcode\nint x;\n",
        "/// @code @endcode\nint x;\n",
        "/** @brief B */\nint x;\n",
        "/**\n * @brief B\n */\nint x;\n",
        "/**\n@brief B\n*/\nint x;\n",
        "/** */\nint x;\n",
        "/**/\nint x;\n",
        "// plain\nint x;\n",
        "//// banner\nint x;\n",
        "/// @ref a::b\n/// @note n\n/// @warning w\nint x;\n",
        "/// @brief B\n\n/// @note N\nint x;\n",
        "int x; ///< trailing\n",
        "struct S { /// doc\n int m; };\n",
        "/// @code\n",
        "///",
        "/**",
        "/** @brief",
    ];

    for source in corpus {
        let tree = parse(source);
        let root = tree.get_red_root();

        assert_eq!(
            tree.to_source_text(),
            source,
            "{source:?} was not reproduced by its tree"
        );

        let comments: Vec<_> = root
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocComment)
            .collect();

        // Every comment node has its text, wherever the comment's bytes ended up.
        for comment in &comments {
            assert!(
                !subtree_text_of(comment).is_empty(),
                "{source:?}: a comment node covers no text at all"
            );
        }

        // The structural invariants a consumer walks by.
        for comment in &comments {
            let bodies: Vec<_> = comment
                .children()
                .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommentBody)
                .collect();

            assert!(
                bodies.len() <= 1,
                "{source:?}: {} bodies under one comment; a consumer iterating commands must not \
                 have to walk a body per line: {:#?}",
                bodies.len(),
                comment
            );

            // Commands are found by *walking* the comment, and each one is inside the body rather
            // than beside it, so "the commands of this comment" is one query with one answer.
            if let Some(body) = bodies.first() {
                for command in body
                    .descendants()
                    .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommand)
                {
                    assert!(
                        command.parent().is_some(),
                        "{source:?}: a command with no parent"
                    );
                }
            }
        }

        // Every argument node sits under a command: an argument with no command would be a
        // cross-reference nobody can attribute.
        for argument in root
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommandArg)
        {
            let under_a_command = argument
                .ancestors()
                .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommand);
            assert!(
                under_a_command,
                "{source:?}: an argument node outside any command: {:#?}",
                argument
            );
        }

        // A code block is always inside a comment, never beside one: it is part of the documentation.
        for block in root
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCodeBlock)
        {
            let inside_a_comment = block
                .ancestors()
                .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocComment);
            assert!(
                inside_a_comment,
                "{source:?}: a code block outside any comment"
            );
        }

        // What a body *says* never begins or ends with the comment's layout.
        //
        // This is the property that makes `get_body()` safe to render: a consumer printing the text of
        // `/** @brief B */` must not print the space before the closer, and one printing the text of a
        // continuation body must not print the `*` of the line marker. Both used to happen, and both
        // were invisible in the tree — the bytes were present and the shape was well formed.
        //
        // A trailing *newline* is allowed, and only that: in `/** @brief B\n */` the line break is where
        // the description's line ends, which is what a consumer joins continuation lines on.
        for body in root
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommandBody)
        {
            let text = body.text().to_string();
            let trimmed = text.trim_end_matches([' ', '\t', '*']);
            assert_eq!(
                trimmed, text,
                "{source:?}: a body ends in the comment's layout, so a consumer rendering it prints \
                 that layout: {text:?}"
            );
            assert!(
                !text.starts_with([' ', '\t']),
                "{source:?}: a body starts with layout the command already implied: {text:?}"
            );
            assert!(
                !text.contains("*/"),
                "{source:?}: a body contains the comment's terminator: {text:?}"
            );
        }
    }
}

/// A command's name is its first token, and the node's text is the name plus everything it owns.
///
/// Both halves matter and they pull in opposite directions: the node has to *contain* its argument and
/// its body, or a consumer has to pair a node with whichever sibling follows it, and the name has to be
/// readable as a name, or matching it against the command table means reconstructing it from the text.
#[test]
fn a_command_is_named_by_its_first_token() {
    let tree = parse("/// @param x the x\nint f(int x);\n");
    let command = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommand)
        .expect("a command");

    let name = command
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == CppKind::Token(CppTokenKind::DocCommandName))
        .expect("a name token");
    assert_eq!(name.text().to_string(), "param");

    // Everything the command owns is inside it, arguments and body included.
    assert!(command.descendants().any(|node| {
        CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommandArg
    }));
    assert!(command.descendants().any(|node| {
        CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommandBody
    }));
}

/// The description a command reports never includes the layout that separates it from the command.
///
/// This is one rule for four spellings, and each of them used to fail it differently: the space before
/// a `*/`, the space after the command's name, the double space Doxygen requires after an argument, and
/// the `\n * ` of a continuation line. They are asserted together because a fix for one that breaks
/// another is exactly how the shape stops being predictable.
#[test]
fn a_description_never_includes_the_layout_around_it() {
    let cases = [
        ("/// @brief B\nint x;\n", "B"),
        ("/// @brief   B\nint x;\n", "B"),
        ("/** @brief B */\nint x;\n", "B"),
        ("/** @brief B  */\nint x;\n", "B"),
        ("/** @brief B\n */\nint x;\n", "B\n"),
        ("/// @param x the x\nint f(int x);\n", "the x"),
        ("/// @param x  the x\nint f(int x);\n", "the x"),
        // A `*` line marker in the middle of a block body *is* text, and stays in it: `/** @brief A\n *
        // B */` describes `A\n * B`. That is a different thing from the layout at the *end* of a body,
        // which is what every case above is about — and the rule that separates them is the one the
        // invariant test states: a body may not *end* in layout.
        ("/** @brief A\n * B */\nint x;\n", "A\n * B"),
    ];

    for (source, expected) in cases {
        let tree = parse(source);
        let body = tree
            .get_red_root()
            .descendants()
            .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocCommandBody)
            .unwrap_or_else(|| panic!("{source:?} has a body"));

        assert_eq!(
            body.text().to_string(),
            expected,
            "{source:?}: the body is what the description says, and nothing around it"
        );
    }
}

fn subtree_text_of(node: &cpp_parser::CppSyntaxNode) -> String {
    node.text().to_string()
}