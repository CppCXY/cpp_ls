//! **`final-class-derived`** and **`override-of-final`** — what `final` forbids below a declaration.
//!
//! `struct Gadget : Widget` where `Widget` is `final` is ill-formed, and so is an override of a function the base
//! declares `final`. The silences are the interesting half: a base nobody read, an override of a name the base does not
//! declare `final`, and the overload case — `f(int) final` beside `f(double)` leaves `f(double) override` legal.

use super::{assert_reported, assert_silent_about, codes_of, codes_in};
use crate::DiagnosticCode;

/// **Deriving from a `final` class** is reported at the deriving class.
#[test]
fn a_class_deriving_from_a_final_class_is_reported() {
    let codes = codes_of(
        "struct Widget final {\n    virtual void draw();\n};\nstruct Gadget : Widget {};\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::FinalClassDerived,
        "`Widget` is final",
    );
}

/// **A base declared in a header this file includes** is the ordinary shape, and it is judged too.
#[test]
fn a_final_base_from_another_file_is_reported() {
    let codes = codes_in(
        &[
            ("/p/widget.h", "struct Widget final {};\n"),
            ("/p/gadget.cpp", "#include \"widget.h\"\nstruct Gadget : Widget {};\n"),
        ],
        "/p/gadget.cpp",
    );
    assert_reported(
        &codes,
        DiagnosticCode::FinalClassDerived,
        "the header says `final`",
    );
}

/// **A base that is not final** is silence — the negative that keeps the rule from firing on every derivation.
#[test]
fn a_class_deriving_from_a_plain_class_is_silent() {
    let codes = codes_of("struct Widget {\n    virtual void draw();\n};\nstruct Gadget : Widget {};\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::FinalClassDerived,
        "`Widget` is not final",
    );
}

/// **A base nobody read** is silence: its `final` may be in the header the analysis has not seen.
#[test]
fn an_unreadable_final_base_is_not_judged() {
    let codes = codes_of("struct Gadget : NotHere {};\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::FinalClassDerived,
        "`NotHere` was not read",
    );
}

/// **Overriding a function a base declares `final`** is reported.
#[test]
fn an_override_of_a_final_function_is_reported() {
    let codes = codes_of(
        "struct Widget {\n    virtual void draw() final;\n};\nstruct Gadget : Widget {\n    void draw() override;\n};\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::OverrideOfFinal,
        "`Widget::draw` is final",
    );
}

/// **An overload the base does not declare final** leaves the override legal: `f(int) final` beside `f(double)`.
#[test]
fn an_override_beside_a_final_overload_is_silent() {
    let codes = codes_of(
        "struct Widget {\n    virtual void draw(int) final;\n    virtual void draw(double);\n};\nstruct Gadget : Widget {\n    void draw(double) override;\n};\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::OverrideOfFinal,
        "`draw(double)` is not the final overload",
    );
}


