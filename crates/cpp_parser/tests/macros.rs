//! **The macro table is gone.** What is left of "macros" in this parser, and what was given up.
//!
//! A macro is expanded in translation phase 4, so the grammar's input is the text *after* preprocessing: an
//! invocation has left behind whatever its body produced — a specifier, a statement, a whole block, or nothing at
//! all. This file used to be about the workaround for not having that: a table of the names a file `#define`s,
//! consulted so that `BOOL_OPTION(x)` with no `;` could be told from `g(x)` with its `;` missing.
//!
//! ```cpp
//! #define NUMBER_OPTION(op) if (auto v = Get(op); !v.empty()) { … }
//!
//! NUMBER_OPTION(tab_width)      // no `;` — the body supplies a whole statement
//! g(tab_width)                  // no `;` — a typo, and the only reading is an error
//! ```
//!
//! **The table was removed, not merely bypassed**, and the reason is on `ParserConfig`: the grammar reads a stream
//! whose offsets are not any file's, while a macro environment answers positionally in a *file's* coordinates, so
//! "is this name a macro here" was being asked against a ruler that does not measure the text in front of it. On
//! the stream this grammar is meant to read the question does not arise either — the invocation is not there.
//!
//! What survives is everything that never needed the table: the **shapes** a macro-shaped name can take, and the
//! spelling conventions that stand in for evidence where a shape has no other reading. Each test below says which
//! of the two it is pinning.

use cpp_parser::{CppParser, CppSyntaxKind, CppSyntaxTree, ParserConfig};

fn tree(source: &str) -> CppSyntaxTree {
    CppParser::parse(source, ParserConfig::default())
}

/// Parse, and require that the result is clean in **both** senses.
fn parses(source: &str) {
    let parsed = tree(source);
    assert_eq!(
        parsed.get_errors(),
        [],
        "{source:?} must parse cleanly, got {:?}",
        parsed.get_errors()
    );

    let unclaimed = parsed.get_red_root().descendants().any(|node| {
        matches!(
            CppSyntaxKind::from(node.kind()),
            CppSyntaxKind::ErrorNode | CppSyntaxKind::MissingNode
        )
    });
    assert!(!unclaimed, "{source:?} leaves an ErrorNode behind");
    assert_eq!(parsed.to_source_text(), source, "{source:?} stays lossless");
}

fn count(source: &str, kind: CppSyntaxKind) -> usize {
    tree(source)
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
        .count()
}

/// The source text of the first node of `kind`.
fn text_of(source: &str, kind: CppSyntaxKind) -> String {
    tree(source)
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == kind)
        .unwrap_or_else(|| panic!("no {kind:?} in {source:?}"))
        .text()
        .to_string()
}

/// **A `#define` in the file does not change a single reading**, which is the contract this file exists for now.
///
/// The pairs below are the same tokens with and without the directive, and both halves must give the same answer —
/// because the parser does not evaluate the directive, and the cooked stream would not contain the invocation in
/// the first place. The first pair is the case the old table was built for; the reading it bought is **given up**,
/// and pinning the loss is the point: on raw text `NUMBER_OPTION(tab_width)` with no `;` is an error, and the
/// `#define` above it does not rescue it.
#[test]
fn a_define_in_the_file_does_not_change_a_reading() {
    for with_define in [
        "void f() {\n    NUMBER_OPTION(tab_width)\n}\n",
        "#define NUMBER_OPTION(op) if (auto v = Get(op); !v.empty()) { }\nvoid f() {\n    NUMBER_OPTION(tab_width)\n}\n",
    ] {
        assert_ne!(
            tree(with_define).get_errors(),
            [],
            "{with_define:?}: a body that is a whole statement is what would make this legal, and reading it is \
             the preprocessor's job"
        );
    }

    // …and the other direction: a call with its `;` reads the same whether or not the name is `#define`d.
    for with_define in [
        "void f() {\n    FOO(x);\n}\n",
        "#define FOO(a) a\nvoid f() {\n    FOO(x);\n}\n",
    ] {
        parses(with_define);
    }
}

/// **The declaration-level shape reads cleanly, and it is worth being exact about what it reads as.**
///
/// gtest's `TEST(A, B) { … }` has no `#define` in the file that writes it — the macro is in an included header —
/// which is exactly the boundary the removed table could never cross. The reading is a **spelling convention**
/// (`looks_like_a_macro_name`), and the negative beside it is why one is needed: `g(x) { }` inside a body is a call
/// whose `;` is missing, followed by a block, and it stays an error.
///
/// What the convention buys here is **not** a `MacroCall`: the shape comes out a `Declaration` whose specifier is
/// the name and whose declarator holds the group, followed by the block — lossless, clean, every token present.
/// The old version of this test asserted no more than that, and the assertion below is deliberately about the
/// group's *contents* rather than about a node kind, because that is the part the grammar is responsible for.
#[test]
fn the_declaration_level_shape_reads_without_any_table() {
    parses("TEST(A, B) { }");
    parses("TEST(FormatPerformance, 1k_row) { int x = 1; }");
    parses("namespace n { TEST(A, B) { } }");
    assert_eq!(count("TEST(A, B) { }", CppSyntaxKind::CompoundStat), 1);

    // The name is not spelled like a macro, so the same shape is the mistake it looks like.
    assert_ne!(
        tree("void f() {\n    g(x) { }\n}\n").get_errors(),
        [],
        "a lowercase name followed by a block is a call with its `;` missing"
    );
}

/// **A macro's arguments are raw tokens**, and this half survives intact: the group is read as a balanced token
/// group and nothing inside it is interpreted.
///
/// A macro's parameters are pasted into identifiers, types and expressions alike, so the grammar must not read
/// them — `TEST(FormatPerformance, 1k_row)` is how gtest writes a test name, and `1k_row` is a `UserDefinedLiteral`
/// that means nothing on its own. What this pins is that the group comes out as **tokens of the group** rather than
/// as a parameter list the grammar tried to make sense of.
#[test]
fn a_macros_arguments_are_raw_tokens() {
    for source in [
        "TEST(1k_row, x.y) { }",
        "TEST(std::vector<int> *) { }",
        "TEST(a, b, c) { }",
    ] {
        parses(source);
        assert_eq!(
            count(source, CppSyntaxKind::ArgumentList),
            1,
            "{source:?}: the group is kept whole"
        );
        assert_eq!(
            count(source, CppSyntaxKind::ParameterList),
            0,
            "{source:?}: and nothing tried to read it as parameters"
        );
    }

    assert_eq!(
        text_of("TEST(FormatPerformance, 1k_row) { }", CppSyntaxKind::ArgumentList).trim_end(),
        "(FormatPerformance, 1k_row)",
        "…and the trivia the group swallowed is the only thing trimmed"
    );
}

/// **The `#undef` case and the `asm` case are the same fact**: a name the file defines is still just a name here.
///
/// `#define FOO(a) a` / `#undef FOO` / `FOO(x)` reads exactly as `FOO(x)` does with no directives at all, and
/// `#define asm(x) g(x)` does not stop `asm(1)` being the compiler's own statement. Saying so keeps the next reader
/// from "fixing" either one by looking the name up — which is the change this file records the removal of.
#[test]
fn a_defined_name_is_still_just_a_name() {
    parses("#define FOO(a) a\nvoid f() {\n    FOO(x);\n}\n");
    parses("#define FOO(a) a\n#undef FOO\nvoid f() {\n    FOO(x);\n}\n");

    let with = tree("#define asm(x) g(x)\nvoid f() { asm(1); }\n");
    let without = tree("void f() { asm(1); }\n");
    assert_eq!(
        with.get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::AsmStat)
            .count(),
        without
            .get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::AsmStat)
            .count(),
        "the directive makes no difference to the reading"
    );
    assert_eq!(
        text_of("#define asm(x) g(x)\nvoid f() { asm(1); }\n", CppSyntaxKind::AsmStat).trim_end(),
        "asm(1);",
        "the trivia after the `;` belongs to the enclosing node, so a node's text may end in it"
    );
}
