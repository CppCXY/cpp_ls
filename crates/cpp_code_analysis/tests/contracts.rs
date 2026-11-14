//! **C++26 contracts** (P2900) end to end: the file parses, nothing is reported, and the contract specifiers are
//! visible in the tree the analysis reads.
//!
//! No available compiler implements contracts, so what is checked here is what a language server must get right
//! without one: no false syntax errors, no false check findings, and declarations that keep their names when a
//! contract is written around them (`pre(…)`/`post(…)` are context-sensitive keywords, so a parser that read them as
//! a macro invocation would lose the function).

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

const CONTRACTS: &str = "\
struct Widget {
    int size;
};

int safe_division(int numerator, int denominator)
    pre(denominator != 0)
    post(result: result * denominator == numerator)
{
    contract_assert(numerator >= 0);
    return numerator / denominator;
}

auto increment = [](int x) pre(x > 0) post(r: r > x) { return x + 1; };
";

fn session() -> Session<MemoryFiles> {
    let files = MemoryFiles::new().with_file("/p/a.cpp", CONTRACTS);
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
}

/// **The file is read, not refused**: no errors and no check findings.
#[test]
fn a_file_with_contracts_has_no_diagnostics() {
    let session = session();
    let found = session
        .diagnostics("/p/a.cpp")
        .expect("the file has a reading");
    assert_eq!(found.errors.len(), 0, "{:?}", found.errors);
    assert!(found.checks.is_empty(), "no check findings: {:?}", found.checks);
}

/// **The contracted function keeps its name and its type**, which is what a contract must not cost: the declarator
/// is still `safe_division`, and the initializer `increment` is still a variable (a lambda, not a macro call).
#[test]
fn a_contract_does_not_hide_the_declaration_it_is_written_on() {
    let session = session();
    let summary = session
        .index()
        .summary(std::path::Path::new("/p/a.cpp"))
        .expect("the file is summarized");
    let names: Vec<&str> = summary
        .declarations
        .iter()
        .map(|fact| fact.name.as_str())
        .collect();
    assert!(
        names.contains(&"safe_division"),
        "the function is declared despite its contracts: {names:?}"
    );
    assert!(
        names.contains(&"increment"),
        "and so is the lambda's variable: {names:?}"
    );
    assert!(
        !names.contains(&"pre") && !names.contains(&"post"),
        "a contract specifier is not a declaration: {names:?}"
    );
}
