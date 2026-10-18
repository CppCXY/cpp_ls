//! **The style rules, named after clang-tidy's own** — and, for now, a statement of what each should say.
//!
//! Each rule has a code, a module with the rationale, and a test here. The tests are `#[ignore]`d because the walks are not
//! written yet: a rule that is registered but silent is honest scaffolding, while a test that asserted "nothing is
//! reported" would be asserting the gap. When a walk lands, its test loses the attribute and turns green — which is what
//! these attributes are for in this project.

use cpp_code_analysis::sema::check::diagnostic_code::DiagnosticCode;

/// **Every style code exists, has a name, and round-trips.**
#[test]
fn the_style_codes_are_named_and_round_trip() {
    let expected = [
        ("braces-around-statements", DiagnosticCode::BracesAroundStatements),
        ("use-nullptr", DiagnosticCode::UseNullptr),
        ("use-override", DiagnosticCode::UseOverride),
        ("simplify-boolean-expr", DiagnosticCode::SimplifyBooleanExpr),
        ("else-after-return", DiagnosticCode::ElseAfterReturn),
        ("cstyle-cast", DiagnosticCode::CStyleCast),
    ];
    for (name, code) in expected {
        assert_eq!(code.name(), name);
        assert_eq!(DiagnosticCode::from_name(name), Some(code));
        assert!(DiagnosticCode::ALL.contains(&code), "{name} is in ALL");
    }
    assert_eq!(DiagnosticCode::ALL.len(), 20, "the six are in the list");
}

/// `readability-braces-around-statements`: a one-statement body should be a block.
#[test]
#[ignore = "the walk is not written; see sema/check/braces_around_statements.rs"]
fn a_one_statement_body_is_reported() {
    let found = codes_of("void f(bool ready) {\n    if (ready) return;\n}\n");
    assert!(found.contains(&DiagnosticCode::BracesAroundStatements), "{found:?}");
    // …and a braced body is not.
    let braced = codes_of("void f(bool ready) {\n    if (ready) { return; }\n}\n");
    assert!(!braced.contains(&DiagnosticCode::BracesAroundStatements), "{braced:?}");
}

/// `modernize-use-nullptr`: `NULL` is `0`.
#[test]
#[ignore = "the walk is not written; see sema/check/use_nullptr.rs"]
fn a_null_macro_is_reported() {
    let found = codes_of("void f(int*);\nvoid g() {\n    f(NULL);\n}\n");
    assert!(found.contains(&DiagnosticCode::UseNullptr), "{found:?}");
    let nullptr = codes_of("void f(int*);\nvoid g() {\n    f(nullptr);\n}\n");
    assert!(!nullptr.contains(&DiagnosticCode::UseNullptr), "{nullptr:?}");
}

/// `modernize-use-override`: an overriding declaration should say so.
#[test]
#[ignore = "the walk is not written; see sema/check/use_override.rs"]
fn an_override_without_the_keyword_is_reported() {
    let found = codes_of(
        "struct Base { virtual void f(); };\nstruct Derived : Base { void f(); };\n",
    );
    assert!(found.contains(&DiagnosticCode::UseOverride), "{found:?}");
    let says_so = codes_of(
        "struct Base { virtual void f(); };\nstruct Derived : Base { void f() override; };\n",
    );
    assert!(!says_so.contains(&DiagnosticCode::UseOverride), "{says_so:?}");
}

/// `readability-simplify-boolean-expr`: `x == true` is `x`.
#[test]
#[ignore = "the walk is not written; see sema/check/simplify_boolean_expr.rs"]
fn a_comparison_against_true_is_reported() {
    let found = codes_of("void f(bool ready) {\n    if (ready == true) { return; }\n}\n");
    assert!(found.contains(&DiagnosticCode::SimplifyBooleanExpr), "{found:?}");
    let plain = codes_of("void f(bool ready) {\n    if (ready) { return; }\n}\n");
    assert!(!plain.contains(&DiagnosticCode::SimplifyBooleanExpr), "{plain:?}");
}

/// `readability-else-after-return`: an `else` after a return is a nesting that is not needed.
#[test]
#[ignore = "the walk is not written; see sema/check/else_after_return.rs"]
fn an_else_after_a_return_is_reported() {
    let found = codes_of(
        "void f(bool ready) {\n    if (ready) { return; } else { g(); }\n}\nvoid g();\n",
    );
    assert!(found.contains(&DiagnosticCode::ElseAfterReturn), "{found:?}");
    let flat = codes_of("void f(bool ready) {\n    if (ready) { return; }\n    g();\n}\nvoid g();\n");
    assert!(!flat.contains(&DiagnosticCode::ElseAfterReturn), "{flat:?}");
}

/// `cppcoreguidelines-pro-type-cstyle-cast`: a C-style cast can remove `const` without saying so.
#[test]
#[ignore = "the walk is not written; see sema/check/cstyle_cast.rs"]
fn a_c_style_cast_is_reported() {
    let found = codes_of("void f(double n) {\n    int i = (int)n;\n}\n");
    assert!(found.contains(&DiagnosticCode::CStyleCast), "{found:?}");
    let named = codes_of("void f(double n) {\n    int i = static_cast<int>(n);\n}\n");
    assert!(!named.contains(&DiagnosticCode::CStyleCast), "{named:?}");
}

/// Run the checks over one file, as `tests/checks.rs` does — the same fixture, kept here so this file stands alone.
fn codes_of(source: &str) -> Vec<DiagnosticCode> {
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };
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