//! Macros: the names a file `#define`s, and the shapes their invocations take.
//!
//! A macro is expanded in translation phase 4, so by the time the grammar runs an invocation has left behind
//! whatever the macro expanded to — a specifier, a statement, a whole block, or nothing. The parser does not run
//! the preprocessor (see `crate::grammar::cpp::stats`), and it does not need to: what it was missing is the
//! **name**.
//!
//! ```cpp
//! #define NUMBER_OPTION(op) if (auto v = Get(op); !v.empty()) { … }
//!
//! NUMBER_OPTION(tab_width)      // no `;` — the macro's body is a whole statement
//! g(tab_width)                  // no `;` — a typo, and the only reading is an error
//! ```
//!
//! The two lines are the same tokens up to the name, and no amount of token inspection separates them. A parser
//! that guesses from the *spelling* (all caps) accepts both and hides the typo; a parser that knows which names
//! this file defined accepts the first and reports the second. That difference is the whole subject of this file,
//! and each test below has its negative half: the reading is taken for a defined macro and **not** for anything
//! else.

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

#[test]
fn a_defined_macro_may_be_a_statement_without_a_semicolon() {
    // The shape the whole table exists for: the macro's body is a complete statement, so the invocation needs no
    // `;` of its own. `LuaStyle.cpp` writes 38 of these (`BOOL_OPTION(x)` and `NUMBER_OPTION(y)`), and each one
    // used to be reported as a call with its `;` missing — 64 diagnostics with the cascades.
    parses(
        "#define NUMBER_OPTION(op) if (auto v = Get(op); !v.empty()) { }\nvoid f() {\n    NUMBER_OPTION(tab_width)\n}\n",
    );
    parses(
        "#define BOOL_OPTION(op) if (auto v = Get(op); !v.empty())\nvoid f() {\n    BOOL_OPTION(a)\n    BOOL_OPTION(b)\n}\n",
    );

    let source = "#define BOOL_OPTION(op) op\nvoid f() {\n    BOOL_OPTION(a)\n}\n";
    assert_eq!(count(source, CppSyntaxKind::MacroCall), 1);
    assert_eq!(
        text_of(source, CppSyntaxKind::MacroCall),
        "BOOL_OPTION(a)\n",
        "the macro call is one node, and the statement ends where the macro's body does"
    );

    // …and the **negative half**, which is the reason the reading is not taken for every all-caps name: without a
    // `#define` in the file there is no evidence, and a call whose `;` is missing is exactly what it looks like.
    for source in [
        "void f() {\n    NUMBER_OPTION(tab_width)\n}\n",
        "void f() {\n    BOOL_OPTION(a)\n    BOOL_OPTION(b)\n}\n",
        "void f() {\n    g(x)\n    h(y)\n}\n",
    ] {
        assert_ne!(
            tree(source).get_errors(),
            [],
            "{source:?} has no macro in sight and must stay an error"
        );
    }
}

#[test]
fn an_undef_takes_the_evidence_away() {
    // `#define` takes effect for the rest of the file and `#undef` takes it away, so the table follows both. A
    // file that undefines a name and then writes `NAME(x)` without a `;` has a typo, not a macro.
    parses("#define FOO(a) a\nvoid f() {\n    FOO(x);\n}\n");
    parses("#define FOO(a) a\n#undef FOO\nvoid f() {\n    FOO(x);\n}\n");

    let source = "#define FOO(a) a\n#undef FOO\nvoid f() {\n    FOO(x)\n}\n";
    assert_ne!(
        tree(source).get_errors(),
        [],
        "after `#undef` the name is not a macro any more"
    );
}

#[test]
fn a_macro_that_expands_to_a_block_takes_the_block_with_it() {
    // `#define IF_EXIST(op) if (…)` — the `{ … }` that follows the invocation is the *macro's* body, so it belongs
    // inside the macro's node. Read that way a consumer can skip the whole construct, and the statements inside
    // are still parsed as statements.
    let source = "#define IF_EXIST(op) if (auto v = Get(op); !v.empty())\nvoid f() {\n    IF_EXIST(a) {\n        g();\n    }\n}\n";
    parses(source);

    assert_eq!(count(source, CppSyntaxKind::MacroCall), 1);
    assert_eq!(
        text_of(source, CppSyntaxKind::MacroCall),
        "IF_EXIST(a) {\n        g();\n    }\n",
        "the block is part of the macro's node"
    );
    assert_eq!(count(source, CppSyntaxKind::CompoundStat), 2);

    // A macro that expands to a plain statement ends at its `;`, and the `;` belongs to the invocation.
    let source = "#define ASSERT(cond) do { } while (false)\nvoid f() {\n    ASSERT(x);\n}\n";
    parses(source);
    assert_eq!(text_of(source, CppSyntaxKind::MacroCall), "ASSERT(x);\n");
}

#[test]
fn a_macros_arguments_are_raw_tokens() {
    // A macro's parameters are pasted into identifiers, types and expressions alike, so nothing inside the
    // parentheses may be interpreted — `TEST(FormatPerformance, 1k_row)` is how gtest writes a test name, and
    // `1k_row` is not a value in any grammar. The group is read as balanced tokens for exactly this reason.
    for source in [
        "#define T(a, b) a##b\nvoid f() {\n    T(1k_row, x.y)\n}\n",
        "#define T(a) a\nvoid f() {\n    T(std::vector<int> *)\n}\n",
        "#define T(...) 0\nvoid f() {\n    T(a, b, c)\n}\n",
    ] {
        parses(source);
        assert_eq!(count(source, CppSyntaxKind::MacroCall), 1, "{source:?}");
    }
}

#[test]
fn the_table_speaks_for_this_file_only() {
    // The boundary the table documents, pinned: a macro from an *included header* is not in it, so the
    // declaration-level shapes keep their spelling fallback — gtest's `TEST(A, B) { … }` has no `#define` here
    // and is still read, because at declaration level the shape has no other reading at all.
    parses("TEST(A, B) { }");
    parses("TEST(FormatPerformance, 1k_row) { int x = 1; }");
    parses("namespace n { TEST(A, B) { } }");
    assert_eq!(count("TEST(A, B) { }", CppSyntaxKind::CompoundStat), 1);
}
