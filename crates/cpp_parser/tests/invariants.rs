//! Structural invariants of the parser.
//!
//! Every test in this file encodes a property that must hold for **all** inputs, including
//! malformed and mid-edit ones. They are the contract the grammar is allowed to be wrong against,
//! but never allowed to break:
//!
//! * **I1 — losslessness.** The tree reproduces the input byte for byte. No token is dropped,
//!   duplicated or re-spanned.
//! * **I2 — well-formedness.** Sibling ranges are contiguous, non-overlapping and in source
//!   order, and every child is contained in its parent.
//! * **I3 — idempotence.** Re-parsing the text of a tree yields the same structure. This catches
//!   grammar functions that depend on parse history rather than on the token stream.
//! * **I4 — totality.** Parsing any input terminates and produces a `TranslationUnit` root.
//!
//! These are checked against a corpus that deliberately includes broken, truncated and
//! macro-heavy C++ — the inputs an editor actually sees.

use cpp_parser::{CppParser, CppSyntaxKind, CppSyntaxNode, CppSyntaxTree, ParserConfig};

/// Collects `(kind, start, end, depth)` for every node in the tree, in pre-order.
fn collect_nodes(root: &CppSyntaxNode) -> Vec<(CppSyntaxKind, usize, usize, usize)> {
    let mut out = Vec::new();
    let mut stack = vec![(root.clone(), 0usize)];

    while let Some((node, depth)) = stack.pop() {
        let range = node.text_range();
        out.push((
            CppSyntaxKind::from(node.kind()),
            usize::from(range.start()),
            usize::from(range.end()),
            depth,
        ));

        // Push children in reverse so the pop order is document order. `children()` is not a
        // `DoubleEndedIterator`, so collect first.
        let children: Vec<_> = node.children().collect();
        for child in children.into_iter().rev() {
            stack.push((child, depth + 1));
        }
    }

    out
}

/// I1: the tree's tokens, concatenated, are exactly the source text.
#[track_caller]
fn assert_lossless(source: &str, tree: &CppSyntaxTree) {
    assert_eq!(
        tree.to_source_text(),
        source,
        "I1 violated: the tree does not reproduce the input"
    );
    assert_eq!(
        tree.text_len(),
        source.len(),
        "I1 violated: tree length differs from input length"
    );
}

/// I2: children are contiguous, ordered, non-overlapping and nested inside their parent.
///
/// This walks the tree in document order and checks that consecutive siblings satisfy
/// `prev.end <= next.start`, that `next.start >= parent.start`, and that the last sibling ends
/// where the parent ends. A violation means the event stream lost or reordered a token, or a node
/// was closed at the wrong nesting level.
#[track_caller]
fn assert_well_formed(tree: &CppSyntaxTree) {
    let root = tree.get_red_root();

    // Exactly one translation unit, and it is the root: a nested or duplicated root makes every
    // downstream `ancestors()`/`get_root()` style query wrong.
    assert_eq!(
        root.descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TranslationUnit)
            .count(),
        1,
        "I2 violated: the tree contains more than one TranslationUnit"
    );

    for node in root.descendants() {
        let parent = node.text_range();
        let mut previous_end: Option<usize> = None;
        let mut first_start: Option<usize> = None;

        for child in node.children_with_tokens() {
            let range = child.text_range();
            let start = usize::from(range.start());
            let end = usize::from(range.end());

            assert!(
                start <= end,
                "I2 violated: inverted range {start}..{end} inside {node:?}"
            );
            assert!(
                start >= usize::from(parent.start()) && end <= usize::from(parent.end()),
                "I2 violated: child {start}..{end} escapes parent {}..{} inside {node:?}",
                usize::from(parent.start()),
                usize::from(parent.end())
            );

            if let Some(prev_end) = previous_end {
                assert!(
                    start >= prev_end,
                    "I2 violated: sibling overlap or out-of-order tokens ({prev_end} > {start}) \
                     inside {node:?}"
                );
            }

            first_start.get_or_insert(start);
            previous_end = Some(end);
        }

        // A node with children must be covered by them; a node's own range may be wider only if
        // it is a token-less wrapper, which does not exist in this tree, so require exact cover.
        if let (Some(first), Some(last)) = (first_start, previous_end) {
            assert_eq!(
                first,
                usize::from(parent.start()),
                "I2 violated: leading gap in {node:?}"
            );
            assert_eq!(
                last,
                usize::from(parent.end()),
                "I2 violated: trailing gap in {node:?}"
            );
        } else {
            assert!(
                parent.is_empty(),
                "I2 violated: childless node {node:?} covers {}..{}",
                usize::from(parent.start()),
                usize::from(parent.end())
            );
        }
    }
}

/// I4: any input parses to a translation unit, and every byte of it ends up in the tree.
#[track_caller]
fn assert_parses(source: &str) -> CppSyntaxTree {
    let tree = CppParser::parse(source, ParserConfig::default());
    assert_eq!(
        tree.root_kind(),
        CppSyntaxKind::TranslationUnit,
        "I4 violated: root is {:?}",
        tree.root_kind()
    );
    assert_lossless(source, &tree);
    assert_well_formed(&tree);
    tree
}

/// The corpus: valid C++, deliberately broken C++, and the macro-heavy shapes that break
/// context-free parsers.
const CORPUS: &[(&str, &str)] = &[
    ("empty", ""),
    ("whitespace only", "   \n\t\n  "),
    ("comment only", "// just a comment\n/* and a block */\n"),
    (
        "hello world",
        "#include <iostream>\n\nint main() {\n    std::cout << \"hi\" << std::endl;\n    return 0;\n}\n",
    ),
    (
        "class with access specifiers",
        "class Foo : public Bar, private Baz {\npublic:\n    Foo();\n    ~Foo();\n    int value() const noexcept;\nprivate:\n    int value_;\n};\n",
    ),
    (
        "templates and nested angles",
        "template <typename T, int N>\nstruct Vec {\n    T data[N];\n    auto begin() -> T* { return data; }\n};\n",
    ),
    (
        // A template-id nested inside another one, with a `>>` that closes both, *and* a non-type
        // argument after it. Kept in the corpus so the invariant tests cover it; see
        // `KNOWN_UNPARSED` for why it is not in the "parses cleanly" set.
        "nested template-id with non-type argument",
        "Vec<std::vector<int>, 3> v;\n",
    ),
    (
        "lambdas",
        "auto f = [x = 1](int y) mutable -> int { return x + y; };\n",
    ),
    (
        "raw strings",
        // Every one of these used to split at the first inner quote and re-lex the rest as C++.
        concat!(
            "auto a = R\"(plain)\";\n",
            "auto b = R\"(has \"quotes\" and \\backslashes)\";\n",
            "auto c = R\"delim(contains )\" without ending)delim\";\n",
            "auto d = u8R\"(utf8 raw)\";\n",
            "auto e = LR\"(wide raw)\";\n",
        ),
    ),
    (
        "prefixed literals",
        "auto a = u8\"utf8\";\nauto b = u\"utf16\";\nauto c = U\"utf32\";\nauto d = L\"wide\";\nauto e = u'c';\n",
    ),
    (
        "numbers and suffixes",
        "auto a = 1'000'000;\nauto b = 0xFF'FF;\nauto c = 0b1010'1010;\nauto d = 42_km;\nauto e = 1.5_deg;\n",
    ),
    (
        "line splices",
        "#define GREETING \\\n    \"hi\"\nint x = 1 + \\\n        2;\n",
    ),
    ("unicode identifiers", "int café = 1;\nint λ = 2;\nint \\u00e9 = 3;\n"),
    (
        "nested-looking block comment",
        "/* /* this does not nest */ int x;\n",
    ),
    (
        "preprocessor soup",
        "#pragma once\n#include <vector>\n#include \"local.h\"\n#define MAX(a, b) ((a) > (b) ? (a) : (b))\n#if defined(FOO) && FOO > 2\nextern \"C\" {\n#endif\nvoid f();\n#ifdef BAR\n}\n#endif\n",
    ),
    (
        "modules",
        "export module my.mod:part;\nimport <iostream>;\nimport :other;\nexport import std.core;\nexport {\n    void exported();\n}\n",
    ),
    (
        "namespace",
        "namespace a {\nint x;\n}\n",
    ),
    (
        "using declarations",
        "using std::vector;\n",
    ),
    (
        "enums",
        "enum Color { Red, Green = 2 };\n",
    ),
    ("missing closing brace", "int main() {\n    return 0;\n"),
    ("missing semicolon", "int x = 1\nint y = 2;\n"),
    ("stray closing brace", "int f() { return 1; } }\nint g();\n"),
    ("truncated mid-expression", "int f() { return a + \n"),
    ("unterminated string", "const char* s = \"oops\nint x;\n"),
    ("unterminated block comment", "int x; /* never closed\n"),
    ("garbage", "@#$%^&* \u{1F600} \u{4e2d}\u{6587} ???\n"),
    (
        "unbalanced preprocessor branch",
        "#if A\nvoid f() {\n#else\nvoid f() {}\n#endif\n}\n",
    ),
    (
        "macro that hides braces",
        "BEGIN_NAMESPACE(foo)\nclass A {};\nEND_NAMESPACE(foo)\n",
    ),
];

#[test]
fn corpus_satisfies_structural_invariants() {
    for (name, source) in CORPUS {
        let tree = assert_parses(source);
        // Sanity: the parse must at least not invent nodes, i.e. the node count is bounded by the
        // number of tokens, which itself is bounded by the source length.
        let nodes = collect_nodes(&tree.get_red_root());
        assert!(
            nodes.len() <= source.len() + 1,
            "{name}: tree has {} nodes for {} bytes, which suggests runaway node creation",
            nodes.len(),
            source.len()
        );
    }
}

/// I3: parsing the reconstruction of a tree gives the same shape.
#[test]
fn corpus_reparse_is_idempotent() {
    for (name, source) in CORPUS {
        let first = assert_parses(source);
        let text = first.to_source_text();
        let second = assert_parses(&text);

        let first_shape = format!("{:#?}", first.get_red_root());
        let second_shape = format!("{:#?}", second.get_red_root());

        assert_eq!(
            first_shape, second_shape,
            "{name}: re-parsing the tree's own text produced a different structure"
        );
    }
}

/// Truncation fuzz: every prefix of every sample must satisfy all invariants. Editors see these
/// on every keystroke, and this is the cheapest way to enumerate "mid-edit" states.
#[test]
fn every_prefix_of_every_sample_is_parseable() {
    for (name, source) in CORPUS {
        for (end, _) in source.char_indices() {
            let prefix = &source[..end];
            let tree = CppParser::parse(prefix, ParserConfig::default());

            assert_eq!(
                tree.to_source_text(),
                prefix,
                "{name}: I1 violated for prefix of length {end}"
            );
            assert_well_formed(&tree);
        }
    }
}

/// Deletion fuzz: dropping a single character must not break the invariants either.
#[test]
fn single_character_deletions_are_parseable() {
    for (name, source) in CORPUS {
        for (offset, ch) in source.char_indices() {
            let mut mutated = String::with_capacity(source.len());
            mutated.push_str(&source[..offset]);
            mutated.push_str(&source[offset + ch.len_utf8()..]);

            let tree = CppParser::parse(&mutated, ParserConfig::default());
            assert_eq!(
                tree.to_source_text(),
                mutated,
                "{name}: I1 violated after deleting {ch:?} at {offset}"
            );
            assert_well_formed(&tree);
        }
    }
}

/// The parser is deterministic: same input, same tree, regardless of how many times it runs or
/// what ran before it. Protects against global state creeping in (interners, caches, counters).
#[test]
fn parsing_is_deterministic() {
    for (name, source) in CORPUS {
        let first = format!("{:#?}", CppParser::parse(source, ParserConfig::default()).get_red_root());
        let second = format!("{:#?}", CppParser::parse(source, ParserConfig::default()).get_red_root());
        assert_eq!(first, second, "{name}: parse is not deterministic");
    }
}

/// Comments and whitespace must survive into the tree. It is tempting to drop trivia for a
/// cleaner AST, but an LSP needs it for hover, folding and formatting, and losing it silently
/// breaks I1.
#[test]
fn trivia_is_preserved() {
    let source = "// leading\nint x = 1; // trailing\n\n/* block */ int y;\n";
    let tree = assert_parses(source);
    let text = tree.to_source_text();

    for needle in ["// leading", "// trailing", "/* block */"] {
        assert!(
            text.contains(needle),
            "trivia {needle:?} was dropped from the tree"
        );
    }
}

/// The event stream must come out of recovery balanced: no node left open, and never a `NodeEnd`
/// for a node that was already closed. This is a stronger check than the tree invariants, because
/// an unbalanced stream still produces a *well-formed* tree — just one nested wrongly.
#[test]
fn recovery_leaves_the_event_stream_balanced() {
    for (name, source) in CORPUS {
        let (_, audit) = CppParser::parse_with_audit(source, ParserConfig::default());

        assert_eq!(
            audit.min_depth, 0,
            "{name}: recovery emitted a NodeEnd for a node that was already closed; \
             a whole subtree would be re-parented"
        );
        assert!(
            audit.unclosed.is_empty(),
            "{name}: {} node(s) left open ({:?}); everything after them would be swallowed",
            audit.unclosed.len(),
            audit.unclosed
        );
        assert!(
            audit.is_balanced(),
            "{name}: event stream is not balanced: {audit:?}"
        );
    }
}

/// A rough "is there anything here but trivia" check, used only to skip corpus entries that are
/// legitimate empty parses. It is intentionally crude: it strips line and block comments and asks
/// whether anything is left.
fn has_code(source: &str) -> bool {
    let mut rest = source;
    let mut out = String::new();

    while let Some(index) = rest.find("//").or_else(|| rest.find("/*")) {
        out.push_str(&rest[..index]);
        if rest[index..].starts_with("//") {
            match rest[index..].find('\n') {
                Some(newline) => rest = &rest[index + newline..],
                None => return !out.trim().is_empty(),
            }
        } else {
            match rest[index + 2..].find("*/") {
                Some(end) => rest = &rest[index + 2 + end + 2..],
                None => return !out.trim().is_empty(),
            }
        }
    }

    out.push_str(rest);
    !out.trim().is_empty()
}

/// Code that is valid C++ and must therefore parse without diagnostics.
///
/// This list is the parser's "do not regress" set. The grammar is still a skeleton, so most of the
/// corpus reports something — but a parser that complains about these would be useless in an
/// editor, and an editor-facing parser that reports false positives gets switched off by its
/// users long before its real capabilities matter.
const MUST_PARSE_CLEANLY: &[&str] = &[
    "empty",
    "whitespace only",
    "comment only",
    "hello world",
    "class with access specifiers",
    "templates and nested angles",
    "namespace",
    "using declarations",
    "enums",
];

/// Input that is genuinely malformed and must therefore leave a mark: either a diagnostic or an
/// `ErrorNode`/`MissingNode` in the tree.
///
/// The property under test is *not* "the parser is accurate" — it is "recovery never silently
/// swallows a problem". A parser that absorbs broken input without saying anything gives an editor
/// no way to show the user that something is wrong.
const MUST_REPORT: &[&str] = &[
    "missing closing brace",
    "missing semicolon",
    "stray closing brace",
    "truncated mid-expression",
    "unterminated string",
    "unterminated block comment",
    "garbage",
];

/// Malformed input the parser currently absorbs without comment, because the grammar it would need
/// to notice is not written yet, or because the construct is valid *preprocessor* code that the C++
/// grammar has no way to judge.
///
/// Listed explicitly so the gap is visible rather than implied, and so this test starts failing (in
/// the good direction) the moment the real handling lands.
const KNOWN_SILENT_ACCEPTANCE: &[&str] = &[
    // `#if`/`#else`/`#endif` with unbalanced braces across branches is something only a preprocessor
    // layer can evaluate; the parser sees directives as leaf nodes and braces that do not match. It
    // is not wrong to accept it — it is wrong to claim anything about it.
    "unbalanced preprocessor branch",
];

#[test]
fn valid_code_parses_cleanly() {
    for (name, source) in CORPUS {
        if !MUST_PARSE_CLEANLY.contains(name) {
            continue;
        }

        let tree = CppParser::parse(source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "{name}: valid code must parse cleanly, got {:?}",
            tree.get_errors()
        );
    }
}

#[test]
fn malformed_code_is_never_silently_accepted() {
    for (name, source) in CORPUS {
        if !MUST_REPORT.contains(name) {
            continue;
        }

        let tree = CppParser::parse(source, ParserConfig::default());
        if !tree.get_errors().is_empty() {
            continue;
        }

        let recoveries = tree
            .get_red_root()
            .descendants()
            .filter(|node| {
                matches!(
                    CppSyntaxKind::from(node.kind()),
                    CppSyntaxKind::ErrorNode | CppSyntaxKind::MissingNode
                )
            })
            .count();

        assert!(
            recoveries > 0 && has_code(source),
            "{name}: malformed input produced neither a diagnostic nor an error/missing node, \
             so the problem was silently swallowed"
        );
    }
}

/// Each entry in [`KNOWN_SILENT_ACCEPTANCE`] must still be silently accepted. When the real
/// declaration grammar lands and these start reporting, this test fails and the entry should be
/// moved into [`MUST_REPORT`] — that is the intended way for the gap to close.
#[test]
fn known_silent_acceptance_set_is_accurate() {
    for (name, source) in CORPUS {
        if !KNOWN_SILENT_ACCEPTANCE.contains(name) {
            continue;
        }

        let tree = CppParser::parse(source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "{name}: this input is now reported ({:?}); move it from KNOWN_SILENT_ACCEPTANCE to \
             MUST_REPORT",
            tree.get_errors()
        );
    }
}

/// Valid C++ the parser does not yet handle correctly. Each entry documents **what** is missing, so
/// the list can only shrink.
///
/// This is deliberately separate from [`MUST_PARSE_CLEANLY`]: that set is the "do not regress" line,
/// and it would be dishonest to keep a known-broken construct in it. The invariant tests still cover
/// these inputs, so they cannot make the tree *invalid* — only imprecise.
fn known_unparsed() -> [(&'static str, &'static str, &'static str); 2] {
    [
        (
            "nested template-id with non-type argument",
            "Vec<std::vector<int>, 3> v;",
            "Template argument lists track the depth of `<`/`>` to decide which `>` closes which \
             list. When an argument is itself a template-id and the list carries further arguments, \
             the inner list's closing `>` is mistaken for the outer list's and the declaration falls \
             back to the expression reading. `Vec<std::vector<int>> v;` and `Vec<A<int>, 3> v;` do \
             work; the combination of the two does not.",
        ),
        (
            "modules",
            "export module my.mod:part;\nimport <iostream>;\nimport :other;\nexport import std.core;\nexport {\n    void exported();\n}\n",
            "C++20 module declarations are not implemented. `module` and `import` are contextual \
             keywords, so they arrive as identifiers and need their own rules for the module \
             declaration, header-unit and partition forms, plus the `export` prefix and block. The \
             `SyntaxKind`s for these exist; the grammar does not.",
        ),
    ]
}

#[test]
fn known_unparsed_set_is_accurate() {
    for (name, source, reason) in known_unparsed() {
        let tree = CppParser::parse(source, ParserConfig::default());

        // The invariant tests cover these too; re-assert the minimum here so a change that makes
        // them parse *validly* is caught and the entry removed.
        assert_eq!(
            tree.to_source_text(),
            source,
            "{name}: losslessness broken on a known-unparsed input"
        );

        assert!(
            !tree.get_errors().is_empty(),
            "{name} now parses cleanly — remove it from known_unparsed. (Documented reason was: \
             {reason})"
        );
    }
}
