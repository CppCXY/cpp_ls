//! **`include-not-found`** — an `#include` whose file is not there.
//!
//! The check adds nothing to the summary's answer: the resolver already looked, and `IncludeFact::resolved` is `None`
//! exactly when the search found nothing. The care is in what is **not** reported — a target that names a macro is not
//! a path until the macro is expanded, and another file's unresolved include is that file's own line rather than this
//! file's finding.

use super::{assert_reported, assert_silent_about, codes_in, codes_of};
use crate::DiagnosticCode;

/// **Both literal forms are reported**: an angled header nobody has, and a quoted one that is not beside the file.
#[test]
fn an_include_that_was_not_found_is_reported() {
    let angled = codes_of("#include <vectr>\nint main() { return 0; }\n");
    assert_reported(
        &angled,
        DiagnosticCode::IncludeNotFound,
        "`<vectr>` is not a header",
    );

    let quoted = codes_of("#include \"helpr.h\"\nint main() { return 0; }\n");
    assert_reported(
        &quoted,
        DiagnosticCode::IncludeNotFound,
        "nothing beside the file is called `helpr.h`",
    );
}

/// **A quoted include that is beside the file is resolved** — the finding is about the search having failed, not about
/// the spelling having delimiters.
#[test]
fn a_quoted_include_that_exists_is_not_reported() {
    let codes = codes_in(
        &[
            ("/p/api.h", "int declared_in_the_header();\n"),
            ("/p/a.cpp", "#include \"api.h\"\nint main() { return 0; }\n"),
        ],
        "/p/a.cpp",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::IncludeNotFound,
        "the quoted header is in the file's own directory",
    );
}

/// **A macro target is not a path**: `#include HEADER` says which header only after the macro is expanded, and a
/// resolver that did not expand it has learned nothing to report.
#[test]
fn an_include_that_names_a_macro_is_not_reported() {
    let codes =
        codes_of("#define HEADER <boost/version.hpp>\n#include HEADER\nint main() { return 0; }\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::IncludeNotFound,
        "the directive's target is a macro, not a literal path",
    );
}

/// **A header's own unresolved include is the header's line**, and this file's finding list must not borrow it.
#[test]
fn an_unresolved_include_in_another_file_is_not_this_files_finding() {
    let codes = codes_in(
        &[
            (
                "/p/broken.h",
                "#include <nowhere_at_all>\nint declared_in_the_header();\n",
            ),
            (
                "/p/a.cpp",
                "#include \"broken.h\"\nint main() { return 0; }\n",
            ),
        ],
        "/p/a.cpp",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::IncludeNotFound,
        "the missing header is written in `broken.h`, not here",
    );
}

/// **An `#include` in a branch this configuration does not take is not a missing file.**
///
/// neovim writes `#if defined(__linux__) / # include <execinfo.h> / #endif`, and no Windows build ever opens that
/// header. The resolver looked and failed because it was asked about a line that is not in the program: the guard
/// is decided `Inactive`, and the finding goes.
#[test]
fn an_include_in_a_branch_nobody_takes_is_not_reported() {
    let codes = codes_of("#if 0\n#include <vectr>\n#endif\nint main() { return 0; }\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::IncludeNotFound,
        "`#if 0` is a branch no compiler reads",
    );
}

/// **…and a branch that *is* taken still reports**, so the silence above is about the guard rather than about
/// `#if` in general.
#[test]
fn an_include_in_a_branch_that_is_taken_is_still_reported() {
    let codes = codes_of("#if 1\n#include <vectr>\n#endif\nint main() { return 0; }\n");
    assert_reported(
        &codes,
        DiagnosticCode::IncludeNotFound,
        "`#if 1` is compiled",
    );
}

/// **A branch the cook skipped is silent, even when the guard layer cannot evaluate the name.**
///
/// `#ifdef NEVER_DEFINED_ANYWHERE` is a branch C says is not compiled (an undefined name in `#ifdef` is false), and
/// the cook puts it in its inactive regions. The guard layer alone would answer `Unknown` and keep the finding —
/// which is why this used to assert that it was reported. The central filter in [`CheckContext::push`] now asks the
/// reading that actually decided the branch, so a diagnostic is never drawn on text the cook did not compile.
#[test]
fn an_include_behind_an_undecided_guard_is_silent_when_the_cook_skipped_it() {
    let codes = codes_of(
        "#ifdef NEVER_DEFINED_ANYWHERE\n#include <vectr>\n#endif\nint main() { return 0; }\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::IncludeNotFound,
        "the cook did not put that branch in the program",
    );
}
