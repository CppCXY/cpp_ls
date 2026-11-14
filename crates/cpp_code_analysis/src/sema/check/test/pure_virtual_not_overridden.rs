//! **`pure-virtual-not-overridden`** — a class that wrote `override` for some of a base's pure virtuals and left the
//! rest.
//!
//! The silences are the ones that make the rule worth having: an interface that declares `= 0` and overrides nothing
//! is a deliberate abstract class, a fully implemented class is the goal, and a base nobody read cannot be judged.

use super::{assert_reported, assert_silent_about, codes_of, codes_in};
use crate::DiagnosticCode;

/// **One of two pure virtuals implemented** is the shape that reads as an oversight.
#[test]
fn a_class_that_implements_some_pure_virtuals_is_reported() {
    let codes = codes_of(
        "struct Shape {\n    virtual void draw() = 0;\n    virtual double area() = 0;\n};\nstruct Circle : Shape {\n    void draw() override;\n};\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::PureVirtualNotOverridden,
        "`Circle` wrote `override` for `draw` and not for `area`",
    );
}

/// **Every pure virtual implemented** is silence — the goal, not a finding.
#[test]
fn a_fully_implemented_class_is_silent() {
    let codes = codes_of(
        "struct Shape {\n    virtual void draw() = 0;\n    virtual double area() = 0;\n};\nstruct Circle : Shape {\n    void draw() override;\n    double area() override;\n};\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::PureVirtualNotOverridden,
        "both are implemented",
    );
}

/// **An interface that overrides nothing** is a deliberate abstract class, not an oversight.
#[test]
fn a_class_that_overrides_nothing_is_silent() {
    let codes = codes_of(
        "struct Shape {\n    virtual void draw() = 0;\n};\nstruct Ring : Shape {};\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::PureVirtualNotOverridden,
        "`Ring` declares a new abstract class on purpose",
    );
}

/// **A class with its own pure virtual and nothing overridden** is an interface: silence.
#[test]
fn a_plain_interface_is_silent() {
    let codes = codes_of("struct Shape {\n    virtual void draw() = 0;\n};\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::PureVirtualNotOverridden,
        "an interface is what `= 0` is for",
    );
}

/// **A base nobody read** cannot be judged.
#[test]
fn an_unreadable_base_is_silent() {
    let codes = codes_of("struct D : NotHere {\n    void draw() override;\n};\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::PureVirtualNotOverridden,
        "`NotHere` was not read",
    );
}

/// **Two levels down**: a middle class leaves one pure virtual, and the concrete class implements it — the middle
/// class is reported and the leaf is not.
#[test]
fn the_incomplete_middle_class_is_reported_and_the_leaf_is_not() {
    let codes = codes_in(
        &[(
            "/p/shapes.h",
            "struct Shape {\n    virtual void draw() = 0;\n    virtual double area() = 0;\n};\nstruct Middle : Shape {\n    void draw() override;\n};\nstruct Circle : Middle {\n    double area() override;\n};\n",
        )],
        "/p/shapes.h",
    );
    assert_reported(
        &codes,
        DiagnosticCode::PureVirtualNotOverridden,
        "`Middle` still has `area`",
    );
    // The finding names the class, so a leaf that implements everything contributes none.
    let named: Vec<(DiagnosticCode, String)> =
        super::named_findings_in(&[(
            "/p/shapes.h",
            "struct Shape {\n    virtual void draw() = 0;\n    virtual double area() = 0;\n};\nstruct Middle : Shape {\n    void draw() override;\n};\nstruct Circle : Middle {\n    double area() override;\n};\n",
        )], "/p/shapes.h");
    assert!(
        !named
            .iter()
            .any(|(code, name)| *code == DiagnosticCode::PureVirtualNotOverridden && name == "Circle"),
        "`Circle` implements both: {named:?}"
    );
}
