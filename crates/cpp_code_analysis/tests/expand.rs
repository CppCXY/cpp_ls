//! Phase B: macro expansion, and the origins that make it usable.
//!
//! Two questions run through every test here, and the second is the one that is easy to forget:
//!
//! * *What does this expand to?* — substitution, stringizing, pasting.
//! * *Where did it come from?* — a token produced by an expansion has to be distinguishable from one
//!   written in the file, or go-to-definition and diagnostics both point at the wrong place.

use cpp_code_analysis::{
    ExpandedToken, MacroTable, Origin, Token, expand, expand_with_budget,
    macros::MacroDef, preprocess::preprocess,
};
use cpp_parser::{CppParser, CppTokenKind, ParserConfig};

// ============================================================================
// Helpers
// ============================================================================

/// Lex a fragment of C++, keeping every token including whitespace.
///
/// **Lexed, not read out of the tree**, and that is the point rather than a shortcut. The tree does not
/// store trivia at all — the parser emits `MarkEvent::Trivia` for it and the tree builder drops it, so
/// whitespace never reaches the CST. Expansion needs it anyway: `F (1)` is not a call to `F`, and that
/// is a fact about the gap between two tokens.
///
/// A consumer does the same thing for the same reason: to expand a region it lexes the file and takes
/// the tokens in that range.
fn lex(source: &str) -> Vec<Token> {
    let mut errors = Vec::new();
    let mut lexer = cpp_parser::CppLexer::new(source, cpp_parser::LexerConfig::default(), &mut errors);

    lexer
        .tokenize()
        .into_iter()
        .map(|token| {
            Token::new(
                token.kind,
                &source[token.range.start_offset..token.range.end_offset()],
                token.range,
            )
        })
        .collect()
}

/// The significant tokens of a fragment: everything but whitespace, newlines and comments.
///
/// Trivia is dropped because *expansion does not depend on it* — the standard's phases remove comments
/// and splices before any of this, and whitespace only matters for the adjacency of a call's
/// parenthesis, which the lexer already recorded. Dropping it here also means the assertions can be
/// about the spelling that matters.
fn tokens_of(source: &str) -> Vec<Token> {
    lex(source)
        .into_iter()
        .filter(|token| !cpp_code_analysis::token::is_trivia(token.kind))
        .collect()
}

/// A macro table built from a source string full of `#define`s.
fn table(defines: &str) -> MacroTable {
    let tree = CppParser::parse(defines, ParserConfig::default());
    preprocess(&tree.get_red_root()).macros
}

/// Expand a use site against a set of definitions.
fn expand_use(defines: &str, use_site: &str) -> cpp_code_analysis::Expansion {
    let macros = table(defines);

    // **Lexed, not filtered**, so that the whitespace is still there when the expander asks whether a
    // `(` is adjacent to the name before it. Filtering it out first would make `F (1)` look like a call,
    // and the adjacency rule is a fact about the gap.
    expand(&lex(use_site), &macros)
}

/// The spelling of every **significant** token in an expansion, in order.
///
/// Whitespace is skipped because it is not what these tests are about: whether a separator survives is
/// asserted separately, by [`joined_reparses`](joined_reparses) and the origin tests.
fn text(expansion: &cpp_code_analysis::Expansion) -> Vec<String> {
    expansion
        .tokens
        .iter()
        .filter(|token| !cpp_code_analysis::token::is_trivia(token.kind()))
        .map(|token| token.text().to_string())
        .collect()
}

/// The significant spelling, joined — for the cases where the token boundaries are not the point.
///
/// `joined` **renders** the stream, separators included, rather than concatenating the spellings: the
/// separators are what make the output re-lexable, they encode the layout the argument had, and dropping
/// them here would hide the very bug they exist to prevent.
fn joined(expansion: &cpp_code_analysis::Expansion) -> String {
    rendered(expansion)
}

/// The whole output as source, separators included.
///
/// The property that matters for a consumer: an expansion is a token stream something else has to read.
fn rendered(expansion: &cpp_code_analysis::Expansion) -> String {
    let mut out = String::new();
    for token in &expansion.tokens {
        if token.space_before && !out.is_empty() {
            out.push(' ');
        }
        out.push_str(token.text());
    }
    out
}

/// One definition, for the tests that need the `MacroDef` itself.
fn definition(defines: &str, name: &str) -> MacroDef {
    table(defines)
        .get(name)
        .unwrap_or_else(|| panic!("{name} is defined"))
        .clone()
}

// ============================================================================
// Object-like macros
// ============================================================================

#[test]
fn an_object_like_macro_is_replaced_by_its_body() {
    let expansion = expand_use("#define VERSION 3\n", "VERSION");

    assert_eq!(joined(&expansion), "3");
    assert!(expansion.diagnostics.is_empty(), "{:?}", expansion.diagnostics);
}

/// An empty body expands to nothing. `#define FEATURE` is a feature flag, and the identifier simply
/// disappears — which is the correct behaviour and looks alarming until you know it.
#[test]
fn a_macro_with_an_empty_body_expands_to_nothing() {
    let expansion = expand_use("#define FEATURE\n", "FEATURE");

    assert!(expansion.is_empty(), "{:?}", text(&expansion));
}

/// A name that is not a macro is left alone, and *not* reported: most identifiers are not macros, and
/// a note for each would bury the ones that are.
#[test]
fn an_identifier_that_is_not_a_macro_is_left_alone() {
    let expansion = expand_use("#define A 1\n", "B");

    assert_eq!(joined(&expansion), "B");
    assert!(
        expansion.diagnostics.is_empty(),
        "not being a macro is not a note: {:?}",
        expansion.diagnostics
    );
}

#[test]
fn a_macro_body_containing_another_macro_is_expanded_too() {
    let expansion = expand_use("#define A 1\n#define B A\n", "B");

    assert_eq!(joined(&expansion), "1");
}

// ============================================================================
// Function-like macros
// ============================================================================

#[test]
fn a_function_like_macro_takes_arguments() {
    let expansion = expand_use(
        "#define MAX(a, b) ((a) > (b) ? (a) : (b))\n",
        "MAX(1, 2)",
    );

    assert_eq!(joined(&expansion), "((1) > (2) ? (1) : (2))");
}

/// **The adjacency rule.** `F (1)` is not a call: the tokens are `F`, `(`, `1`, `)`. A check on the
/// next *significant* token would expand it anyway and produce `(1)` where a compiler leaves `F (1)`.
#[test]
fn a_space_before_the_parenthesis_means_it_is_not_a_call() {
    let expansion = expand_use("#define F(x) (x)\n", "F (1)");

    assert_eq!(
        joined(&expansion),
        "F (1)",
        "the name is left as it is, and the rest is copied"
    );
    assert!(matches!(
        expansion.diagnostics.first().map(|it| &it.note),
        Some(cpp_code_analysis::ExpansionNote::NoArgumentList { .. })
    ));
}

/// A function-like name with no argument list at all is not a call either.
#[test]
fn a_function_like_macro_without_arguments_is_not_expanded() {
    let expansion = expand_use("#define F(x) (x)\n", "F + 1");

    assert_eq!(joined(&expansion), "F + 1");
    assert!(matches!(
        expansion.diagnostics.first().map(|it| &it.note),
        Some(cpp_code_analysis::ExpansionNote::NoArgumentList { .. })
    ));
}

/// A call that is still being typed has no closing `)`. The name is left unexpanded, which is the
/// least surprising thing to show while the user is mid-keystroke.
#[test]
fn an_unterminated_argument_list_is_not_expanded() {
    let expansion = expand_use("#define F(x) (x)\n", "F(1, 2");

    assert_eq!(joined(&expansion), "F(1, 2");
    assert!(matches!(
        expansion.diagnostics.first().map(|it| &it.note),
        Some(cpp_code_analysis::ExpansionNote::UnterminatedArgumentList { .. })
    ));
}

/// An argument containing a comma inside brackets is one argument, not two.
#[test]
fn a_comma_inside_brackets_does_not_split_an_argument() {
    let expansion = expand_use("#define ID(x) x\n", "ID((1, 2))");

    assert_eq!(joined(&expansion), "(1, 2)");
    assert!(expansion.diagnostics.is_empty(), "{:?}", expansion.diagnostics);
}

/// An argument that is a macro is expanded before it is substituted — the standard's prescan.
#[test]
fn an_argument_is_expanded_before_substitution() {
    let expansion = expand_use("#define INNER 7\n#define ID(x) x\n", "ID(INNER)");

    assert_eq!(joined(&expansion), "7");
}

/// The wrong number of arguments means the tokens are not a call to this macro. Expanding anyway would
/// invent code that is not there, so the name is left alone.
#[test]
fn the_wrong_number_of_arguments_is_not_expanded() {
    let expansion = expand_use("#define MAX(a, b) 0\n", "MAX(1)");

    assert_eq!(joined(&expansion), "MAX(1)");
    assert!(matches!(
        expansion.diagnostics.first().map(|it| &it.note),
        Some(cpp_code_analysis::ExpansionNote::WrongArgumentCount {
            expected: 2,
            found: 1,
            ..
        })
    ));
}

/// `F()` is a call with zero arguments, not one empty argument. The difference is observable for a
/// macro declared with no parameters.
#[test]
fn an_empty_argument_list_is_zero_arguments() {
    let expansion = expand_use("#define NOTHING() 42\n", "NOTHING()");

    assert_eq!(joined(&expansion), "42");
    assert!(expansion.diagnostics.is_empty(), "{:?}", expansion.diagnostics);
}

#[test]
fn a_variadic_macro_collects_the_rest_of_its_arguments() {
    let expansion = expand_use("#define SUM(f, ...) f(__VA_ARGS__)\n", "SUM(1, 2, 3)");

    assert_eq!(joined(&expansion), "1(2 , 3)");
}

/// A variadic macro called with nothing for `__VA_ARGS__` substitutes nothing. `LOG("x")` for
/// `#define LOG(f, ...)` is ordinary code.
#[test]
fn a_variadic_macro_with_no_extra_arguments_substitutes_nothing() {
    let expansion = expand_use("#define LOG(f, ...) f(__VA_ARGS__)\n", "LOG(\"x\")");

    assert_eq!(joined(&expansion), "\"x\"( )");
}

// ============================================================================
// Stringize and paste
// ============================================================================

#[test]
fn stringize_turns_an_argument_into_a_string_literal() {
    let expansion = expand_use("#define STR(x) #x\n", "STR(hello)");

    assert_eq!(text(&expansion), vec!["\"hello\""]);
    assert_eq!(expansion.tokens[0].kind(), CppTokenKind::StringLiteral);
}

/// The spelling, not the value: whitespace between the argument's tokens becomes one space, and the
/// quotes and backslashes inside it are escaped so the literal re-lexes to the same text.
#[test]
fn stringize_produces_the_spelling_of_the_argument() {
    let expansion = expand_use("#define STR(x) #x\n", "STR(a   +   b)");
    assert_eq!(joined(&expansion), "\"a + b\"");

    let expansion = expand_use("#define STR(x) #x\n", "STR(\"quoted\")");
    assert_eq!(joined(&expansion), "\"\\\"quoted\\\"\"");
}

/// Pasting joins two tokens and then **re-lexes**: `+` and `=` are `+=`, one token. Concatenating the
/// spelling without re-lexing would leave two tokens where the language has one.
#[test]
fn paste_re_lexes_the_result() {
    let expansion = expand_use("#define CAT(a, b) a ## b\n", "CAT(+, =)");

    assert_eq!(text(&expansion), vec!["+="]);
    assert_eq!(expansion.tokens[0].kind(), CppTokenKind::PlusAssign);
}

#[test]
fn paste_builds_a_longer_identifier() {
    let expansion = expand_use("#define GLUE(a, b) a ## b\n", "GLUE(foo, bar)");

    assert_eq!(text(&expansion), vec!["foobar"]);
    assert_eq!(expansion.tokens[0].kind(), CppTokenKind::Identifier);
}

#[test]
fn paste_builds_a_number() {
    let expansion = expand_use("#define NUM(a, b) a ## b\n", "NUM(1, 2)");

    assert_eq!(text(&expansion), vec!["12"]);
    assert_eq!(expansion.tokens[0].kind(), CppTokenKind::IntegerLiteral);
}

/// A paste that produces nothing readable keeps both halves rather than inventing a token or dropping
/// text. The standard leaves it undefined; keeping what was written is the least surprising answer.
#[test]
fn a_paste_that_produces_nothing_readable_keeps_both_halves() {
    let expansion = expand_use("#define BAD(a, b) a ## b\n", "BAD(+, *)");

    assert_eq!(joined(&expansion), "+ *");
}

/// Stringizing is applied to the argument as written, *not* after expanding it. `STR(INNER)` is
/// `"INNER"`, which is the whole reason `#` exists.
#[test]
fn stringize_does_not_expand_its_argument_first() {
    let expansion = expand_use("#define INNER 7\n#define STR(x) #x\n", "STR(INNER)");

    assert_eq!(joined(&expansion), "\"INNER\"");
}

// ============================================================================
// Origins
// ============================================================================

/// A token written in the file is `Origin::Source`, and a token from a macro body is not.
#[test]
fn a_token_from_a_macro_body_is_marked_as_expanded() {
    let expansion = expand_use("#define VERSION 3\n", "int x = VERSION;");

    let kinds: Vec<(String, bool)> = expansion
        .tokens
        .iter()
        .filter(|token| !cpp_code_analysis::token::is_trivia(token.kind()))
        .map(|token| (token.text().to_string(), token.is_expanded()))
        .collect();

    assert_eq!(
        kinds,
        vec![
            ("int".to_string(), false),
            ("x".to_string(), false),
            ("=".to_string(), false),
            ("3".to_string(), true),
            (";".to_string(), false),
        ]
    );
}

/// **The two ranges disagree on purpose, and both are needed.** A reader following a link wants the
/// macro; a reader fixing a problem wants the line they are looking at.
#[test]
fn navigation_reaches_the_macro_and_a_diagnostic_reaches_the_call_site() {
    let defines = "#define VERSION 3\n";
    let expansion = expand_use(defines, "int x = VERSION;");

    let expanded: &ExpandedToken = expansion
        .tokens
        .iter()
        .find(|token| token.is_expanded())
        .expect("an expanded token");

    // The macro's name is written in the `#define`, so navigation lands there…
    let definition_range = definition(defines, "VERSION").range;
    assert_eq!(expanded.navigation_range(), definition_range);

    // …while a diagnostic lands on the use, which is in the *use site* and not in the defines.
    let range = expanded.diagnostic_range();
    assert!(
        range.start_offset < "int x = VERSION;".len(),
        "the diagnostic range is in the use site: {range:?}"
    );
    assert!(
        range.start_offset >= "int x = ".len(),
        "and specifically on the macro's name: {range:?}"
    );
}

/// The call site covers the name *and* its arguments, because that is the extent a reader recognizes
/// as "the thing I wrote".
#[test]
fn the_call_site_covers_the_whole_invocation() {
    let expansion = expand_use("#define MAX(a, b) 0\n", "MAX(1, 2)");

    let range = match &expansion.tokens[0].origin {
        Origin::Expanded { invocations } => invocations
            .first()
            .expect("an invocation")
            .call_site,
        other => panic!("expected an expansion, got {other:?}"),
    };

    assert_eq!(range.start_offset, 0);
    assert_eq!(
        range.length,
        "MAX(1, 2)".len(),
        "the arguments are part of the call"
    );
}

/// A pasted token is marked as pasted, and reports against the call site — its own text exists in no
/// file, so there is nothing else it could report against.
#[test]
fn a_pasted_token_reports_against_the_call_site() {
    let expansion = expand_use("#define CAT(a, b) a ## b\n", "CAT(+, =)");

    assert!(matches!(expansion.tokens[0].origin, Origin::Pasted { .. }));
    assert!(expansion.tokens[0].is_expanded());
}

#[test]
fn a_stringized_token_is_marked_as_stringized() {
    let expansion = expand_use("#define STR(x) #x\n", "STR(hi)");

    assert!(matches!(
        expansion.tokens[0].origin,
        Origin::Stringized { .. }
    ));
}

/// An expansion *inside* an expansion reports against the **outermost** call whose text is on screen.
///
/// For `#define A 1` / `#define B A` used as `B`, the `1` that comes out was written in `A`'s body, and the
/// only text a reader can point at is this line — so the diagnostic lands on the `B`, not on the `A` three
/// bytes into the previous `#define`. Walking the chain one step at a time and reporting the *first* step
/// is what keeps the range inside the file the user is looking at.
#[test]
fn an_inner_expansion_reports_against_the_outer_call() {
    let expansion = expand_use("#define A 1\n#define B A\n", "B");

    let range = expansion.tokens[0].diagnostic_range();
    assert_eq!(
        range.start_offset, 0,
        "the outermost invocation, which is the `B` on this line"
    );
    assert_eq!(range.length, 1);
}

// ============================================================================
// Termination
// ============================================================================

/// `#define A A` must not loop. The standard's rule is the hide set — a macro is not expanded while it
/// is already being expanded — and this is that rule.
#[test]
fn a_self_referential_macro_terminates() {
    let expansion = expand_use("#define A A\n", "A");

    assert_eq!(joined(&expansion), "A");
    assert!(matches!(
        expansion.diagnostics.first().map(|it| &it.note),
        Some(cpp_code_analysis::ExpansionNote::Recursive { .. })
    ));
}

#[test]
fn mutually_recursive_macros_terminate() {
    let expansion = expand_use("#define A B\n#define B A\n", "A");

    assert!(
        expansion
            .diagnostics
            .iter()
            .any(|it| matches!(it.note, cpp_code_analysis::ExpansionNote::Recursive { .. })),
        "{:?}",
        expansion.diagnostics
    );
    assert!(expansion.tokens.len() < 16, "it stopped: {:?}", text(&expansion));
}

/// A macro that only *mentions* itself in a branch that is not taken still terminates, because the hide
/// set is about the expansion chain and not about the text.
#[test]
fn a_macro_that_mentions_itself_after_other_tokens_terminates() {
    let expansion = expand_use("#define A 1 + A\n", "A");

    assert!(
        expansion.tokens.len() < 16,
        "it stopped: {:?}",
        text(&expansion)
    );
}

/// The budget is a backstop for a stream that grows without the hide set catching it in time.
///
/// Asserted as "the cap held", not as "the budget ran out": whether a given chain exhausts the budget
/// before the hide set stops it is a detail of the chain, and pinning it would make the test a statement
/// about one spelling rather than about the mechanism.
#[test]
fn the_token_budget_stops_a_runaway_expansion() {
    let macros = table("#define A B B\n#define B C C\n#define C D D\n#define D E E\n#define E F F\n");
    let tokens = tokens_of("A");

    let expansion = expand_with_budget(&tokens, &macros, 20);

    assert!(
        expansion.tokens.len() <= 20,
        "the budget is an upper bound: {} tokens",
        expansion.tokens.len()
    );
}

// ============================================================================
// Robustness
// ============================================================================

/// Whatever the input, expansion terminates and does not panic.
#[test]
fn expansion_never_panics() {
    let defines = concat!(
        "#define EMPTY\n",
        "#define ONE 1\n",
        "#define F(x) (x)\n",
        "#define TWO(a, b) a b\n",
        "#define STR(x) #x\n",
        "#define CAT(a, b) a ## b\n",
        "#define VAR(f, ...) f(__VA_ARGS__)\n",
        "#define REC A\n",
        "#define A REC\n",
        "#define true 1\n",
    );
    let macros = table(defines);

    let cases = [
        "",
        "ONE",
        "ONE ONE ONE",
        "F",
        "F(",
        "F()",
        "F(1",
        "F(1)",
        "F((((1))))",
        "F(,)",
        "F(,,)",
        "TWO(1)",
        "TWO(1, 2, 3)",
        "STR()",
        "STR(#)",
        "STR(\"a\\b\")",
        "CAT(,)",
        "CAT(+, *)",
        "CAT(CAT(a, b), c)",
        "VAR()",
        "VAR(1)",
        "VAR(1, 2, 3)",
        "__VA_ARGS__",
        "true",
        "REC",
        "A",
        "EMPTY EMPTY EMPTY",
        "#",
        "##",
        "\\",
        "((((((((((",
        "\"unterminated",
        "'",
        "1'000",
        "\u{1F600}",
        "MAX(1, 2)",
    ];

    for source in cases {
        let tokens = tokens_of(source);
        let expansion = expand(&tokens, &macros);

        // Touch the results so the walk is part of what is being tested.
        for token in &expansion.tokens {
            let _ = token.diagnostic_range();
            let _ = token.navigation_range();
        }
    }
}

/// Bounded input produces bounded output, which is the property an editor depends on: a keystroke must
/// not be able to hang the process.
#[test]
fn expansion_work_is_bounded_by_the_budget() {
    // A chain that grows by two at each level: without a budget this is 2^n tokens.
    let defines = "#define A B B\n#define B C C\n#define C D D\n#define D E E\n#define E F F\n\
                   #define F G G\n#define G H H\n#define H I I\n#define I J J\n#define J K K\n";
    let macros = table(defines);
    let tokens = tokens_of("A");

    let expansion = expand(&tokens, &macros);

    assert!(
        expansion.tokens.len() <= cpp_code_analysis::expand::MAX_TOKENS,
        "the cap held: {} tokens",
        expansion.tokens.len()
    );
}

/// A macro named after a keyword expands. `#define true 1` is legal, and the lexer hands the name over
/// as a keyword token because it cannot know it is inside a directive.
#[test]
fn a_macro_named_after_a_keyword_expands() {
    let expansion = expand_use("#define true 1\n", "true");

    assert_eq!(joined(&expansion), "1");
}

/// Every token of an expansion carries a range inside the file it was written in, so a consumer can
/// always slice the document — even for a pasted token, whose range is its left half's.
#[test]
fn every_expanded_token_has_a_usable_range() {
    let defines = "#define VERSION 3\n#define CAT(a, b) a ## b\n#define STR(x) #x\n";
    let expansion = expand_use(defines, "VERSION CAT(+, =) STR(hi)");
    let use_site = "VERSION CAT(+, =) STR(hi)";

    for token in &expansion.tokens {
        let range = token.token.range;
        assert!(
            range.end_offset() <= use_site.len() || range.end_offset() <= defines.len(),
            "a token's range is inside one of the two files: {token:?}"
        );
    }
}

/// The expansion of a real-world macro. Asserted as a whole because the point is that all the pieces
/// work together, and a unit test per operator would not catch a substitution that breaks pasting.
#[test]
fn a_realistic_macro_expands_correctly() {
    let defines = "#define MAX(a, b) ((a) > (b) ? (a) : (b))\n";
    let expansion = expand_use(defines, "int m = MAX(x + 1, y);");

    assert_eq!(
        joined(&expansion),
        "int m = ((x + 1) > (y) ? (x + 1) : (y));"
    );
    assert!(expansion.diagnostics.is_empty(), "{:?}", expansion.diagnostics);

    // The `x` in the expansion came from the argument, which came from the file — and it is still
    // marked as expanded, because its *position in the output* is a macro body's.
    let expanded = expansion
        .tokens
        .iter()
        .filter(|token| token.is_expanded())
        .count();
    assert!(expanded > 8, "most of the expression came from the macro");
}


