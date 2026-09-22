//! Direct-initialisation versus a call: the same tokens, two readings.
//!
//! `Widget w(1, 2);` and `g(1, 2);` are the same shape, and nothing in the tokens says which is which. C++ settles
//! it by looking `Widget` up; a parser that has not resolved names yet has to guess from what the file says about
//! itself and from what the two forms usually look like. These tests pin the guesses, so that a later change to
//! the heuristic is a *visible* change to a documented set of cases rather than a silent shift in what the tree
//! means.
//!
//! The two mistakes are not equally expensive, and the cases are written to keep the cheaper one:
//!
//! * reading a **call** as a declaration loses the callee and its arguments and invents a binding;
//! * reading a **declaration** as a call leaves a variable unbound, and the tokens are still all there.
//!
//! So a case that the file gives no evidence about stays a call.

use cpp_parser::{CppParser, CppSyntaxKind, ParserConfig};

struct Parsed {
    calls: usize,
    initializers: usize,
    errors: Vec<String>,
}

fn parse(source: &str) -> Parsed {
    let tree = CppParser::parse(source, ParserConfig::default());
    let root = tree.get_red_root();
    let count = |kind: CppSyntaxKind| {
        root.descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
            .count()
    };

    Parsed {
        calls: count(CppSyntaxKind::CallExpr),
        initializers: count(CppSyntaxKind::Initializer),
        errors: tree
            .get_errors()
            .iter()
            .map(|error| error.message.to_string())
            .collect(),
    }
}

/// `T x(...)` read as a declaration of a variable initialised from the arguments.
fn assert_declaration(source: &str) {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "{source:?} should parse cleanly, got {:?}",
        parsed.errors
    );
    assert_eq!(
        parsed.initializers, 1,
        "{source:?} should be a declaration with a direct initialiser"
    );
    assert_eq!(
        parsed.calls, 0,
        "{source:?} should not contain a call expression"
    );
}

/// `name(...)` read as a call.
fn assert_call(source: &str) {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "{source:?} should parse cleanly, got {:?}",
        parsed.errors
    );
    assert_eq!(parsed.calls, 1, "{source:?} should be a call expression");
    assert_eq!(
        parsed.initializers, 0,
        "{source:?} should not contain a direct initialiser"
    );
}

#[test]
fn a_keyword_type_makes_it_a_declaration() {
    // No lookup needed: no expression begins with `int`, so the reading is forced.
    assert_declaration("void f() { int a(1); }\n");
    assert_declaration("void f() { double d(1.5); }\n");
    assert_declaration("void f() { unsigned long n(0); }\n");
    assert_declaration("void f() { char c('x'); }\n");
}

#[test]
fn a_type_the_file_declares_makes_it_a_declaration() {
    // The class-like head is what puts `Widget` in the table, and the table is what decides the reading.
    assert_declaration("struct Widget {};\nvoid f() { Widget w(1, 2); }\n");
    assert_declaration("class Widget {};\nvoid f() { Widget w(1, 2); }\n");
    assert_declaration("union Widget {};\nvoid f() { Widget w(1, 2); }\n");
    assert_declaration("enum class Widget {};\nvoid f() { Widget w(1, 2); }\n");
}

#[test]
fn an_alias_makes_it_a_declaration() {
    assert_declaration("using Widget = int;\nvoid f() { Widget w(1, 2); }\n");
    assert_declaration("typedef int Integer;\nvoid f() { Integer i(1); }\n");
    assert_declaration("typedef struct Point { int x; } Point;\nvoid f() { Point p(1); }\n");
}

#[test]
fn a_nested_type_is_declared_where_it_is_written() {
    assert_declaration("struct Outer { struct Inner {}; void m() { Inner i(1); } };\n");
}

#[test]
fn a_type_name_stays_a_type_after_its_scope_closes() {
    // The table is deliberately not a scope stack: a name once seen as a type keeps the declaration reading,
    // which is what makes a type declared in one function usable in the next.
    assert_declaration("void g() { struct Widget {}; Widget w(1); }\n");
}

#[test]
fn a_name_the_file_never_declares_stays_a_call() {
    // The leading name is not a type as far as this file knows, and the argument is a literal, so neither
    // signal fires. The merge into the enclosing declaration above is a separate defect; what is pinned here is
    // that the tokens are read as an expression rather than turned into an invented variable.
    assert_eq!(
        parse("void f() { Widget w(1, 2); }\n").initializers,
        0,
        "an unknown type name with literal arguments should not become a direct initialiser"
    );
}

#[test]
fn a_bare_name_call_stays_a_call() {
    assert_call("void f() { g(1); }\n");
    assert_call("void f() { g(1, 2); }\n");
    assert_call("void f() { g(); }\n");
    assert_call("void f() { obj.m(1); }\n");
}

#[test]
fn a_function_declaration_is_not_a_direct_initialiser() {
    // The other half of the most vexing parse: empty parentheses are a parameter list, never an argument list.
    let parsed = parse("struct Widget {};\nWidget make();\n");
    assert!(parsed.errors.is_empty(), "got {:?}", parsed.errors);
    assert_eq!(parsed.initializers, 0, "`T x()` is a function declaration");
}

#[test]
fn an_unknown_type_with_literal_arguments_stays_an_expression() {
    // Both signals have to be absent for the declaration reading to be refused, and here the *leading name* is
    // the one that is unknown. These two are the documented cost of not having a symbol table.
    for source in [
        "void f() { Widget w(1, 2); }\n",
        "void f() { std::string s(\"x\"); }\n",
    ] {
        assert_eq!(
            parse(source).initializers,
            0,
            "{source:?} has no type-name evidence, so it should not become a direct initialiser"
        );
    }
}

#[test]
fn unknown_leading_names_become_declarations_at_file_scope() {
    // `Max(a, b);` at file scope: the leading name is unknown, but a *statement* at file scope cannot be a call,
    // and every element could be a declarator. `a + 1` could not.
    for source in [
        "Max(a, b);\n",
        "Widget w(mode);\n",
        "Result r(first, second, third);\n",
    ] {
        assert_declaration(source);
    }
}

#[test]
fn an_argument_that_is_not_a_bare_name_keeps_it_a_call() {
    // Each element has to be a name. A literal, a path, an operator or a nested list is a value, not a
    // declarator — so the file-scope guess is withdrawn even though a declaration would be grammatical.
    for source in [
        "Max(a, 1);\n",
        "Max(p.first, q);\n",
        "Max(a::b, c);\n",
        "Max(a + 1, b);\n",
        "Max(*p, b);\n",
        "Max(g(a), b);\n",
    ] {
        assert_eq!(
            parse(source).initializers,
            0,
            "{source:?} has an element that is not a bare name"
        );
    }
}

#[test]
fn the_file_scope_guess_does_not_reach_inside_a_body() {
    // The same shape where a call is the ordinary thing to find. Inside a body the guess is not worth making, so
    // `Max(a, b);` stays a call and keeps its arguments.
    assert_call("void f() { Max(a, b); }\n");
    assert_call("void f() { Max(a); }\n");
}

#[test]
fn a_nested_construction_is_an_initializer() {
    // `Widget w(Inner(1));` builds a temporary to initialise from. The element is a *construction* rather than a
    // bare name, and a parameter list has no such element — `Inner(1)` is not a type — so the reading is decided
    // by the declaration rather than by the shape alone. This is the case that came out as a parameter of type
    // `Inner` named `Inner` and defaulted from `1` before the scanner learned to accept it.
    assert_declaration("struct Inner {};\nstruct Widget {};\nWidget w(Inner(1));\n");
}

#[test]
fn a_call_nested_inside_an_element_is_not_a_construction() {
    // One level only. `f(g(a), b)` has a call *inside* an element, which is a value being passed rather than one
    // being built, so the file-scope guess is not extended to it.
    assert_eq!(
        parse("Max(g(a), b);\n").initializers,
        0,
        "a call nested inside an element should not make the list a declarator list"
    );
}
