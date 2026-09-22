//! Phase A: directives, macros, and conditions.
//!
//! Tests are organised by the question they answer rather than by the module they exercise, because
//! the modules are not independent: a `#define`'s meaning depends on which `#if` branch it is in, and
//! both depend on the tokens the parser produced.

use cpp_code_analysis::{
    Directive, DirectiveKind, FilePreprocessing, Include, IncludeForm, MacroValues, Token, Value,
    Visibility,
    directive::parse_directive_tokens,
    guard::{Branch, GuardStack},
    macros::MacroTable,
    preprocess::preprocess,
};
use cpp_parser::{CppParser, ParserConfig, source_range};

// ============================================================================
// Helpers
// ============================================================================

/// Preprocess a source string.
fn run(source: &str) -> FilePreprocessing {
    let tree = CppParser::parse(source, ParserConfig::default());
    assert_eq!(
        tree.to_source_text(),
        source,
        "the parse must stay lossless before anything is read out of it"
    );
    preprocess(&tree.get_red_root())
}

/// Every directive's kind, in order.
fn kinds(source: &str) -> Vec<DirectiveKind> {
    run(source)
        .directives
        .iter()
        .map(|spanned| spanned.directive.kind())
        .collect()
}

/// The first directive with this kind.
fn first(source: &str, kind: DirectiveKind) -> Directive {
    run(source)
        .directives
        .into_iter()
        .find(|spanned| spanned.directive.kind() == kind)
        .map(|spanned| spanned.directive)
        .unwrap_or_else(|| panic!("no {kind:?} directive in {source:?}"))
}

/// Parse a single directive written on its own line.
fn directive(text: &str) -> Directive {
    let tree = CppParser::parse(text, ParserConfig::default());
    let node = tree
        .get_red_root()
        .descendants()
        .find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind())
                == cpp_parser::CppSyntaxKind::PreprocessorDirective
        })
        .expect("a directive node");

    parse_directive_tokens(&cpp_code_analysis::token::tokens_of(&node), source_range(node.text_range()))
}

/// The macro a `#define` introduces.
fn macro_of(source: &str) -> cpp_code_analysis::MacroDef {
    match first(source, DirectiveKind::Define) {
        Directive::Define(define) => define
            .macro_def
            .unwrap_or_else(|| panic!("{source:?} defines something")),
        other => panic!("expected a define, got {other:?}"),
    }
}

// ============================================================================
// Reading a directive
// ============================================================================

#[test]
fn every_directive_kind_is_recognised() {
    assert_eq!(
        kinds(concat!(
            "#include <vector>\n",
            "#define A 1\n",
            "#undef A\n",
            "#if X\n",
            "#elif Y\n",
            "#else\n",
            "#endif\n",
            "#ifdef FOO\n",
            "#ifndef BAR\n",
            "#pragma once\n",
            "#error nope\n",
            "#warning careful\n",
            "#line 3\n",
        )),
        vec![
            DirectiveKind::Include,
            DirectiveKind::Define,
            DirectiveKind::Undef,
            DirectiveKind::If,
            DirectiveKind::Elif,
            DirectiveKind::Else,
            DirectiveKind::Endif,
            DirectiveKind::Ifdef,
            DirectiveKind::Ifndef,
            DirectiveKind::Pragma,
            DirectiveKind::Error,
            DirectiveKind::Warning,
            DirectiveKind::Line,
        ]
    );
}

/// A directive this layer does not know is not an error. Implementations add their own, and a
/// consumer still needs to see the line.
#[test]
fn an_unknown_directive_keeps_its_name() {
    match directive("#whatever 1 2\n") {
        Directive::Other { name } => assert_eq!(&*name, "whatever"),
        other => panic!("expected Other, got {other:?}"),
    }
}

/// A `#` alone on a line is legal and does nothing.
#[test]
fn a_null_directive_is_recognised() {
    assert_eq!(directive("#\n"), Directive::Null);
    assert_eq!(directive("#  \n"), Directive::Null);
}

/// A directive that is still being typed must not panic or invent arguments.
#[test]
fn a_truncated_directive_is_read_as_far_as_it_goes() {
    for text in [
        "#\n", "#if\n", "#ifdef\n", "#define\n", "#include\n", "#undef\n", "#pragma\n",
        "#error\n",
    ] {
        let tree = CppParser::parse(text, ParserConfig::default());
        assert_eq!(tree.to_source_text(), text, "{text:?}");
        let _ = preprocess(&tree.get_red_root());
    }
}

// ============================================================================
// #include
// ============================================================================

#[test]
fn an_angle_include_is_an_angle_include() {
    match first("#include <vector>\n", DirectiveKind::Include) {
        Directive::Include(Include {
            form, target, is_next, ..
        }) => {
            assert_eq!(form, IncludeForm::Angle);
            assert_eq!(&*target, "vector");
            assert!(!is_next);
        }
        other => panic!("expected an include, got {other:?}"),
    }
}

/// The form is the whole difference between the two spellings: a quoted header is searched next to the
/// including file first, an angled one is not.
#[test]
fn a_quoted_include_keeps_its_form() {
    match first("#include \"local.h\"\n", DirectiveKind::Include) {
        Directive::Include(Include { form, target, .. }) => {
            assert_eq!(form, IncludeForm::Quote);
            assert_eq!(&*target, "local.h");
        }
        other => panic!("expected an include, got {other:?}"),
    }
}

/// `#include HEADER` is well formed — its target is a macro. Calling it malformed would report an error
/// on correct code, and `#include BOOST_VERSION_HEADER` is not rare.
#[test]
fn an_include_of_a_macro_is_not_malformed() {
    match first("#include BOOST_HEADER\n", DirectiveKind::Include) {
        Directive::Include(Include { form, target, .. }) => {
            assert_eq!(form, IncludeForm::Macro);
            assert_eq!(&*target, "BOOST_HEADER");
        }
        other => panic!("expected an include, got {other:?}"),
    }
}

#[test]
fn include_next_is_distinguished_from_include() {
    let preprocessing = run("#include_next <stdio.h>\n");
    let directive = &preprocessing.directives[0].directive;

    assert_eq!(directive.kind(), DirectiveKind::IncludeNext);
    assert!(directive.as_include().expect("an include").is_next);
}

// ============================================================================
// #define
// ============================================================================

#[test]
fn an_object_like_macro_has_no_parameters() {
    let definition = macro_of("#define VERSION 3\n");

    assert_eq!(&*definition.name, "VERSION");
    assert_eq!(definition.params, None);
    assert!(!definition.is_function_like());
    assert_eq!(
        definition
            .body
            .significant()
            .map(|token| token.text())
            .collect::<Vec<_>>(),
        vec!["3"]
    );
}

#[test]
fn a_function_like_macro_has_parameters() {
    let definition = macro_of("#define MAX(a, b) ((a) > (b) ? (a) : (b))\n");

    assert_eq!(&*definition.name, "MAX");
    let params = definition.params.as_ref().expect("parameters");
    assert_eq!(
        params.iter().map(|p| &*p.name).collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert!(!definition.is_variadic());
    assert_eq!(definition.min_arguments(), 2);
    assert!(definition.accepts_argument_count(2));
    assert!(!definition.accepts_argument_count(1));
    assert!(!definition.accepts_argument_count(3));
}

/// **The whitespace decides.** `#define A (1)` defines an object-like macro whose body is `(1)`;
/// `#define A(x) x` defines a function-like one. Both spell a parenthesis after the name, and reading
/// the first as function-like would make `A` uncallable in ordinary code.
#[test]
fn the_space_before_the_parenthesis_decides_the_kind_of_macro() {
    let object_like = macro_of("#define A (1)\n");
    assert_eq!(object_like.params, None);
    assert!(!object_like.is_function_like());

    let function_like = macro_of("#define A(x) x\n");
    assert!(function_like.is_function_like());
    assert_eq!(function_like.min_arguments(), 1);
}

#[test]
fn a_variadic_macro_is_recognised() {
    let definition = macro_of("#define LOG(fmt, ...) printf(fmt, __VA_ARGS__)\n");

    assert!(definition.is_variadic());
    assert_eq!(definition.min_arguments(), 1);
    assert!(definition.accepts_argument_count(1));
    assert!(definition.accepts_argument_count(7));
    assert!(!definition.accepts_argument_count(0));
}

/// The GNU spelling `#define F(a...)` means the same as `#define F(a, ...)`.
#[test]
fn the_gnu_variadic_spelling_means_the_same_thing() {
    let definition = macro_of("#define F(a...)\n");

    assert!(definition.is_variadic());
    assert_eq!(definition.min_arguments(), 0);
}

/// `#define FOO` expands to nothing, which is a legal and common feature flag.
#[test]
fn a_macro_with_an_empty_body_is_recognised() {
    let definition = macro_of("#define FEATURE_ON\n");

    assert!(definition.body.is_empty());
    assert_eq!(definition.body.significant().count(), 0);
}

/// The two operators are recorded by position, because expansion cannot find them again from the text:
/// `#` stringizes only when a parameter follows, and `##` pastes tokens, not characters.
#[test]
fn stringize_and_paste_are_recorded() {
    let stringize = macro_of("#define STR(x) #x\n");
    assert_eq!(stringize.body.stringize.len(), 1);
    assert!(stringize.body.paste.is_empty());

    let paste = macro_of("#define CAT(a, b) a ## b\n");
    assert_eq!(paste.body.paste.len(), 1);
    assert!(paste.body.stringize.is_empty());
    assert!(paste.body.tokens[paste.body.paste[0]].is(cpp_parser::CppTokenKind::HashHash));
}

/// A `#` that is not followed by a parameter is not the stringize operator, and recording it as one
/// would make expansion of a macro a compiler rejects look like it worked.
#[test]
fn a_hash_that_does_not_stringize_is_not_recorded() {
    let definition = macro_of("#define HASHLIT(x) a # b\n");

    assert!(
        definition.body.stringize.is_empty(),
        "the `#` is not followed by a parameter: {:?}",
        definition.body
    );
}

/// A splice continues the directive, which is why the lexer gives it a kind of its own.
#[test]
fn a_line_splice_continues_a_macro_body() {
    let definition = macro_of("#define BOTH(a, b) \\\n    do { a; b; } while (0)\n");

    let text: Vec<&str> = definition
        .body
        .significant()
        .map(|token| token.text())
        .collect();

    assert_eq!(
        text,
        vec!["do", "{", "a", ";", "b", ";", "}", "while", "(", "0", ")"],
        "the spliced newline is gone and the body is whole"
    );
}

/// Comments are replaced by a space in translation phase 3, so they are not part of what a macro
/// expands to.
#[test]
fn a_comment_in_a_body_is_not_part_of_it() {
    let definition = macro_of("#define X 1 /* gone */ + 2\n");

    let text: Vec<&str> = definition
        .body
        .significant()
        .map(|token| token.text())
        .collect();
    assert_eq!(text, vec!["1", "+", "2"]);
}

// ============================================================================
// The macro table
// ============================================================================

#[test]
fn a_macro_is_visible_after_its_definition() {
    let preprocessing = run("#define FOO 1\nint x;\n");

    assert!(preprocessing.macros.is_defined("FOO"));
    assert!(!preprocessing.macros.is_defined("BAR"));
}

/// `#undef` shadows; it does not delete. A consumer asking about an earlier position still gets the
/// earlier answer.
#[test]
fn undef_shadows_without_deleting() {
    let preprocessing = run("#define FOO 1\n#undef FOO\n");

    assert!(!preprocessing.macros.is_defined("FOO"));

    let offset_inside_the_definition = 10;
    assert!(
        preprocessing
            .macros
            .is_defined_at("FOO", offset_inside_the_definition),
        "the macro was defined at that offset"
    );
}

/// A redefinition shadows, so `defined(FOO)` at the end sees the later one — but both are still in the
/// file's list, which is what a consumer drawing the file needs.
#[test]
fn a_redefinition_shadows_the_earlier_one() {
    let preprocessing = run("#define N 1\n#define N 2\n");

    let definition = preprocessing.macros.get("N").expect("defined");
    assert_eq!(
        definition
            .body
            .significant()
            .map(|token| token.text())
            .collect::<Vec<_>>(),
        vec!["2"]
    );
    assert_eq!(preprocessing.macros.len(), 1, "one name, not two");
}

#[test]
fn defined_names_are_sorted_and_deduplicated() {
    let preprocessing = run("#define B 1\n#define A 1\n#define B 2\n");

    assert_eq!(preprocessing.macros.defined_names(), vec!["A", "B"]);
}

// ============================================================================
// Conditions
// ============================================================================

/// A table that defines nothing.
fn no_macros() -> impl MacroValues {
    cpp_code_analysis::condition::NoMacros
}

/// Evaluate a condition written inside `#if ...`.
fn condition_value(source: &str, macros: &impl MacroValues) -> Value {
    let directive = first(source, DirectiveKind::If);
    match directive {
        Directive::Conditional { condition, .. } => cpp_code_analysis::evaluate(&condition, macros),
        other => panic!("expected a conditional, got {other:?}"),
    }
}

#[test]
fn arithmetic_follows_c_precedence() {
    let cases = [
        ("#if 1 + 2 * 3 == 7\n", true),
        ("#if (1 + 2) * 3 == 9\n", true),
        ("#if 1 << 4 == 16\n", true),
        ("#if 10 / 3 == 3\n", true),
        ("#if 10 % 3 == 1\n", true),
        ("#if 2 > 1 && 1 > 2\n", false),
        ("#if 2 > 1 || 1 > 2\n", true),
        ("#if !0\n", true),
        ("#if ~0 == -1\n", true),
        ("#if 1 ? 2 : 3\n", true),
        ("#if 0 ? 2 : 0\n", false),
    ];

    for (source, expected) in cases {
        assert_eq!(
            condition_value(source, &no_macros()).is_true(),
            Some(expected),
            "{source:?}"
        );
    }
}

/// Literals in all the bases a preprocessor accepts, including digit separators and suffixes.
#[test]
fn integer_literals_are_read_the_way_a_preprocessor_reads_them() {
    let cases = [
        ("#if 0x10 == 16\n", true),
        ("#if 010 == 8\n", true),
        ("#if 0b1010 == 10\n", true),
        ("#if 1'000 == 1000\n", true),
        ("#if 42ULL == 42\n", true),
    ];

    for (source, expected) in cases {
        assert_eq!(
            condition_value(source, &no_macros()).is_true(),
            Some(expected),
            "{source:?}"
        );
    }
}

/// **The rule the whole feature-flag idiom rests on.** An undefined identifier is `0`, not an error,
/// and `defined` is the way to ask whether it exists.
#[test]
fn an_undefined_identifier_is_zero_and_not_an_error() {
    assert_eq!(condition_value("#if FOO\n", &no_macros()), Value::Known(0));

    assert_eq!(
        condition_value("#if defined(FOO)\n", &no_macros()),
        Value::Known(0)
    );
    assert_eq!(
        condition_value("#if !defined(FOO)\n", &no_macros()),
        Value::Known(1)
    );
}

/// `defined X` without parentheses is legal.
#[test]
fn defined_works_without_parentheses() {
    let mut macros = MacroTable::new();
    macros.define(macro_of("#define FOO 1\n"));

    assert_eq!(
        condition_value("#if defined FOO\n", &macros),
        Value::Known(1)
    );
}

/// **`defined` does not expand its operand.** `#define FOO BAR` followed by `defined(FOO)` is true:
/// the question is whether `FOO` is defined, not what it expands to.
#[test]
fn defined_does_not_expand_its_operand() {
    let mut macros = MacroTable::new();
    macros.define(macro_of("#define FOO NOT_DEFINED_AT_ALL\n"));

    assert_eq!(
        condition_value("#if defined(FOO)\n", &macros),
        Value::Known(1)
    );
}

/// A macro whose body is a number is that number.
#[test]
fn a_numeric_macro_evaluates_to_its_value() {
    let mut macros = MacroTable::new();
    macros.define(macro_of("#define VERSION 3\n"));

    assert_eq!(
        condition_value("#if VERSION > 2\n", &macros),
        Value::Known(1)
    );
    assert_eq!(
        condition_value("#if VERSION > 3\n", &macros),
        Value::Known(0)
    );
}

/// **A macro whose value cannot be read is `Unknown`, not `false`.** This is the case that dominates
/// real code: `_MSC_VER` on a machine that is not MSVC is not "0", it is "not knowable here", and
/// answering `false` would grey out the Windows branch on every other platform.
#[test]
fn a_macro_with_an_unreadable_body_is_unknown() {
    let mut macros = MacroTable::new();
    macros.define(macro_of("#define _MSC_VER (1900 + 1)\n"));

    assert_eq!(condition_value("#if _MSC_VER > 1900\n", &macros), Value::Unknown);
    assert_eq!(
        condition_value("#if _MSC_VER > 1900\n", &macros).is_true(),
        None,
        "and `is_true` says so rather than guessing"
    );
}

/// `#if defined(X) && X > 2` is legal with `X` undefined precisely because `&&` short-circuits.
#[test]
fn logical_operators_short_circuit() {
    let mut macros = MacroTable::new();
    // `X` is defined but its value is unreadable, so the right side of `&&` is `Unknown` — and it must
    // not be consulted, because the left side already decided.
    macros.define(macro_of("#define X SOMETHING\n"));

    assert_eq!(
        condition_value("#if defined(NOPE) && X > 2\n", &macros),
        Value::Known(0),
        "the left side is false, so the right side is never evaluated"
    );
    assert_eq!(
        condition_value("#if defined(X) || X > 2\n", &macros),
        Value::Known(1),
        "the left side is true, so the right side is never evaluated"
    );
}

/// Division by zero has no value. `Unknown` has the same effect on a caller — the region cannot be
/// decided — without inventing one.
#[test]
fn division_by_zero_is_unknown_rather_than_a_panic() {
    assert_eq!(condition_value("#if 1 / 0\n", &no_macros()), Value::Unknown);
    assert_eq!(condition_value("#if 1 % 0\n", &no_macros()), Value::Unknown);
}

/// A condition that does not parse makes the region undecidable. It is the normal state of a line
/// being typed, so it is not reported as an error.
#[test]
fn a_condition_that_does_not_parse_is_unknown() {
    for source in ["#if\n", "#if 1 +\n", "#if (1\n", "#if defined(\n"] {
        assert_eq!(
            condition_value(source, &no_macros()),
            Value::Unknown,
            "{source:?}"
        );
    }
}

/// `true` and `false` are not keywords in a conditional: they are identifiers, and `0` unless a macro
/// says otherwise.
#[test]
fn true_and_false_are_identifiers_in_a_condition() {
    assert_eq!(condition_value("#if true\n", &no_macros()), Value::Known(0));

    let mut macros = MacroTable::new();
    macros.define(macro_of("#define true 1\n"));
    assert_eq!(condition_value("#if true\n", &macros), Value::Known(1));
}

/// **A macro name may be a keyword.** `#define true 1` and `#define private public` are legal and used
/// in real test suites, and the lexer cannot know it is inside a directive — so the name arrives as a
/// keyword token. Refusing it would lose the definition silently: the file still round-trips and the
/// directive is still in the tree, and the macro simply does not exist.
#[test]
fn a_macro_may_be_named_after_a_keyword() {
    let definition = macro_of("#define true 1\n");
    assert_eq!(&*definition.name, "true");

    let preprocessing = run("#define private public\n");
    assert!(
        preprocessing.macros.is_defined("private"),
        "the definition is in the table: {:?}",
        preprocessing.macros.defined_names()
    );
}

/// A name that is not a word at all is not a name. `#define 1 2` has nothing to define, and reading
/// `1` as the name would put a macro called `1` into the table.
#[test]
fn a_literal_is_not_a_macro_name() {
    let preprocessing = run("#define 1 2\n");

    assert!(
        preprocessing.macros.is_empty(),
        "nothing was defined: {:?}",
        preprocessing.macros.defined_names()
    );
    assert_eq!(
        preprocessing.directives[0].directive.kind(),
        DirectiveKind::Define,
        "but the directive is still a #define, and still in the tree"
    );
}

// ============================================================================
// Guards and visibility
// ============================================================================

/// Code at file scope is always compiled, and the answer stays cheap for it.
#[test]
fn code_at_file_scope_is_active() {
    let preprocessing = run("int x;\n");
    assert!(preprocessing.directives.is_empty());

    let guard = preprocessing.guard_at(0);
    assert!(guard.is_empty());
    assert_eq!(guard.visibility(&no_macros()), Visibility::Active);
}

/// `#if 0` is the one conditional whose answer is never in doubt, and code inside it is not compiled.
#[test]
fn code_inside_if_zero_is_inactive() {
    let preprocessing = run("#if 0\nint x;\n#endif\nint y;\n");
    let offset_of_x = 8;

    assert_eq!(
        preprocessing.visibility_at(offset_of_x),
        Visibility::Inactive
    );
}

/// A condition that cannot be decided makes the whole region undecidable, and `Unknown` is reported as
/// reachable but not diagnosable — which is what keeps completion offering both branches while neither
/// is called an error.
#[test]
fn an_undecidable_guard_is_reachable_but_not_diagnosable() {
    // `_MSC_VER` has to be *defined* for its value to be unreadable; an identifier that is not defined
    // at all is `0` by the standard, which is a decided answer. The unreadable case is a macro whose
    // body is not a number, which is what a real toolchain's headers produce.
    let preprocessing = run("#define _MSC_VER BUILD_NUMBER\n#if _MSC_VER > 1900\nint x;\n#endif\n");

    let offset_of_x = preprocessing
        .directives
        .iter()
        .find(|spanned| spanned.directive.kind() == DirectiveKind::Endif)
        .map(|spanned| spanned.range.start_offset)
        .expect("the #endif");

    let visibility = preprocessing.visibility_at(offset_of_x);

    assert_eq!(visibility, Visibility::Unknown);
    assert!(visibility.is_reachable(), "it might be compiled");
    assert!(
        !visibility.is_diagnosable(),
        "but do not report errors in it"
    );
}

/// An identifier that is not defined at all is `0`, so a condition over it is *decided* — the region is
/// not compiled, and it is not a case of not knowing.
#[test]
fn an_undefined_identifier_makes_a_condition_decided_not_unknown() {
    let preprocessing = run("#if _MSC_VER > 1900\nint x;\n#endif\n");

    let offset_of_x = preprocessing
        .directives
        .iter()
        .find(|spanned| spanned.directive.kind() == DirectiveKind::Endif)
        .map(|spanned| spanned.range.start_offset)
        .expect("the #endif");

    assert_eq!(
        preprocessing.visibility_at(offset_of_x),
        Visibility::Inactive,
        "0 > 1900 is false, and the standard says an undefined macro is 0"
    );
}

/// The two halves of an `#if`/`#else` are decided opposite ways once the configuration is known.
#[test]
fn the_two_branches_of_an_if_are_decided_oppositely() {
    let preprocessing = run("#if defined(WIN32)\nint win;\n#else\nint posix;\n#endif\n");

    let mut macros = MacroTable::new();
    macros.define(macro_of("#define WIN32 1\n"));

    let win_offset = preprocessing
        .directives
        .iter()
        .find(|spanned| spanned.directive.kind() == DirectiveKind::If)
        .map(|spanned| spanned.range.end_offset())
        .expect("the #if");

    let elsewhere = preprocessing
        .directives
        .iter()
        .find(|spanned| spanned.directive.kind() == DirectiveKind::Else)
        .map(|spanned| spanned.range.end_offset())
        .expect("the #else");

    // `guard_at` stops at the directive *starting* before the offset, so an offset just past the `#if`
    // line is inside the first branch and one past the `#else` line is inside the second.
    assert_eq!(
        preprocessing.guard_at(win_offset).visibility(&macros),
        Visibility::Active,
        "WIN32 is defined, so the first branch is compiled"
    );
    assert_eq!(
        preprocessing.guard_at(elsewhere).visibility(&macros),
        Visibility::Inactive,
        "and the second is not"
    );
}

/// The nesting is recorded, and an outer `#if 0` settles the region whatever an inner one says.
#[test]
fn an_outer_condition_settles_an_inner_one() {
    let preprocessing = run("#if 0\n#if UNDECIDABLE\nint x;\n#endif\n#endif\n");
    let offset_of_x = preprocessing
        .directives
        .iter()
        .find(|spanned| spanned.directive.kind() == DirectiveKind::Endif)
        .map(|spanned| spanned.range.start_offset)
        .expect("the inner #endif");

    assert_eq!(
        preprocessing.visibility_at(offset_of_x),
        Visibility::Inactive,
        "`#if 0` decided it, so the undecidable inner condition never matters"
    );
}

/// A guard reports its own depth, which is what a consumer needs to draw the structure of a file.
///
/// The offset asked about must be *past* a directive for it to be in force: `#if A` does not guard the
/// line it is written on.
#[test]
fn a_guard_reports_its_depth() {
    let preprocessing = run("#if A\n#if B\nint x;\n#endif\n#endif\n");

    let ends: Vec<usize> = preprocessing
        .directives
        .iter()
        .map(|spanned| spanned.range.end_offset())
        .collect();

    assert_eq!(preprocessing.guard_at(0).depth(), 0, "before anything");
    assert_eq!(preprocessing.guard_at(ends[0]).depth(), 1, "inside the outer region");
    assert_eq!(
        preprocessing.guard_at(ends[1]).depth(),
        2,
        "inside both regions"
    );
}

/// An `#if` that is never closed is the normal state of a file being edited, so it is reported as an
/// unclosed guard rather than as an error.
#[test]
fn an_unclosed_conditional_leaves_an_unclosed_guard() {
    let preprocessing = run("#if A\nint x;\n");

    assert!(
        !preprocessing.unclosed_guard.is_empty(),
        "the file ends inside a conditional"
    );
}

/// `#elif` picks up where the branches before it left off.
#[test]
fn an_elif_is_an_alternative_to_the_branches_before_it() {
    let mut stack = GuardStack::new();

    let branch = |kind: DirectiveKind| Branch {
        kind,
        tokens: Vec::new(),
        name: None,
        range: cpp_parser::SourceRange::new(0, 1),
    };

    stack.observe(DirectiveKind::If, branch(DirectiveKind::If));
    stack.observe(DirectiveKind::Elif, branch(DirectiveKind::Elif));

    let guard = stack.guard();
    assert_eq!(
        guard.depth(),
        1,
        "an #elif continues the region, it does not nest"
    );
    assert_eq!(guard.regions()[0].branches.len(), 2);
    assert_eq!(guard.regions()[0].active_branch, 1);
}

// ============================================================================
// Robustness
// ============================================================================

/// Whatever the input, preprocessing terminates and does not panic.
///
/// Directives are attacker-controlled text from the editor's point of view — they are whatever the
/// user has typed so far — so this belongs in a test rather than in an argument.
#[test]
fn preprocessing_never_panics() {
    let cases = [
        "#",
        "#\n",
        "##\n",
        "#define\n",
        "#define(\n",
        "#define A(\n",
        "#define A(\n",
        "#define A(,)\n",
        "#define A(a,\n",
        "#define A(a, ...)\n",
        "#define A(#)\n",
        "#define A(##)\n",
        "#define A(#x)\n",
        "#if\n",
        "#if #\n",
        "#if 0x\n",
        "#if 1/0\n",
        "#if ((((((((((\n",
        "#if defined\n",
        "#if defined()\n",
        "#if 'x'\n",
        "#if \"x\"\n",
        "#if ...\n",
        "#include\n",
        "#include <\n",
        "#include \"\n",
        "#include <a\n",
        "#include \"a\n",
        "#undef\n",
        "#else\n",
        "#endif\n",
        "#endif\n#endif\n#endif\n",
        "#if\n#else\n#else\n#endif\n",
        "#define \\\n",
        "#define A \\\n",
        "#if \\\n",
        "#pragma\n",
        "#error\n",
        "#error \"unterminated\n",
        "#line\n",
        "#line 999999999999999999999999\n",
        "#if 99999999999999999999999999\n",
        "#if 0b\n",
        "#if 0x\n",
        "#if 08\n",
        "#if 1 ? 2\n",
        "#if 1:2\n",
        "#if )\n",
        "#if ]\n",
    ];

    for source in cases {
        let preprocessing = run(source);
        // Touch the results, so the walk is part of what is being tested rather than elided.
        let _ = preprocessing.macros.len();
        let _ = preprocessing.visibility_at(0);
        let _ = preprocessing.guard_at(usize::MAX);
    }
}

/// Every token in a body carries a range that is inside the file it came from.
///
/// The invariant that keeps "go to definition" from pointing outside the document.
#[test]
fn every_token_points_inside_the_file() {
    let source = "#define MAX(a, b) ((a) > (b) ? (a) : (b))\n#define F(x, ...) #x __VA_ARGS__\n";
    let preprocessing = run(source);

    for spanned in &preprocessing.directives {
        let tokens: Vec<Token> = match &spanned.directive {
            Directive::Define(define) => define
                .macro_def
                .as_ref()
                .map(|definition| definition.body.tokens.clone())
                .unwrap_or_default(),
            Directive::Conditional { condition, .. } => condition.clone(),
            Directive::Pragma { tokens } => tokens.clone(),
            _ => Vec::new(),
        };

        for token in tokens {
            assert!(
                token.range.end_offset() <= source.len(),
                "a token's range is inside the file: {token:?}"
            );
        }
    }
}
