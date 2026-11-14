//! **What each check says, and what it refuses to say** — one file per diagnostic code.
//!
//! # Why this exists beside `tests/checks.rs`
//!
//! `tests/checks.rs` asks the **product's** question: one source, through [`crate::Session::diagnostics`], and the
//! codes that come out. That is the right shape for the end-to-end fixtures — a finding passes the config, the
//! model's three-valued answers and the rendering before it is shown — and the wrong shape for the *behaviour of one
//! rule*, which needs thirty small cases rather than one large one.
//!
//! So the rules' own fixtures live here, next to the rules, split by [`DiagnosticCode`]: a reader who is changing
//! `argument-count` opens `argument_count.rs` and sees every shape that rule answers for — and, more importantly,
//! every shape it must stay **silent** on. The silence half is the contract: an editor that underlines correct code
//! is one the user turns off, and then it reports nothing at all, forever.
//!
//! # The questions every file asks
//!
//! ```text
//!   codes_of(source)              what this file produces, through the whole session
//!   codes_in(files, asking)       the same for a project of several files
//!   named_findings_in(...)        (code, name) — for a rule that may report about several names at once
//! ```
//!
//! Both go through [`crate::Session::diagnostics`], which is what a client is shown. A file that asserts on a code
//! **not** being present is the common case, and it says so in its message rather than in a comment.

use crate::{
    CompilerConfig, DiagnosticCode, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// The codes one file produces, through the whole session — exactly what a diagnostics request answers.
pub(super) fn codes_of(source: &str) -> Vec<DiagnosticCode> {
    codes_in(&[("/p/a.cpp", source)], "/p/a.cpp")
}

/// The codes a file produces in a project of several files.
///
/// The rules that read another declaration need it: a default argument written in a header, a constructor of a class
/// declared elsewhere, a forward declaration in an included header. `files` is `(path, text)`, and `asking` is the
/// file the findings are asked of.
pub(super) fn codes_in(files: &[(&str, &str)], asking: &str) -> Vec<DiagnosticCode> {
    session_with(files, asking)
        .diagnostics(std::path::Path::new(asking))
        .map(|found| {
            found
                .checks
                .iter()
                .map(|finding| finding.code)
                .chain(found.errors.iter().map(|error| error.code))
                .collect()
        })
        .unwrap_or_default()
}

/// The findings of one file as `(code, name)` — for a rule that may report about **several names at once**.
///
/// A silence assertion about one name needs this: "no `MacroRedefined` at all" fails on the fixture's *other* line,
/// and the message is not the place to look for a name when the finding carries it. See `macro_redefined.rs`.
pub(super) fn named_findings_in(
    files: &[(&str, &str)],
    asking: &str,
) -> Vec<(DiagnosticCode, String)> {
    session_with(files, asking)
        .diagnostics(std::path::Path::new(asking))
        .map(|found| {
            found
                .checks
                .iter()
                .map(|finding| (finding.code, finding.name.clone()))
                .chain(found.errors.iter().map(|error| (error.code, String::new())))
                .collect()
        })
        .unwrap_or_default()
}

/// **No finding of this code, with the list in the message** — the assertion almost every fixture here makes.
pub(super) fn assert_silent_about(codes: &[DiagnosticCode], code: DiagnosticCode, why: &str) {
    assert!(
        !codes.contains(&code),
        "{why} (the file produced {codes:?})"
    );
}

/// **This code is present, with the list in the message.**
pub(super) fn assert_reported(codes: &[DiagnosticCode], code: DiagnosticCode, why: &str) {
    assert!(codes.contains(&code), "{why} (the file produced {codes:?})");
}

/// The session every fixture is asked through — indexed, with the asking file's rendering built.
fn session_with(files: &[(&str, &str)], asking: &str) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    for (path, text) in files {
        memory = memory.with_file(*path, *text);
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files(
        files
            .iter()
            .map(|(path, _)| std::path::PathBuf::from(*path)),
    );
    session.want_cooked_reading(std::path::Path::new(asking));
    session.index_everything();
    session
}

mod access_invisible;
mod abstract_class_instantiated;
mod pure_virtual_not_overridden;
mod argument_count;
mod assignment_in_condition;
mod braces_around_statements;
mod concept_not_satisfied;
mod contract_placement;
mod final_hierarchy;
mod final_misplaced;
mod override_nonexistent;
mod cstyle_cast;
mod dangling_field;
mod dangling_return;
mod else_after_return;
mod implicit_bool_conversion;
mod include_not_found;
mod macro_parentheses;
mod macro_redefined;
mod missing_include;
mod missing_return;
mod redefinition;
mod return_type_mismatch;
mod simplify_boolean_expr;
mod syntax_error;
mod undefined_field;
mod undefined_name;
mod unnecessary_branch;
mod unused;
mod use_after_free;
mod use_auto;
mod use_nullptr;
mod use_override;
