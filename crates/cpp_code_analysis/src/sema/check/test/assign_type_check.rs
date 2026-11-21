//! **`assign-type-check`** — a value that does not convert to the type it is assigned to, and the shapes that must
//! stay silent.
//!
//! The silent half is the contract this file mostly documents: a name the **implementation** reserves is supplied by
//! the compiler and cannot be judged (a system header's macro body expands into declarations of that shape), and a
//! braced initialiser, a construction, a default argument and a dedicated `auto` are other questions.

use super::{assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **A name the implementation reserves is not the reader's declaration.**
///
/// Measured on libuv 1.53.1's Windows build: `src/uv-common.c:576` reported
/// `` `__atomic_exchange_val` is initialised with a value that does not convert to `void` `` — the name comes from
/// the expansion of `atomic_exchange_explicit(…)` inside the project's own macro, so the declaration the rule judged
/// is one this layer made up. `docs/c-support.md` §7 records the measurement; the `undefined-name` rule has had this
/// exemption since the C corpus and this is the same claim about a different half of the reading.
#[test]
fn a_reserved_name_a_macro_body_expands_to_is_silent() {
    for source in [
        "void f(void) {\n    void __atomic_exchange_val = 0;\n}\n",
        "void f(void) {\n    int __builtin_alloca_val = 0;\n}\n",
        "void f(void) {\n    void __sync_fetch_and_add_val = 0;\n}\n",
    ] {
        let codes = codes_of(source);
        assert_silent_about(
            &codes,
            DiagnosticCode::AssignTypeCheck,
            "the name is one the compiler supplies, not a declaration the file wrote",
        );
    }
}

/// And the rule still reports the ordinary shape: a string where an `int` is declared.
#[test]
fn a_string_assigned_to_an_int_is_still_reported() {
    let codes = codes_of("void f(void) {\n    int count = \"three\";\n}\n");
    assert!(
        codes.contains(&DiagnosticCode::AssignTypeCheck),
        "a string literal does not convert to an int: {codes:?}"
    );
}
