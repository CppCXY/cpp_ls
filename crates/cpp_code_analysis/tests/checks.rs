//! **What the checks say, and what they refuse to say.**
//!
//! One positive per rule — the smallest fixture that must produce a finding — and a negative beside it, because
//! this layer's contract is not "find problems" but "**never invent one**": an editor that underlines correct code
//! is one the user turns off, and then it reports nothing at all, forever.
//!
//! The fixtures are asked through [`Session::diagnostics`] rather than through a check called directly, so that
//! what is asserted is what a client is shown: the run, the codes, and the two filters a finding passes on its way
//! out (the config, and the model's three-valued answers).

use cpp_code_analysis::sema::check::{RuleSeverity, RuleTag};
use cpp_code_analysis::{
    CompilerConfig, DiagnosticCode, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// **The codes a file produces** — through the whole session, exactly as a diagnostics request does.
///
/// The ask and the pump are both here because a rendering is built when something asks for it: see
/// `tests/completion.rs`'s `cooked_view` for the note every fixture in this workspace shares.
fn codes_of(source: &str) -> Vec<DiagnosticCode> {
    codes_of_a_file("/p/a.cpp", source)
}

/// [`codes_of`] with the **file name the caller chooses**, because the language a file is read as depends on it: a
/// `.c` file is C, and a rule that is only about C++ must not fire there.
fn codes_of_a_file(path: &str, source: &str) -> Vec<DiagnosticCode> {
    let files = MemoryFiles::new().with_file(path, source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from(path)]);
    session.want_cooked_reading(std::path::Path::new(path));
    session.index_everything();

    session
        .diagnostics(std::path::Path::new(path))
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

/// **A template's body is not checked until it is instantiated, and it must not be.**
///
/// Reported from an editor: this file produced several errors, and none of them is a fact about it. A template is
/// checked when it is *instantiated*, with the arguments written at that call site — so inside the body of a template
/// nothing is known about `C` and `Fn` at all:
///
/// ```text
/// C x{42};        `C` could be anything; whether `{42}` fits is decided per instantiation
/// op(x);          `Fn` could be any callable, or a function pointer, or a member pointer
/// return x.n;     `x.n` is the canonical dependent name — it exists only for the C it is instantiated with
/// ```
///
/// Every one of those is a **false positive** if reported, and this layer's whole contract is that it never invents
/// a finding: an editor that underlines correct code is one the user turns off, and then it reports nothing at all,
/// forever (see this file's header). A template is the place that matters most, because dependent code is exactly
/// what a compiler refuses to diagnose and what a reader most needs *not* underlined.
#[test]
fn a_template_body_is_not_checked() {
    let codes = codes_of(TEMPLATE);
    assert!(
        codes.is_empty(),
        "nothing in a template body is knowable before instantiation: {codes:?}"
    );
}

/// **The suppression is about the type being a parameter, not about the member access.**
///
/// The fix for [`a_template_body_is_not_checked`] is a `continue` in `undefined-field`, and a `continue` that fires
/// too often is how the check stops working without anything going red. So the same shape is written twice, once
/// with a template parameter and once with a real class:
///
/// ```text
/// template <class C> void f() { C x; x.n; }   nothing — `C` is decided at instantiation
/// struct C { int n; };  void g() { C x; x.n; } nothing — `n` is declared
/// struct C { int n; };  void g() { C x; x.m; } UndefinedField — `C` is a class with no `m`
/// ```
///
/// The third is the one that matters: it is the check's whole reason to exist, and it goes through every step the
/// template case now skips past except the last.
#[test]
fn a_real_class_still_reports_an_undefined_member() {
    let declares_it = codes_of("struct C { int n; };\nvoid g() { C x; x.n; }\n");
    assert!(
        declares_it.is_empty(),
        "`C` declares `n`, so there is nothing to say: {declares_it:?}"
    );

    let does_not = codes_of("struct C { int n; };\nvoid g() { C x; x.m; }\n");
    assert!(
        does_not.contains(&DiagnosticCode::UndefinedField),
        "`C` is a class that declares no `m`, which is the finding this check exists for: {does_not:?}"
    );

    // **And the name is not what decides it.** A file that has both a parameter called `C` and a class called `C`
    // asks the question the suppression has to get right, and it is the *type* that answers it: inside the template
    // `C` is the parameter, so `x.m` is unknown; outside it `C` is the class, so `x.m` is a finding. A suppression
    // keyed on the name would be silent in both, and this is the case that says so.
    let both = codes_of(
        "template <class C> void f() { C x; x.m; return; }\n\
         struct C { int n; };\n\
         void g() { C x; x.m; return; }\n",
    );
    assert!(
        both.contains(&DiagnosticCode::UndefinedField),
        "the class `C` outside the template still declares no `m`: {both:?}"
    );
}

/// The reported file, verbatim.
const TEMPLATE: &str = r#"template <class C, class Fn>
constexpr int consteval_validate_destruction(Fn op) {
    C x{42};
    op(x);
    return x.n;
}
"#;

/// **What the reported file actually produces, and why it is not `Unused`.**
///
/// Reported from an editor: this file produced no diagnostic at all, twice — first with a `return 0` that had no
/// `;` (a real parser defect, fixed: `return` was the one statement that said nothing when its terminator was
/// missing), and then with the semicolons written, where the server correctly published an **empty list**.
///
/// The second report is the one this test documents, and the answer is not a bug: the file parses, so
/// `syntax-error` is absent, and the only thing the checks have to say about it is that its three `#include`s do not
/// resolve — which is a fact about the machine the analysis runs on, not about the code:
///
/// ```text
/// in the tests:      [IncludeNotFound, IncludeNotFound, IncludeNotFound]   no compiler search paths exist
/// on a real server:  []                                                    the machine's own paths find them
/// ```
///
/// So a server reporting nothing for this file is **right**, and the expectation that it should say something was
/// wrong. `full.size();` discarding its result is the thing a reader would want underlined, and **no check covers
/// it** — that is a missing rule rather than a broken channel, and it is recorded here rather than asserted, because
/// a test that demanded it would be asserting a rule this layer does not have.
#[test]
fn the_reported_file_reports_only_its_unresolved_includes() {
    let codes = codes_of(REPORTED);
    assert_eq!(
        codes,
        vec![
            DiagnosticCode::IncludeNotFound,
            DiagnosticCode::IncludeNotFound,
            DiagnosticCode::IncludeNotFound
        ],
        "the file parses; the only claim the checks make about it is about the machine's include paths"
    );
}

/// The reported file, verbatim.
const REPORTED: &str = r#"#include <iostream>
#include <string>
#include <cstdio>
int main() {
    int a = 1, b = 2;
    int sum = a + b;
    std::cout << "The sum of " << a << " and " << b << " is: " << sum << std::endl;
    std::string full;
    std::cout << "Full name: " << full << std::endl;
    std::cout << "hello world " << std::endl;
    full.size();
    full.length();
    full.size();
    printf("Full name Length: %zu\n", full.size());
    // printf("Full name Length using length(): %zu\n", full.length());
    return 0;
}
"#;

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

/// **`>=` is not an assignment**, and the reason a token-kind test cannot tell is worth writing down.
///
/// The lexer **splits** `>=` into two one-byte tokens on purpose, so that `>>` can collapse in
/// `vector<vector<int>>` (see `CppLexer::push`). So the `=` of a `>=` arrives as
/// [`CppTokenKind::Assign`] — the same kind a real assignment has. Measured on
///
/// ```cpp
/// if (a >= b) { … }   →  Greater@32..33 ">"   Assign@33..34 "="   ← one operator, two tokens
/// if (a = b)  { … }   →                          Assign@74..75 "="  ← the real thing
/// ```
///
/// so `is_an_assignment`, which asked only for the kind, reported **both** as `if (a >= b) assigns rather than
/// compares`. The distinction is positional: a real assignment's `=` is the **first** token of the condition, while
/// a split `>=` has its `>` before it. Asking for that is asking the tree what the operator was.
#[test]
fn a_greater_equal_is_not_an_assignment() {
    let source = "\
int f(int a, int b) {
    if (a >= b) {
        return 1;
    }
    if (a <= b) {
        return 2;
    }
    if (a == b) {
        return 3;
    }
    if (a = b) {
        return 4;
    }
    return 0;
}
";
    let found = codes_with_lines(source);
    let lines: Vec<usize> = found
        .iter()
        .filter(|(code, _)| *code == DiagnosticCode::AssignmentInCondition)
        .map(|(_, line)| *line)
        .collect();

    // **Every comparison the lexer splits**, which is the whole family: `>=` and `<=` are each two one-byte tokens
    // so that `>>` can collapse in `vector<vector<int>>` (see `CppLexer::push`), and the `=` half arrives as an
    // `Assign` — the same kind a real assignment has. `==` is not split and was never affected.
    for (line, written) in [(1, "a >= b"), (4, "a <= b"), (7, "a == b")] {
        assert!(
            !lines.contains(&line),
            "`if ({written})` is a comparison, not an assignment: {found:?}"
        );
    }
    assert!(
        lines.contains(&10),
        "`if (a = b)` is the defect this check exists for: {found:?}"
    );
}

/// **The same regression, and the same cause** — see `assignment_in_condition_is_reported_and_the_deliberate_idiom_is_not`.
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

/// **A finding lands on the line the reader wrote**, not on the line its offset means in the rendering.
///
/// This is the test for the crossing [`CheckContext::add`] makes. A rendering has no comments and a header inlined
/// in place of its `#include`, so a range read from the **tree** is a different offset in a different text — and
/// before the crossing existed, `unused`'s finding for a local written after a comment was published **on the
/// comment's line**, because the same number was read against the file.
#[test]
fn a_finding_after_a_comment_lands_on_the_line_the_reader_wrote() {
    // Line 0 is a comment the cook removes; the local is on line 1.
    let source = "// a comment the cook removes\nint main() { int never_used = 1; return 0; }\n";
    let lines = lines_of(&codes_with_lines(source));

    assert_eq!(
        lines,
        vec![(DiagnosticCode::Unused, 1)],
        "the unused local is on line 1, where it is written"
    );

    // **And the assertion can fail**, which is what makes it worth having: the same construct sits at a *smaller*
    // offset in the rendering — the comment is not there — so an untranslated finding would have been published at
    // `rendering_at` and read against the file as **line 0**. The two numbers are asserted to differ, so this test
    // fails if the crossing is ever removed.
    let file_at = source.find("never_used").expect("the fixture writes it");
    let rendering = cpp_parser::CppParser::parse(
        "int main() { int never_used = 1; return 0; }\n",
        cpp_parser::ParserConfig::default(),
    );
    let rendering_at = rendering
        .get_red_root()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.text() == "never_used")
        .map(|token| usize::from(token.text_range().start()))
        .expect("the local is in the rendering too");
    assert!(
        rendering_at < file_at,
        "the rendering is shorter than the file ({rendering_at} vs {file_at}), which is why the crossing exists"
    );
}

/// The codes, each with the **file** line its range starts on.
fn codes_with_lines(source: &str) -> Vec<(DiagnosticCode, usize)> {
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
                .map(|finding| {
                    let at = finding.range.start_offset.min(source.len());
                    (finding.code, source[..at].matches('\n').count())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn lines_of(codes: &[(DiagnosticCode, usize)]) -> Vec<(DiagnosticCode, usize)> {
    codes.to_vec()
}

/// **The severity and the tag come from the rule**, not from the finding: `unused` is the quietest thing this
/// analysis says, and a client is told so — a hint, with the code greyed out as unnecessary.
#[test]
fn unused_is_a_hint_that_a_client_may_grey_out() {
    assert_eq!(DiagnosticCode::Unused.severity(), RuleSeverity::Hint);
    assert_eq!(DiagnosticCode::Unused.tags(), &[RuleTag::Unnecessary]);

    // …and the loud ones stay loud, so that the table is a decision rather than a default.
    assert_eq!(DiagnosticCode::SyntaxError.severity(), RuleSeverity::Error);
    assert_eq!(
        DiagnosticCode::UnnecessaryBranch.severity(),
        RuleSeverity::Warning
    );
    assert!(DiagnosticCode::ParamTypeCheck.tags().is_empty());
}

/// **A value that cannot convert to a written builtin type is reported** at both places a value enters an object
/// whose type is written down: the declaration and the assignment.
/// **Known not to work, and the test is the record of it.**
///
/// The declaration half passes and the assignment half reports nothing, and the reason is *not* the shape: the tree
/// of `count = "four"` is
///
/// ```text
/// BinaryExpr@28..42
///   IdentifierExpr@28..34   (its text is "count " - the trailing space is inside the node)
///   Token(Assign)@34..35    "="
///   LiteralExpr@36..42      "\\"four\\""
/// ```
///
/// — one `BinaryExpr`, two operand nodes, and the operator as a token between them, which is what the walk looks
/// for. So the cause is downstream of the shape and has not been found yet; the failure is asserted out loud rather
/// than left as a red test, and this note is what the next round starts from.
#[test]
fn assign_type_check_reports_both_places_a_value_is_assigned() {
    let declared = "int count = \"three\";\nint main() { return count; }\n";
    assert!(
        codes_of(declared).contains(&DiagnosticCode::AssignTypeCheck),
        "a string is not an int: {:?}",
        codes_of(declared)
    );

    let assigned = "int main() { int count = 0; count = \"four\"; return count; }\n";
    assert!(
        codes_of(assigned).contains(&DiagnosticCode::AssignTypeCheck),
        "and the same question at an assignment: {:?}",
        codes_of(assigned)
    );
}

/// **A name the file declares two ways is not judged** — and the report that made this a rule.
///
/// Reported from an editor as *"an extremely wrong diagnostic"*. `LuaSource.cpp` reads
///
/// ```cpp
/// std::size_t LuaSource::GetColumn(std::size_t offset) const {
///     auto line = GetLine(offset);
///     auto lineStartOffset = _lineOffsetVec[line];
/// ```
///
/// and the check said "`line` is initialised with a value that does not convert to `std::size_t`" about the **deduced**
/// `line`, because its map was keyed by the bare name over every variable the file declares and another function
/// writes `std::size_t line;`. Measured on that file: seven `line`s whose `type_of` is `auto` and two whose is
/// `std::size_t`.
#[test]
fn assign_type_check_stays_quiet_about_a_name_the_file_declares_two_ways() {
    // Two functions, both declaring `line`: one deduced, one written. The deduced one must not be judged against the
    // written one's type, and neither may the reverse (the two values are compatible here, so this is only about the
    // name collision — a *wrong* pair is the fixture below).
    let source = "struct Source { std::size_t GetLine(std::size_t offset); };\n\
                  void one(Source &s, std::size_t offset) {\n\
                  \x20   auto line = s.GetLine(offset);\n\
                  \x20   auto next = line;\n\
                  }\n\
                  void two() {\n\
                  \x20   std::size_t line = 1;\n\
                  \x20   line = 2;\n\
                  }\n";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::AssignTypeCheck),
        "neither `line` is judged"
    );

    // **And a single written declaration is still judged**, which is the check's whole job.
    let wrong = "void f() {\n    int count = \"zero\";\n}\n";
    assert!(
        codes_of(wrong).contains(&DiagnosticCode::AssignTypeCheck),
        "one written type is one answer"
    );
}

/// **A class target stays quiet, and this test is the record of why.** `std::string line = 1231;` is not legal C++,
/// and this layer does not report it: "does `std::string` have a constructor taking an `int`" is a question whose
/// answer is in a header the analysis has not read, so `convertible_to` answers `Unknown` — and `Unknown` is never
/// a finding. The alternative is a rule that reports conversions it cannot see, which is what this layer is written
/// against.
#[test]
fn assign_type_check_stays_quiet_when_the_target_is_a_class_it_cannot_see_into() {
    let source =
        "#include <string>\nstd::string line = 1231;\nint main() { return (int)line.size(); }\n";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::AssignTypeCheck),
        "a conversion nobody modelled is not a fact about the program: {:?}",
        codes_of(source)
    );
}
/// **Known not to fire yet, and this test is the record of it.**
///
/// The two negatives pass — a public member and a member read from inside the class are correctly quiet — so the
/// member lookup, the access specifier reading and the "inside the class" question all work. What is missing is the
/// positive: a private member reached from `main` is not reported, and the cause has not been found. The next round
/// starts here, with the same method that found the last two: print what the check sees, one level closer to the
/// decision than the last probe.
#[test]
fn access_invisible_is_reported_outside_the_class() {
    let source = "class Widget {\n    int secret;\npublic:\n    int size;\n};\n\nint main() { Widget w; return w.secret; }\n";
    assert!(
        codes_of(source).contains(&DiagnosticCode::AccessInvisible),
        "`secret` is private and `main` is not Widget: {:?}",
        codes_of(source)
    );
}

#[test]
fn access_invisible_is_not_reported_for_a_public_member_or_from_inside() {
    let public_member =
        "class Widget {\npublic:\n    int size;\n};\n\nint main() { Widget w; return w.size; }\n";
    assert!(
        !codes_of(public_member).contains(&DiagnosticCode::AccessInvisible),
        "`size` is public: {:?}",
        codes_of(public_member)
    );

    let from_inside =
        "class Widget {\n    int secret;\npublic:\n    int mine() { return secret; }\n};\n";
    assert!(
        !codes_of(from_inside).contains(&DiagnosticCode::AccessInvisible),
        "a member function may read its own class private member: {:?}",
        codes_of(from_inside)
    );
}

/// **Known not to fire yet, and this test is the record of it.**
///
/// The negatives pass. The positive does not, and the reason is *not* the shape of a definition any more: the
/// predicate was fixed to match the tree this grammar actually builds — a `Declaration` whose `CompoundStat` is a
/// child while its `ParameterList` is a grandchild inside `InitDeclarator > Declarator` — and the finding still does
/// not come out. So the cause is downstream of the shape.
#[test]
fn missing_return_is_reported_for_a_body_that_never_returns() {
    assert!(
        codes_of("int count() { }\nint main() { return count(); }\n")
            .contains(&DiagnosticCode::MissingReturn),
        "the body returns nothing and the type is not void"
    );
}

#[test]
fn missing_return_is_not_reported_for_void_or_for_a_returning_body() {
    assert!(
        !codes_of("void stop() { }\nint main() { return 0; }\n")
            .contains(&DiagnosticCode::MissingReturn),
        "void expects nothing"
    );
    assert!(
        !codes_of("int count() { return 1; }\nint main() { return count(); }\n")
            .contains(&DiagnosticCode::MissingReturn),
        "a body that returns is the ordinary case"
    );
}

/// **A condition with a comparison in it is not an assignment**, and this is the user's own line: a `<` is not a
/// bracket, and treating it as one is how the condition's text was read past its own `)` and into the body, where
/// the first statement `const auto message = readMessage();` has an `=` of its own. Reported from a real session as
/// `assigns rather than compares` on a line whose condition is two comparisons.
/// **The line this check was reported on, verbatim.** A condition that is a compound — `&&`, a string literal, a
/// `<` — is refused before anything inside it is looked at, so no reading of the text can make it an assignment.
#[test]
fn the_users_own_condition_is_not_reported() {
    let source = "\
int main(int argc, char** argv) {
    if (arg == \"--port\" && i + 1 < argc) {
        return 2;
    }
    const auto message = readMessage();
    return (int)message;
}
";
    let codes = codes_of(source);
    assert!(
        !codes.contains(&DiagnosticCode::AssignmentInCondition),
        "a compound condition is not this rule: {codes:?}"
    );
}
#[test]
fn a_less_than_in_a_condition_is_not_an_assignment() {
    let source = "\
int main(int argc, char** argv) {
    if (argc == 2 && argc + 1 < argc) {
        const auto message = argc;
        return message;
    }
    return 0;
}
";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::AssignmentInCondition),
        "two comparisons are not an assignment: {:?}",
        codes_of(source)
    );
}
/// **A regression this round, left visible rather than hidden.**
///
/// The check went quiet when its condition test was rewritten onto tokens: `CppTokenKind` is a **nested** enum — an
/// operator is `Operator(OperatorKind::…)`, a bracket is `Punctuation(Punct::…)` — so a flat `match` against
/// `Assign` **compiles and never matches**. That is the same shape as the defects this whole round is about: a
/// hand-written comparison of a type the layer does not own, failing silently instead of failing to build. The
/// **A class declared in this file, and a value that cannot build it.**
///
/// The class is written **here** rather than included, and that is the lesson of three fixtures before this one: an
/// `#include` in a fixture does not resolve (see the guard test below), so a class "from a header" is a class that
/// does not exist — and the check that asks about it has nothing to find. With the definition in the file, the
/// question this rule exists for is actually put: `MyString` declares no constructor, so an `int` cannot build it.
///
/// The conversion relation itself cannot answer this — `convertible_to` lives in the type engine and cannot see a
/// declaration, so it answers `Unknown("a conversion between these two types is not decided by their shapes")` for
/// any class target. Deciding it needs the class's own definition, which is exactly what a check may read when the
/// class is in the file it is checking.
#[test]
fn a_class_in_this_file_that_no_value_can_build_is_reported() {
    let source = "\
struct MyString {
    int size;
};

int main() {
    MyString line = 1231;
    return line.size;
}
";
    let codes = codes_of(source);
    assert!(
        codes.contains(&DiagnosticCode::AssignTypeCheck),
        "`MyString` is defined here, declares no constructor, and an `int` cannot build it: {codes:?}"
    );
}
/// **A finding underlines what it is about and nothing more**, which is a claim about the *range* rather than about
/// the message — and the range is where a crossing can go wrong quietly: the start is a position and the end is the
/// end of the last byte covered, and asking the same question for both walks the end forward over the punctuation
/// and the newline that the next span owns.
#[test]
fn a_finding_underlines_only_the_construct_it_is_about() {
    // A builtin target, because `assign-type-check` fires there — see
    // `a_string_initialised_from_an_int_is_measured_here` for the class target, which it does not answer yet.
    // **No constructor declared**, which is the whole question: a class with one might take the value, and deciding
    // whether it does is overload resolution rather than a walk over a tree.
    let header = "struct MyString { int size; };\n";
    let source = "#include \"mystring.h\"\n\nint main() {\n    int count = \"three\";\n    return count;\n}\n";
    let (_, range) = first_finding_of(source, header, DiagnosticCode::AssignTypeCheck);

    assert_eq!(
        &source[range.start_offset..range.end_offset()],
        "\"three\"",
        "the finding is about the value, not about the statement it is written in"
    );
}

/// **A member of a `using` alias is the member of the class it names.**
///
/// Reported from an editor: `undefined-field` underlined `result.size()` on a `std::string`, and the measurement
/// behind it is `members_of(std::string) -> 0 member(s), unlisted []` — an *empty but complete* list, which the
/// check reads as "this class has no members". The empty answer came from the alias being treated as a class with
/// no members instead of being followed to the class it names.
#[test]
fn a_member_reached_through_a_using_alias_is_not_undefined() {
    let source = "\
struct Base { int size; };
using Alias = Base;
void use() {
    Alias a;
    (void)a.size;
}
";
    let codes = codes_of(source);
    assert!(
        !codes.contains(&DiagnosticCode::UndefinedField),
        "the alias names `Base`, and `Base` has `size`: {codes:?}"
    );
}

/// **A deduced `auto` is not a conversion question** — `auto&` least of all.
///
/// Reported from an editor: `auto& config = configs.emplace_back(…);` was underlined with "`config` is initialised
/// with a value that does not convert to `auto&`". The type written there is not a type: it is an instruction to
/// deduce one from the initialiser, so every spelling of it — `auto`, `auto&`, `const auto&`, `auto&&`, `auto*` —
/// is outside this check's question. See `assign_type_check::is_a_deduced_auto`.
#[test]
fn a_deduced_auto_is_not_a_conversion_question() {
    let source = "\
struct Widget { int n; };
Widget& make();
void use() {
    int value = 1;
    auto plain = value;
    auto* pointer = &value;
    auto& reference = make();
    const auto& constant = make();
    auto&& forwarded = make();
    (void)plain; (void)pointer; (void)reference; (void)constant; (void)forwarded;
}
";
    let codes = codes_of(source);
    assert!(
        !codes.contains(&DiagnosticCode::AssignTypeCheck),
        "a deduced type is not a written one, so nothing here can fail to convert: {codes:?}"
    );
}

/// The first finding of one code in a two-file fixture, with its range in the **file's** coordinates.
fn first_finding_of(
    source: &str,
    header: &str,
    code: DiagnosticCode,
) -> (usize, cpp_parser::SourceRange) {
    let files = MemoryFiles::new()
        .with_file("/p/a.cpp", source)
        .with_file("/p/mystring.h", header);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/mystring.h"),
    ]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();

    for finding in session
        .diagnostics(std::path::Path::new("/p/a.cpp"))
        .map(|found| found.checks)
        .unwrap_or_default()
    {
        if finding.code == code {
            return (0, finding.range);
        }
    }
    panic!("no `{}` finding", code.name());
}
/// **A member a class does not have is reported** — and this test did not exist, which is why the check was blind to
/// `a.b` for as long as it has: it filtered on `MemberExpr`, a kind this grammar does not produce for a dot
/// (`w.secret` parses as an `IndexExpr`; see `CheckContext::member_accesses`).
#[test]
fn undefined_field_is_reported_for_a_name_the_class_does_not_have() {
    let source = "\
struct Widget {
    int size;
};

int main() {
    Widget w;
    return w.sizze;
}
";
    assert!(
        codes_of(source).contains(&DiagnosticCode::UndefinedField),
        "`sizze` is not a member of `Widget`: {:?}",
        codes_of(source)
    );
}

/// **And a member it does have is not reported** — the negative that keeps the positive honest.
#[test]
fn undefined_field_is_not_reported_for_a_member_that_is_there() {
    let source = "\
struct Widget {
    int size;
};

int main() {
    Widget w;
    return w.size;
}
";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::UndefinedField),
        "`size` is a member of `Widget`: {:?}",
        codes_of(source)
    );
}
/// **An `#include` in a fixture does not resolve, and a test that assumes it does measures nothing.**
///
/// This is the trap this workspace has fallen into three times — an angle include with no search path, a class whose
/// header was never inlined, and this test in its first two versions, which asserted that a check sees "the file and
/// not the header" while the header was never read in the first place.
///
/// What the measurement says: with `MemoryFiles`, a quoted include beside the file, the header registered as a
/// project file **and** `CompilerConfig::with_include_path("/p")`, the view's text comes out **shorter than the
/// file** — the `#include` line and the comment are gone and nothing is inlined, i.e. the header was not found.
///
/// So the assertion is the honest one, and it is a guard rather than a claim:
///
/// ```text
///   a fixture that means to exercise an include MUST first prove the include resolved,
///   by asserting that the rendering's text contains something only the header has
/// ```
#[test]
fn an_include_in_a_fixture_does_not_resolve_unless_the_test_proves_it_does() {
    let header = "// a header\nstruct FromTheHeader { int size; };\n";
    let source =
        "#include \"big.h\"\n\nint main() {\n    int never_used = 1;\n    return never_used;\n}\n";

    let files = MemoryFiles::new()
        .with_file("/p/a.cpp", source)
        .with_file("/p/big.h", header);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default().with_include_path("/p"),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/big.h"),
    ]);
    // The pump, the ask, the pump — the contract `tests/completion.rs` states once for the workspace.
    session.index_everything();
    let _ = session.view("/p/a.cpp");
    session.index_everything();

    let view = session.view("/p/a.cpp").expect("a view");
    eprintln!(
        "INCLUDE: view text {} bytes, file {} bytes, inlined={}",
        view.source.len(),
        source.len(),
        view.source.contains("FromTheHeader")
    );
    assert!(
        !view.source.contains("FromTheHeader"),
        "if this ever starts passing, every test that assumed an include resolves needs revisiting"
    );
}
/// **A class from another file, judged from the index** — which is the whole point of "the finding is about this
/// file, the knowledge is the project's".
///
/// The class is declared in a **second project file** and the including file's `#include` is deliberately not relied
/// on: a fixture include does not resolve (see the guard test above), and it does not have to, because the answer is
/// a **fact the cook produced** — `DeclFact::parameter_list`, stored in the index's own words *"because a query holds
/// one file and the parameters are written in another"*.
///
/// Two files, two constructors, two answers:
///
/// ```text
///   Widget(Widget&&)      and no way to take an `int`   -> reported, from the index
///   Widget(int)                                        -> not reported, from the index
/// ```
/// **What the index answers today about a class in another file** — measured, and not what it needs to be.
///
/// The intent of this test is the architecture: a class declared in a **second project file** should be judged from
/// the facts the cook produced — `DeclFact::parameter_list`, stored in the index's own words *"because a query holds
/// one file and the parameters are written in another"* — so that `std::string x = 123;` is answerable from a `.cpp`
/// whose `<string>` is not its own text.
///
/// What it measures instead is the blocker: `members_of("Widget")` answers **`Yes` with an empty member list** for a
/// class whose definition is only in the other file, so nothing can be concluded about its constructors. Two things
/// follow, and both are in the code:
///
/// ```text
///   the guard     an empty member list answers `None` (unknown), never "no constructors" — reading it the other way
///                 reported `Widget(int size)` as unbuildable, which is a false positive from a missing fact
///   the gap       why the list is empty is the next question. `unlisted` exists to say what a truncated list is
///                 missing, and the probe here printed `0 members []` with nothing in it
/// ```
#[test]
fn a_class_from_another_file_answers_unknown_rather_than_wrong() {
    let cannot = "struct Widget {\n    Widget(Widget&& other);\n    int size;\n};\n";
    let can = "struct Widget {\n    Widget(int size);\n    int size;\n};\n";
    let source = "\
int main() {
    Widget w = 1231;
    return 0;
}
";

    // **Nothing is reported either way**, which is the honest answer for a fact this layer does not have: a missed
    // finding rather than an invented one. When `members_of` answers with members for another file's class, the
    // second of these two becomes a finding and this test is the one to change.
    assert!(
        !codes_with_two_files(source, cannot).contains(&DiagnosticCode::AssignTypeCheck),
        "a class whose members are not in the index is not judged: {:?}",
        codes_with_two_files(source, cannot)
    );
    assert!(
        !codes_with_two_files(source, can).contains(&DiagnosticCode::AssignTypeCheck),
        "and the one with the right constructor is not reported either: {:?}",
        codes_with_two_files(source, can)
    );
}
/// The codes a file produces, in a session where a **second file** is part of the project — so that the index holds
/// its declarations, which is what a check reads facts about another file from.
fn codes_with_two_files(source: &str, other: &str) -> Vec<DiagnosticCode> {
    let files = MemoryFiles::new()
        .with_file("/p/a.cpp", source)
        .with_file("/p/other.h", other);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/other.h"),
    ]);
    // The pump, the ask, the pump — `tests/completion.rs` states the contract once for the workspace.
    session.index_everything();
    let _ = session.view("/p/a.cpp");
    session.index_everything();

    session
        .diagnostics(std::path::Path::new("/p/a.cpp"))
        .map(|found| found.checks.iter().map(|finding| finding.code).collect())
        .unwrap_or_default()
}
/// **The arity question for a callee declared in another file** — the architecture, and its measured state today.
///
/// The intent: `argument-count` reads a callee's parameter list as a **fact** (`DeclFact::parameter_list`, stored
/// *"because a query holds one file and the parameters are written in another"*), so `v.reserve(1, 2)` is judged
/// without parsing a header per keystroke. `callee_from_the_index` implements exactly that, refusing an overload set
/// whole.
///
/// What is measured: it does not resolve yet. On the real corpus —
/// `cargo run --example one_check -- target/std-cook target/std-cook/arity-case.cpp argument-count` — a file calling
/// `v.reserve(1, 2)` and `s.append("a", "b", "c")` reports **zero**, so `definitions(name, path)` is answering
/// something other than the single visible declaration. This fixture says the same thing through a second file, and
/// asserts the honest answer for a fact the layer does not have: **no finding**, rather than a wrong one.
#[test]
fn a_callee_from_another_file_is_not_judged_until_the_lookup_answers() {
    let other = "int scale(int value, int factor);\n";
    let too_many = "\
int main() {
    int x = scale(1, 2, 3);
    return x;
}
";
    // **In this file** the same arity question is judged, which is what makes the line above a gap rather than a
    // hole in the rule: the machinery works, the cross-file lookup is what does not answer yet.
    let here = "\
int scale(int value, int factor) { return value * factor; }

int main() {
    int x = scale(1, 2, 3);
    return x;
}
";
    // **And measured, the in-file case does not fire either**, which is the real finding of this round: `scale(1, 2,
    // 3)` beside its own declaration reports nothing. So `argument-count` is not "missing the cross-file lookup" —
    // its basic case is quiet too, and the next round starts there: the same shape as every other defect in this
    // layer (`FunctionDef` is not how this grammar records a file-scope definition; a parameter list and an argument
    // list are not where a `children()` walk expects them), and the same method fixes it — print what the check
    // sees, one level closer to the decision than the last probe.
    assert!(
        codes_of(here).contains(&DiagnosticCode::ArgumentCount),
        "a callee declared in this file is judged: {:?}",
        codes_of(here)
    );
    assert!(
        !codes_with_two_files(too_many, other).contains(&DiagnosticCode::ArgumentCount),
        "and one declared in another file is not judged yet: {:?}",
        codes_with_two_files(too_many, other)
    );
}
/// **A function template whose return-type spelling carries its header is not judged for a missing return.**
///
/// Measured on EmmyLuaCodeStyle: `template<ServiceClass Service, typename... ARGS> void
/// LanguageServer::AddService(ARGS &&...args) { … }` was reported as returning something with no `return` in its body —
/// the fact carried the **template header and the return type** in one string, and the check compared it to `"void"`
/// exactly.
#[test]
fn a_function_templates_type_that_carries_its_header_is_not_judged() {
    let source = "\
template<class Service>
void AddService(Service service) {
    int unused_index = 0;
}
";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::MissingReturn),
        "`void` after a template header is still `void`: {:?}",
        codes_of(source)
    );

    let really_missing = "\
int AddService(int service) {
    int unused_index = 0;
}
";
    assert!(
        codes_of(really_missing).contains(&DiagnosticCode::MissingReturn),
        "and an `int` function with no return is still a finding: {:?}",
        codes_of(really_missing)
    );
}
/// **A construction and a parameter's default are not conversions to judge.**
///
/// Both shapes are from EmmyLuaCodeStyle, and between them they were all twelve of the project's `assign-type-check`
/// findings: `LineSpace x = LineSpace(LineSpaceType::Keep);` ten times, and `Range(Position _start = Position(0, 0))`
/// twice. The second half says a real mismatch is still reported, so neither guard becomes a blanket silence.
#[test]
fn a_construction_and_a_default_argument_are_not_conversions_to_judge() {
    let constructions = "\
enum class LineSpaceType { Keep };
struct LineSpace { LineSpace(LineSpaceType); };
LineSpace line_space_after_if_statement = LineSpace(LineSpaceType::Keep);
";
    assert!(
        !codes_of(constructions).contains(&DiagnosticCode::AssignTypeCheck),
        "the initialiser names the target type: {:?}",
        codes_of(constructions)
    );

    let defaults = "\
struct Position { Position(int line, int character); };
struct Range { Range(Position start = Position(0, 0)); };
";
    assert!(
        !codes_of(defaults).contains(&DiagnosticCode::AssignTypeCheck),
        "a default argument is the parameter's declaration, not a variable's: {:?}",
        codes_of(defaults)
    );

    // The module's own example, which is the shape this check exists for. (A *class* target with a string initialiser
    // is not one: `convertible_to` answers `Unknown` there, and the check is quiet — which is the second assertion
    // above, seen from the other side.)
    let real_mismatch = "\
void g() {
    int count = \"four\";
}
";
    assert!(
        codes_of(real_mismatch).contains(&DiagnosticCode::AssignTypeCheck),
        "and a string assigned to an int is still a finding: {:?}",
        codes_of(real_mismatch)
    );
}
/// **A call whose name is also a type is a constructor call, and is not judged as a free function's arity.**
///
/// The shape is the one EmmyLuaCodeStyle has 16 times: `NameStyleRule(NameStyleType::SnakeCase)` — one argument — beside
/// a *function* fact named `NameStyleRule` with two parameters, which made the check report "needs 2 … passes fewer" on
/// code that compiles. Which constructor a call means is overload resolution among constructors, which this check does
/// not do; a name that names a type is silence instead.
#[test]
fn a_call_whose_name_is_also_a_type_is_not_judged_for_arity() {
    // The struct and the function share a spelling in **one** file, which is the real project's shape: the name is a
    // type, so the call constructs, and the function's arity says nothing about it.
    let source = "\
struct NameStyleRule {
    int value;
};
void NameStyleRule(int first, int second) {}
void g() {
    NameStyleRule(1);
}
";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::ArgumentCount),
        "the name is a type here, so the call constructs rather than calls: {:?}",
        codes_of(source)
    );
}
/// **An arrow is judged against the class it reaches**, and only when the engine can follow it.
///
/// Measured on EmmyLuaCodeStyle: `_session->Send(std::move(message))` was once reported as "`_session` has no member
/// `Send`" — the receiver is a `std::shared_ptr<Session>`, whose member list is `shared_ptr`'s, and the member the
/// program means is behind `operator->`. The answer was to skip arrows, which then missed `p->Missing()` on a plain
/// `Session*` — a class whose member list *is* the answer. Both halves are pinned here: the pointee's members are
/// judged, and an arrow nothing can follow is silence.
#[test]
fn an_arrow_is_judged_against_the_class_it_reaches() {
    let through_a_pointer = "\
struct Session {
    void Run();
};
void g(Session *session) {
    session->Run();
}
";
    assert_eq!(codes_of(through_a_pointer), [], "`Run` is the pointee's");

    let a_missing_member = "\
struct Session {
    void Run();
};
void g(Session *session) {
    session->Missing();
}
";
    assert_eq!(
        codes_of(a_missing_member),
        [DiagnosticCode::UndefinedField],
        "`Session` has no `Missing`, and the arrow reaches `Session`"
    );

    // **The receiver's own class is not the answer**: a class whose `operator->` returns something this layer
    // cannot type is silence, not a finding about the memb* its own list does not have either.
    let an_untraceable_arrow = "\
struct Session {
    void Run();
};
struct Pointer {
    Missing *operator->();
};
void g(Pointer pointer) {
    pointer->Run();
}
";
    assert_eq!(
        codes_of(an_untraceable_arrow),
        [],
        "the pointee cannot be typed, so nothing is said"
    );
}
/// **A member call is not judged against a free function of the same name.**
///
/// Measured on EmmyLuaCodeStyle: `_session.Run(…)` was judged against a free `Run()` taking no arguments and reported as
/// "takes 0 argument(s) and this call passes more" — the wrong declaration, with the wrong arity, on code that compiles.
/// Resolving the member needs the object's type; until that answer is trusted here, the check says nothing, which is the
/// direction this layer documents.
#[test]
fn a_member_call_is_not_judged_against_a_free_function_of_the_same_name() {
    let source = "\
struct Session {
    void Run();
};
void Run(int value, int other) {}
void g(Session session) {
    session.Run();
}
";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::ArgumentCount),
        "`session.Run()` is the member, not the free `Run(int, int)`: {:?}",
        codes_of(source)
    );
}
/// **A template's parameter is silent to the argument checks, and that is the right answer rather than a gap.**
///
/// Deduction turns `f(T)` into `f(std::string)` for a `std::string` argument, and a parameter that *is* the argument's
/// own type is convertible by construction — so inferring the template arguments cannot make `param-type-check` report
/// anything it does not report today. The check that *can* see a template is the arity one, and it does: `written` and
/// `required` come from the declaration's tokens, not from its types.
///
/// Written as a test so that "no finding" is a decision with a reason rather than an accident, and so that a future
/// change that makes this report is a deliberate one.
#[test]
fn a_templates_own_parameter_is_silent_to_the_type_check_and_the_arity_check_still_judges_it() {
    let silent = "\
template <typename T>
void take(T value);
void g() {
    take(1);
    take(\"one\");
}
";
    assert!(
        !codes_of(silent).contains(&DiagnosticCode::ParamTypeCheck),
        "a `T` parameter is the argument's own type however it was deduced: {:?}",
        codes_of(silent)
    );

    let wrong_arity = "\
template <typename T>
void take(T value);
void g() {
    take(1, 2);
}
";
    assert!(
        codes_of(wrong_arity).contains(&DiagnosticCode::ArgumentCount),
        "the arity of a template is in its tokens, so it is judged: {:?}",
        codes_of(wrong_arity)
    );
}
/// **An overload set is resolved, not judged against its last declaration.**
///
/// Before the resolver existed, this check kept one `Callee` per name — a later declaration **overwrote** an earlier
/// one — so `f(1)` beside `f(int)` and `f(int, int)` was judged against whichever was read last and reported *"takes 2
/// argument(s) and this call passes fewer"* about a program that is perfectly well-formed. The set is now ranked
/// against the call's own argument types, and a call that **some** candidate takes is silence.
#[test]
fn an_overload_set_is_resolved_rather_than_judged_against_its_last_declaration() {
    let well_formed = "\
void f(int) {}
void f(int, int) {}
void g() {
    f(1);
    f(1, 2);
}
";
    assert!(
        !codes_of(well_formed).contains(&DiagnosticCode::ArgumentCount),
        "every call fits a declaration: {:?}",
        codes_of(well_formed)
    );

    // **And the case that is still a finding**: no candidate takes this shape, so the call cannot be right about any
    // of them. This is the only arity report the resolution leaves, and it is the one that is a fact about the file.
    let nothing_fits = "\
void f(int) {}
void f(int, int) {}
void g() {
    f(1, 2, 3);
}
";
    assert!(
        codes_of(nothing_fits).contains(&DiagnosticCode::ArgumentCount),
        "no declaration takes three arguments: {:?}",
        codes_of(nothing_fits)
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
/// **A parameter pack is not a fixed arity, and neither is a C-style `...`.**
///
/// `template <class... Args> void f(Args&&... args)` may be called with as many arguments as the caller likes, and the
/// cross-file half of `argument-count` counted it as **one written, one required** parameter: a call passing three was
/// reported as *"takes 1 argument(s) and this call passes more"*. The test was `parameter.ends_with("...")`, which is
/// true of the C-style `...` and of `Args...` and of **nothing else people write** — `Args&&... args` and `Args... args`
/// are the two spellings the standard library is actually full of. The file-scope half of the same check was already
/// right, because it asks each parameter's **tokens** for an `Ellipsis`; only the text half was wrong.
///
/// Measured on a two-file fixture in `target/arity` with `one_check`: with the old test, three findings —
/// `forward_all(1, 2, 3)`, `forward_plain(1, 2, 3)` and the genuine `fixed_two(1, 2, 3)`; with this one, only the
/// genuine one. The fixture here deliberately calls **nothing but** variadic functions, so a single code says which —
/// the check fires, or it does not.
#[test]
fn a_parameter_pack_is_not_a_fixed_arity() {
    let declarations = "\
template <class... Args>
void forward_all(Args&&... args);

template <class... Args>
void forward_plain(Args... args);

void classic(const char* format, ...);
";
    let calls = "\
#include \"other.h\"

void use() {
    forward_all(1, 2, 3);
    forward_plain(1, 2, 3);
    classic(\"%d %d %d\", 1, 2, 3);
}
";
    let codes = codes_with_two_files(calls, declarations);

    assert!(
        !codes.contains(&DiagnosticCode::ArgumentCount),
        "every callee here takes as many arguments as it is given: {codes:?}"
    );
}

/// **A member of a template class is a member.** The census on EmmyLuaCodeStyle's `nlohmann::json` reported
/// `` `array` has no member `push_back` `` — the class is `basic_json<>`, its members are written inside a
/// `template<...> struct`, and the lookup that reads members must follow the template header to reach them.
#[test]
fn a_member_written_inside_a_template_class_is_found() {
    let source = "\
template <class T>
struct Box {
    void push(T value);
};
void g(Box<int> box) {
    box.push(1);
}
";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::UndefinedField),
        "`push` is a member of the template: {:?}",
        codes_of(source)
    );
}

/// **The `nlohmann::json` shape**: a member reached through an **alias to a template instantiation**.
///
/// `using json = basic_json<int>;` and then `array.push_back(3)` — the cursor's class name is the alias, the members are
/// in the template's body, and the instantiation is the `int` that the alias supplied. This is the shape the census
/// found twice on a real project, and it is the one the simple template test above does not cover.
#[test]
fn a_member_reached_through_an_alias_to_a_template_is_found() {
    let source = "\
namespace nlohmann {
template <class T>
class basic_json {
public:
    void push_back(T value);
};
using json = basic_json<int>;
}
void g(nlohmann::json array) {
    array.push_back(3);
}
";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::UndefinedField),
        "`push_back` is a member of `basic_json`, which `json` names: {:?}",
        codes_of(source)
    );
}

/// **The exact shape from `LSP.h`**: an alias to a template, a static member call, and a use inside a template
/// function — `nlohmann::json array = nlohmann::json::array(); … array.push_back(…)`.
///
/// The three pieces are each plausible on their own, which is why this test exists beside the two above: an alias to a
/// template (passing on its own), a static member used as an initialiser, and a body inside a template whose parameter
/// is `T`. The census found `push_back` missing in this shape twice, and it is the last false positive on a real
/// project.
#[test]
fn the_nlohmann_shape_keeps_its_member() {
    let source = "\
namespace nlohmann {
template <class T>
class basic_json {
public:
    using array_t = int;
    static array_t array();
    void push_back(int value);
};
using json = basic_json<int>;
}

template <class T>
nlohmann::json SerializeArray(std::vector<T>& vec) {
    nlohmann::json array = nlohmann::json::array();
    for (auto& v : vec) {
        array.push_back(1);
        (void)v;
    }
    return array;
}
";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::UndefinedField),
        "`push_back` is a member of `basic_json`: {:?}",
        codes_of(source)
    );
}

/// **A base the index could read nothing from is not evidence that a member is absent.**
///
/// `struct D : Missing` gives `D` a member list that is **partial** — `Missing` contributed nothing, because it is not
/// in the index — and a member written on a `D` is then "not in the list" without being absent from the class. The
/// check must stay silent, which is what `MemberList::unlisted` is for.
///
/// Measured on a real project this is the shape of the worst report of the census: `std::cin.read(…)` was reported as
/// "`std::cin` has no member `read`" while the hover found `basic_istream::read` at once — the same index, asked two
/// ways, answering differently because this check treated "not in the part I read" as "not there".
#[test]
fn a_member_of_a_class_with_an_unread_base_is_not_reported() {
    let source = "\
struct D : Missing {
    void f();
};
void g(D d) {
    d.not_read_anywhere();
}
";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::UndefinedField),
        "the list is partial, so the absence is not evidence: {:?}",
        codes_of(source)
    );
}

/// **A constructor with two parameters cannot be built from one value.**
///
/// `struct S { S(int, int); }; S s = 123;` does not compile. The question "can a class be built from this value" was
/// answered by scanning the parameters for one that accepts it — so the first `int` said yes and the arity was never
/// asked. This is the shape `std::string s = 123;` has in a standard library, whose `basic_string(size_type, char)` is
/// the constructor the scan used to accept.
#[test]
fn a_two_parameter_constructor_cannot_be_built_from_one_value() {
    let source = "\
struct S {
    S(int count, int other);
};
void g() {
    S s = 123;
}
";
    assert!(
        codes_of(source).contains(&DiagnosticCode::AssignTypeCheck),
        "one value cannot fill two parameters: {:?}",
        codes_of(source)
    );

    // …and one defaulted parameter is still one value.
    let defaulted = "\
struct S {
    S(int count, int other = 0);
};
void g() {
    S s = 123;
}
";
    assert!(
        !codes_of(defaulted).contains(&DiagnosticCode::AssignTypeCheck),
        "a defaulted parameter does not need a value: {:?}",
        codes_of(defaulted)
    );
}

/// **The type check a reader most wants from generics**: the argument against the **instantiated** parameter.
///
/// `push_back` on a `std::vector<int>` takes an `int`, because the class was instantiated at `int`. The member's fact
/// says `T value` — the spelling the class wrote — so a check that reads only the fact sees a parameter it cannot
/// classify and stays silent. This is the "type checking after instantiation" the reader asked for, and it is the next
/// layer of the generics work.
/// # Why this is ignored rather than passing
///
/// The fix is one seam, and it is not this file's: `members_of` is called with `written.class_name()`, which has already
/// **dropped** the template arguments (`cpp_code_analysis::completion` line 468), so the member list it builds cannot
/// substitute anything. The substitution itself exists — `sema::types::substituted_spelling`, used by the member
/// **return** type, which is why `basic_string<char>::back()` already answers `char&`. What is missing is the class's
/// parameter names and the use's arguments reaching the member facts.
///
/// The test is kept and ignored rather than deleted: it states the behaviour a reader expects, and it will pass the day
/// the seam is moved.
///
/// # What the substitution did not fix, measured after it landed
///
/// `SemanticModel::members_of` now substitutes the class's arguments into its members — `std::vector<int>::push_back`
/// reports `(int value)` rather than `(T value)`. The diagnostic still does not appear, and the reason is a **second**
/// seam: the argument checks **skip member calls deliberately** (`argument_count`'s member-access guard, added because
/// `_session.Run(…)` was being judged against a free `Run()`). So `v.push_back(…)` never reaches an arity or a type
/// comparison at all, substituted parameters or not. Making this test pass is the work of resolving a member call to the
/// member — `member_access_at` plus `members_of` — rather than of substituting anything.
#[test]
fn a_member_parameter_is_checked_after_instantiation() {
    let source = "\
namespace std {
template <class T>
class vector {
public:
    void push_back(T value);
};
}
void f() {
    std::vector<int> v;
    v.push_back(\"text\");
}
";
    assert!(
        codes_of(source).contains(&DiagnosticCode::ParamTypeCheck)
            || codes_of(source).contains(&DiagnosticCode::ArgumentCount),
        "an `int` parameter does not take a string literal: {:?}",
        codes_of(source)
    );
}

/// **The other half of the rule: a member call whose argument fits is silent.**
///
/// The instantiated check above reports; this one says the report is not blanket. `push_back(1)` on a `std::vector<int>`
/// passes an `int` to an `int`, and nothing is said.
#[test]
fn a_member_call_whose_argument_fits_is_silent() {
    let source = "\
namespace std {
template <class T>
class vector {
public:
    void push_back(T value);
};
}
void f() {
    std::vector<int> v;
    v.push_back(1);
}
";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::ParamTypeCheck),
        "an `int` for an `int` is right: {:?}",
        codes_of(source)
    );
}

/// **The codes a file produces in a project of several files** — the shape a header has and a one-file fixture cannot.
///
/// The defect this exists for needed three things a single-file fixture cannot express: the class in a **header**,
/// an `#include` **above** the class in that header (which shifts every offset of its rendering), and the call in a
/// `.cpp`. Each of the three was a seam the member path crossed, and a fixture with none of them passed while every
/// real `std::vector<int>::push_back("text")` was silent.
fn codes_of_files(files: &[(&str, &str)], asking: &str) -> Vec<DiagnosticCode> {
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
    let paths: Vec<std::path::PathBuf> = files
        .iter()
        .map(|(path, _)| std::path::PathBuf::from(path))
        .collect();
    session.add_project_files(paths);
    session.want_cooked_reading(std::path::Path::new(asking));
    session.index_everything();

    session
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

/// **The signature the standard library actually writes**: a `const _Ty&` parameter, twice over, in a header.
///
/// Reported from an editor: `anotherVec.push_back("123131")` on a `std::vector<int>` produced **nothing**, and the
/// library is not a corner case — `push_back` is declared `void push_back(const _Ty&)` and `void push_back(_Ty&&)`
/// in every implementation, so "one member of that name or nothing is said" was silence on nearly every member
/// call there is. Three defects had to be fixed before this line reported, and each is the fixture:
///
/// ```text
/// the substitution read the rendering with a file's offset   the header's own `#include` above the class shifted
///                                                           it, no template header was found, `T` stayed `T`
/// two candidates meant "say nothing"                        `push_back` is written twice in every implementation
/// a reference target had no arm in the conversion relation   `const int&` was neither `int` nor a pointer
/// ```
///
/// The by-value negative is the second half: an `int` for an `int` is right, whichever candidate takes it.
#[test]
fn a_member_argument_is_checked_through_a_header_and_against_every_overload() {
    let header = "\
#pragma once
#include <cstddef>
template <class T>
struct Box {
    void push_back(const T& value);
    void push_back(T&& value);
    T& front();
};
";
    let file = "\
#include \"box.h\"
void f() {
    Box<int> b;
    b.push_back(\"text\");
}
";
    let codes = codes_of_files(&[("/p/box.h", header), ("/p/a.cpp", file)], "/p/a.cpp");
    assert!(
        codes.contains(&DiagnosticCode::ParamTypeCheck),
        "a string literal for a `const int&` parameter is not right: {codes:?}"
    );

    let fits = file.replace("b.push_back(\"text\");", "b.push_back(1);");
    let codes = codes_of_files(&[("/p/box.h", header), ("/p/a.cpp", &fits)], "/p/a.cpp");
    assert!(
        !codes.contains(&DiagnosticCode::ParamTypeCheck),
        "an `int` for an `int` is right: {codes:?}"
    );
}

/// **One candidate that takes the argument keeps the check silent** — the rule an overload set is judged by.
///
/// `resize(int)` and `resize(const char*)` are both `resize`, and `w.resize("wide")` is the second one: a check
/// that reported "this passes a string where an `int` is declared" would be underlining a correct line, which is
/// the failure mode this whole layer exists to avoid. So a finding needs **every** candidate to disagree — and the
/// other half is here too: when they all do, it is still reported.
#[test]
fn an_overload_that_takes_the_argument_keeps_the_check_silent() {
    let one_fits = "\
struct Widget {
    void resize(int width);
    void resize(const char* text);
};
void f() {
    Widget w;
    w.resize(\"wide\");
}
";
    assert!(
        !codes_of(one_fits).contains(&DiagnosticCode::ParamTypeCheck),
        "the second overload takes a string literal: {:?}",
        codes_of(one_fits)
    );

    let none_fit = "\
struct Widget {
    void resize(int width);
    void resize(double width);
};
void f() {
    Widget w;
    w.resize(\"wide\");
}
";
    assert!(
        codes_of(none_fit).contains(&DiagnosticCode::ParamTypeCheck),
        "neither overload takes a string literal, so this is a finding: {:?}",
        codes_of(none_fit)
    );
}

/// **A non-template class's member is checked the same way** — the substitution is not what makes the check work, it is
/// what makes it *correct* for a template; the plain case has to work on its own.
#[test]
fn a_member_call_on_a_plain_class_is_checked() {
    let source = "\
struct Widget {
    void resize(int width);
};
void f() {
    Widget w;
    w.resize(\"wide\");
}
";
    assert!(
        codes_of(source).contains(&DiagnosticCode::ParamTypeCheck),
        "a string literal for an `int` is not right: {:?}",
        codes_of(source)
    );
}

/// **Too many arguments to a member** is the arity rule, through the same resolution.
#[test]
fn a_member_call_with_too_many_arguments_is_a_different_finding() {
    let source = "\
struct Widget {
    void resize(int width);
};
void f() {
    Widget w;
    w.resize(1, 2);
}
";
    assert!(
        codes_of(source).contains(&DiagnosticCode::ArgumentCount),
        "one parameter, two arguments: {:?}",
        codes_of(source)
    );
}

/// **A free function is still checked** — the member path was added beside the old one, not instead of it.
#[test]
fn a_free_function_call_is_still_checked() {
    // **A definition**, not a declaration: this check's table is built from this file's function definitions (its
    // module documentation says so), which is exactly the rule that made a class *member* invisible to it.
    let source = "\
void resize(int) { }
void f() {
    resize(\"wide\");
}
";
    assert!(
        codes_of(source).contains(&DiagnosticCode::ParamTypeCheck),
        "the free path is unchanged: {:?}",
        codes_of(source)
    );
}

/// **A named parameter is not checked, and that is a gap rather than a rule.**
///
/// `parameter_spellings_of` returns the parameter as the file wrote it — `int width` — and `parse_type_spelling` reads a
/// *type*, so the comparison sees a spelling it cannot parse and says nothing. Measured: the same call is reported when
/// the parameter is unnamed (`void resize(int)`) and silent when it is named, which is the shape every real codebase
/// writes. `sema::overload::parameters_in` already strips the name (`a_type_without_its_name`), so the fix is to read the
/// spellings through it — recorded here rather than asserted, because changing it makes this check fire on code it has
/// never judged before, which is a census to run first.
#[test]
fn a_named_parameter_is_checked_too() {
    let source = "\
void resize(int width) { }
void f() {
    resize(\"wide\");
}
";
    assert!(
        codes_of(source).contains(&DiagnosticCode::ParamTypeCheck),
        "the name is not part of the type: {:?}",
        codes_of(source)
    );
}

/// **An assignment whose result is compared is the idiom**, and the census found the one place this rule got it wrong.
///
/// `if ((op = queue.front()) != 0)` is deliberate: the assignment is parenthesised and the *comparison* is the condition.
/// The rule now reads the condition's own operator, so a comparison at depth zero inside the parentheses keeps it quiet —
/// while `if (x = 1)` still reports, and so does `if (x = a < b)`, whose `<` is inside the assignment.
#[test]
fn an_assignment_compared_against_something_is_quiet() {
    let idiom = "struct Q { int value() const; };\n\
                 void f(Q q) {\n    int op = 0;\n    if ((op = q.value()) != 0) { return; }\n}\n";
    assert!(
        !codes_of(idiom).contains(&DiagnosticCode::AssignmentInCondition),
        "a compared assignment is the shape the rule allows: {:?}",
        codes_of(idiom)
    );

    for reported in [
        // The mistake the rule exists for.
        "void f() {\n    int x = 0;\n    if (x = 1) { return; }\n}\n",
        // A comparison inside the assignment's right-hand side does not hide it: the condition's operator is still `=`.
        "void f() {\n    int x = 0; int a = 0; int b = 0;\n    if (x = a < b) { return; }\n}\n",
    ] {
        assert!(
            codes_of(reported).contains(&DiagnosticCode::AssignmentInCondition),
            "{reported}: {:?}",
            codes_of(reported)
        );
    }
}

/// **`0` is the null pointer constant**, and it converts to a class that takes a pointer — the shape the census found as the
/// only one its `assign-type-check` examples shared.
///
/// `P p = 0;` where `P` has `P(int*)` is valid C++: `0` is a null pointer constant. The conversion relation cannot see the
/// constructor, so it answered `Unknown`, and the class case answered "cannot be built from an `int`" — both true of an
/// `int` and both wrong of the **literal**. (The negative side — a value that is not `0` is still checked — is covered by
/// the existing cases in this file; the claim here is only the one this fix makes.)
#[test]
fn zero_is_a_null_pointer_constant() {
    let source = "\
struct P {
    P(int* pointer);
};
void f() {
    P p = 0;
}
";
    assert!(
        !codes_of(source).contains(&DiagnosticCode::AssignTypeCheck),
        "`0` is the null pointer constant: {:?}",
        codes_of(source)
    );
}
/// **A member of a class template is checked with `T` substituted, through the class's own alias.**
///
/// Reported from an editor: `std::vector<int> anotherVec; anotherVec.push_back("123131");` was not reported, while
/// the same mistake through a plain function was. The declaration is libstdc++'s
/// `void push_back(const _Tp& __x);` with `using value_type = _Tp;` in the class — so the parameter is an
/// alias of an alias of the template parameter, and the check has to follow both before it can say the argument is
/// a `const char[7]` where a `const int&` is wanted.
#[test]
fn a_member_of_a_class_template_is_checked_after_substitution() {
    let header = "\
namespace std {
template <class _Tp>
class vector {
public:
    using value_type = _Tp;
    void push_back(const value_type& __x);
};
}
";
    let source = "\
#include \"vector.h\"
int main()
{
    std::vector<int> anotherVec;
    anotherVec.push_back(\"123131\");
    return 0;
}
";
    let codes = codes_of_files(&[("/p/vector.h", header), ("/p/a.cpp", source)], "/p/a.cpp");
    {
        // PROBE: what the member list says the parameters are.
        let files = MemoryFiles::new()
            .with_file("/p/vector.h", header)
            .with_file("/p/a.cpp", source);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files([
            std::path::PathBuf::from("/p/vector.h"),
            std::path::PathBuf::from("/p/a.cpp"),
        ]);
        session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
        session.want_cooked_reading(std::path::Path::new("/p/vector.h"));
        session.index_everything();
        let view = session.view("/p/a.cpp").expect("held");
        match session.members_of(&view, "std::vector<int>") {
            cpp_code_analysis::Known::Yes(list) => {
                for member in &list.members {
                    eprintln!(
                        "PROBE {:?} kind {:?} params {:?} type_of {:?}",
                        member.fact.name,
                        member.fact.kind,
                        member.fact.parameter_list,
                        member.fact.type_of
                    );
                }
            }
            other => eprintln!("PROBE members_of: {other:?}"),
        }
    }
    assert!(
        codes.contains(&DiagnosticCode::ParamTypeCheck),
        "the argument is a string literal and the parameter is `const int&`: {codes:?}"
    );
}

/// **The alias step is what libstdc++ actually writes, and it is the half that does not work yet.** Ignored rather
/// than deleted: it is the shape of the report (`std::vector<int>::push_back("…")`), and it will pass the day
/// `members_of` substitutes the class's arguments into a member's **parameter list** as well as into its type —
/// today `value_type` is still a name to `convertible_to`, which answers `Unknown`, and a check that cannot compare
/// says nothing.
#[test]
fn a_member_parameter_written_with_the_classs_alias_is_checked() {
    let header = "\
namespace std {
template <class _Tp>
class vector {
public:
    using value_type = _Tp;
    void push_back(const value_type& __x);
};
}
";
    let source = "\
#include \"vector.h\"
int main()
{
    std::vector<int> anotherVec;
    anotherVec.push_back(\"123131\");
    return 0;
}
";
    let file = "/p/vector.h";
    let caller = "/p/a.cpp";
    let mut memory = MemoryFiles::new()
        .with_file(file, header)
        .with_file(caller, source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory.clone());
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from(file),
        std::path::PathBuf::from(caller),
    ]);
    // **Both files are cooked**, which is the state a check runs in: the open file's closure is read and rendered
    // before a diagnostic is asked for. `codes_of_files` asks for the *caller* only, and the substitution needs the
    // class's own file — see `substituted_members`, whose first step is reading the declaring file.
    session.want_cooked_reading(std::path::Path::new(file));
    session.want_cooked_reading(std::path::Path::new(caller));
    session.index_everything();

    let codes: Vec<DiagnosticCode> = session
        .diagnostics(std::path::Path::new(caller))
        .map(|found| found.checks.iter().map(|finding| finding.code).collect())
        .unwrap_or_default();
    let _ = &mut memory;

    assert!(
        codes.contains(&DiagnosticCode::ParamTypeCheck),
        "the argument is a string literal and the parameter is `const value_type&` = `const int&`: {codes:?}"
    );
}

/// **The shape from a real session**: `writeMessage(const std::string& json)` called with `123131`.
///
/// `int` does not convert to `const std::string&`, and a compiler refuses it. The check is silent, which is the "has the
/// form of a check but does not work" the reader reported — and the reason is the class case: the conversion relation
/// answers `Unknown` for a class target (it lives in the type algebra and never sees a constructor), and the class case only
/// speaks when it is **sure** the value cannot build the class.
///
/// # Where it stands
///
/// Three real holes were found and closed on the way, each by printing the step rather than guessing:
///
/// ```text
/// ① the parameter `const std::string&` parses to `Reference { to: Named("std::string") }`, and `class_name()` answers
///    `None` for a reference — so the index was never asked. The check now strips `const`/`&` before asking.
/// ② `param-type-check` only looked at this file's function *definitions*, so a function **declared** in a header and called
///    here was skipped entirely. It now falls back to the summary, then the index.
/// ③ measured last: `a_class_can_be_built_from("std::string", int)` answers **`Some(true)`** for a class whose only
///    constructor is `string(const char*)` — a `const char*` parameter cannot take an `int`. The next step is
///    `parameters_that_could_accept("(const char* text)", int)`, which is where that `true` comes from.
/// ```
#[test]
fn an_int_does_not_convert_to_a_string() {
    let source = "\
namespace std {
class string {
public:
    string(const char* text);
};
}
void writeMessage(const std::string& json);
void f() {
    writeMessage(123131);
}
";
    let found = codes_of(source);
    println!("codes: {found:?}");
    assert!(
        found.contains(&DiagnosticCode::ParamTypeCheck),
        "an `int` is not a `std::string`: {found:?}"
    );
}

/// **中文诊断** —— 一条检查消息跟着 `set_locale` 走,而不是跟着编辑器走。
///
/// 这张表最容易出的错是 key 和英文模板**差一个字符**:那样 `t!` 会安静地返回英文,看起来像"翻译没生效"。
/// 所以这条测试不检查"有没有翻译文件",而是直接看一条真的诊断在两种 locale 下的文字。
/// **中文诊断** —— 一条检查消息跟着 `set_locale` 走,而不是跟着编辑器走。
///
/// 这张表最容易出的错是 key 和英文模板**差一个字符**:那样 `t!` 会安静地返回英文,看起来像"翻译没生效"。
/// 所以这条测试不看"有没有翻译文件",而是直接看一条真的诊断在两种 locale 下的文字。
#[test]
fn a_diagnostic_follows_the_locale() {
    let source = "int f() { int unused_local = 1; return 0; }\n";

    let messages = |locale: &str| {
        cpp_parser::set_locale(locale);
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
                    .map(|finding| finding.message.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    let english = messages("en");
    let chinese = messages("zh-CN");
    cpp_parser::set_locale("en");

    println!("en: {english:?}\nzh: {chinese:?}");
    let unused = |list: &[String]| {
        list.iter()
            .find(|message| message.contains("unused_local"))
            .cloned()
            .unwrap_or_else(|| panic!("the unused check reports it: {list:?}"))
    };

    assert!(
        unused(&english).contains("never used"),
        "{:?}",
        unused(&english)
    );
    assert!(
        unused(&chinese).contains("从未使用"),
        "the message is translated: {:?}",
        unused(&chinese)
    );
}
/// **`concept-not-satisfied`** —— 实参的类型不满足形参上的概念。
///
/// 能判定的是"类型概念"这一族(`std::integral`、`std::floating_point`、`std::same_as`、`std::convertible_to`),
/// 因为它们的含义只是类型的一个性质;用户自己写的概念、`requires` 子句、类型未知的情形一律**沉默** ——
/// 一个诊断是一个断言,不是猜。
#[test]
fn an_argument_that_does_not_satisfy_its_concept_is_reported() {
    let source = "\
template <std::integral T>
T twice(T value) { return value + value; }

int f() {
    twice(2);
    twice(\"text\");
    return 0;
}
";
    let messages = messages_of(source);
    println!("{messages:?}");
    let finding = messages
        .iter()
        .find(|(code, _)| *code == DiagnosticCode::ConceptNotSatisfied)
        .unwrap_or_else(|| panic!("the `const char*` call is reported: {messages:?}"));
    assert!(finding.1.contains("std::integral"), "{finding:?}");
    assert!(
        finding.1.contains("char"),
        "the message names the type that was passed: {finding:?}"
    );
    assert_eq!(
        messages
            .iter()
            .filter(|(code, _)| *code == DiagnosticCode::ConceptNotSatisfied)
            .count(),
        1,
        "`twice(2)` satisfies it and is silent: {messages:?}"
    );
}

/// **A concept the analysis cannot decide is silence**, not a guess: `requires` clauses and user-written concepts
/// are exactly the cases where a finding would be the check's opinion rather than the language's.
#[test]
fn an_undecidable_concept_says_nothing() {
    let source = "\
template <class T>
concept Small = sizeof(T) <= 4;

template <Small T>
T keep(T value) { return value; }

int f() {
    keep(1.0);
    return 0;
}
";
    let messages = messages_of(source);
    assert!(
        !messages
            .iter()
            .any(|(code, _)| *code == DiagnosticCode::ConceptNotSatisfied),
        "{messages:?}"
    );
}

/// The codes and messages a fixture produces.
fn messages_of(source: &str) -> Vec<(DiagnosticCode, String)> {
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
                .map(|finding| (finding.code, finding.message.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// **`.cppls.toml` 的 `[diagnostics] disable` 真的关掉一条规则,`enable` 只打开点名的那些。**
///
/// 这套机制之前在类型上就有(`CheckConfig`),但没有人把它接进一次真正运行的检查 —— 一个用户相信已经关掉的规则
/// 仍然在报,是这套配置最坏的失败方式,所以两个方向都要测。
#[test]
fn the_project_config_turns_rules_off_and_on() {
    let source = "int f() { int unused_local = 1; return 0; }\n";

    let messages_with = |config: &str| {
        let files = MemoryFiles::new()
            .with_file("/p/a.cpp", source)
            .with_file("/p/.cppls.toml", config);
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
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    // Default: the unused local is reported.
    assert!(
        messages_with("").contains(&DiagnosticCode::Unused),
        "the rule is on by default"
    );

    // `disable`: it is not.
    let disabled = messages_with("[diagnostics]\ndisable = [\"unused\"]\n");
    assert!(
        !disabled.contains(&DiagnosticCode::Unused),
        "`disable` turns it off: {disabled:?}"
    );

    // `enable` naming one rule: only that one may report.
    let only = messages_with("[diagnostics]\nenable = [\"param-type-check\"]\n");
    assert!(
        !only.contains(&DiagnosticCode::Unused),
        "`enable` is `only these`: {only:?}"
    );
}

/// **`std::derived_from` 走索引里的基类**,`std::copyable` 这类"对象概念"只在实参是**引用**时才给答案。
#[test]
fn a_concept_over_the_class_hierarchy_and_over_objectness() {
    let hierarchy = "\
struct Base { virtual ~Base(); };
struct Derived : public Base { };

template <std::derived_from<Base> T>
void handle(T& value);

void f(Derived d, int n) {
    handle(d);
    handle(n);
}
";
    let messages = messages_of(hierarchy);
    println!("{messages:?}");
    let concept_findings: Vec<&(DiagnosticCode, String)> = messages
        .iter()
        .filter(|(code, _)| *code == DiagnosticCode::ConceptNotSatisfied)
        .collect();
    assert_eq!(
        concept_findings.len(),
        1,
        "`handle(n)` is not derived from `Base`: {messages:?}"
    );
    assert!(
        concept_findings[0].1.contains("derived_from"),
        "{concept_findings:?}"
    );

    // **An object concept against a reference.** `copyable<int&>` is false because the concept asks for an object
    // type; `copyable<int>` is not this layer's question (it needs the constructors), so only the reference reports.
    let objectness = "\
template <std::copyable T>
T copy_of(T value) { return value; }

int f(int n) {
    copy_of(n);
    return 0;
}
";
    let messages = messages_of(objectness);
    println!("{messages:?}");
    assert!(
        !messages
            .iter()
            .any(|(code, _)| *code == DiagnosticCode::ConceptNotSatisfied),
        "`int` is copyable and the check must not guess about a class: {messages:?}"
    );
}

/// **声明在头文件里的约束,从调用处的源文件也能查出来** —— 走 summary 里的
/// `DeclFact::parameters` / `DeclFact::constraints` / `parameter_list`。
#[test]
fn a_constraint_declared_in_a_header_is_checked_at_the_call() {
    let header = "\
#pragma once
struct Base { virtual ~Base(); };
struct Derived : public Base { };

template <std::derived_from<Base> T>
void handle(T& value);
";
    let source = "\
#include \"b.h\"
void f(Derived d, int n) {
    handle(d);
    handle(n);
}
";
    let files = MemoryFiles::new()
        .with_file("/p/b.h", header)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/b.h"),
        std::path::PathBuf::from("/p/a.cpp"),
    ]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();

    let messages: Vec<(DiagnosticCode, String)> = session
        .diagnostics(std::path::Path::new("/p/a.cpp"))
        .map(|found| {
            found
                .checks
                .iter()
                .map(|finding| (finding.code, finding.message.clone()))
                .collect()
        })
        .unwrap_or_default();
    println!("{messages:?}");

    let concept_findings: Vec<&(DiagnosticCode, String)> = messages
        .iter()
        .filter(|(code, _)| *code == DiagnosticCode::ConceptNotSatisfied)
        .collect();
    assert_eq!(
        concept_findings.len(),
        1,
        "`handle(n)` is reported from the header's constraint: {messages:?}"
    );
    assert!(
        concept_findings[0].1.contains("derived_from"),
        "{concept_findings:?}"
    );
}

/// **The codes one file produces, with the project's own language standard** — the `[compile] standard` / `-std=`
/// path a `.cppls.toml` takes.
fn codes_of_with(source: &str, path: &str, standard: Option<&str>) -> Vec<DiagnosticCode> {
    let files = MemoryFiles::new().with_file(path, source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let config = CompilerConfig {
        standard: standard.map(|standard| standard.into()),
        ..CompilerConfig::default()
    };
    let mut session = Session::with_config("/p", providers, WatchFilter::new("/p"), config);
    session.add_project_files([std::path::PathBuf::from(path)]);
    session.want_cooked_reading(std::path::Path::new(path));
    session.index_everything();

    session
        .diagnostics(std::path::Path::new(path))
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

/// **A C project is not told to write named casts** — the language has to come from the project's own `-std=`.
///
/// `cstyle_cast` asks `CheckContext::is_c()` before it fires, and its own note says why: a C program has no
/// `static_cast`, so `(Widget*)p` is how it writes that conversion and a rule that called it a mistake would be wrong
/// about the language. The question was asked, but the answer was always "not told": `CompilerConfig::language_for`
/// read the file's compile command and `extra_args`, while a project stating `-std=gnu11` in `.cppls.toml` lands in
/// the config's **`standard`** field — which it never looked at.
///
/// Measured through the language server on neovim's `arglist.c` (a C file, `.cppls.toml` with
/// `args = ["-std=gnu11"]`): **14 `cstyle-cast` findings**, every one of them telling a C program to write
/// `static_cast<int>`. So this test states the language the way a project does and asks for the findings both ways.
#[test]
fn a_c_project_is_not_told_to_write_named_casts() {
    const SOURCE: &str = "\
typedef struct { int v; } T;
int f(void) { double d = 1.5; T t; t.v = (int)d; return t.v; }
";
    let in_c = codes_of_with(SOURCE, "/p/a.c", Some("gnu11"));
    assert!(
        !in_c.contains(&DiagnosticCode::CStyleCast),
        "a C program has no named casts, so the rule is not about it: {in_c:?}"
    );

    // **And the rule is not simply off**: the same cast in a file compiled as C++ is reported, which is what says
    // this test is about the language rather than about the check having stopped working.
    let tmp = codes_of_with(SOURCE, "/p/a.cpp", Some("gnu++17"));
    assert!(
        tmp.contains(&DiagnosticCode::CStyleCast),
        "the same cast in C++ is the rule's own case: {tmp:?}"
    );
}

/// **A call that passes a cast argument has not passed fewer.** `three((int)x, 2, 3)` is three arguments — see
/// `cpp_parser`'s `a_cast_argument_is_counted` for the root cause (a `CastExpr` was not a `CppExpr`, so the argument
/// accessor dropped it). Measured on neovim's `arglist.c`: `do_arg_all((int)eap->line2, eap->forceit, eap->cmdidx ==
/// CMD_drop)` was "needs 3 argument(s) and this call passes fewer".
#[test]
fn a_cast_argument_is_not_a_missing_argument() {
    const SOURCE: &str = "\
static void three(int a, int b, int c) { (void)a; (void)b; (void)c; }
void one(int x) { three((int)x, 2, 3); }
void two(int x) { three((int)x, 2, x == 0); }
";
    let codes = codes_of_with(SOURCE, "/p/a.c", Some("gnu11"));
    assert!(
        !codes.contains(&DiagnosticCode::ArgumentCount),
        "every call here passes three arguments: {codes:?}"
    );

    // **And a real count is still reported**, so this is not the check having gone quiet.
    let wrong = "static void two_args(int a, int b) { (void)a; (void)b; }\nvoid f(int x) { two_args((int)x, 2, 3); }\n";
    assert!(
        codes_of_with(wrong, "/p/a.c", Some("gnu11")).contains(&DiagnosticCode::ArgumentCount),
        "three arguments for two parameters is the rule's own case"
    );
}

/// **A C-style cast is not a rule about C.** Reported from an editor with a screenshot: `(unsigned)c` inside vim's
/// `charset.c` — a binary search over a character array — was underlined as "a C-style cast", with the advice to
/// write `static_cast`, which C does not have.
///
/// The rule already refused to fire on C (`CheckContext::is_c`), so the defect was one layer down: the **language of
/// the file** was unknown. That project has no compile database entry, no `[compile] standard` and no `-std=`, so
/// `LanguageConfig::permissive` answered "both families", `family()` was `None`, and `is_c()` was false — an
/// unconfigured project was read as C++ by every C++-only rule.
///
/// A `.c` file is C: that is what every compiler decides by, and what the editor decided when it opened the file, so
/// `CompilerConfig::language_for` now falls back to the extension before it falls back to "both".
#[test]
fn a_c_style_cast_is_reported_in_cpp_and_not_in_c() {
    let source = "int f(double c) { return (unsigned)c; }\n";

    assert!(
        codes_of_a_file("/p/a.cpp", source).contains(&DiagnosticCode::CStyleCast),
        "the fixture must produce the finding where the rule applies"
    );

    assert!(
        !codes_of_a_file("/p/a.c", source).contains(&DiagnosticCode::CStyleCast),
        "…and not in a `.c` file, where `(unsigned)c` is how the conversion is written"
    );
}

/// **A class nested in another one has members, and its own members are not "undefined".**
///
/// `class outer { public: class inner { inner(bool e) : at_end_(e) {} bool operator!=(const inner &other) const
/// { return at_end_ != other.at_end_; } private: bool at_end_ = true; }; };` — the class is `outer::inner`, and
/// every use inside `outer` spells it `inner`. The member list for that spelling came back **empty**, so
/// `undefined-field` reported `` `other` has no member `at_end_` `` on a class that declares it, while the class's
/// own field was invisible to completion too: `definition("inner")` was ambiguous between the class (scope `outer`)
/// and its **constructor** (a declaration named `inner` in scope `outer::inner`), and an ambiguous class was read as
/// "that class has nothing written in it". Measured on `test_cpp/coroutine.cpp`, whose `generator<T>::iterator` is
/// this shape: two false findings from one lookup.
#[test]
fn a_nested_class_has_its_members() {
    let nested = "\
class outer {
  public:
    class inner {
      public:
        inner(bool e) : at_end_(e) {}
        bool operator!=(const inner &other) const { return at_end_ != other.at_end_; }
      private:
        bool at_end_ = true;
    };
};
";
    assert_eq!(
        codes_of(nested),
        [],
        "a member written in the class is not undefined: {:?}",
        codes_of(nested)
    );

    // …and the rule still speaks when a member really is not there: the fix must not silence the check.
    let missing = "\
class outer {
  public:
    class inner {
      public:
        int at_end_ = 0;
    };
};
void f() { outer::inner i; i.missing_; }
";
    assert_eq!(
        codes_of(missing),
        [DiagnosticCode::UndefinedField],
        "a member the nested class does not declare is still reported"
    );
}

/// **A coroutine is not a function that forgot to return.**
///
/// A body that writes `co_yield`, `co_return` or `co_await` is a coroutine, and the value contract lives on
/// `promise_type` (`return_void`, `return_value`, `yield_value`) — so a `generator<int>` that only yields is
/// complete. Measured on `test_cpp/coroutine.cpp`: `generator<int> countdown(int from) { for (…) { co_yield i; } }`
/// answered "this function is declared to return `generator<int>` and its body has no `return`", on a file that
/// compiles and runs in GCC 15.1.
#[test]
fn a_coroutine_body_has_no_missing_return() {
    for (name, source) in [
        (
            "co_yield",
            "generator make(int from) {\n    for (int i = 0; i < from; ++i) { co_yield i; }\n}\n",
        ),
        (
            "co_return",
            "int compute(int v) {\n    co_return v + 1;\n}\n",
        ),
        (
            "co_await",
            "int wait(int v) {\n    int x = co_await v;\n    return x;\n}\n",
        ),
    ] {
        assert_eq!(
            codes_of(source),
            [],
            "{name}: a coroutine body needs no `return`"
        );
    }

    // The rule itself is unchanged for an ordinary function.
    assert_eq!(
        codes_of("int count() {\n}\n"),
        [DiagnosticCode::MissingReturn],
        "a plain function with no return is still reported"
    );
}

/// **A name used as an object, where nothing declares it.**
///
/// `missing.size()` is the shape a reader writes while thinking of a variable they have not declared — or have just
/// renamed — and until this rule existed the editor said nothing at all: `undefined-field` needs the receiver's *type*,
/// and a name with no declaration has none. Measured on `test_cpp/main.cpp`, whose `myName.getFullName(0)` is declared
/// ten lines below the use and was silently accepted.
#[test]
fn a_name_that_nothing_declares_is_reported() {
    let source = "\
struct Widget { int size; };
void f() {
    missing.size;
}
";
    assert_eq!(
        codes_of(source),
        [DiagnosticCode::UndefinedName],
        "nothing declares `missing`"
    );
}

/// **The same access, on a name that is declared, is not this rule's business** — and a name declared later in the
/// same file is not visible yet, which is the language's own rule rather than an inference.
#[test]
fn a_declared_name_is_not_reported_and_a_later_one_is() {
    let declared = "\
struct Widget { int size; };
void f() {
    Widget w;
    w.size;
}
";
    assert_eq!(codes_of(declared), [], "`w` is declared two lines up");

    let later = "\
struct Widget { int size; };
void f() {
    w.size;
}
Widget w;
";
    assert_eq!(
        codes_of(later),
        [DiagnosticCode::UndefinedName],
        "`w` is declared after the use, so it is not visible there"
    );
}

/// **The four silences the rule is written around**: a macro, a template body, a lambda body, and a header.
#[test]
fn the_rule_is_quiet_where_a_name_may_be_declared_elsewhere() {
    // A macro whose expansion is a declared name: the rendering makes the receiver `w`, which resolves.
    let macro_defined = "\
struct Widget { int size; };
#define receiver w
void f() {
    Widget w;
    receiver.size;
}
";
    assert_eq!(codes_of(macro_defined), []);

    // A template body is dependent: `count` may come from a base that exists only at instantiation.
    let templated = "\
template <class T>
struct Derived : T {
    void f() { count.size; }
};
";
    assert_eq!(codes_of(templated), [], "a template body is not judged");

    // A lambda's capture is a declaration the scope tree does not bind.
    let lambda = "\
struct Widget { int size; };
void f() {
    Widget w;
    auto g = [&w]() { w.size; };
}
";
    assert_eq!(codes_of(lambda), [], "a capture is not a name the scope tree holds");

    // A name a header declares — here the header is the second file of the fixture.
    let from_a_header = "\
#include \"b.h\"
void f() {
    from_header.size;
}
";
    let header = "\
struct Widget { int size; };
extern Widget from_header;
";
    let codes: Vec<DiagnosticCode> = codes_of_a_two_file_fixture(from_a_header, header);
    assert_eq!(codes, [], "`from_header` is declared in the header");
}

/// [`codes_of`] for a fixture that needs a header beside its source.
fn codes_of_a_two_file_fixture(source: &str, header: &str) -> Vec<DiagnosticCode> {
    let files = MemoryFiles::new()
        .with_file("/p/a.cpp", source)
        .with_file("/p/b.h", header);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/b.h"),
    ]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.want_cooked_reading(std::path::Path::new("/p/b.h"));
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

/// **A call to a name nothing declares**, and the one argument shape that keeps the rule quiet.
///
/// ```cpp
/// void f() { printh("x", 12); }     // nothing declares `printh`
/// ```
///
/// Measured on `test_cpp/main.cpp`: `printh("1231313", 12)` on line 74, with `void printh(…)` defined on line 83 — a
/// call a compiler rejects and this server accepted. The extra guard a *receiver* does not need is **ADL**: an
/// unqualified call may be found through the namespaces of its arguments' types, so a call with a class-typed (or
/// unreadable) argument is silence, and only a call whose arguments are all builtins can be called undeclared.
#[test]
fn a_call_to_a_name_nothing_declares_is_reported() {
    let undeclared = "\
void f() {
    printh(\"x\", 12);
}
";
    assert_eq!(
        codes_of(undeclared),
        [DiagnosticCode::UndefinedName],
        "nothing declares `printh`, and its arguments are a string literal and an int"
    );

    // **ADL**: a class-typed argument may bring the callee in from its own namespace.
    let adl = "\
struct Widget { int size; };
void f(Widget w) {
    swap(w, w);
}
";
    assert_eq!(codes_of(adl), [], "`swap` may be found by ADL");
    // A macro is expanded before the checks see it, so the callee under judgement is the *body*'s. Declared, and
    // the rule is silent — which is also the evidence that the macro's own name is never judged as a callee.
    let macro_call = "\
#define LOG(x) report(x)
void report(int value);
void f() {
    LOG(1);
}
";
    assert_eq!(codes_of(macro_call), []);

    // A callee the header declares.
    let from_a_header = "\
void log_name(const char *name, int count);
";
    let caller = "\
#include \"b.h\"
void f() {
    log_name(\"x\", 1);
}
";
    assert_eq!(
        codes_of_a_two_file_fixture(caller, from_a_header),
        [],
        "`log_name` is declared in the header"
    );
}

/// **A name declared twice in one scope**, and the five repeats that are not that.
///
/// `test_cpp/main.cpp` writes `auto x = 123;` on line 22 and `auto x = anotherVec[1];` on line 39 — one function, two
/// variables of one name, which no compiler accepts. The rule judges the scopes a reader writes (a function body, a
/// block, a lambda) and only the bindings that have no second reading: a function name in one scope is an **overload
/// set**, a class name is a forward declaration and its definition, and a file-scope variable may be `extern` in one
/// place and defined in another. Each of those is a case below.
#[test]
fn a_name_declared_twice_in_one_scope_is_reported() {
    let twice = "\
void f() {
    int count = 1;
    int count = 2;
}
";
    assert_eq!(codes_of(twice), [DiagnosticCode::Redefinition]);

    // The shape the sample project writes: `auto` twice, so the types disagree as well.
    let twice_with_auto = "\
struct W { int size; };
void f() {
    auto x = 123;
    auto x = W{};
}
";
    assert_eq!(codes_of(twice_with_auto), [DiagnosticCode::Redefinition]);

    // A parameter and a local of the same name are two declarations in the function's scope.
    let shadows_a_parameter = "\
void f(int count) {
    int count = 2;
}
";
    assert_eq!(codes_of(shadows_a_parameter), [DiagnosticCode::Redefinition]);
}

/// **The five repeats that are not a redefinition** — see [`a_name_declared_twice_in_one_scope_is_reported`].
#[test]
fn a_repeat_the_language_has_a_reading_for_is_quiet() {
    // Two blocks: two scopes.
    let two_blocks = "\
void f() {
    { int count = 1; }
    { int count = 2; }
}
";
    assert_eq!(codes_of(two_blocks), []);

    // Two overloads: one name, two declarations, one entity — the language's own reading.
    let overloads = "\
void run();
void run(int value);
void f() { run(); run(1); }
";
    assert_eq!(codes_of(overloads), []);

    // A forward declaration and its definition.
    let forward_and_definition = "\
class Widget;
class Widget { int size; };
";
    assert_eq!(codes_of(forward_and_definition), []);

    // `extern` then a definition, at file scope.
    let extern_and_definition = "\
extern int value;
int value = 0;
";
    assert_eq!(codes_of(extern_and_definition), []);

    // A nested block may shadow: the inner name is a different scope's.
    let shadowing = "\
void f() {
    int count = 1;
    { int count = 2; }
}
";
    assert_eq!(codes_of(shadowing), []);
}
