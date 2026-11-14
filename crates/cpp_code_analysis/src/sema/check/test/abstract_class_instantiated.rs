//! **`abstract-class-instantiated`** — a by-value object whose class has an unoverridden pure virtual.
//!
//! The positives are the two shapes an object of an abstract class takes: a local and a member. The silences are the
//! ones that keep the rule off correct code — a pointer, a reference, a derived class that overrides every pure
//! virtual, a class whose pure virtual is *defined* out of class (still abstract — a pure virtual's definition does not
//! make the class concrete), and a type the index never read.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **A local of an abstract class** is the plain case.
#[test]
fn a_local_of_an_abstract_class_is_reported() {
    let codes = codes_of(
        "struct Shape {\n    virtual void draw() = 0;\n};\nvoid f() {\n    Shape s;\n}\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::AbstractClassInstantiated,
        "`Shape` has an unoverridden `= 0`",
    );
}

/// **A member of an abstract class** is the same error one level out.
#[test]
fn a_member_of_an_abstract_class_is_reported() {
    let codes = codes_of(
        "struct Shape {\n    virtual void draw() = 0;\n};\nstruct Holder {\n    Shape shape;\n};\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::AbstractClassInstantiated,
        "`Holder` cannot hold a `Shape` by value",
    );
}

/// **A derived class that overrides the pure virtual** is concrete, and its objects are fine.
#[test]
fn an_object_of_an_overriding_class_is_silent() {
    let codes = codes_of(
        "struct Shape {\n    virtual void draw() = 0;\n};\nstruct Circle : Shape {\n    void draw() override;\n};\nvoid f() {\n    Circle c;\n}\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::AbstractClassInstantiated,
        "`Circle::draw` implements `Shape::draw`",
    );
}

/// **A derived class that does not override it** is abstract itself: its objects are reported.
#[test]
fn an_object_of_a_non_overriding_class_is_reported() {
    let codes = codes_of(
        "struct Shape {\n    virtual void draw() = 0;\n};\nstruct Ring : Shape {};\nvoid f() {\n    Ring r;\n}\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::AbstractClassInstantiated,
        "`Ring` inherits the pure `draw` and still has no body for it",
    );
}

/// **A pointer or a reference to an abstract class** is what the class exists for: silence, both ways.
#[test]
fn a_pointer_or_reference_to_an_abstract_class_is_silent() {
    let codes = codes_of(
        "struct Shape {\n    virtual void draw() = 0;\n};\nvoid f(Shape &r) {\n    Shape *p = &r;\n}\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::AbstractClassInstantiated,
        "a pointer and a reference point at a derived object",
    );
}

/// **A pure virtual defined out of class is still pure**: the class keeps its abstractness.
#[test]
fn a_defined_pure_virtual_does_not_make_the_class_concrete() {
    let codes = codes_of(
        "struct Shape {\n    virtual void draw() = 0;\n};\nvoid Shape::draw() {}\nvoid f() {\n    Shape s;\n}\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::AbstractClassInstantiated,
        "`Shape::draw` has a body, and is still `= 0`",
    );
}

/// **A class nothing declares** — a type from a header the analysis has not read — is silence.
#[test]
fn an_unknown_type_is_not_judged() {
    let codes = codes_of("void f() {\n    Widget w;\n}\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::AbstractClassInstantiated,
        "`Widget` was not read, so there is nothing to ask",
    );
}

/// **A template-id** is a different class per instantiation and the summary keeps one spelling: silence.
#[test]
fn a_template_id_is_not_judged() {
    let codes = codes_of(
        "template <class T>\nstruct Box {\n    virtual void put(T) = 0;\n};\nvoid f() {\n    Box<int> b;\n}\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::AbstractClassInstantiated,
        "`Box<int>` is not the spelling the member list is keyed by",
    );
}
