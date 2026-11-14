//! **`override-nonexistent`** — `override` on a function no base declares.
//!
//! The rule is only as strong as what this layer can prove: a class with no bases, and a class whose every base was
//! read and none of which declares the name. A base that could not be read, and a base that *does* declare the name
//! (whose signature is not compared — see the module), are both silence.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **A class with no base at all**: `override` can only be ill-formed.
#[test]
fn override_in_a_class_with_no_base_is_reported() {
    let codes = codes_of("struct Alone {\n    void run() override;\n};\n");
    assert_reported(
        &codes,
        DiagnosticCode::OverrideNonexistent,
        "`override` needs a base",
    );
}

/// **A base that does not declare the name**: nothing can be overridden.
#[test]
fn override_of_a_name_no_base_declares_is_reported() {
    let codes = codes_of(
        "struct Base { virtual void run(); };\nstruct Derived : Base { void walk() override; };\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::OverrideNonexistent,
        "`Base` declares `run`, not `walk`",
    );
}

/// **`override` outside a class definition** — a free function cannot override anything.
#[test]
fn override_on_a_free_function_is_reported() {
    let codes = codes_of("void run() override;\n");
    assert_reported(
        &codes,
        DiagnosticCode::OverrideNonexistent,
        "a free function has no base class",
    );
}

/// **A base that does declare it** is silent — that is the shape the keyword exists for.
#[test]
fn override_of_a_name_a_base_declares_is_silent() {
    let codes = codes_of(
        "struct Base { virtual void run(); };\nstruct Derived : Base { void run() override; };\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::OverrideNonexistent,
        "`Base` declares `run`",
    );
}

/// **A base whose header was never read** is silence: the name may be in it.
#[test]
fn an_unreadable_base_is_not_judged() {
    let codes = codes_of("struct Derived : NotHere { void walk() override; };\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::OverrideNonexistent,
        "`NotHere` was not read, so `walk` may be its member",
    );
}

/// **A member defined out of line** has no bases in its own fact — the header's declaration is the one that knows
/// them, and the `.cpp` must not be told it has no base class.
#[test]
fn a_member_defined_in_a_cpp_is_not_judged_by_the_cpp() {
    let codes = super::codes_in(
        &[
            ("/p/base.h", "struct Base { virtual void run(); };\n"),
            (
                "/p/derived.h",
                "#include \"base.h\"\nstruct Derived : Base { void run() override; };\n",
            ),
            (
                "/p/derived.cpp",
                "#include \"derived.h\"\nvoid Derived::run() {}\n",
            ),
        ],
        "/p/derived.cpp",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::OverrideNonexistent,
        "the `.cpp` has no bases of its own",
    );
}
/// **A destructor marked `override`** is legal and must stay silent: `~B` overrides `~A`, and the two spellings never
/// match — a name comparison would report every such destructor in a class with a base.
#[test]
fn an_overriding_destructor_is_silent() {
    let codes = codes_of(
        "struct A { virtual ~A(); };\nstruct B : A { ~B() override; };\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::OverrideNonexistent,
        "`~B` overrides `~A`, whose name is a different spelling",
    );
}