//! **What the checks say, and what they refuse to say.**
//!
//! One positive per rule — the smallest fixture that must produce a finding — and a negative beside it, because
//! this layer's contract is not "find problems" but "**never invent one**": an editor that underlines correct code
//! is one the user turns off, and then it reports nothing at all, forever.
//!
//! The fixtures are asked through [`Session::diagnostics`] rather than through a check called directly, so that
//! what is asserted is what a client is shown: the run, the codes, and the two filters a finding passes on its way
//! out (the config, and the model's three-valued answers).

use cpp_code_analysis::{
    CompilerConfig, DiagnosticCode, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// **The codes a file produces** — through the whole session, exactly as a diagnostics request does.
///
/// The ask and the pump are both here because a rendering is built when something asks for it: see
/// `tests/completion.rs`'s `cooked_view` for the note every fixture in this workspace shares.
fn codes_of(source: &str) -> Vec<DiagnosticCode> {
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();

    session
        .diagnostics(std::path::Path::new("/p/a.cpp"))
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

/// **A file that is correct produces nothing.** The contract of the layer, and the first assertion of every rule
/// below — a check that cannot be quiet about this file is a check that will fire on a real one.
#[test]
fn a_file_with_nothing_wrong_reports_nothing() {
    let codes = codes_of("int add(int a, int b) { return a + b; }\n");
    assert!(
        codes.is_empty(),
        "a plain function is not a finding: {codes:?}"
    );
}

#[test]
fn syntax_error_is_reported() {
    let codes = codes_of("int main( { return 0; }\n");
    assert!(
        codes.contains(&DiagnosticCode::SyntaxError),
        "an unclosed parameter list does not parse: {codes:?}"
    );
}

#[test]
fn unused_is_reported_for_a_local_nothing_reads() {
    let codes = codes_of("int main() { int never_used = 1; return 0; }\n");
    assert!(
        codes.contains(&DiagnosticCode::Unused),
        "`never_used` is written once in the file: {codes:?}"
    );
}

#[test]
fn unused_is_not_reported_for_a_local_that_is_used() {
    let codes = codes_of("int main() { int used = 1; return used; }\n");
    assert!(
        !codes.contains(&DiagnosticCode::Unused),
        "`used` is read on the next line: {codes:?}"
    );
}

#[test]
fn unused_is_not_reported_for_a_name_that_says_it_is_deliberate() {
    // `_name` is how a reader says "I know" — the convention, and the reason the rule stays off those names.
    let codes = codes_of("int main() { int _unused = 1; return 0; }\n");
    assert!(
        !codes.contains(&DiagnosticCode::Unused),
        "a leading underscore is a deliberate choice: {codes:?}"
    );
}

#[test]
fn include_not_found_is_reported_for_a_header_nobody_has() {
    let codes = codes_of("#include <vectr>\nint main() { return 0; }\n");
    assert!(
        codes.contains(&DiagnosticCode::IncludeNotFound),
        "`<vectr>` is not a header: {codes:?}"
    );
}

#[test]
fn macro_redefined_is_reported_for_a_second_different_definition() {
    let codes = codes_of("#define MAX 10\n#define MAX 20\nint main() { return MAX; }\n");
    assert!(
        codes.contains(&DiagnosticCode::MacroRedefined),
        "one name, two bodies, no `#undef`: {codes:?}"
    );
}

#[test]
fn macro_redefined_is_not_reported_for_an_identical_definition() {
    // The one shape the language permits, and it is what a header included twice produces.
    let codes = codes_of("#define MAX 10\n#define MAX 10\nint main() { return MAX; }\n");
    assert!(
        !codes.contains(&DiagnosticCode::MacroRedefined),
        "an identical redefinition is legal: {codes:?}"
    );
}

#[test]
fn unnecessary_branch_is_reported_for_a_literal_condition() {
    let codes = codes_of("int main() { if (false) { return 1; } return 0; }\n");
    assert!(
        codes.contains(&DiagnosticCode::UnnecessaryBranch),
        "`if (false)` cannot run: {codes:?}"
    );
}

#[test]
fn unnecessary_branch_is_not_reported_for_a_name() {
    // A name is a question, not a fact about the text: folding it is another layer's job.
    let codes = codes_of("int main() { if (DEBUG) { return 1; } return 0; }\n");
    assert!(
        !codes.contains(&DiagnosticCode::UnnecessaryBranch),
        "`DEBUG` is not a literal: {codes:?}"
    );
}

/// **Every code has a name, and the name round-trips** — the property a client's filter and a config file both
/// depend on, asserted here because it is the one thing that cannot be checked by looking at a fixture.
#[test]
fn every_code_has_a_name_and_the_name_comes_back() {
    for code in DiagnosticCode::ALL {
        let name = code.name();
        assert!(!name.is_empty(), "{code:?} has no name");
        assert_eq!(
            DiagnosticCode::from_name(name),
            Some(code),
            "`{name}` does not come back as {code:?}"
        );
    }
}
