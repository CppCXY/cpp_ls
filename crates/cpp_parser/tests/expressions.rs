//! Expression forms: the parenthesized expression, the conditional operator, and the lambda.
//!
//! Three things that were missing together, and for one reason. `parse_primary_expr` had no rule for a `(` at
//! all, so every expression that *began* with one failed — and `(a > b) ? x : y` begins with one, which is why
//! the conditional operator looked broken while `a ? b : c` worked. The lambda is the same shape of gap in a
//! different place: `[` is both an index and a capture list, so it needs a decision rather than a rule.
//!
//! The shapes are pinned rather than just "it parses", because these three produce nodes a consumer navigates:
//! a `ParenExpr` that keeps its parentheses, a `TernaryExpr` with three parts, and a `LambdaExpr` whose capture
//! list is its own node.

use cpp_parser::{CppParser, CppSyntaxKind, ParserConfig};

fn parses(source: &str) {
    let tree = CppParser::parse(source, ParserConfig::default());
    assert_eq!(
        tree.get_errors(),
        [],
        "{source:?} must parse cleanly, got {:?}",
        tree.get_errors()
    );
    assert_eq!(tree.to_source_text(), source, "{source:?} stays lossless");
}

fn count(source: &str, kind: CppSyntaxKind) -> usize {
    let tree = CppParser::parse(source, ParserConfig::default());
    tree.get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
        .count()
}

#[test]
fn a_parenthesized_expression_is_its_own_node() {
    // The parentheses are kept rather than dropped: they are not redundant, since a consumer that rewrites the
    // expression has to know where the grouping was.
    for source in [
        "void f() { x = (a); }\n",
        "void f() { x = (a + b); }\n",
        "void f() { x = (f()); }\n",
        "void f() { x = ((a)); }\n",
        "void f() { x = a * (b + c); }\n",
        "void f() { if ((a)) { } }\n",
        "void f() { while ((a && b)) { } }\n",
        "void f() { f((a), (b)); }\n",
    ] {
        parses(source);
    }

    assert_eq!(
        count("void f() { x = ((a)); }\n", CppSyntaxKind::ParenExpr),
        2,
        "nested parentheses are two nodes"
    );
}

#[test]
fn the_conditional_operator_parses_around_a_parenthesized_condition() {
    // The case that looked like a missing operator and was a missing operand rule.
    for source in [
        "void f() { auto x = a ? b : c; }\n",
        "void f() { x = (a > b) ? a : b; }\n",
        "void f() { auto x = (a > b) ? a : b; }\n",
        "void f() { return (a) ? (b) : (c); }\n",
        "void f() { x = (a ? b : c) ? d : e; }\n",
        "void f() { x = cond ? (a + b) : (c + d); }\n",
        "void f() { x = a ? b : c ? d : e; }\n",
        "void f() { x = a == b ? 1 : 2; }\n",
    ] {
        parses(source);
    }

    assert_eq!(
        count(
            "void f() { x = a ? b : c ? d : e; }\n",
            CppSyntaxKind::TernaryExpr
        ),
        2,
        "a conditional in the false branch nests"
    );
}

#[test]
fn a_conditional_is_weaker_than_assignment() {
    // `x = a ? b : c` assigns the conditional, not the other way round: the conditional sits inside whatever
    // holds the assignment. C++ reads it that way because the conditional binds tighter than the assignment
    // family.
    //
    // The assignment is *not* asserted to be a `BinaryExpr` here, and the reason is worth recording: `x = ...;`
    // at the start of a statement is read as a **declaration** of a variable named `x` of an unknown type, with
    // an `Initializer` — the same file-local-table limitation that keeps `Widget w(1, 2);` an expression, seen
    // from the other side. `a ? b : c` is then the initialiser, and that is what is pinned.
    let tree = CppParser::parse("void f() { x = a ? b : c; }\n", ParserConfig::default());
    let root = tree.get_red_root();
    let initializer = root
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Initializer)
        .expect("the conditional is an initialiser");

    assert!(
        initializer
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TernaryExpr),
        "the conditional has to be inside the initialiser"
    );
    assert_eq!(
        count("void f() { x = a ? b : c; }\n", CppSyntaxKind::TernaryExpr),
        1
    );
}

#[test]
fn a_lambda_is_its_own_node_with_its_capture_list() {
    for source in [
        "void f() { auto a = [] {}; }\n",
        "void f() { auto b = []() { return 1; }; }\n",
        "void f() { auto c = [](int x) { return x; }; }\n",
        "void f() { auto d = [&](int x) { return x + 1; }; }\n",
        "void f() { auto e = [x](int y) { return x + y; }; }\n",
        "void f() { auto g = [&x, y]() { return x + y; }; }\n",
        "void f() { auto h = [=]() { return x; }; }\n",
        "void f() { auto i = [this]() { return n; }; }\n",
        "void f() { auto j = [*this]() { return n; }; }\n",
        "void f() { auto k = []() mutable { }; }\n",
        "void f() { auto l = []() constexpr { }; }\n",
        "void f() { auto m = []() noexcept { }; }\n",
        "void f() { auto n = [](int x) -> int { return x; }; }\n",
        "void f() { auto o = [p = 1]() { return p; }; }\n",
        "void f() { auto q = [n](auto x) { return x; }; }\n",
        "void f() { auto r = [x, &y]() { }; }\n",
        "auto file_scope = [](int x) { return x; };\n",
        "void f() { std::sort(v.begin(), v.end(), [](int i, int j) { return i < j; }); }\n",
        "void f() { call([](int i) { return i; }, 1); }\n",
    ] {
        parses(source);
    }

    // The capture list is a node of its own, so a consumer does not have to re-read the lambda's tokens to find
    // what it captures.
    assert_eq!(
        count(
            "void f() { auto g = [&x, y]() { return x + y; }; }\n",
            CppSyntaxKind::LambdaCaptureList
        ),
        1
    );
    assert_eq!(
        count(
            "void f() { auto g = [&x, y]() { return x + y; }; }\n",
            CppSyntaxKind::LambdaCapture
        ),
        2,
        "one node per capture"
    );
    assert_eq!(
        count(
            "void f() { auto g = [&x, y]() { return x + y; }; }\n",
            CppSyntaxKind::LambdaExpr
        ),
        1
    );

    // An init-capture is one capture, not two: the initialiser belongs to the name it initialises.
    assert_eq!(
        count(
            "void f() { auto o = [p = 1]() { return p; }; }\n",
            CppSyntaxKind::LambdaCapture
        ),
        1
    );
}

#[test]
fn the_index_operator_is_not_stolen_by_the_lambda_rule() {
    // `[` starts both a capture list and an index, and only what *follows* the `]` tells them apart. Every index
    // spelling in the corpus has to keep its own reading.
    for source in [
        "void f() { arr[i] = 1; }\n",
        "void f() { auto x = a[b]; }\n",
        "void f() { auto x = a[b + c]; }\n",
        "void f() { auto x = m[k][j]; }\n",
        "void f() { int a[] = {1, 2}; }\n",
        "void f() { f(a[0], b[1]); }\n",
    ] {
        parses(source);
    }

    assert_eq!(
        count("void f() { auto x = a[b]; }\n", CppSyntaxKind::LambdaExpr),
        0,
        "an index is not a lambda"
    );
    assert_eq!(
        count("void f() { auto x = a[b]; }\n", CppSyntaxKind::IndexExpr),
        1
    );
}

#[test]
fn a_lambda_body_is_a_compound_statement() {
    // The body is parsed by the statement rule, which is what keeps `return`, a local declaration and a nested
    // lambda inside it working like any other block.
    for source in [
        "void f() { auto g = []() { int x = 1; return x; }; }\n",
        "void f() { auto g = []() { if (a) { return 1; } return 2; }; }\n",
        "void f() { auto g = []() { auto h = []() { return 1; }; return h(); }; }\n",
        "void f() { auto g = [](int x) { for (int i = 0; i < x; ++i) { } }; }\n",
    ] {
        parses(source);
    }
}
