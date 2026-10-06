//! Semantic checks: what [`cpp_code_analysis::sema::check`] claims, and what it refuses to claim.
//!
//! Two questions per check, and the second is the one that matters:
//!
//! ```text
//! does it fire on the construct it is about?          a check that never fires is not a check
//! does it stay silent on everything else?             a check that fires on correct code is worse than none
//! ```
//!
//! Every test here is written against a **fixture whose right answer is known**, never against "whatever the
//! code does today" — a test that pins current behaviour cannot tell a fix from a regression.

use cpp_code_analysis::{Checks, CompilerConfig, MemoryFiles, SummaryStore};

/// Index `files`, starting from `/q/main.cpp`, and run every check over it.
///
/// `main` is `/q/main.cpp`'s text, passed rather than read back out of the provider: a check is handed the
/// bytes its summary's ranges are offsets into, and a test that derived them from somewhere else would be
/// checking a different file from the one the index saw.
fn findings(files: MemoryFiles, main: &str) -> Vec<cpp_code_analysis::Finding> {
    // **The same text, parsed again**, because a check is handed the tree the reading was made from and the
    // index does not hand its tree out. The parse is deterministic, so this is the same tree — and a test that
    // parsed something else would be checking a different file.
    let tree = cpp_parser::CppParser::parse(main, cpp_parser::ParserConfig::default());
    let root = tree.get_red_root();

    // **A directory of this call's own.** Every test in this file goes through here and they run in parallel,
    // so a shared name meant one test removing the directory another was still writing into — a flake that
    // showed up as `13 passed; 1 failed` in a full run and `14 passed` when the file was run alone. The counter
    // is enough: the name only has to be unique within one process, and unique per *call* rather than per test,
    // because one test can ask more than once.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "cppls-check-tests-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the test directory");

    let mut store = SummaryStore::with_provider(&dir, CompilerConfig::default(), files);
    store.index_includes_from(std::path::Path::new("/q/main.cpp"), Default::default());

    // **The scopes of the same file**, built from the same tree a check is handed: a check that asks about a *type*
    // resolves a name through them, and a fixture that passed a different file's scopes would be checking a
    // different program.
    let scopes = cpp_code_analysis::build_scopes(&root, &cpp_code_analysis::NoMacroBodies);

    let index = store.index();
    let Some(summary) = index.summary(std::path::Path::new("/q/main.cpp")) else {
        panic!("main.cpp must be indexed");
    };

    Checks {
        path: std::path::Path::new("/q/main.cpp"),
        summary,
        index,
        source: main,
        tree: &root,
        scopes: &scopes,
    }
    .run()
}

/// **The construct the check is about.** A literal target that is not there, in both spellings.
///
/// The two forms are one check because they are one mistake with two syntaxes, and they are asserted together
/// so that a change which fixes one and breaks the other cannot pass.
#[test]
fn an_include_whose_file_is_absent_is_reported() {
    const MAIN: &str = "#include <no_such_header_at_all>\n#include \"no_such_local_header.h\"\nint main() { return 0; }\n";

    let found = findings(
        MemoryFiles::new().with_file("/q/main.cpp", MAIN),
        MAIN,
    );

    let messages: Vec<&str> = found.iter().map(|finding| finding.message.as_str()).collect();
    assert_eq!(
        found.len(),
        2,
        "one finding per unresolvable literal include: {messages:?}"
    );
    assert!(
        messages[0].contains("no_such_header_at_all"),
        "an angle include names the header and where it was looked for: {messages:?}"
    );
    assert!(
        messages[1].contains("no_such_local_header.h"),
        "a quoted include names the file too: {messages:?}"
    );
    assert!(
        found.iter().all(|finding| finding.check == "an_include_is_found"),
        "each finding says which check produced it"
    );
    assert!(
        found[0].range.start_offset < found[1].range.start_offset,
        "findings are in the order they appear in the file"
    );
}

/// **The one form the check must refuse**, which is the whole of the care in it.
///
/// `#include MACRO` names a target that is not known until the macro is expanded, so a resolver that has not
/// expanded it has learned nothing. Reporting it would be reporting the *shape* of the directive — "this is not
/// a literal path" — as though it were a fact about the file system, which is exactly the `Unknown` this layer
/// promises never to report.
#[test]
fn a_macro_target_that_did_not_resolve_is_not_reported() {
    const MAIN: &str =
        "#define WHICH_HEADER <vector>\n#include WHICH_HEADER\nint main() { return 0; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert!(
        found.is_empty(),
        "a macro target is `Unknown`, and this layer reports nothing on `Unknown`: {found:?}"
    );
}

/// **A file whose includes all resolve reports nothing** — the other half of "stay silent on correct code".
///
/// `/q/good.h` is a real file in the same provider, so the quoted include is found and the check has no claim
/// to make. Without this, a check that reported *every* include would pass the test above.
#[test]
fn an_include_that_resolves_is_not_reported() {
    const MAIN: &str = "#include \"good.h\"\nint main() { Good good; return good.g; }\n";

    let found = findings(
        MemoryFiles::new()
            .with_file("/q/good.h", "#pragma once\nstruct Good { int g; };\n")
            .with_file("/q/main.cpp", MAIN),
        MAIN,
    );

    assert!(
        found.is_empty(),
        "the include resolved, so there is nothing to say about it: {found:?}"
    );
}

/// **The construct this check is about**: an `#error` the compiler would reach.
///
/// Measured before the check existed: this file produced **nothing at all** — zero errors, because the
/// directive is not a syntax error, and the message never reached a consumer.
#[test]
fn an_error_in_a_compiled_branch_is_reported() {
    const MAIN: &str = "#error this build is not supported\nint main() { return 0; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert_eq!(found.len(), 1, "one `#error`, one finding: {found:?}");
    assert_eq!(found[0].check, "an_error_the_file_asks_for");
    assert!(
        found[0].message.contains("this build is not supported"),
        "the author's own sentence is the most useful thing here, so it is shown: {}",
        found[0].message
    );
}

/// **The half that makes this a check rather than a text search**: a branch that is not compiled.
///
/// `#error` in the arm of an `#if` that was not taken is how a portable header explains which platforms it
/// supports. Reporting it would fire on nearly every header in the standard library's closure.
#[test]
fn an_error_in_a_branch_that_is_not_compiled_is_not_reported() {
    const MAIN: &str =
        "#if 0\n#error only for the other platform\n#endif\nint main() { return 0; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert!(
        found.is_empty(),
        "`#if 0` is decided and not taken, so a compiler never reads that line: {found:?}"
    );
}

/// …and the same file with the `#error` in the branch that **is** taken, so that the test above cannot pass by
/// reporting nothing ever.
#[test]
fn an_error_in_the_taken_branch_of_the_same_conditional_is_reported() {
    const MAIN: &str =
        "#if 0\n#define UNUSED 1\n#else\n#error the branch that is compiled\n#endif\nint main() { return 0; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert_eq!(
        found.len(),
        1,
        "the same `#if`, read the other way round, is a finding: {found:?}"
    );
    assert!(found[0].message.contains("the branch that is compiled"));
}

/// **A condition nothing can decide is `Unknown`, and this layer reports nothing on `Unknown`.**
///
/// This is the ordinary case in real code — `#ifdef _WIN32` in a file whose closure says nothing about the
/// platform — and treating "cannot tell" as "compiled" would fire on every one of them.
#[test]
fn an_error_under_an_undecidable_condition_is_not_reported() {
    const MAIN: &str = "#ifdef SOMETHING_NOBODY_DEFINES\n#error might or might not be read\n#endif\nint main() { return 0; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert!(
        found.is_empty(),
        "nothing here can decide the condition, so the directive is not known to be compiled: {found:?}"
    );
}

/// `#warning` is a diagnostic a reader may ignore, and it is not what this check is named for.
///
/// It goes unchecked rather than reported at a lower severity because this layer has one severity, and a
/// channel that mixes "this compilation stops" with "you may want to look at this" is a channel whose most
/// important message is lost among the others.
#[test]
fn a_warning_directive_is_not_reported() {
    const MAIN: &str = "#warning this build is untested\nint main() { return 0; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert!(
        found.is_empty(),
        "`#warning` is not `#error`, and this check reports only the one: {found:?}"
    );
}

/// **Two `#error`s, one live and one not** — the check attributes each to its own region rather than to the
/// file, which is the whole of the containment test in it.
#[test]
fn only_the_live_error_of_two_is_reported() {
    const MAIN: &str = "#if 0\n#error dead\n#endif\n#error live\nint main() { return 0; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert_eq!(found.len(), 1, "exactly one of the two is compiled: {found:?}");
    assert!(
        found[0].message.contains("live"),
        "and it is the one outside the dead region: {}",
        found[0].message
    );
}
///
/// The second is the finding: it is the one a reader has to look at, and the message names the first so the two
/// lines can be compared.
#[test]
fn a_macro_defined_twice_with_a_different_body_is_reported() {
    const MAIN: &str = "#define BUFFER_SIZE 256\n#define BUFFER_SIZE 512\nint main() { return BUFFER_SIZE; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert_eq!(
        found.len(),
        1,
        "the second definition is the finding, and the first is not: {found:?}"
    );
    assert_eq!(found[0].check, "a_macro_is_not_redefined");
    assert_eq!(found[0].name, "BUFFER_SIZE");
    assert!(
        found[0].range.start_offset > MAIN.find("512").unwrap_or(0) - 20,
        "the finding points at the second definition, not the first: {:?}",
        found[0].range
    );
    assert!(
        found[0].message.contains("256") || found[0].message.contains("offset"),
        "the message says where the definition it replaces is: {}",
        found[0].message
    );
}

/// **The shape the standard library writes, which this check must never report.**
///
/// `#if _HAS_CXX23 / #define X 1 / #else / #define X 2 / #endif` defines one name twice with different bodies,
/// and at most one of the two is ever in force — so it is not a redefinition. `_FMT_P2286_BEGIN` and
/// `_FMT_P2286_END` in MSVC's `__msvc_formatter.hpp` are exactly this, and a check that reported it would fire
/// on every standard-library header the user includes.
///
/// The guard here is `#if 0` / `#else`, which is decidable: the first branch is `Inactive` and the second is
/// `Active`, so exactly one definition is in force and there is nothing to report.
#[test]
fn a_definition_in_the_other_branch_of_one_conditional_is_not_a_redefinition() {
    const MAIN: &str =
        "#if 0\n#define WHICH 1\n#else\n#define WHICH 2\n#endif\nint main() { return WHICH; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert!(
        found.is_empty(),
        "only the `#else` branch is compiled, so `WHICH` is defined once: {found:?}"
    );
}

/// **The same shape with a condition nobody can decide** — which is the ordinary case, not the corner.
///
/// `#if SOME_NAME_THIS_FILE_NEVER_DEFINES` cannot be evaluated, so [`fact_in_force`] answers `false` for the
/// definitions in *both* branches: neither is in force, and two facts that are not in force are not a
/// redefinition of each other. That is the direction the guard layer documents — *"`Unknown` keeps the fact
/// out … this can lose evidence, never invent it"* — and it is the reason this check is quiet on the standard
/// library, where nearly every `#if` is one of these.
///
/// The nearby failure this pins: a check that compared bodies by *position* alone, or that treated "cannot
/// tell" as "both are compiled", would report every one of those pairs.
#[test]
fn a_definition_under_an_undecidable_condition_is_not_reported() {
    const MAIN: &str = "#ifdef SOMETHING_THIS_FILE_NEVER_DEFINES\n#define WIDTH 80\n#else\n#define WIDTH 120\n#endif\nint main() { return WIDTH; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert!(
        found.is_empty(),
        "neither branch can be ruled in, so neither definition can be said to replace the other: {found:?}"
    );
}

/// **An identical redefinition is what the standard allows**, and every header that includes another's
/// `#define` twice relies on it. The check compares the two replacement lists for exactly this case.
#[test]
fn a_macro_defined_twice_with_the_same_body_is_not_reported() {
    const MAIN: &str = "#define LIMIT 100\n#define LIMIT 100\nint main() { return LIMIT; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert!(
        found.is_empty(),
        "an identical redefinition is legal, and the standard says so: {found:?}"
    );
}

/// **A name undefined and defined again is not a redefinition** — the second `#define` is the name's first
/// definition after the `#undef`, and nothing was in force to replace.
#[test]
fn a_definition_after_an_undef_is_not_a_redefinition() {
    const MAIN: &str =
        "#define MODE 1\n#undef MODE\n#define MODE 2\nint main() { return MODE; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert!(
        found.is_empty(),
        "the `#undef` ended the first definition, so the second replaces nothing: {found:?}"
    );
}

/// **The first type check fires on the line it is about.**
///
/// `int count = "three";` — no conversion makes an `int` hold a string literal. This is the whole of what the
/// check claims, and a check that never fires is not a check.
#[test]
fn a_string_literal_assigned_to_a_number_is_reported() {
    const MAIN: &str = "int main() { int count = \"three\"; return count; }\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert_eq!(found.len(), 1, "exactly the one line: {found:?}");
    assert_eq!(found[0].name, "count");
    assert_eq!(found[0].check, "an_initializer_does_not_convert");
    assert!(
        found[0].message.contains("int") && found[0].message.contains("count"),
        "the message names the type and the variable: {}",
        found[0].message
    );
}

/// **The check is silent on every shape that is not that one** — which is the half that decides whether the check
/// is worth having.
///
/// Each line below is ordinary C++ that a version of this check with a hand-written rule would underline:
/// a `char*` holding a literal (ill-formed since C++11 and accepted by every compiler in use), a class with a
/// constructor from `const char*` — which is what `std::string s = "x";` *is*, and the single most common line in
/// modern C++ — an integer initialised from another integer, a `double` from an `int`, and `auto` holding a literal.
///
/// `S` **is** declared here, with the constructor a converting one has, so the case that matters is not "a class we
/// know nothing about" but "a class whose constructor could take this" — which is exactly what the relation
/// refuses to decide, because deciding it needs the class's members.
#[test]
fn a_type_check_is_silent_about_everything_it_cannot_prove() {
    const MAIN: &str = "\
struct S { S(const char*); };\n\
int main() {\n\
    const char* p = \"literal\";\n\
    char buffer[] = \"literal\";\n\
    S s = \"literal\";\n\
    int a = 1;\n\
    int b = a;\n\
    double d = 1;\n\
    auto x = \"literal\";\n\
    long n = 1L;\n\
    return 0;\n\
}\n";

    let found = findings(MemoryFiles::new().with_file("/q/main.cpp", MAIN), MAIN);

    assert!(
        found.is_empty(),
        "every line here is ordinary C++, and underlining it is the failure this layer exists to avoid: {found:?}"
    );
}
