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

    parse_directive_tokens(
        &cpp_code_analysis::token::tokens_of(&node),
        source_range(node.text_range()),
    )
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
        "#\n",
        "#if\n",
        "#ifdef\n",
        "#define\n",
        "#include\n",
        "#undef\n",
        "#pragma\n",
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
            form,
            target,
            is_next,
            ..
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

/// An angle include the parser did **not** fold into one token still names the right file.
///
/// The fallback reconstructs the target from the tokens between the delimiters, and it used to start at the
/// `<` itself — so the target came out `<bits/c++config.h`, which resolves to nothing, which means the file
/// containing it is never cached (`index::store` refuses to store a summary with an unresolved include) and
/// every declaration in it is invisible.
///
/// The fixture is spelled with a space between the `#` and the directive name, which is legal and which is what
/// keeps this path reachable: the fold is attempted only when the token right after the `#` is the directive
/// name, so `#  include <…>` always comes through here.
#[test]
fn an_unfolded_angle_include_names_the_file_between_its_delimiters() {
    // `c++config` is why the fold is worth a fallback at all: `++` splits the name into several tokens, and the
    // fold's rule for which tokens may appear inside one is a list of what cannot rather than of what can.
    match first("#  include <bits/c++config.h>\n", DirectiveKind::Include) {
        Directive::Include(Include {
            form,
            target,
            is_next,
            ..
        }) => {
            assert_eq!(form, IncludeForm::Angle);
            assert_eq!(&*target, "bits/c++config.h", "the delimiters are not part of the name");
            assert!(!is_next);
        }
        other => panic!("expected an include, got {other:?}"),
    }
}

/// The same, for a name made of tokens nothing folds: the reconstruction is the **text** between the
/// delimiters, and a space is a character a header name may contain.
#[test]
fn an_unfolded_angle_include_keeps_the_text_verbatim() {
    match first("#  include <sys/a-b.h>\n", DirectiveKind::Include) {
        Directive::Include(Include { target, .. }) => assert_eq!(&*target, "sys/a-b.h"),
        other => panic!("expected an include, got {other:?}"),
    }
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

/// A table that has read *part* of a translation unit: a name it does not mention may still be defined by a
/// file it has not seen, which is the state an index is always in. See [`cpp_code_analysis::Lookup`].
struct HalfKnown;

impl MacroValues for HalfKnown {
    fn lookup(&self, name: &str) -> cpp_code_analysis::Lookup<'_> {
        match name {
            // Defined by a `#define` this table read, whose body it did not keep.
            "HERE" => cpp_code_analysis::Lookup::DefinedWithoutAValue,
            // Explicitly `#undef`ed by a file this table read: a definite no.
            "GONE" => cpp_code_analysis::Lookup::Undefined,
            // Nothing this table read mentions it. Not the same answer.
            _ => cpp_code_analysis::Lookup::Unanswered,
        }
    }
}

/// A name nothing the table read mentions is **not** `0`: the standard's rule is about a complete input.
#[test]
fn a_name_the_table_cannot_speak_about_is_not_zero() {
    // This is the shape of the mistake the distinction exists to prevent: `#ifdef NT_INCLUDED` in a header,
    // where `NT_INCLUDED` is defined by nothing the index read. Answering `false` there greys out code that
    // compiles — and `#if defined(X)` or `#if X` would have done it silently, because before this the
    // evaluator had only "defined" and "not defined" to answer with.
    for condition in [
        "#if FOO\n",
        "#if defined(FOO)\n",
        "#if !defined(FOO)\n",
        "#if FOO == 0\n",
        "#if defined(FOO) && defined(HERE)\n",
        "#if !FOO\n",
        "#if FOO ? 1 : 2\n",
    ] {
        assert_eq!(
            condition_value(condition, &HalfKnown),
            Value::Unknown,
            "{condition:?} asks about a name this table cannot speak about"
        );
    }
}

/// The two answers such a table *can* give, so that "cannot speak about it" does not swallow them.
#[test]
fn a_table_that_read_part_of_the_input_still_answers_what_it_read() {
    assert_eq!(
        condition_value("#if defined(HERE)\n", &HalfKnown),
        Value::Known(1),
        "the body is not held, and `defined` does not need it"
    );
    assert_eq!(
        condition_value("#if HERE\n", &HalfKnown),
        Value::Unknown,
        "but a *value* cannot be read out of a body the table does not have — not even as `1`"
    );
    assert_eq!(
        condition_value("#if defined(GONE)\n", &HalfKnown),
        Value::Known(0),
        "an `#undef` this table read is a definite no"
    );
    assert_eq!(
        condition_value("#if GONE\n", &HalfKnown),
        Value::Known(0),
        "and the standard's `0` is right there, because the file that undefined it was read"
    );
    assert_eq!(
        condition_value("#if defined(HERE) && !defined(GONE)\n", &HalfKnown),
        Value::Known(1),
        "a condition over names the table read is decided, whatever it does not mention"
    );
}

/// `#ifdef` asks the same question as `defined`, so it has to answer it the same way.
#[test]
fn an_ifdef_on_a_name_the_table_cannot_speak_about_is_undecided() {
    let preprocessing = run("#ifdef FOO\nint x;\n#endif\n#if HERE\nint y;\n#endif\n");

    let visibility = |offset: usize| preprocessing.guard_at(offset).visibility(&HalfKnown);

    let at = |kind: DirectiveKind| {
        preprocessing
            .directives
            .iter()
            .find(|spanned| spanned.directive.kind() == kind)
            .map(|spanned| spanned.range.end_offset())
            .expect("the directive")
    };

    assert_eq!(
        visibility(at(DirectiveKind::Ifdef)),
        Visibility::Unknown,
        "`#ifdef FOO`: FOO may be defined by a file this table has not read"
    );
    assert_eq!(
        visibility(at(DirectiveKind::If)),
        Visibility::Unknown,
        "`#if HERE`: HERE is certainly a macro, and a condition with no readable value is unknown, not false"
    );
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

/// **A macro's body is an expression, and it is read as one** — and a body that is not an expression is `Unknown`,
/// not `false`.
///
/// The second half is the case that dominates real code: `_MSC_VER` on a machine that is not MSVC is not "0", it is
/// "not knowable here", and answering `false` would grey out the Windows branch on every other platform.
///
/// The first half is what used to be wrong (B97): a body that is not a **single integer literal** was `Unknown`, so
/// `#define _MSC_VER (1900 + 1)` — an expression, a name, an operator — answered nothing. The Windows and libstdc++
/// headers are full of those bodies, and `#if WINAPI_FAMILY_PARTITION (…)` could never be decided because of it.
#[test]
fn a_macros_body_is_read_as_an_expression() {
    let mut macros = MacroTable::new();
    macros.define(macro_of("#define _MSC_VER (1900 + 1)\n"));

    assert_eq!(
        condition_value("#if _MSC_VER > 1900\n", &macros),
        Value::Known(1),
        "`(1900 + 1) > 1900` is an expression, and a body is one too"
    );

    // A body that is **not** an expression stays `Unknown`: `do { } while (0)` is a statement, and no value can be
    // read out of it.
    let mut macros = MacroTable::new();
    macros.define(macro_of("#define ASSERT_ALL(x) do { } while (0)\n"));
    macros.define(macro_of("#define WEIRD int x\n"));

    assert_eq!(condition_value("#if WEIRD\n", &macros), Value::Unknown);
    assert_eq!(
        condition_value("#if WEIRD\n", &macros).is_true(),
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
    // The undecidable case has to be a body nothing can be read out of — **not** a name: an identifier that is not
    // defined is `0` by the standard, which is a decided answer, so `#define _MSC_VER BUILD_NUMBER` is `0 > 1900`,
    // and that is `Known(0)`. What really cannot be decided is a body that is not an expression at all.
    let preprocessing = run("#define _MSC_VER int x\n#if _MSC_VER > 1900\nint x;\n#endif\n");

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
    assert_eq!(
        preprocessing.guard_at(ends[0]).depth(),
        1,
        "inside the outer region"
    );
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

/// **`#else` does not end a region.** It continues the one it is in, so everything after it is at the same
/// depth as the `#if` — and a `#define` in an `#else` branch is inside a conditional, not at file scope.
///
/// Getting this wrong is one line and corrupts the depth of every directive after it in the file, which is
/// how a consumer asking "which `#if` is this in" ends up finding none.
#[test]
fn an_else_does_not_change_the_depth() {
    let sources = [
        "#if 1\n#define A\n#else\n#define B\n#endif\n",
        "#if 1\n#define A\n#elif 2\n#define B\n#else\n#define C\n#endif\n",
    ];

    for source in sources {
        let depths: Vec<(DirectiveKind, usize)> = run(source)
            .directives
            .iter()
            .map(|spanned| (spanned.directive.kind(), spanned.condition_depth))
            .collect();

        assert_eq!(
            depths[0],
            (DirectiveKind::If, 0),
            "the `#if` is at file scope: {depths:?}"
        );
        assert_eq!(
            depths[1],
            (DirectiveKind::Define, 1),
            "and so its body is one deeper: {depths:?}"
        );

        assert!(
            depths
                .iter()
                .filter(|(kind, _)| matches!(kind, DirectiveKind::Define))
                .all(|(_, depth)| *depth == 1),
            "every branch's body is at the same depth: {depths:?}"
        );

        assert_eq!(
            depths.last(),
            Some(&(DirectiveKind::Endif, 0)),
            "and the `#endif` closes the region: {depths:?}"
        );
    }
}

/// A region inside an `#else` nests from the same depth, so the depth still counts conditionals correctly.
///
/// `#else` reports the depth of the region's **body**, not of the `#if` line, because that is where it is
/// written: an `#else` line sits inside the region it belongs to, at the same depth as the `#define`s in
/// either arm. What it must not do is open another level — the `#if B` inside the `#else` is nested exactly
/// one deeper and not two, which is the mistake that would make every later directive in the file look as if
/// it were inside one more conditional than it is.
///
/// Each `#endif` reports the depth of the `#if` it *closes* — the depth that line was written at — so the
/// inner one reports `1` and the outer `0`. Reporting the depth after the pop would make the two `#endif`s in
/// a nested file look like a flat pair.
#[test]
fn nesting_inside_an_else_still_counts() {
    let depths: Vec<(DirectiveKind, usize)> =
        run("#if A\n#else\n#if B\n#define X\n#endif\n#endif\n")
            .directives
            .iter()
            .map(|spanned| (spanned.directive.kind(), spanned.condition_depth))
            .collect();

    assert_eq!(
        depths,
        vec![
            (DirectiveKind::If, 0),
            (DirectiveKind::Else, 1),
            (DirectiveKind::If, 1),
            (DirectiveKind::Define, 2),
            (DirectiveKind::Endif, 1),
            (DirectiveKind::Endif, 0),
        ]
    );
}

/// The depth a file's directives report is what `guard_at` rebuilds its regions from, so the two have to
/// agree about an `#else` — which is what the `#define` in its branch depends on.
#[test]
fn a_define_in_an_else_branch_is_inside_a_guard() {
    let preprocessing = run("#if 1\n#define A\n#else\n#define B\n#endif\n");

    let offset = preprocessing
        .directives
        .iter()
        .find(|spanned| {
            matches!(spanned.directive.kind(), DirectiveKind::Define)
                && spanned
                    .directive
                    .defines()
                    .is_some_and(|def| &*def.name == "B")
        })
        .map(|spanned| spanned.range.start_offset)
        .expect("the `#else` branch's define");

    let guard = preprocessing.guard_at(offset + 1);
    assert_eq!(
        guard.depth(),
        1,
        "it is inside the region, not at file scope"
    );
    assert!(
        matches!(
            guard.visibility(&preprocessing.macros_at(offset)),
            cpp_code_analysis::Visibility::Inactive
        ),
        "and the branch it is in is the one not taken"
    );
}

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

// ============================================================================
// What a condition does with the names it reads
// ============================================================================

/// The definitions a file writes, as the table a condition is evaluated against.
fn table_of(source: &str) -> MacroTable {
    run(source).macros
}

/// Evaluate one condition against a table, the way every caller does: lex it, then ask.
fn evaluate(condition: &str, macros: &impl MacroValues) -> Value {
    let mut errors = Vec::new();
    let mut lexer =
        cpp_parser::CppLexer::new(condition, cpp_parser::LexerConfig::default(), &mut errors);
    let tokens: Vec<Token> = lexer
        .tokenize()
        .into_iter()
        .filter(|token| !cpp_parser::is_trivia(token.kind))
        .map(|token| {
            let text = &condition[token.range.start_offset..token.range.end_offset()];
            Token::new(token.kind, text, token.range)
        })
        .collect();

    cpp_code_analysis::condition::evaluate(&tokens, macros)
}

/// **A condition expands the names it reads** — the question the whole Windows-header family turns on.
///
/// `#if WINAPI_FAMILY_PARTITION (WINAPI_PARTITION_APP)` is `((WINAPI_FAMILY & 0x2) == 0x2)`, and `WINAPI_FAMILY` is
/// a name that another `#define` gives a value to, written across two lines with a `\` splice. If the evaluator
/// only read literals, every one of those guards would be `Unknown` — and that is exactly the shape measured as the
/// blocker for `STDMETHOD` reaching `commdlg.h` (`docs/index-design.md` B96).
#[test]
fn a_condition_expands_a_name_whose_body_names_another() {
    // The plain nesting first: `A` is `B`, and `B` is `3`.
    let table = table_of("#define B 3\n#define A B\n");
    assert_eq!(evaluate("A", &table), Value::Known(3), "`A` is `B` is `3`");
    assert_eq!(evaluate("A + 1", &table), Value::Known(4), "and it is a value");

    // Then the splice, which is how `WINAPI_FAMILY_DESKTOP_APP` is written: the definition is one line to the
    // preprocessor, and the tokens after the splice are part of the same body.
    let table = table_of("#define PART_DESKTOP 0x1\n#define PART_APP 0x2\n#define DESKTOP (PART_DESKTOP \\\n | PART_APP)\n");
    assert_eq!(
        evaluate("DESKTOP", &table),
        Value::Known(3),
        "a body written across two lines is `0x1 | 0x2`"
    );

    // And the function-like macro that puts the two together — the real guard, in miniature.
    let table = table_of(
        "#define PART_DESKTOP 0x1\n\
         #define PART_APP 0x2\n\
         #define FAMILY (PART_DESKTOP \\\n | PART_APP)\n\
         #define PARTITION(v) ((FAMILY & v) == v)\n",
    );
    assert_eq!(
        evaluate("PARTITION ( PART_APP )", &table),
        Value::Known(1),
        "`(0x1 | 0x2) & 0x2` is `0x2`, so the partition holds"
    );
}
