//! Direct-initialisation versus a call: the same tokens, two readings.
//!
//! `Widget w(1, 2);` and `g(1, 2);` have the same first two tokens and the same parentheses, and nothing in
//! them says which is which. C++ settles it by looking `Widget` up; a parser that has not resolved names yet
//! has to guess from what the file says about itself and from what the two forms usually look like. These
//! tests pin the guesses, so that a later change to the heuristic is a *visible* change to a documented set of
//! cases rather than a silent shift in what the tree means.
//!
//! # The two mistakes are not equally expensive
//!
//! Reading a **call** as a declaration loses the callee and its arguments and invents a binding; reading a
//! **declaration** as a call leaves a variable unbound with all its tokens still in the tree. So a case the
//! file gives no evidence about stays a call — *unless* the statement names a declarator, which a call never
//! does. A call is `g(1, 2)`: one name, then arguments. A declaration is `Widget w(1, 2)`: a type, then a
//! name, then arguments. That second name is the evidence this file can always see, whether or not it has ever
//! heard of `Widget`, and it is what makes an undeclared type work:
//!
//! ```text
//! Widget w(1, 2, 3);   a declaration — a name and an argument list
//! g(1, 2, 3);          a call — no name for the arguments to initialise
//! A(B);                a call — `B` is an argument, not a declarator
//! ```
//!
//! The name alone is not enough either, because a parameter list is also `type name(...)`: `void f(int a(1))`
//! declares `a` with a default argument. What separates them is what the parentheses *hold* — see
//! [`the_arguments_look_like_values`](cpp_parser) and the module documentation of `grammar::cpp::decls`. A
//! literal, a call or an operator that no type contains can only be a value, and a value list can only be an
//! initialiser.

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
///
/// The initializer's own expressions may contain calls — `Widget w(Inner(1))` builds a temporary, and
/// `Widget w(g(), h())` calls two functions — so "no call expression anywhere" is asserted only where the
/// arguments are known not to be calls. See [`a_call_in_the_arguments_is_not_a_call_statement`].
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
}

/// `T x(...)` with arguments that are not themselves calls: no `CallExpr` may appear at all.
fn assert_plain_declaration(source: &str) {
    assert_declaration(source);
    assert_eq!(
        parse(source).calls,
        0,
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
fn a_type_the_file_never_declares_is_still_a_declaration() {
    // The type comes from a header this parser never read — which is the normal state of a file being edited.
    // The declarator's name is the evidence, and it is always there to be read.
    assert_declaration("void f() { Widget w(1, 2, 3); }\n");
    assert_declaration("void f() { Widget w(1); }\n");
    assert_declaration("void f() { Widget w(\"name\"); }\n");
    assert_declaration("void f() { std::string s(\"x\"); }\n");
    assert_declaration("Widget w(1, 2, 3);\n");
}

#[test]
fn a_declarator_with_no_name_leaves_the_parentheses_alone() {
    // `A(B);` has one name, so the parentheses hold an argument rather than an initialiser, and there is
    // nothing for an initialiser to attach to. This is the shape that used to swallow the rest of the
    // enclosing block: at file scope inside a namespace it reported three errors and left two stray `}`.
    let parsed = parse("namespace ns { A(B); }\n");
    assert!(
        parsed.errors.is_empty(),
        "the namespace should close cleanly, got {:?}",
        parsed.errors
    );
}

#[test]
fn a_bare_name_call_stays_a_call() {
    assert_call("void f() { g(1); }\n");
    assert_call("void f() { g(1, 2); }\n");
    assert_call("void f() { g(); }\n");
    assert_call("void f() { obj.m(1); }\n");
    assert_call("void f() { A(B); }\n");
    assert_call("void f() { A(B + C); }\n");
}

#[test]
fn a_function_declaration_is_not_a_direct_initialiser() {
    // The other half of the most vexing parse: empty parentheses are a parameter list, never an argument list.
    let parsed = parse("struct Widget {};\nWidget make();\n");
    assert!(parsed.errors.is_empty(), "got {:?}", parsed.errors);
    assert_eq!(parsed.initializers, 0, "`T x()` is a function declaration");
}

#[test]
fn a_parameter_list_is_not_a_value_list() {
    // The line the value scan must not cross, in the two places a parameter list can stand: a local function
    // declaration and a function's own parameters. `a` is a valid parameter type as well as a valid argument,
    // so nothing about the *name* separates these from `Widget w(1)` — only the tokens inside the parentheses
    // do, and none of these holds a literal, a call, or an operator no type contains.
    for source in [
        "void f() { int a(b); }\n",
        "void f() { int a(b, c); }\n",
        "void f(int (*fp)(int, double));\n",
        "void f(int* p, int& r, const int* q);\n",
    ] {
        assert_eq!(
            parse(source).initializers,
            0,
            "{source:?} holds a parameter list, not a value list"
        );
    }

    // A default argument makes a literal legal inside the list, and the `Initializer` that results is the
    // *parameter's* — one node, on the parameter, with the parameter list still the reading. Getting this
    // wrong in the other direction is what a value scan keyed on literals alone would do.
    let with_default = parse("void f(int a = 1);\n");
    assert!(
        with_default.errors.is_empty(),
        "a defaulted parameter should parse, got {:?}",
        with_default.errors
    );
    assert_eq!(with_default.initializers, 1);
}

#[test]
fn a_value_list_is_not_a_parameter_list() {
    // The other side of the same line, and the shapes that made it worth drawing. A literal, a call and an
    // operator no type contains can only be a value, so a parameter list is not a reading of them: the
    // declaration is the variable, and the parentheses initialise it.
    assert_plain_declaration("void f() { Widget w(1, 2, 3); }\n");
    assert_plain_declaration("void f() { Widget w(!flag); }\n");
}

#[test]
fn a_call_in_the_arguments_is_not_a_call_statement() {
    // `Widget w(g(), h())` constructs from two results. The `CallExpr`s are the *arguments*, so the statement
    // is still a declaration — which is the whole point: a parameter list cannot hold a call, and reading one
    // here would declare a function whose parameters are named after two functions.
    assert_declaration("void f() { Widget w(g(), h()); }\n");
    assert_eq!(parse("void f() { Widget w(g(), h()); }\n").calls, 2);

    assert_declaration("void f() { Widget w(Inner(1)); }\n");
    assert_eq!(parse("void f() { Widget w(Inner(1)); }\n").calls, 1);
}

#[test]
fn an_element_is_judged_by_the_token_after_the_name() {
    // A bare name is not evidence either way — `a` is a type in `Widget w(a)` and a value in `Widget w(a + 1)`
    // — so the follower is what decides.
    assert_plain_declaration("void f() { Widget w(a + 1); }\n");
    assert_plain_declaration("void f() { Widget w(a == b); }\n");

    // A multiplication where a pointer would also parse. Both readings are grammatical, the tokens do not
    // choose, and the call is the cheaper mistake — so this stays an expression rather than becoming a
    // declaration of a parameter named `c` of type `b*`.
    let multiplication = parse("void f() { Widget w(a * b); }\n");
    assert!(
        multiplication.errors.is_empty(),
        "`a * b` should still parse, got {:?}",
        multiplication.errors
    );
    assert_eq!(multiplication.initializers, 0, "`a * b` is a product");
}

#[test]
fn an_unnamed_pointer_parameter_is_the_one_reading_left_to_the_type_table() {
    // The documented cost of this rule. `Widget w(*p)` is a dereference — `*p` has no name for the pointer to
    // decorate — but a parameter list is also a reading of those tokens, because `void f(*p)` declares an
    // unnamed pointer, and that reading succeeds first and is never revisited.
    //
    // Kept as a call-shaped expression rather than making the parameter reading conditional, because the
    // conditional would have to be a guess about a shape this rare. Nothing is lost silently: the variable `w`
    // goes unbound, and the tokens are all still in the tree.
    let parsed = parse("void f() { Widget w(*p); }\n");
    assert_eq!(parsed.initializers, 0);
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
