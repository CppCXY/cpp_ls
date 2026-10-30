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
        (
            "braces-around-statements",
            DiagnosticCode::BracesAroundStatements,
        ),
        ("use-nullptr", DiagnosticCode::UseNullptr),
        ("use-override", DiagnosticCode::UseOverride),
        ("simplify-boolean-expr", DiagnosticCode::SimplifyBooleanExpr),
        ("else-after-return", DiagnosticCode::ElseAfterReturn),
        ("cstyle-cast", DiagnosticCode::CStyleCast),
        ("use-auto", DiagnosticCode::UseAuto),
        ("macro-parentheses", DiagnosticCode::MacroParentheses),
        (
            "implicit-bool-conversion",
            DiagnosticCode::ImplicitBoolConversion,
        ),
    ];
    for (name, code) in expected {
        assert_eq!(code.name(), name);
        assert_eq!(DiagnosticCode::from_name(name), Some(code));
        assert!(DiagnosticCode::ALL.contains(&code), "{name} is in ALL");
    }
    assert_eq!(
        DiagnosticCode::ALL.len(),
        29,
        "every code is in the list, `redefinition` included"
    );
}

/// `readability-braces-around-statements`: a one-statement body should be a block.
#[test]
fn a_one_statement_body_is_reported() {
    let found = codes_of("void f(bool ready) {\n    if (ready) return;\n}\n");
    assert!(
        found.contains(&DiagnosticCode::BracesAroundStatements),
        "{found:?}"
    );
    // …and a braced body is not.
    let braced = codes_of("void f(bool ready) {\n    if (ready) { return; }\n}\n");
    assert!(
        !braced.contains(&DiagnosticCode::BracesAroundStatements),
        "{braced:?}"
    );
}

/// `modernize-use-nullptr`: `NULL` is `0`.
#[test]
fn a_null_macro_is_reported() {
    let found = codes_of("void f(int*);\nvoid g() {\n    f(NULL);\n}\n");
    assert!(found.contains(&DiagnosticCode::UseNullptr), "{found:?}");
    let nullptr = codes_of("void f(int*);\nvoid g() {\n    f(nullptr);\n}\n");
    assert!(
        !nullptr.contains(&DiagnosticCode::UseNullptr),
        "{nullptr:?}"
    );
}

/// `modernize-use-override`: an overriding declaration should say so.
#[test]
fn an_override_without_the_keyword_is_reported() {
    let found =
        codes_of("struct Base { virtual void f(); };\nstruct Derived : Base { void f(); };\n");
    assert!(found.contains(&DiagnosticCode::UseOverride), "{found:?}");
    let says_so = codes_of(
        "struct Base { virtual void f(); };\nstruct Derived : Base { void f() override; };\n",
    );
    assert!(
        !says_so.contains(&DiagnosticCode::UseOverride),
        "{says_so:?}"
    );
}

/// **The finding names the member it is about, and lands on it** — the coordinate bug that made this rule name one
/// member and underline another.
///
/// A comment in the file shifts every *rendering* offset away from the file's, so a check that slices the rendering with
/// a fact's file offsets reads somebody else's declaration: the members that do say `override` came out as if they did
/// not, and the range pointed at a line the message was not about. `classes.cpp` of the C++ corpus produced four of
/// them, naming `describe`, `clone`, `value` and `sum` while underlining `left + right`, `virtual`, `VirtualBase {` and
/// `static_assert`.
#[test]
fn an_override_is_named_and_located_in_a_file_with_a_comment() {
    let source = "\
// the cook drops this comment, so the file's offsets and the rendering's differ
struct Base {
    virtual void really_overrides();
    virtual void renamed_out();
};
struct Derived : Base {
    void really_overrides() override;
    void renamed_out();
};
";
    let findings = findings_of(source);
    let override_findings: Vec<&cpp_code_analysis::Finding> = findings
        .iter()
        .filter(|finding| finding.code == DiagnosticCode::UseOverride)
        .collect();
    assert_eq!(
        override_findings.len(),
        1,
        "one member forgot the keyword: {findings:?}"
    );
    let finding = override_findings[0];
    assert!(
        finding.message.contains("renamed_out"),
        "the message names the member: {}",
        finding.message
    );
    let at = finding.range.start_offset;
    assert!(
        source[at..].starts_with("renamed_out"),
        "the range is on the name the message is about, not at {at}: {:?}",
        &source[at.saturating_sub(20)..(at + 20).min(source.len())]
    );
}

/// `readability-simplify-boolean-expr`: `x == true` is `x`.
#[test]
fn a_comparison_against_true_is_reported() {
    let found = codes_of("void f(bool ready) {\n    if (ready == true) { return; }\n}\n");
    assert!(
        found.contains(&DiagnosticCode::SimplifyBooleanExpr),
        "{found:?}"
    );
    let plain = codes_of("void f(bool ready) {\n    if (ready) { return; }\n}\n");
    assert!(
        !plain.contains(&DiagnosticCode::SimplifyBooleanExpr),
        "{plain:?}"
    );
}

/// `readability-else-after-return`: an `else` after a return is a nesting that is not needed.
#[test]
fn an_else_after_a_return_is_reported() {
    let found =
        codes_of("void f(bool ready) {\n    if (ready) { return; } else { g(); }\n}\nvoid g();\n");
    assert!(
        found.contains(&DiagnosticCode::ElseAfterReturn),
        "{found:?}"
    );
    let flat =
        codes_of("void f(bool ready) {\n    if (ready) { return; }\n    g();\n}\nvoid g();\n");
    assert!(!flat.contains(&DiagnosticCode::ElseAfterReturn), "{flat:?}");
}

/// `cppcoreguidelines-pro-type-cstyle-cast`: a C-style cast can remove `const` without saying so.
#[test]
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

/// Run the checks over one file and keep the findings themselves, for a test that asserts on a message or a range.
fn findings_of(source: &str) -> Vec<cpp_code_analysis::Finding> {
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
        .map(|found| found.checks.clone())
        .unwrap_or_default()
}
/// `modernize-use-auto`, narrowed to the case that is never wrong: an iterator declared by its full spelling.
#[test]
fn a_full_iterator_spelling_is_reported() {
    let found = codes_of("void f() {\n    std::vector<int>::iterator it;\n    (void)it;\n}\n");
    assert!(found.contains(&DiagnosticCode::UseAuto), "{found:?}");

    // …and a type that is not an iterator spelling is not the rule's shape.
    let not_an_iterator =
        codes_of("void f() {\n    std::vector<int>::value_type value;\n    (void)value;\n}\n");
    assert!(
        !not_an_iterator.contains(&DiagnosticCode::UseAuto),
        "{not_an_iterator:?}"
    );
}

/// `bugprone-macro-parentheses`: a replacement list that is not parenthesised.
#[test]
fn an_unparenthesised_macro_is_reported() {
    let found = codes_of("#define TWICE(x) x * 2\nint f(int n) { return TWICE(n + 1); }\n");
    assert!(
        found.contains(&DiagnosticCode::MacroParentheses),
        "{found:?}"
    );

    // …and the parenthesised form is the answer, so it is silent.
    let parenthesised =
        codes_of("#define TWICE(x) ((x) * 2)\nint f(int n) { return TWICE(n + 1); }\n");
    assert!(
        !parenthesised.contains(&DiagnosticCode::MacroParentheses),
        "{parenthesised:?}"
    );

    // A macro that is one token, or a call, or a block, is not this rule's shape.
    for quiet in [
        "#define ANSWER 42\nint f() { return ANSWER; }\n",
        "#define MAX(a, b) ((a) > (b) ? (a) : (b))\nint f(int x, int y) { return MAX(x, y); }\n",
    ] {
        let found = codes_of(quiet);
        assert!(
            !found.contains(&DiagnosticCode::MacroParentheses),
            "{quiet}: {found:?}"
        );
    }
}

/// **The cost of reading text instead of the tree**: a comment and a string literal are not code.
///
/// Every text-based rule has this hole, and it is the same hole in each: the spelling is in the file, and the file also
/// contains prose and data that spell the same characters. These assertions state what the tree/token layer is for — a
/// comment saying "`NULL` is what we used to write" is not a `NULL` a pointer is initialised with.
#[test]
fn a_comment_and_a_string_literal_are_not_code() {
    // `NULL` in a comment.
    let in_a_comment = codes_of("// NULL is what we used to write\nint f() { return 0; }\n");
    assert!(
        !in_a_comment.contains(&DiagnosticCode::UseNullptr),
        "a comment is not a use: {in_a_comment:?}"
    );

    // `NULL` in a string literal.
    let in_a_string = codes_of("void f();\nvoid g() { f(\"NULL\"); }\n");
    assert!(
        !in_a_string.contains(&DiagnosticCode::UseNullptr),
        "a string literal is not a use: {in_a_string:?}"
    );

    // `(int)` in a comment, and `== true` in a string.
    let cast_in_a_comment = codes_of("// we removed (int) casts everywhere\nvoid f() { }\n");
    assert!(
        !cast_in_a_comment.contains(&DiagnosticCode::CStyleCast),
        "a comment is not a cast: {cast_in_a_comment:?}"
    );
    let boolean_in_a_string = codes_of(
        "void f(bool ready) {\n    if (ready) { g(\"x == true y\"); }\n}\nvoid g(const char*);\n",
    );
    assert!(
        !boolean_in_a_string.contains(&DiagnosticCode::SimplifyBooleanExpr),
        "a string is not a condition: {boolean_in_a_string:?}"
    );

    // `#define TWICE(x) x * 2` inside a block comment.
    let macro_in_a_comment = codes_of("/*\n#define TWICE(x) x * 2\n*/\nint f() { return 0; }\n");
    assert!(
        !macro_in_a_comment.contains(&DiagnosticCode::MacroParentheses),
        "a commented-out macro is not a macro: {macro_in_a_comment:?}"
    );
}

/// **The rule that needs the type**: `if (count)` converts an `int` to `bool`, and `if (flag)` does not.
///
/// The two differ by the declaration and nothing else — this is what the scope tree, the index and the semantic model are
/// for. A text rule cannot see it, and a rule that flagged both would be wrong about every `bool` in the project.
#[test]
fn an_arithmetic_condition_is_an_implicit_conversion() {
    let reported = codes_of("void f(int count) {\n    if (count) { return; }\n}\n");
    assert!(
        reported.contains(&DiagnosticCode::ImplicitBoolConversion),
        "an `int` in a condition is a conversion: {reported:?}"
    );

    // A `bool` is the type the conversion would produce, so it is not a conversion.
    let quiet = codes_of("void f(bool ready) {\n    if (ready) { return; }\n}\n");
    assert!(
        !quiet.contains(&DiagnosticCode::ImplicitBoolConversion),
        "a `bool` is not converted to `bool`: {quiet:?}"
    );

    // A pointer is the same conversion and the idiom every codebase uses; the rule leaves it alone.
    let a_pointer = codes_of("void f(int* p) {\n    if (p) { return; }\n}\n");
    assert!(
        !a_pointer.contains(&DiagnosticCode::ImplicitBoolConversion),
        "`if (ptr)` is the nullable-pointer idiom: {a_pointer:?}"
    );

    // `while` is the same question and the same walk.
    let looped = codes_of("void f(int n) {\n    while (n) { --n; }\n}\n");
    assert!(
        looped.contains(&DiagnosticCode::ImplicitBoolConversion),
        "`while (n)` converts too: {looped:?}"
    );
}

/// **A preprocessor condition is not a cast** — the false positive the first census found, in its own words.
///
/// 813 findings over 46 files, and the examples were `#if (_MSC_VER)` and `#if (ASIO_USE_TS_EXECUTOR_AS_DEFAULT)`: an
/// identifier in parentheses on a directive line, following an `#`, and read by a scanner as a cast whose operand is
/// whatever came next. A census is what turns "the rule is tested" into "the rule is right".
#[test]
fn a_preprocessor_condition_is_not_a_cast() {
    for quiet in [
        "#if (_MSC_VER)\nint f();\n#endif\n",
        "#if (ASIO_USE_TS_EXECUTOR_AS_DEFAULT)\nint f();\n#endif\n",
        "#if (_MSC_VER)\nint a;\n#elif (OTHER)\nint b;\n#endif\n",
    ] {
        let found = codes_of(quiet);
        assert!(
            !found.contains(&DiagnosticCode::CStyleCast),
            "{quiet}: {found:?}"
        );
    }

    // …and a real cast is still reported, on a directive line or not.
    let still_reported = codes_of("#define CAST(x) (int)(x)\nvoid f(int n) { (void)CAST(n); }\n");
    assert!(
        still_reported.contains(&DiagnosticCode::CStyleCast),
        "the cast in the macro body is written in the file: {still_reported:?}"
    );
}

/// **A parameter list is not a cast** — the census's second false positive, after the preprocessor conditions.
///
/// 376 findings remained after the `#` fix, and the examples were `(const any_io_executor& e)` and `(nullptr_t)`:
/// parenthesised **parameter** spellings inside declarations, which are a type with a name after it rather than a value being
/// cast. The tell is the trailing identifier — a cast's parenthesised spelling has no name after its type.
#[test]
fn a_parameter_list_is_not_a_cast() {
    for quiet in [
        "using handler = void (const Widget& value);\nstruct Widget { };\n",
        "void f(int (int n));\n",
        "void g(void (*callback)(int value));\n",
    ] {
        let found = codes_of(quiet);
        assert!(
            !found.contains(&DiagnosticCode::CStyleCast),
            "{quiet}: {found:?}"
        );
    }

    // …and a cast of a named type is still reported.
    let still = codes_of("struct Widget { };\nvoid f(const Widget& w) { (void)(Widget)w; }\n");
    assert!(
        still.contains(&DiagnosticCode::CStyleCast),
        "a cast is a type with no name after it: {still:?}"
    );
}

/// **A rule is not always a rule in both languages**: a C program has no `static_cast`, so a C-style cast is the only cast
/// there is.
///
/// The same source, two compile flags: `-std=c++17` is told to name the cast it meant, `-std=c11`
/// is not — because the alternative the message recommends does not exist in that language.
#[test]
fn a_c_style_cast_is_not_a_rule_in_c() {
    let source = "void f(double n) {\n    int i = (int)n;\n    (void)i;\n}\n";

    let cpp = codes_of_with(source, "-std=c++17");
    assert!(
        cpp.contains(&DiagnosticCode::CStyleCast),
        "C++ has `static_cast`, so the cast should be named: {cpp:?}"
    );

    let c = codes_of_with(source, "-std=c11");
    assert!(
        !c.contains(&DiagnosticCode::CStyleCast),
        "C has no other cast to recommend: {c:?}"
    );

    // …and an unconfigured project is not "C": with both families enabled the rule keeps firing, which is the conservative
    // direction for a project nobody told us about.
    let unconfigured = codes_of(source);
    assert!(
        unconfigured.contains(&DiagnosticCode::CStyleCast),
        "`None` is not C: {unconfigured:?}"
    );
}

/// Run the checks over one file **compiled with a given `-std=`**, which is what the language-sensitive rules read.
fn codes_of_with(source: &str, standard: &str) -> Vec<DiagnosticCode> {
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let config = CompilerConfig {
        extra_args: vec![standard.to_string()],
        ..CompilerConfig::default()
    };
    let mut session = Session::with_config("/p", providers, WatchFilter::new("/p"), config);
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

/// **A template argument is not a cast** — the third false-positive class the census produced, after preprocessor
/// conditions and parameter lists.
///
/// `template <class T = (Executor)>` writes a parenthesised name inside an angle-bracket list, which a scanner reads as a
/// cast whose operand follows. The tell is the line: an unmatched `<` before the `(`.
#[test]
fn a_template_argument_is_not_a_cast() {
    for quiet in [
        "struct Executor { };\ntemplate <class T = (Executor)> struct Box { T value; };\n",
        "template <class T, class U = (T)> struct Pair { T first; U second; };\n",
    ] {
        let found = codes_of(quiet);
        assert!(
            !found.contains(&DiagnosticCode::CStyleCast),
            "{quiet}: {found:?}"
        );
    }

    // …and a cast on a line with no `<` is still reported.
    let still = codes_of("void f(double n) { int i = (int)n; (void)i; }\n");
    assert!(
        still.contains(&DiagnosticCode::CStyleCast),
        "a cast is still a cast: {still:?}"
    );
}
