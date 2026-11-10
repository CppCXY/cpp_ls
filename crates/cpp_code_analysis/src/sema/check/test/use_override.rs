//! **`use-override`** — a member function that overrides a base's virtual and does not say `override`.
//!
//! The rule needs three facts together: a class with bases, a member function written without `override` or `final`,
//! and a base that **actually declares that name** — the last clause is what keeps a freshly written virtual that
//! overrides nothing out of the report. A class with no bases, a member the base does not declare, and a member that
//! already says `override` or `final` are all silence below.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The plain shape**: `Derived::f` overrides `Base::f` and does not say so — and it need not repeat `virtual`,
/// which is the first version's bug this fixture keeps fixed.
#[test]
fn an_override_without_the_keyword_is_reported() {
    let codes =
        codes_of("struct Base { virtual void f(); };\nstruct Derived : Base { void f(); };\n");
    assert_reported(
        &codes,
        DiagnosticCode::UseOverride,
        "`Derived::f` overrides `Base::f`, and neither keyword is written",
    );
}

/// **Saying either keyword is the answer**: `override` for an ordinary override, and `final` also ends the chain.
#[test]
fn a_member_that_says_override_or_final_is_silent() {
    let overridden = codes_of(
        "struct Base { virtual void f(); };\nstruct Derived : Base { void f() override; };\n",
    );
    assert_silent_about(
        &overridden,
        DiagnosticCode::UseOverride,
        "`override` says it",
    );

    let finalised = codes_of(
        "struct Base { virtual void f(); };\nstruct Derived : Base { void f() final; };\n",
    );
    assert_silent_about(&finalised, DiagnosticCode::UseOverride, "`final` says it");
}

/// **A virtual that overrides nothing is not an override**: the base does not declare the name, so there is nothing
/// for `override` to check.
#[test]
fn a_member_the_base_does_not_declare_is_silent() {
    let codes = codes_of(
        "struct Base { virtual void f(); };\nstruct Derived : Base { virtual void g(); };\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::UseOverride,
        "`Base` declares no `g`, so there is no override to annotate",
    );
}

/// **A class with no bases has nothing to override** — the declarations the rule walks are filtered on a non-empty
/// base list.
#[test]
fn a_class_with_no_bases_is_silent() {
    let codes = codes_of("struct Alone { virtual void f(); };\n");
    assert_silent_about(&codes, DiagnosticCode::UseOverride, "the class has no base");
}
