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

/// An assignment is an **expression**, not a declaration with a nameless declarator.
///
/// The mirror image of everything above, and the more dangerous direction: `x = 1;` used to come out as
/// `Declaration(DeclSpecifierSeq(x), InitDeclarator(=, Initializer(1)))` — a well-formed tree of the right size,
/// with **no error, no `ErrorNode` and no `MissingNode`**, describing a variable whose declarator named nothing.
/// Every assignment to a name this file had not seen a type for was read that way, which inside a function body
/// is most of them.
///
/// It was invisible to all three of the checks this project had: losslessness and well-formedness hold for a
/// wrong tree as much as for a right one, `gaps.rs` looks for errors and error nodes, and the scope walker
/// *already* declines to bind a declarator that named nothing — so the false tree and the true one produced the
/// same (empty) set of names.
///
/// The gate is that an initializer needs something to initialise. It cannot be "no name was parsed", because a
/// **qualified** declarator is read by the specifier sequence rather than by the declarator rule —
/// `int ns::count = 0;` folds `ns::count` into the type and names nothing either. That shape keeps its
/// declaration reading, and `a_qualified_declarator_keeps_its_reading` pins it.
#[test]
fn an_assignment_is_not_a_declaration() {
    for source in [
        "void f() { x = 1; }\n",
        "void f() { x = a; }\n",
        "void f() { x = a + b; }\n",
        "void f() { x = a ? b : c; }\n",
        "void f() { x = f(); }\n",
        "void f() { value = other; }\n",
        "void f() { count = 0; }\n",
        "void f() { result = compute(); }\n",
        "void f() { cache = table[key]; }\n",
        "x = 1;\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());

        assert_eq!(
            tree.get_errors(),
            [],
            "{source:?} is valid code and must parse cleanly"
        );

        // The statement is an `ExpressionStat`. Asserted on the *statement* rather than on the absence of a
        // declaration, because `void f() { ... }` is one: the statement is the node child of the `CompoundStat`
        // that holds it — a leaf token has no children, so the filter picks out the constructs.
        let statement = CppParser::parse(source, ParserConfig::default())
            .get_red_root()
            .descendants()
            .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CompoundStat)
            .and_then(|body| {
                body.children()
                    .filter(|child| child.children().next().is_some())
                    .last()
            })
            .map(|node| CppSyntaxKind::from(node.kind()))
            // A file-scope input has no body; the statement is the root's own child.
            .or_else(|| {
                CppParser::parse(source, ParserConfig::default())
                    .get_red_root()
                    .children()
                    .next()
                    .map(|node| CppSyntaxKind::from(node.kind()))
            });

        assert_eq!(
            statement,
            Some(CppSyntaxKind::ExpressionStat),
            "{source:?} is an assignment, so its statement must be an expression"
        );
        assert_eq!(
            count_of(source, CppSyntaxKind::Initializer),
            0,
            "{source:?} has no initialiser — an initialiser needs a declarator name"
        );
    }
}

/// A qualified declarator keeps its declaration reading, because the name folded into the type.
///
/// `int ns::count = 0;` runs the specifier sequence over `ns::count` and leaves nothing for the declarator rule
/// to name — the same "no name was parsed" state as `x = 1;` has. What separates them is that `ns::count` is
/// written as a type: a qualified name needs at least two segments, and a bare undeclared `x` is one.
///
/// This is C++'s own reading when `count` is a static member, and `int ns::Widget::count = 0;` has parsed that
/// way for as long as the file-local table has existed.
#[test]
fn a_qualified_declarator_keeps_its_reading() {
    for source in [
        "int ns::count = 0;\n",
        "int ns::Widget::count = 0;\n",
        "int A::b = 1;\n",
        "int Outer::Inner::value = 42;\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());

        assert_eq!(
            tree.get_errors(),
            [],
            "{source:?} must parse cleanly, got {:?}",
            tree.get_errors()
        );
        assert_eq!(tree.to_source_text(), source, "{source:?} stays lossless");
        assert!(
            tree.get_red_root()
                .descendants()
                .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declaration),
            "{source:?} declares something"
        );
    }
}

/// A **cv-qualifier after the type** must not push the declarator's name into the type.
///
/// `char const w[] = { 'a' };` used to read the type as `char const w` and the declarator as `[]` — which then
/// became a **structured binding**, with no diagnostic at all. `char const w[2] = { 'a' };` was the same reading
/// with a bound in it, and that is where it was finally reported.
///
/// Two defects were stacked, and the second hid the first:
///
/// 1. `type_is_already_complete` judged the token immediately before the name, and a cv-qualifier answered "not
///    complete" — but a qualifier neither finishes a type nor unfinishes one, and `const char w` and
///    `char const w` are the same type. It now steps over qualifiers and judges what is in front of them;
/// 2. `has_type_specifier` — the flag saying "a *type* has been named in this sequence" — was **assigned** per
///    specifier rather than accumulated, so a cv-qualifier cleared it. Fixing only the first leaves the second
///    answering "no type yet", which is the same wrong reading by a different route.
///
/// `char const* p` was never affected: the `*` ends the specifier sequence before any name is seen, which is why
/// the common spelling hid the defect.
#[test]
fn a_cv_qualifier_after_the_type_does_not_swallow_the_name() {
    for source in [
        "char const w[] = { 'a' };\n",
        "char const w[2] = { 'a' };\n",
        "char const w[2];\n",
        "int const x = 1;\n",
        "int const x;\n",
        "static char const w[2] = { 'a' };\n",
        "unsigned const int y = 1;\n",
        "struct S const s;\n",
        "char const *p = 0;\n",
        "const char w[] = { 'a' };\n",
        "alignas(16) const MyType value;\n",
        "void f() { char const buf[4] = { 0 }; }\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(
            tree.get_errors(),
            [],
            "{source:?} must parse cleanly, got {:?}",
            tree.get_errors()
        );
        assert!(
            count_of(source, CppSyntaxKind::InitDeclarator) >= 1,
            "{source:?}: the declaration has a declarator"
        );
        assert_eq!(
            count_of(source, CppSyntaxKind::StructuredBinding),
            0,
            "{source:?}: and the declarator is not a structured binding of nothing"
        );
    }

    // The array bound belongs to the declarator, which is what the wrong reading moved into the type.
    assert_eq!(count_of("char const w[2];\n", CppSyntaxKind::ArrayType), 1);
    assert_eq!(count_of("char const w[2];\n", CppSyntaxKind::Declarator), 1);
}

/// Count the nodes of one kind in `source`.
fn count_of(source: &str, kind: CppSyntaxKind) -> usize {
    CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
        .count()
}

/// A function whose parameter is an **unnamed** type: `void f(T);`, not `void f(T t);`.
///
/// The list of bare names is the one shape a direct-initialiser and a parameter list share — `Widget w(T)` and
/// `void f(T)` are the same tokens — and at file scope the initialiser reading was preferred for it, because the
/// shape it was written for is `Max(a, b);`: a declaration with **no type at all**, where a call statement at
/// file scope is not a thing. The preference was guessing.
///
/// A type keyword in front of the declarator's name is not a guess. `void f(T);` has a type, so the parentheses
/// are a parameter list, and reading them as a value made the declaration a **variable** — `f` initialised with
/// the value `T`. That reading is well formed, lossless and reported nothing: an A0-class wrong tree, and the
/// definition spelled the same way was worse still, because the body's `{` then had no declaration to belong to
/// and the whole thing was refused.
///
/// The forms that must **not** change are the ones the preference exists for, and they are asserted beside it.
#[test]
fn an_unnamed_parameter_of_an_unknown_type_is_a_parameter() {
    for source in [
        "void f(T);\n",
        "void f(T) { }\n",
        "int f(T);\n",
        "template <typename T> void f(T);\n",
        "template <typename T> void f(T) { }\n",
        "template <typename T> void g(T, T);\n",
        "void f(std::vector<T>);\n",
        "struct S { void f(T); };\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(
            tree.get_errors(),
            [],
            "{source:?} must parse cleanly, got {:?}",
            tree.get_errors()
        );
        assert_eq!(
            count_of(source, CppSyntaxKind::ParameterList),
            1,
            "{source:?}: the parentheses are a parameter list"
        );
        assert_eq!(
            count_of(source, CppSyntaxKind::Initializer),
            0,
            "{source:?}: the declaration is not a variable initialised from a value"
        );
    }

    // One parameter, unnamed: the count is the assertion, because a wrong reading produces zero here.
    assert_eq!(
        count_of("void f(T);\n", CppSyntaxKind::Parameter),
        1,
        "the parameter is read even though it has no name"
    );

    // And the shapes the initialiser preference exists for keep their reading — a declaration with **no type**,
    // and a declaration whose type is a name this file knows.
    assert_declaration("Widget w(1, 2);\n");
    assert_declaration("Widget w(Inner(1));\n");
    assert_declaration("std::string s(other);\n");
    // The shape the whole preference exists for, unchanged — see
    // `unknown_leading_names_become_declarations_at_file_scope`.
    assert_declaration("Max(a, b);\n");

    // One consequence is worth pinning: a keyword type in front of the name now buys the **same** reading at
    // file scope that it has always had inside a body, where the parameter reading is tried first. `int a(b)`
    // is a function declaring one unnamed parameter of type `b` in both places — which is what C++ says when
    // `b` names a type, and the reading this file already committed to at block scope.
    assert_eq!(
        count_of("int a(b);\n", CppSyntaxKind::ParameterList),
        1,
        "at file scope"
    );
    assert_eq!(
        count_of("void f() { int a(b); }\n", CppSyntaxKind::ParameterList),
        2,
        "the same reading inside a body — one for `f`, one for `a`"
    );

    // A qualified declarator name with an unnamed parameter of a bare unknown type used to be a gap, and a loud
    // one: the name is folded into the type by the specifier sequence (`void Widget::draw`), so the declarator has
    // no name of its own, and `(T)` was read as an *initializer* — a list of bare names is the one shape the two
    // readings share — leaving the definition without a parameter list. It is fixed by the same signal that
    // identifies the shape: a qualified name in type position is the head of a definition, so its parentheses are
    // a parameter list and nothing else. These are the spellings where it must hold.
    for source in [
        "void Widget::draw(T) { }\n",
        "static void Widget::draw(T) { }\n",
        "void Widget::draw(int) { }\n",
        "void Widget::draw(Canvas&) { }\n",
        "void ns::C::method() { }\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(
            tree.get_errors(),
            [],
            "{source:?} must parse cleanly, got {:?}",
            tree.get_errors()
        );
        assert_eq!(
            count_of(source, CppSyntaxKind::ParameterList),
            1,
            "{source:?}: the parentheses are a parameter list"
        );
    }

    assert_eq!(
        count_of("void Widget::draw(T) { }\n", CppSyntaxKind::Parameter),
        1,
        "the unnamed parameter is read"
    );
}

/// A qualified declarator name behind a **storage specifier**, with a template-id in the name.
///
/// `void A::f<int>(int);` always read — the declaration's first token is the type keyword `void`. `static void
/// A::f<int>(int);` did not, and neither did `static void Widget::draw(T) { }`, because the questions that decide
/// whether a nameless declarator's parentheses are a parameter list were asked of the declaration's **first**
/// token: with `static` in front, the walk back found a storage specifier instead of the type and answered no.
///
/// The question is now asked of the whole head rather than its first token — "does this declaration name a
/// *qualified* type?" — which is the shape that actually matters: a qualified name in type position is the head
/// of a definition, and its declarator has no name of its own for the suffixes to attach to.
#[test]
fn a_qualified_declarator_behind_a_storage_specifier_is_still_a_definition() {
    for source in [
        "static void A::f<int>(int);\n",
        "inline void A::f<int>(int);\n",
        "extern void A::f<int>(int);\n",
        "constexpr void A::f<int>(int) { }\n",
        "static void A<int>::f<int>(int);\n",
        "static void A::f<int>(int) { }\n",
        "void A::f<int>(int) { }\n",
        "static void A::f(int);\n",
        "static void Widget::draw(T) { }\n",
        "static void Widget::draw(Canvas&) { }\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(
            tree.get_errors(),
            [],
            "{source:?} must parse cleanly, got {:?}",
            tree.get_errors()
        );
        assert_eq!(
            count_of(source, CppSyntaxKind::ParameterList),
            1,
            "{source:?}: the parentheses are a parameter list"
        );
    }

    // The template-id stays in the *type*, which is where a definition's qualified name belongs: the specifier
    // sequence walks `A::f<int>` as one name, so the declarator holds nothing.
    assert_eq!(
        count_of(
            "static void A::f<int>(int);\n",
            CppSyntaxKind::TemplateArgumentList
        ),
        1
    );
    assert_eq!(
        count_of(
            "static void A::f<int>(int);\n",
            CppSyntaxKind::InitDeclarator
        ),
        1
    );
}
