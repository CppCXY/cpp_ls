//! **`macro-redefined`** — a `#define` that changes a name whose definition is already in force.
//!
//! The language permits a name to be defined more than once, and the rule's four conditions are each one of the ways
//! that is legal: the definition has to be **of this file**, **in force**, with **no `#undef` between** it and the
//! earlier one, and with a **body that differs**. The negatives below are that contract read in order — a definition
//! from a header, a definition in a branch nobody takes, an `#undef`, and a body that is identical.

use super::{assert_reported, assert_silent_about, codes_of, named_findings_in};
use crate::DiagnosticCode;

/// **The plain case**: one name, two different bodies, no `#undef` between them.
#[test]
fn a_second_definition_with_a_different_body_is_reported() {
    let object_like = codes_of("#define MAX 10\n#define MAX 20\nint main() { return MAX; }\n");
    assert_reported(
        &object_like,
        DiagnosticCode::MacroRedefined,
        "`MAX` changes from 10 to 20",
    );

    let function_like = codes_of(
        "#define TWICE(x) ((x) * 2)\n#define TWICE(x) ((x) * 3)\nint f(int n) { return TWICE(n); }\n",
    );
    assert_reported(
        &function_like,
        DiagnosticCode::MacroRedefined,
        "a function-like macro's replacement list changed too",
    );
}

/// **An identical redefinition is legal** — it is what a header included twice produces — and the bodies are compared
/// trimmed, so the newline a body range reaches is not a difference.
#[test]
fn an_identical_definition_is_not_reported() {
    let object_like = codes_of("#define MAX 10\n#define MAX 10\nint main() { return MAX; }\n");
    assert_silent_about(
        &object_like,
        DiagnosticCode::MacroRedefined,
        "the two replacement lists are the same text",
    );

    let function_like = codes_of(
        "#define TWICE(x) ((x) * 2)\n#define TWICE(x) ((x) * 2)\nint f(int n) { return TWICE(n); }\n",
    );
    assert_silent_about(
        &function_like,
        DiagnosticCode::MacroRedefined,
        "and so are these two",
    );
}

/// **An `#undef` between the two makes the second the name's first definition again.**
#[test]
fn an_undef_between_the_two_is_not_a_redefinition() {
    let codes =
        codes_of("#define MAX 10\n#undef MAX\n#define MAX 20\nint main() { return MAX; }\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::MacroRedefined,
        "no definition was in force when the second `MAX` was written",
    );
}

/// **A definition in a branch nobody takes is not in force** — the two definitions sit in mutually exclusive branches
/// of one `#if`, so at most one of them was ever written by the compiler and they cannot be each other's earlier
/// definition.
#[test]
fn a_definition_in_a_dead_branch_is_not_in_force() {
    let codes = codes_of(
        "#if 0\n#define MAX 10\n#else\n#define MAX 20\n#endif\nint main() { return MAX; }\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::MacroRedefined,
        "the two definitions are in mutually exclusive branches, so at most one is in force",
    );
}

/// **A definition in a header this file does not include is that header's business.**
///
/// The check reads the macros **in force** in this file, so a `MAX` defined in a header nobody includes is not a
/// redefinition here — while the same name defined twice in this file, or once in an included header and once here
/// with a different body, is exactly what the rule is about: a compiler warns on both.
#[test]
fn a_definition_in_a_header_this_file_does_not_include_is_not_a_redefinition() {
    let findings = named_findings_in(
        &[
            ("/p/other.h", "#define MAX 10\n"),
            (
                "/p/a.cpp",
                "#define MAX 20\n#define MIN 1\n#define MIN 2\nint main() { return MIN; }\n",
            ),
        ],
        "/p/a.cpp",
    );
    assert!(
        !findings
            .iter()
            .any(|(code, name)| *code == DiagnosticCode::MacroRedefined && name == "MAX"),
        "`MAX` is defined once in this file, and the header is not included: {findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|(code, name)| *code == DiagnosticCode::MacroRedefined && name == "MIN"),
        "while `MIN` really is redefined in this file: {findings:?}"
    );

    // **A definition *through* an include is that header's business too**, and this is the rule's stated scope: only
    // the file's own `#define`s are compared. A header legitimately redefines a name after `#undef`, and reporting the
    // include's line here would point at code the reader may not own.
    let findings = named_findings_in(
        &[
            ("/p/api.h", "#define MAX 10\n"),
            ("/p/a.cpp", "#include \"api.h\"\n#define MAX 20\n"),
        ],
        "/p/a.cpp",
    );
    assert!(
        !findings
            .iter()
            .any(|(code, name)| *code == DiagnosticCode::MacroRedefined && name == "MAX"),
        "the rule compares this file's own definitions, not the headers': {findings:?}"
    );
}
