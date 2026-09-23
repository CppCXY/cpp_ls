//! Post-C++20 / C++23 constructs, and the two operators that were missing from the table.
//!
//! Five groups that a census of common C++ turned up, and the shape of each fix differs enough to be worth
//! reading together:
//!
//! * an **explicit object parameter** is a *silent* gap — the member came out as nothing and a phantom member
//!   was read from the tokens after it, with no diagnostic at all;
//! * an **alternative operator spelling** is in the language, not an extension, and it arrives as an
//!   `Identifier` because the lexer has no keyword for it;
//! * a **throw-expression** is the second half of a keyword that already had a statement rule;
//! * **`extern template`** and **`inline namespace`** are keyword pairs where neither keyword is a type.

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

    let bad = tree.get_red_root().descendants().any(|node| {
        matches!(
            CppSyntaxKind::from(node.kind()),
            CppSyntaxKind::ErrorNode | CppSyntaxKind::MissingNode
        )
    });
    assert!(!bad, "{source:?} left an error or missing node in the tree");
}

fn count(source: &str, kind: CppSyntaxKind) -> usize {
    CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
        .count()
}

// ============================================================================
// C++23 explicit object parameter
// ============================================================================

#[test]
fn an_explicit_object_parameter_is_a_parameter() {
    for source in [
        "struct S { void f(this S& self); };\n",
        "struct S { void f(this S&& self) &&; };\n",
        "struct S { void f(this auto&& self) {} };\n",
        "struct S { int g(this S& self, int x); };\n",
        "struct S { void h(this S& self) const; };\n",
        "struct S { void i(this S&); };\n",
        "struct S { void k(this S* self); };\n",
        "struct S { void j(this S& self) { self.x = 1; } };\n",
        "struct S { void l(this S& self) noexcept; };\n",
    ] {
        parses(source);
    }
}

/// The member is **one** member, which is what the silent version got wrong.
///
/// Before the rule the parameter list gave up at `this`, the declaration reading failed, and the tokens after
/// it — `S& self);` — were read as a second, phantom member. There was no diagnostic, so nothing reported it.
#[test]
fn an_explicit_object_parameter_does_not_create_a_second_member() {
    let source = "struct S { void f(this S& self); };\n";
    assert_eq!(
        count(source, CppSyntaxKind::Parameter),
        1,
        "one parameter, not zero and not two"
    );
    assert_eq!(count(source, CppSyntaxKind::ThisExpr), 1);

    // The class holds exactly one member declaration, and the parameter belongs to it.
    let tree = CppParser::parse(source, ParserConfig::default());
    let root = tree.get_red_root();
    let class_body = root
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ClassBody)
        .expect("a class body");
    let members = class_body
        .children()
        .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declaration)
        .count();
    assert_eq!(members, 1, "the class declares one member");
}

/// `this` as an **argument** is still an argument, which is the half the new rule must not take.
#[test]
fn the_operator_this_is_still_an_expression() {
    for source in [
        "void f() { g(this); }\n",
        "void f() { g(this, 1); }\n",
        "void f() { return this->x; }\n",
        "struct S { void m() { f(this); } };\n",
        "struct S { S& operator=(const S&) { return *this; } };\n",
    ] {
        parses(source);
    }
}

// ============================================================================
// Alternative operator spellings
// ============================================================================

/// The alphabetic spellings of the operators are real C++, so a parser that ignores them rejects valid code.
#[test]
fn alternative_operator_spellings_parse() {
    for source in [
        "void f() { auto x = a and b; }\n",
        "void f() { auto x = a or b; }\n",
        "void f() { auto x = not a; }\n",
        "void f() { auto x = a bitand b; }\n",
        "void f() { auto x = a bitor b; }\n",
        "void f() { auto x = a xor b; }\n",
        "void f() { auto x = compl a; }\n",
        "void f() { auto x = a not_eq b; }\n",
        "void f() { a and_eq b; }\n",
        "void f() { a or_eq b; }\n",
        "void f() { a xor_eq b; }\n",
        "void f() { if (a and b) { } }\n",
        "void f() { while (not done) { } }\n",
        "void f() { auto x = not (a or b); }\n",
    ] {
        parses(source);
    }
}

/// The spelling becomes the operator, so the tree has the same shape the punctuation would give it.
#[test]
fn an_alternative_spelling_produces_the_same_node_as_its_symbol() {
    for (word, symbol) in [
        ("a and b", "a && b"),
        ("a or b", "a || b"),
        ("a bitand b", "a & b"),
        ("a bitor b", "a | b"),
        ("a xor b", "a ^ b"),
        ("a not_eq b", "a != b"),
    ] {
        let spelled = format!("void f() {{ auto x = {word}; }}\n");
        let punctuated = format!("void f() {{ auto x = {symbol}; }}\n");
        parses(&spelled);
        assert_eq!(
            count(&spelled, CppSyntaxKind::BinaryExpr),
            count(&punctuated, CppSyntaxKind::BinaryExpr),
            "{word:?} must be as binary as {symbol:?}"
        );
    }

    // `not` is unary where `!` is.
    assert_eq!(
        count("void f() { auto x = not a; }\n", CppSyntaxKind::UnaryExpr),
        1
    );
    assert_eq!(
        count("void f() { auto x = !a; }\n", CppSyntaxKind::UnaryExpr),
        1
    );
}

/// A name that merely *looks* like a spelling is still a name.
#[test]
fn an_identifier_that_is_not_a_spelling_stays_a_name() {
    for source in [
        "void f() { auto x = android; }\n",
        "void f() { auto x = notation; }\n",
        "void f() { auto x = orbit; }\n",
        "void f() { auto x = bitand_mask; }\n",
        "void f() { not_a_keyword(); }\n",
    ] {
        parses(source);
        assert_eq!(
            count(source, CppSyntaxKind::BinaryExpr) + count(source, CppSyntaxKind::UnaryExpr),
            0,
            "{source:?} has no operator in it"
        );
    }
}

// ============================================================================
// throw-expression
// ============================================================================

#[test]
fn a_throw_expression_parses_where_a_value_is_expected() {
    for source in [
        "void f() { x = throw 1; }\n",
        "void f() { auto y = cond ? throw 1 : 2; }\n",
        "void f() { return throw 1; }\n",
        "void f() { g(throw 1); }\n",
        "void f() { throw; }\n",
        "void f() { throw E{}; }\n",
        "void f() { throw 1; }\n",
    ] {
        parses(source);
    }
}

/// The statement and the expression are different nodes, and that is the point of having both.
#[test]
fn a_throw_statement_and_a_throw_expression_are_different_nodes() {
    assert_eq!(
        count("void f() { throw 1; }\n", CppSyntaxKind::ThrowStat),
        1,
        "a statement at the start of a statement"
    );
    assert_eq!(
        count("void f() { x = throw 1; }\n", CppSyntaxKind::ThrowExpr),
        1,
        "an expression where a value is expected"
    );
    assert_eq!(
        count("void f() { x = throw 1; }\n", CppSyntaxKind::ThrowStat),
        0
    );
}

// ============================================================================
// extern template, inline namespace
// ============================================================================

#[test]
fn an_explicit_instantiation_declaration_parses() {
    for source in [
        "extern template struct S<int>;\n",
        "extern template class C<int>;\n",
        "extern template void f<int>(int);\n",
        "extern template struct S<int, char>;\n",
    ] {
        parses(source);
    }

    // The `extern` and the `template` are still in the declaration's own tokens, so a consumer can see that
    // this is an instantiation *declaration* rather than a definition.
    let tree = CppParser::parse("extern template struct S<int>;\n", ParserConfig::default());
    let declaration = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declaration)
        .expect("a declaration");
    let text = declaration.text().to_string();
    assert!(text.starts_with("extern template"), "got {text:?}");
}

#[test]
fn an_inline_namespace_parses() {
    for source in [
        "inline namespace v1 { }\n",
        "inline namespace v1 { int x; }\n",
        "namespace a { inline namespace b { } }\n",
        "inline namespace v1 = a::b;\n",
        "inline namespace v1 { namespace v2 { } }\n",
    ] {
        parses(source);
    }
}

/// `inline` in front of anything else is still a specifier.
#[test]
fn inline_before_something_else_is_unchanged() {
    for source in [
        "inline int f() { return 1; }\n",
        "inline constexpr int k = 1;\n",
        "struct S { inline static int x = 0; };\n",
        "inline namespace v1 { inline int g(); }\n",
    ] {
        parses(source);
    }
}

// ============================================================================
// decltype in type position
// ============================================================================

#[test]
fn decltype_in_type_position_parses() {
    for source in [
        "decltype(x) y;\n",
        "decltype(x) y = 1;\n",
        "decltype(auto) y;\n",
        "decltype(auto) x = f();\n",
        "decltype(x)* p;\n",
        "decltype(x) v[2];\n",
        "decltype(a + b) c;\n",
        "const decltype(x) y = 1;\n",
        "noexcept(f()) g();\n",
        "using T = decltype(x);\n",
        "struct S { decltype(x) y = 1; };\n",
        "decltype(x) f() { return {}; }\n",
        "void g() { decltype(x) y = 1; }\n",
        "void h() { decltype(auto) z = f(); }\n",
        "void i() { for (decltype(x) n = 0; n < 3; ++n) { } }\n",
    ] {
        parses(source);
    }
}

/// A `decltype` in **operand** position is refused rather than misread, which is the cheaper direction.
///
/// `decltype(x)` is not a value, so `decltype(x) + 1;` is not an expression — and it must not be silently
/// accepted as one either. The anchor that makes the *declaration* reading reachable asks whether a declarator
/// follows the type-id, which is exactly what separates these two.
#[test]
fn decltype_where_a_value_is_expected_is_refused() {
    for source in [
        "void f() { decltype(x); }\n",
        "void f() { decltype(x) + 1; }\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(tree.to_source_text(), source, "{source:?} stays lossless");
    }
}

/// The declaration is a declaration **with a declarator**, which is what the defect got wrong.
///
/// Before the fix the declarator's own name was absorbed into the type, so the declaration came out with none —
/// and that is invisible to a test that only asks "does it parse".
#[test]
fn a_decltype_declaration_has_a_declarator() {
    for source in ["decltype(x) y = 1;\n", "decltype(auto) y = f();\n"] {
        assert_eq!(
            count(source, CppSyntaxKind::InitDeclarator),
            1,
            "{source:?} has one init-declarator"
        );
        assert_eq!(
            count(source, CppSyntaxKind::Declarator),
            1,
            "{source:?} has a declarator of its own"
        );
        assert_eq!(count(source, CppSyntaxKind::Initializer), 1);
    }
}
