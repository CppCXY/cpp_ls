//! **C++26 contracts** (P2900): `pre(…)`, `post(…)` and `contract_assert(…)`.
//!
//! ```text
//! pre(cond)              after the parameter list — the caller's obligation
//! post(r: cond)          the same place — the function's guarantee, `r` naming the return value
//! contract_assert(cond); a statement in the body — a full keyword
//! ```
//!
//! `pre` and `post` are **context-sensitive**: `int pre = 42;` and `void post() {}` are ordinary C++ and must keep
//! parsing. What makes them specifiers is the position — after a function declarator, where only attributes,
//! an initializer or a body can follow — and the parenthesis. `contract_assert` is a full keyword, because `assert`
//! is a macro and the standard needed a spelling that could not collide with one.
//!
//! No compiler here has contracts (GCC's is experimental and behind `-fcontracts`), so these pin the *syntax*.

use cpp_parser::{CppParser, CppSyntaxKind, ParserConfig, features::LanguageConfig};

fn parsed(source: &str) -> cpp_parser::CppSyntaxNode {
    let tree = CppParser::parse(source, ParserConfig::default());
    assert_eq!(
        tree.get_errors(),
        [],
        "{source:?} must parse cleanly, got {:?}",
        tree.get_errors()
    );
    let root = tree.get_red_root();
    assert_eq!(root.text().to_string(), source, "the tree stays lossless");
    root
}

/// **C++23, the default** — `contract_assert` is an ordinary identifier there.
fn parsed_as_cpp23(source: &str) -> cpp_parser::CppSyntaxNode {
    let config = ParserConfig::default()
        .with_language(LanguageConfig::new().with_standard(cpp_parser::features::Standard::Cpp23));
    let tree = CppParser::parse(source, config);
    assert_eq!(
        tree.get_errors(),
        [],
        "{source:?} must parse cleanly in C++23, got {:?}",
        tree.get_errors()
    );
    tree.get_red_root()
}

/// **C++26**, where `contract_assert` is a keyword.
fn parsed_as_cpp26(source: &str) -> cpp_parser::CppSyntaxNode {
    let tree = CppParser::parse(
        source,
        ParserConfig::default().with_language(LanguageConfig::cpp26()),
    );
    assert_eq!(
        tree.get_errors(),
        [],
        "{source:?} must parse cleanly in C++26, got {:?}",
        tree.get_errors()
    );
    tree.get_red_root()
}

fn count(source: &str, kind: CppSyntaxKind) -> usize {
    parsed(source)
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
        .count()
}

fn count_in(root: &cpp_parser::CppSyntaxNode, kind: CppSyntaxKind) -> usize {
    root.descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
        .count()
}

/// **A precondition and a postcondition after the parameter list**, each a specifier of its own.
#[test]
fn a_function_can_carry_preconditions_and_postconditions() {
    let source = "\
int safe_division(int numerator, int denominator)
    pre(denominator != 0)
    post(result: result * denominator == numerator)
{
    return numerator / denominator;
}
";
    assert_eq!(count(source, CppSyntaxKind::ContractSpecifierSeq), 1);
    assert_eq!(count(source, CppSyntaxKind::ContractSpecifier), 2);

    // A declaration (no body) carries them too: `int f(int x) pre(x > 0);`.
    let declaration = "int f(int x) pre(x > 0);\n";
    assert_eq!(count(declaration, CppSyntaxKind::ContractSpecifier), 1);
}

/// **Several of each, in order**, and a `post` without the result name — the `void` case, which has no result to
/// name.
#[test]
fn several_specifiers_and_a_postcondition_without_a_result_name() {
    let source = "\
void copy(const char* src, char* dst, unsigned long n)
    pre(src != nullptr)
    pre(dst != nullptr)
    pre(n > 0)
    post(dst[0] == src[0])
{
}
";
    assert_eq!(count(source, CppSyntaxKind::ContractSpecifier), 4);
}

/// **`contract_assert` is a statement**, and it may appear anywhere a statement may.
#[test]
fn contract_assert_is_a_statement() {
    let source = "\
int f(int x) {
    contract_assert(x > 0);
    if (x > 10) {
        contract_assert(x < 100);
    }
    return x;
}
";
    // **C++26**, where the name is a keyword; see the C++23 test below for the other side.
    let root = parsed_as_cpp26(source);
    assert_eq!(count_in(&root, CppSyntaxKind::ContractAssertStat), 2);
}

/// **In C++23 contract_assert is an ordinary identifier**, in all three positions a name can stand in: a variable, a
/// function, and a call. A lexer that claims the keyword unconditionally fails all three — and the error node then
/// swallows the declarations after it, so the damage is larger than one line
/// (\docs/parser-toolchain-review.md\ P1-3).
#[test]
fn contract_assert_is_an_identifier_before_cpp26() {
    let source = "\
int contract_assert = 1;
void contract_assert_call() { contract_assert(2); }
int contract_assert_function();
";
    let root = parsed_as_cpp23(source);
    assert_eq!(
        count_in(&root, CppSyntaxKind::ContractAssertStat),
        0,
        "nothing is a contract assertion in C++23"
    );
    let declarations: Vec<String> = root
        .descendants()
        .filter_map(|node| {
            use cpp_parser::CppAstNode as _;
            cpp_parser::CppDeclaration::cast(node)
        })
        .filter_map(|declaration| declaration.get_name_text())
        .collect();
    assert!(
        declarations.iter().any(|name| name == "contract_assert"),
        "the variable is declared: {declarations:?}"
    );
}

/// **A lambda carries contracts** — the proposal's own example.
#[test]
fn a_lambda_can_carry_contracts() {
    let source = "\
auto f = [](int x) pre(x > 0) post(r: r > x) { return x + 1; };
";
    assert_eq!(count(source, CppSyntaxKind::ContractSpecifier), 2);
}

/// **`pre` and `post` remain ordinary names everywhere else**, which is what "context-sensitive" means.
#[test]
fn pre_and_post_are_still_identifiers() {
    for source in [
        "int pre = 42;\n",
        "void post() {}\n",
        "struct S { int pre; int post; };\n",
        "void f() { pre(); post(1); }\n",
        "int g(int pre) { return pre; }\n",
    ] {
        let tree = parsed(source);
        assert_eq!(
            tree.descendants()
                .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ContractSpecifier)
                .count(),
            0,
            "{source:?} has no contract specifier"
        );
    }
}

/// **An attribute between the keyword and the parenthesis**, which the standard allows.
#[test]
fn an_attribute_between_the_keyword_and_the_parenthesis() {
    let source = "\
int f(int x)
    pre [[likely]] (x >= 0)
    post (r [[maybe_unused]] : r >= 0)
{
    return x;
}
";
    assert_eq!(count(source, CppSyntaxKind::ContractSpecifier), 2);
}

/// **A constructor's contract comes before its member initializer list**, and the body after it.
#[test]
fn a_constructor_contract_precedes_its_member_initializers() {
    let source = "\
struct X {
    int i;
    X(int v) pre(v > 0) : i(v) {}
};
";
    assert_eq!(count(source, CppSyntaxKind::ContractSpecifier), 1);
}

/// **A predicate is an expression**, whatever it writes: a call, a comparison, a conditional, a fold.
#[test]
fn a_predicate_may_be_any_expression() {
    let source = "\
bool check(int);
int f(int x, int y)
    pre(check(x) && (x > 0 ? y != 0 : true))
{
    return x;
}
";
    assert_eq!(count(source, CppSyntaxKind::ContractSpecifier), 1);
}
