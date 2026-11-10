//! **`redefinition`** — a name declared twice in one function body, block or lambda.
//!
//! Only the kinds the language has no second reading for are judged: a function name is an **overload set**, a class
//! name may be a forward declaration and its definition, and a file-scope variable may be `extern` here and defined
//! there — while two variables of one name in one scope cannot both exist. The rule also leaves the scopes a reader
//! does not write a declaration in, which is why a class body and a file are quiet below.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The plain case**: two variables of one name in one function body. `auto` is the spelling the sample project
/// wrote, and the type does not matter — the name does.
#[test]
fn a_local_declared_twice_in_one_scope_is_reported() {
    let twice = codes_of("void f() {\n    int count = 1;\n    int count = 2;\n}\n");
    assert_reported(
        &twice,
        DiagnosticCode::Redefinition,
        "`count` is declared twice in the function's own scope",
    );

    let auto_twice = codes_of("void f() {\n    auto x = 123;\n    auto x = 0;\n}\n");
    assert_reported(
        &auto_twice,
        DiagnosticCode::Redefinition,
        "two `auto` declarations of `x` are two variables with one name",
    );
}

/// **A parameter and a local are two declarations in the same scope**, so this is the same mistake one line apart.
#[test]
fn a_parameter_and_a_local_share_the_function_scope() {
    let shadows_a_parameter = codes_of("void f(int count) {\n    int count = 2;\n}\n");
    assert_reported(
        &shadows_a_parameter,
        DiagnosticCode::Redefinition,
        "the parameter and the local `count` are both declared in `f`'s scope",
    );
}

/// **The repeats the language has a reading for are quiet** — an overload set, a forward declaration and its
/// definition, and `extern` followed by a definition.
#[test]
fn a_repeat_the_language_has_a_reading_for_is_quiet() {
    let overloads = codes_of("void run();\nvoid run(int value);\n");
    assert_silent_about(
        &overloads,
        DiagnosticCode::Redefinition,
        "two functions of one name are two overloads, which is the language's own reading",
    );

    let forward_and_definition = codes_of("class Widget;\nclass Widget { int size; };\n");
    assert_silent_about(
        &forward_and_definition,
        DiagnosticCode::Redefinition,
        "`class Widget;` and its definition are one entity written twice",
    );

    let extern_and_definition = codes_of("extern int value;\nint value = 0;\n");
    assert_silent_about(
        &extern_and_definition,
        DiagnosticCode::Redefinition,
        "`extern int value;` promises the same entity the definition provides",
    );
}

/// **A repeat in another scope is a shadow, not a redefinition** — and the scopes this rule does not judge (a class
/// body, a file) are quiet even when they hold a name twice.
#[test]
fn a_repeat_in_another_scope_is_a_shadow() {
    let two_blocks = codes_of("void f() {\n    { int count = 1; }\n    { int count = 2; }\n}\n");
    assert_silent_about(
        &two_blocks,
        DiagnosticCode::Redefinition,
        "the two `count`s are in two sibling blocks, which are two scopes",
    );

    let shadowing = codes_of("void f() {\n    int count = 1;\n    { int count = 2; }\n}\n");
    assert_silent_about(
        &shadowing,
        DiagnosticCode::Redefinition,
        "the inner `count` is a different scope's declaration, which C++ lets shadow",
    );

    let a_class_body = codes_of("struct S {\n    int size;\n    int size;\n};\n");
    assert_silent_about(
        &a_class_body,
        DiagnosticCode::Redefinition,
        "the rule judges function bodies, blocks and lambdas, and a class body is none of them",
    );
}
