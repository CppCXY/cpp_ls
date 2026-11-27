//! **`deref-type-check`** — `*` applied to something that is not a pointer to an object.
//!
//! The silence half is the contract here: a pointer of any pointee, a reference, an array and every name this layer
//! cannot resolve are all left alone, because a rule that underlines correct C is one a reader turns off.

use super::{assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **A number has nothing behind it.** `int value; *value` is an error in every compiler, and the rule's first shape.
#[test]
fn a_number_is_not_dereferenceable() {
    let codes = codes_of("int f(void) {\n    int value = 1;\n    return *value;\n}\n");
    assert!(codes.contains(&DiagnosticCode::DerefTypeCheck), "{codes:?}");
}

/// **`void` is not an object type**: `*memory = 1` through a `void *` has no size to write.
#[test]
fn a_void_pointer_has_nothing_behind_it() {
    let codes = codes_of("int f(void *memory) {\n    *memory = 1;\n    return 0;\n}\n");
    assert!(codes.contains(&DiagnosticCode::DerefTypeCheck), "{codes:?}");
}

/// **A reference is peeled**: in C++ `*r` is an error when `r` is an `int &`, and fine when it is an `int *&`.
#[test]
fn a_reference_to_a_number_is_not_dereferenceable() {
    let codes = codes_of("int f(int &number) {\n    return *number;\n}\n");
    assert!(codes.contains(&DiagnosticCode::DerefTypeCheck), "{codes:?}");

    let through = codes_of("int f(int *&pointer) {\n    return *pointer;\n}\n");
    assert_silent_about(
        &through,
        DiagnosticCode::DerefTypeCheck,
        "a reference to a pointer dereferences to what it points at",
    );
}

/// **Every pointer is dereferenceable**, and so is an array — the ordinary case, and the one that must stay silent.
#[test]
fn a_pointer_and_an_array_are_dereferenceable() {
    let pointer = codes_of("int f(int *values) {\n    return *values;\n}\n");
    assert_silent_about(
        &pointer,
        DiagnosticCode::DerefTypeCheck,
        "a pointer to int is what `*` is for",
    );

    let array = codes_of("int f(void) {\n    int values[4] = {0};\n    return *values;\n}\n");
    assert_silent_about(
        &array,
        DiagnosticCode::DerefTypeCheck,
        "an array dereferences to its first element",
    );

    let pointer_to_pointer = codes_of("int f(int **values) {\n    return **values;\n}\n");
    assert_silent_about(
        &pointer_to_pointer,
        DiagnosticCode::DerefTypeCheck,
        "`**` peels two pointers",
    );
}

/// **A name that may be a typedef is left alone.** `size_t` is a `Named` to this layer, and a name may be a typedef
/// for a pointer — only the file's own declarations would say which, and guessing costs a false positive on a
/// correct `*` of every `typedef`-ed pointer in a C project.
#[test]
fn a_name_that_may_be_a_typedef_is_not_judged() {
    let codes = codes_of("int f(size_t handle) {\n    return *handle;\n}\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::DerefTypeCheck,
        "`size_t` is a name, and a name may be a pointer's typedef",
    );
}
