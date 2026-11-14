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
/// **A type splice stands in expression position too** — a functional cast or a temporary, which is how a reflected
/// type is given a value: `typename [: ^^Widget :]{5}` and `typename [: r :](1)`.
///
/// Verified against GCC 16.2 (`-std=c++26 -freflection`), which accepts both; our parser read `typename` there as a
/// type specifier only, so the statement failed at the `{`.
#[test]
fn a_type_splice_stands_in_expression_position() {
    let source = "\
struct Widget { int x; };
constexpr auto r = ^^Widget;
auto t = typename [: ^^Widget :]{5};
auto u = typename [: r :](1);
";
    parses(source);
}
/// **`template for` — the expansion statement** (P1306, C++26), which every reflection program uses to walk a list of
/// reflections at compile time.
///
/// ```cpp
/// template for (constexpr auto e : enumerators) { … }
/// ```
///
/// Verified against GCC 16.2 (`-std=c++26 -freflection`), which accepts it — and this parser used to leave the
/// `template` token flat in the enclosing block while reading the `for` as an ordinary loop, with **no error reported**:
/// a silent misparse, which is the shape this project treats as a defect. The node is `ExpansionStat`, and the loop
/// inside it is the ordinary `ForStat` because the header is the ordinary range-for header.
#[test]
fn an_expansion_statement_is_a_statement_of_its_own() {
    let source = "\
void f() {
    static constexpr auto enumerators = g();
    template for (constexpr auto e : enumerators) {
        use([:e:]);
    }
}
";
    let tree = CppParser::parse(source, ParserConfig::default());
    assert_eq!(tree.get_errors(), [], "the expansion statement parses cleanly");
    let expansion = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ExpansionStat)
        .expect("the expansion statement has a node of its own");
    assert!(
        expansion.text().to_string().starts_with("template for"),
        "the node covers the whole statement: {:?}",
        expansion.text().to_string()
    );
}
/// **`^^::` reflects the global namespace**, and it is the form libstdc++`s own `<meta>` writes:
/// `access_context { ^^::, info {} }` (`meta:329`). Verified with GCC 16.2 (`-std=c++26 -freflection`:
/// `static_assert(std::meta::is_namespace(^^::))`). Our parser read the `::` as the head of a name and reported
/// `expected a name after `::``.
#[test]
fn a_reflection_of_the_global_scope_is_a_reflection() {
    parses("constexpr auto g = ^^::;\n");
    // …and a global name keeps working, because `::` followed by a name is that name.
    parses("constexpr auto g = ^^::Widget;\n");
}