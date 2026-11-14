//! **C++26 static reflection** (P2996): the two operators and the positions they are written in.
//!
//! ```text
//! ^^T                 a reflection: a value of type std::meta::info representing T
//! [: r :]             a splice: where the reflection stands, what it represents stands
//! typename [: r :]    a type splice (a type)
//! d.[: r :]           a member splice (a member)
//! typename [: r :]<…> a splice specialization
//! ```
//!
//! None of this can be compiled here — GCC 16 is not available on this machine — so the tests pin the *syntax*: the
//! tokens, the nodes, and that nothing else in the file is disturbed. The grammar is P2996R13's as adjusted by P3687
//! (splice *template arguments* were removed for C++26 and are not accepted; a bare `[: r :]` in a template argument
//! list is an expression, and a type there needs its `typename`).
//!
//! Two decisions are worth knowing while reading them:
//!
//! * `^^` is **one token**. `a ^ ^b` is not an expression in any version of the language — `^` has a left operand and
//!   no unary form — so two adjacent carets can only be the reflection operator, and the lexer answers one kind.
//! * a **splice is decided by its two delimiters**. `[:` cannot start a lambda's capture list (a capture is a name,
//!   `&`, `=` or `this`), and it cannot follow a `.` or an `->` as a subscript, which is what makes the member form
//!   unambiguous without a lookup.

use cpp_parser::{CppParser, CppSyntaxKind, ParserConfig};

fn tree(source: &str) -> cpp_parser::CppSyntaxNode {
    CppParser::parse(source, ParserConfig::default()).get_red_root()
}

fn kinds(source: &str) -> Vec<CppSyntaxKind> {
    tree(source)
        .descendants()
        .map(|node| CppSyntaxKind::from(node.kind()))
        .collect()
}

fn parses(source: &str) {
    let parsed = CppParser::parse(source, ParserConfig::default());
    assert_eq!(
        parsed.get_errors(),
        [],
        "{source:?} must parse cleanly, got {:?}",
        parsed.get_errors()
    );
    assert_eq!(
        parsed.to_source_text(),
        source,
        "{source:?} stays lossless"
    );
}

/// **`^^` is the reflection operator and a node of its own**, over a type, a qualified member, and a template name.
#[test]
fn a_reflection_is_an_expression_of_the_meta_info_type() {
    for operand in [
        "int",
        "Widget",
        "Widget::x",
        "std::array",
        "decltype(x)",
        "::Widget",
    ] {
        let source = format!("auto r = ^^{operand};\n");
        parses(&source);
        assert_eq!(
            kinds(&source)
                .into_iter()
                .filter(|kind| *kind == CppSyntaxKind::ReflectExpr)
                .count(),
            1,
            "`^^{operand}` is one `ReflectExpr`"
        );
    }
}

/// **A splice is an expression**, in the positions an expression stands in: an initializer, an argument, a return.
#[test]
fn a_splice_is_an_expression() {
    for source in [
        "constexpr auto r = ^^int;\nint v = [:r:];\n",
        "constexpr auto r = ^^f;\nvoid g() { std::meta::info x = [:r:]; }\n",
        "constexpr auto r = ^^int;\nauto f() { return [:r:]; }\n",
    ] {
        parses(source);
        assert!(
            kinds(source).contains(&CppSyntaxKind::SpliceExpr),
            "{source:?} holds a splice node"
        );
    }
}

/// **A type splice is a type**: after `typename`, where a dependent type-name would stand, with template arguments
/// allowed after it (`typename [: r :] <int>` is the splice specialization).
#[test]
fn a_type_splice_stands_where_a_type_name_stands() {
    parses("constexpr auto r = ^^int;\nusing T = typename[:r:];\n");
    assert!(
        kinds("constexpr auto r = ^^int;\nusing T = typename[:r:];\n")
            .contains(&CppSyntaxKind::TypenameType),
        "the type splice is a `TypenameType` holding a `SpliceExpr`"
    );

    parses("template <class T> struct Box {};\nusing S = typename[:^^Box<int>:]<int>;\n");
}

/// **A member splice follows `.` or `->`**: `d.[: r :]`, the form the proposal's own examples use for a member a
/// reflection names.
#[test]
fn a_member_splice_follows_the_access_operator() {
    parses("constexpr auto r = ^^Widget::x;\nvoid f(Widget w) { auto& v = w.[:r:]; }\n");
    parses("constexpr auto r = ^^Widget::x;\nvoid f(Widget* p) { auto& v = p->[:r:]; }\n");
}

/// **The splice does not swallow the neighbouring constructs**: a lambda's capture list, an array subscript, and a
/// bit-field's colon all still parse the way they did.
#[test]
fn the_splice_is_told_apart_from_its_neighbours() {
    for source in [
        "void f() { auto g = [x]() { return x; }; }\n",
        "void f(int* p) { int v = p[1]; }\n",
        "struct S { unsigned a : 3; };\n",
        "void f(int a, int b) { int c = a ? b : 1; }\n",
        "auto x = [] { return 1; }();\n",
    ] {
        parses(source);
        assert!(
            !kinds(source).contains(&CppSyntaxKind::SpliceExpr),
            "{source:?} holds no splice"
        );
    }
}

/// **A reflection is not a block literal.** `^` alone is still Apple's block — `^(int n) { … }` — and the two are
/// told apart by the token, not by a lookahead inside the parser.
#[test]
fn a_single_caret_is_still_a_block_literal() {
    parses("void f() { auto b = ^(int n) { return n; }; }\n");
    let kinds = kinds("void f() { auto b = ^(int n) { return n; }; }\n");
    assert!(
        !kinds.contains(&CppSyntaxKind::ReflectExpr),
        "one caret is not a reflection"
    );
}
