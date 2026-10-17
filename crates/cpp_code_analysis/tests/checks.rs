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
/// **An arrow reaches through something this check cannot see.**
///
/// Measured on EmmyLuaCodeStyle: `_session->Send(std::move(message))` was reported as "`_session` has no member
/// `Send`" — the receiver is a `std::shared_ptr<Session>`, whose member list is `shared_ptr`'s, and the member the
/// program means is behind `operator->`. The guard says an arrow is not evidence; the second half says the check still
/// reports a `.` on a class whose member list really does not have the name, so the guard cannot become a blanket
/// silence.
#[test]
fn a_member_reached_through_an_arrow_is_not_judged_as_missing() {
    let through_an_arrow = "\
struct Session {
    void Run();
};
void g(Session* session) {
    session->Run();
    session->Missing();
}
";
    assert!(
        !codes_of(through_an_arrow).contains(&DiagnosticCode::UndefinedField),
        "the pointee's members are not this check's question: {:?}",
        codes_of(through_an_arrow)
    );

    let through_a_dot = "\
struct Session {
    void Run();
};
void g(Session session) {
    session.Missing();
}
";
    assert!(
        codes_of(through_a_dot).contains(&DiagnosticCode::UndefinedField),
        "and a `.` on a class whose member list has no such name is still reported: {:?}",
        codes_of(through_a_dot)
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
