//! The cooked token stream: what a compiler would parse.
//!
//! Every test here is about one of the three things cooking removes — the directives, the branches that are
//! not compiled, and the macro names — and about what a consumer can still ask afterwards. The shapes are
//! deliberately the ones a real header is written in rather than the smallest input that would compile.

use cpp_code_analysis::{CookedStream, Origin, cook};

/// Cook a source string, without a tree.
fn cooked(source: &str) -> CookedStream {
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    cook(source, &tokens)
}

/// The spellings of the cooked tokens, in order.
fn words(stream: &CookedStream) -> Vec<String> {
    stream
        .tokens
        .iter()
        .map(|token| token.token.text().to_string())
        .collect()
}

/// Cook and return the spellings joined by a space.
fn text(source: &str) -> String {
    cooked(source).spellings()
}

#[test]
fn an_object_like_macro_is_replaced_by_its_body() {
    assert_eq!(text("#define N 42\nint x = N;\n"), "int x = 42 ;");
}

#[test]
fn a_function_like_macro_takes_its_arguments() {
    assert_eq!(
        text("#define MAX(a, b) ((a) > (b) ? (a) : (b))\nint m = MAX(x, y);\n"),
        "int m = ( ( x ) > ( y ) ? ( x ) : ( y ) ) ;"
    );
}

#[test]
fn a_macro_argument_is_not_expanded_twice() {
    // `TWICE(f)(2)` is `f(2)` if the argument is substituted once — the classic reason expansion is a
    // rescan rather than a text substitution.
    assert_eq!(
        text("#define TWICE(f) f\nint x = TWICE(g)(2);\n"),
        "int x = g ( 2 ) ;"
    );
}

#[test]
fn the_instructions_to_the_processor_are_gone() {
    // Including the `#include`: a file's stream stops at its own tokens, and the included file is its own stream.
    // `#define`, `#if`/`#endif` and `#include` are instructions *to* the processor, so none of them survives into
    // what the processor produces.
    assert_eq!(text("#include <vector>\n#define A 1\nint x = A;\n"), "int x = 1 ;");

    // **`#pragma` is the exception, and it is not a special case** — it is the one directive that is a statement
    // about the program rather than an instruction about the text. Every compiler keeps it in `-E` output, and
    // `#pragma pack` changes the meaning of declarations after it, so dropping it would be reading a different
    // program. See `a_pragma_stays_in_the_stream_the_way_the_compiler_keeps_it` for the full rule and the
    // measurement against `cl.exe`.
    assert_eq!(text("#pragma once\nint x;\n"), "# pragma once int x ;");
}

#[test]
fn only_the_compiled_branch_survives() {
    assert_eq!(text("#if 0\nint no;\n#else\nint yes;\n#endif\n"), "int yes ;");
    assert_eq!(text("#if 1\nint yes;\n#else\nint no;\n#endif\n"), "int yes ;");
}

#[test]
fn an_elif_chain_takes_the_first_branch_that_holds() {
    let source = "#define B 2\n#if B == 1\nint one;\n#elif B == 2\nint two;\n#else\nint other;\n#endif\n";
    assert_eq!(text(source), "int two ;");
}

#[test]
fn a_define_in_a_branch_nobody_compiles_does_not_take_effect() {
    // The whole reason the walk keeps its own table: `preprocess` records every definition — a consumer
    // asking "where is this name defined" wants to see the ones that are not compiled too — but a cooked
    // stream is one configuration, and in that configuration this `#define` never ran.
    let source = "#if 0\n#define NOPE 1\n#endif\n#ifdef NOPE\nint wrong;\n#else\nint right;\n#endif\n";
    assert_eq!(text(source), "int right ;");
}

#[test]
fn a_define_in_a_compiled_branch_does_take_effect() {
    let source = "#if 1\n#define YES 1\n#endif\n#ifdef YES\nint right;\n#else\nint wrong;\n#endif\n";
    assert_eq!(text(source), "int right ;");
}

#[test]
fn a_conditional_inside_a_dead_branch_is_dead_too() {
    let source = "#if 0\n#if 1\nint never;\n#endif\nint also_never;\n#endif\nint here;\n";
    assert_eq!(text(source), "int here ;");
}

#[test]
fn the_dead_branch_is_reported_as_one_span() {
    // A consumer greys out a region, so the pieces of one dead branch have to join: the runs of tokens
    // between its directives and the directives themselves are one range.
    let source = "int a;\n#if 0\nint b;\n#define M 1\nint c;\n#endif\nint d;\n";
    let stream = cooked(source);
    assert_eq!(words(&stream), ["int", "a", ";", "int", "d", ";"]);

    assert_eq!(stream.inactive.len(), 1, "{:?}", stream.inactive);
    let dead = &stream.inactive[0];
    assert_eq!(
        &source[dead.start_offset..dead.end_offset()],
        "#if 0\nint b;\n#define M 1\nint c;\n#endif\n"
    );
}

#[test]
fn an_undef_stops_a_macro_from_applying() {
    let source = "#define N 1\n#undef N\nint x = N;\n";
    assert_eq!(text(source), "int x = N ;");
}

#[test]
fn a_definition_later_in_the_file_does_not_apply_earlier() {
    // Position is the whole reason the table is asked with an offset: this file has two meanings for one
    // name, and the first use must get the first one.
    let source = "#define N 1\nint a = N;\n#undef N\n#define N 2\nint b = N;\n";
    assert_eq!(text(source), "int a = 1 ; int b = 2 ;");
}

#[test]
fn stringize_and_paste_are_the_operators_and_not_text() {
    assert_eq!(text("#define S(x) #x\nchar* s = S(a b);\n"), "char * s = \"a b\" ;");
    assert_eq!(text("#define P(a, b) a##b\nint xy = P(x, y);\n"), "int xy = xy ;");
}

#[test]
fn a_token_from_a_macro_body_keeps_its_origin() {
    let stream = cooked("#define N 42\nint x = N;\n");
    let forty_two = stream
        .tokens
        .iter()
        .find(|token| token.token.text() == "42")
        .expect("the body's token is in the stream");

    match &forty_two.origin {
        Origin::Expanded { invocations } => {
            assert_eq!(invocations.len(), 1);
            assert_eq!(&*invocations[0].name, "N");
        }
        other => panic!("a token of a body must say which call produced it: {other:?}"),
    }

    // …and a token the user typed is still the file's own.
    let user_token = stream
        .tokens
        .iter()
        .find(|token| token.token.text() == "x")
        .expect("`x` is in the stream");
    assert_eq!(user_token.origin, Origin::Source);
}

#[test]
fn a_condition_nobody_can_decide_is_counted_and_treated_as_false() {
    // `#if SOMETHING` with no definition anywhere: C says an identifier that is not defined evaluates to
    // 0, so the `#else` is what is compiled — and the count says the stream is one possible configuration
    // rather than a certainty.
    let stream = cooked("#if SOMETHING\nint a;\n#else\nint b;\n#endif\n");
    assert_eq!(words(&stream), ["int", "b", ";"]);
    assert_eq!(stream.assumed_undefined, 1);
}

#[test]
fn a_file_with_nothing_to_cook_is_its_own_tokens() {
    // No directives, no macros: the cooked stream is the file's own tokens without trivia. This is the
    // level-0 case of the configuration ladder, and it must not need a toolchain to be right.
    let source = "struct S { int a; };\n";
    assert_eq!(text(source), "struct S { int a ; } ;");
    let stream = cooked(source);
    assert!(stream.inactive.is_empty());
    assert_eq!(stream.assumed_undefined, 0);
    assert!(stream.diagnostics.is_empty());
}

#[test]
fn cooking_is_idempotent_on_its_own_output() {
    // The cooked stream has no directives left, so cooking it again changes nothing. A stream that still
    // contained a `#` would fail this, which is the point of asserting it.
    let source = "#define N 1\n#if N\nint a = N;\n#endif\n";
    let once = cooked(source).spellings();
    let twice = text(&once);
    assert_eq!(once, twice);
}

// ============================================================================
// M3: the rendering, and the map back to the file
// ============================================================================

/// A macro table built from a string of `#define`s — the "configuration" a level-1 cook starts with.
fn defines(source: &str) -> cpp_code_analysis::MacroTable {
    let mut table = cpp_code_analysis::MacroTable::new();

    for line in source.lines() {
        let Some(definition) = line.strip_prefix("#define ") else {
            continue;
        };
        let (tokens, _) = cpp_parser::lex(definition, &cpp_parser::LexerConfig::default());
        let range = cpp_parser::SourceRange::new(0, definition.len());
        let tokens: Vec<cpp_code_analysis::Token> = tokens
            .iter()
            .map(|token| {
                cpp_code_analysis::Token::new(
                    token.kind,
                    &definition[token.range.start_offset..token.range.end_offset()],
                    token.range,
                )
            })
            .collect();

        if let Some(definition) =
            cpp_code_analysis::preprocess::macros::parse_define(&tokens, range)
        {
            table.define(definition);
        }
    }

    table
}

/// Parse a cooked stream's rendering, the way the real tree will.
fn parse_rendered(rendered: &cpp_code_analysis::RenderedCooked) -> cpp_parser::CppSyntaxTree {
    cpp_parser::CppParser::parse(&rendered.text, cpp_parser::ParserConfig::default())
}

/// How many tokens of a tree spell `word` — a query that needs no accessor and cannot be fooled by shape.
fn count_of(tree: &cpp_parser::CppSyntaxTree, word: &str) -> usize {
    tree.get_red_root()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.text() == word)
        .count()
}

#[test]
fn the_raw_reading_reads_every_branch_and_the_cooked_one_reads_the_compiled_branch() {
    // **What M3 buys that no shape rule can.** A conditional is not a parse problem: the raw tree is a
    // well-formed reading of a file that has *both* members in it, and no rule can tell it otherwise — the
    // answer is in the macro table, not in the grammar. The cooked stream answers it before the parser runs,
    // so the tree describes one configuration, which is what a compiler's tree describes and what every
    // query above it is really asking about.
    let source = "struct S {\n#if FEATURE\n  int only_when_on;\n#else\n  int only_when_off;\n#endif\n};\n";

    let raw = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());
    assert_eq!(raw.get_errors(), [], "both branches are readable as text");
    assert_eq!(count_of(&raw, "only_when_on"), 1);
    assert_eq!(count_of(&raw, "only_when_off"), 1);

    // `FEATURE` is not defined, so C's rule applies and the `#else` branch is the compiled one.
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let cooked = cook(source, &tokens);
    let rendered = cooked.render();
    let tree = parse_rendered(&rendered);

    assert_eq!(tree.get_errors(), []);
    assert_eq!(count_of(&tree, "only_when_on"), 0);
    assert_eq!(count_of(&tree, "only_when_off"), 1);
    assert_eq!(count_of(&tree, "S"), 1, "the class itself is still there");

    // …and the same file with the feature on is one line of configuration away. `FEATURE` is defined as `1`
    // and not as an empty body on purpose: `#if FEATURE` after a replacement that leaves nothing is an
    // expression with no expression in it, which C does not define — GCC rejects it, we treat it as
    // not-taken, and neither is a fact about *this* file.
    let cooked = cpp_code_analysis::cook_with(
        source,
        tokens.as_slice(),
        &defines("#define FEATURE 1\n"),
    );
    let tree = parse_rendered(&cooked.render());
    assert_eq!(count_of(&tree, "only_when_on"), 1);
    assert_eq!(count_of(&tree, "only_when_off"), 0);
}

#[test]
fn the_annotation_families_read_the_same_way_cooked_as_they_do_raw() {
    // The families the shape rules were written for. All four read in the *raw* tree today,
    // because those rules work — so this asserts **parity**: cooking must not read worse than the rules do,
    // and it must read them without knowing a single macro name. When the level-2 census shows the rules are
    // no longer needed, this test is what says cooking still covers them.
    let sal = "#define _Check_return_wat_\n#define _Check_return_opt_\n#define _Success_(x)\n#define _In_\n#define _ACRTIMP __declspec(dllimport)\n";
    let nodiscard = "#define _NODISCARD [[nodiscard]]\n";

    let cases: &[(&str, &str)] = &[
        (
            "_Check_return_wat_\n_Success_(return == 0)\n_ACRTIMP errno_t __cdecl fopen_s(FILE** _Stream);\n",
            sal,
        ),
        ("_ACRTIMP int __cdecl _rmtmp(void);\n", sal),
        (
            "_Check_return_opt_\n_ACRTIMP int __cdecl __stdio_common_vfwprintf(\n    _In_ unsigned __int64 _Options,\n    FILE* _Stream\n    );\n",
            sal,
        ),
        ("_NODISCARD int f(void);\n", nodiscard),
    ];

    for (source, configuration) in cases {
        let raw = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());

        let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
        let cooked =
            cpp_code_analysis::cook_with(source, tokens.as_slice(), &defines(configuration));
        let rendered = cooked.render();
        let tree = parse_rendered(&rendered);

        assert_eq!(
            tree.get_errors(),
            raw.get_errors(),
            "cooked and raw must agree about {source:?}; rendered as {:?}",
            rendered.text
        );
    }
}
#[test]
fn a_pragma_stays_in_the_stream_the_way_the_compiler_keeps_it() {
    // **A `#pragma` is part of the program, not an instruction that disappears.** Every compiler this server models
    // keeps pragmas in its preprocessed output — `cl -E` prints `#pragma once`, `#pragma region` and `#pragma pack`,
    // clang and gcc the same — so a cooker that consumed them was reading a different program from the one compiled.
    //
    // Measured against `cl.exe` on `#include <sal.h>` before this: our stream was 38 tokens and the compiler's 42, and
    // **every one of the four remaining differences was a `#`** (`sal.h:13`, `sal.h:707`, `sal.h:1471`,
    // `concurrencysal.h:18`). With the `#` kept as well, that file matches exactly: 42 and 42, no differences.
    let source = "#pragma once\n#pragma pack(push, 1)\n#pragma region Name\nint x;\n#pragma endregion Name\n";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let cooked = cook(source, &tokens);

    // The `#`, the word `pragma`, and the arguments — nothing else, and nothing missing.
    assert_eq!(
        cooked.spellings(),
        "# pragma once # pragma pack ( push , 1 ) # pragma region Name int x ; # pragma endregion Name",
        "a pragma survives with the arguments that give it meaning: `pack(push, 1)` is not the same pragma as `pack`"
    );

    // **No trivia**, which is the half that took a second attempt: the first version pushed the directive's whole
    // span, so the newline after each pragma and any comment on it became tokens, and the count went *up* on a fix
    // meant to make the two streams agree.
    assert!(
        !cooked.spellings().contains('\n'),
        "whitespace stays out of the stream: {:?}",
        cooked.spellings()
    );

    // And a pragma in a **region nobody compiles** is not in the stream, because it is not in the program — the same
    // rule every other directive follows.
    let source = "#if 0\n#pragma pack(1)\n#endif\nint y;\n";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let cooked = cook(source, &tokens);
    assert_eq!(
        cooked.spellings(),
        "int y ;",
        "a pragma in a dead branch is not compiled and does not appear"
    );
}

#[test]
fn an_empty_body_leaves_nothing_behind_and_the_declaration_still_reads() {
    // `_NODISCARD` is the case that needed a shape rule on the raw side: an empty-bodied macro read as a type
    // name. Cooked, it is simply not there — and nothing downstream has to know it ever was.
    let source = "_NODISCARD int f(void);\n";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let cooked =
        cpp_code_analysis::cook_with(source, &tokens, &defines("#define _NODISCARD\n"));

    assert_eq!(cooked.spellings(), "int f ( void ) ;");
    assert_eq!(parse_rendered(&cooked.render()).get_errors(), []);
}

#[test]
fn a_token_out_of_a_body_maps_back_to_the_define() {
    let source = "#define API __declspec(dllexport)\nAPI int f(void);\n";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let rendered = cook(source, &tokens).render();

    // `__declspec` in the rendering came out of the `#define` on line 1, so that is where a consumer must send
    // the reader — not to line 2, where the invocation is.
    let offset = rendered
        .text
        .find("__declspec")
        .expect("it is in the rendering");
    let written = rendered.written_at(offset).expect("it was written somewhere");
    assert_eq!(
        &source[written.start_offset..written.end_offset()],
        "__declspec"
    );
    assert!(
        written.start_offset < source.find("API int").unwrap(),
        "the body's own position is inside the `#define`, before the call site"
    );

    // …and a token the file wrote maps to itself.
    let offset = rendered.text.find("int").expect("it is in the rendering");
    let written = rendered.written_at(offset).expect("it was written somewhere");
    assert_eq!(&source[written.start_offset..written.end_offset()], "int");
}

#[test]
fn a_pasted_token_maps_to_the_call_site() {
    // `xy` exists nowhere in the file, so no range spells it. The expander records the call site for exactly
    // this reason, and the map reports that.
    let source = "#define P(a, b) a##b\nint xy = P(x, y);\n";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let rendered = cook(source, &tokens).render();

    let offset = rendered.text.rfind("xy").expect("the pasted token");
    let written = rendered.written_at(offset).expect("the call site is known");
    let at = &source[written.start_offset..written.end_offset()];
    assert!(
        written.start_offset >= source.find("P(x").unwrap(),
        "a pasted token points at the call site it was joined at, not at the argument text: {at:?}"
    );
}

#[test]
fn a_node_maps_to_the_span_its_tokens_cover() {
    let source = "#define U unsigned\nU int a;\nU int b;\n";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let rendered = cook(source, &tokens).render();
    let tree = parse_rendered(&rendered);

    let declaration = tree
        .get_red_root()
        .descendants()
        .find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::Declaration
        })
        .expect("a declaration is in the rendered tree");

    let range = cpp_parser::source_range(declaration.text_range());
    let written = rendered.written_span(range).expect("it came from somewhere");
    assert!(
        &source[written.start_offset..written.end_offset()].contains("int a"),
        "the node's span covers its own tokens: {:?}",
        &source[written.start_offset..written.end_offset()]
    );
}

// ============================================================================
// Level 2: the macros a file's includes contribute
// ============================================================================

/// An environment built by hand: `(name, function_like, body)` as an included header would supply it.
fn environment(entries: &[(&str, bool, &str)]) -> cpp_parser::MacroEnvironment {
    cpp_parser::MacroEnvironment::from_included_macros(entries.iter().map(
        |(name, function_like, body)| {
            cpp_parser::IncludedMacro::defined_with_body(
                0,
                name,
                *function_like,
                cpp_parser::MacroBody::Unknown,
                Some(body),
            )
        },
    ))
}

#[test]
fn a_macro_from_an_included_header_is_expanded_by_the_cooked_stream() {
    // What level 2 buys: `_STD_BEGIN` is `namespace std {` in `yvals_core.h`, so the class below is inside
    // `std` — a fact that is nowhere in this file. Cooked, it is simply there.
    let source = "_STD_BEGIN\nclass W { int a; };\n_STD_END\n";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let configuration = cpp_code_analysis::configuration_from_environment(&environment(&[
        ("_STD_BEGIN", false, "namespace std {"),
        ("_STD_END", false, "}"),
    ]));

    assert_eq!(configuration.function_like_without_parameters, 0);
    assert_eq!(configuration.without_a_body, 0);
    assert_eq!(configuration.unreadable, 0);

    let rendered = cpp_code_analysis::cook_with(source, &tokens, &configuration.table).render();
    assert!(
        rendered.text.starts_with("namespace std {"),
        "the body of the included macro is what the parser sees: {:?}",
        rendered.text
    );
    assert_eq!(parse_rendered(&rendered).get_errors(), []);
}

#[test]
fn a_function_like_definition_without_its_parameters_is_counted_and_not_guessed() {
    // Substitution is by parameter name, and the evidence the index keeps does not carry the parameter list.
    // Expanding `_Success_` anyway would leave `x` standing where the argument belongs — a wrong tree rather
    // than a missing one — so it is counted and left alone. That count is the size of the gap.
    let configuration = cpp_code_analysis::configuration_from_environment(&environment(&[
        ("_Success_", true, "((void)0)"),
        ("_In_", false, ""),
    ]));

    assert_eq!(configuration.function_like_without_parameters, 1);
    assert_eq!(
        configuration.table.defined_names(),
        ["_In_"],
        "the object-like one is in the table and the function-like one is not"
    );
}

#[test]
fn a_definition_with_no_body_stored_is_counted_separately() {
    let environment = cpp_parser::MacroEnvironment::from_included_macros([
        cpp_parser::IncludedMacro::defined_with_body(
            0,
            "NO_BODY",
            false,
            cpp_parser::MacroBody::Unknown,
            None,
        ),
    ]);
    let configuration = cpp_code_analysis::configuration_from_environment(&environment);

    assert_eq!(configuration.without_a_body, 1);
    assert!(configuration.table.defined_names().is_empty());
}

#[test]
fn an_object_like_body_in_force_is_used_by_default_and_can_be_turned_off() {
    // The in-force channel is where MSVC's `_ACRTIMP` arrives from (`corecrt.h` defines it inside a conditional
    // region), so a body there is unusable only because nobody said whether the macro takes parameters. A caller
    // that knows — the index does, from `MacroFact::function_like` — can hand it over as object-like, and then
    // the declaration reads.
    let environment = cpp_parser::MacroEnvironment::from_included_macros([])
        .with_bodies_in_force([
            (
                Box::from("_ACRTIMP"),
                Some(false),
                Box::from("__declspec(dllimport)"),
            ),
            (Box::from("_Success_"), Some(true), Box::from("((void)0)")),
            (Box::from("_Unknown_"), None, Box::from("int")),
        ]);

    let source = "_ACRTIMP int f(void);\n";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());

    // **By default the object-like one is used** — the switch is level 2, and the measurement that made it the
    // default is on [`configuration_from_environment_with`]. The function-like one and the unclassified one are
    // not: the count says how many were left behind.
    let default = cpp_code_analysis::configuration_from_environment(&environment);
    assert_eq!(default.in_force_without_a_parameter_list, 2);
    let rendered = cpp_code_analysis::cook_with(source, &tokens, &default.table).render();
    assert!(
        rendered.text.starts_with("__declspec ( dllimport ) int f"),
        "{:?}",
        rendered.text
    );

    // …and the conservative reading — "a body is not a definition", so nothing from that channel is used — is
    // asked for explicitly.
    let conservative = cpp_code_analysis::configuration_from_environment_with(&environment, false);
    assert_eq!(conservative.in_force_without_a_parameter_list, 3);
    let rendered = cpp_code_analysis::cook_with(source, &tokens, &conservative.table).render();
    assert!(rendered.text.starts_with("_ACRTIMP"), "{:?}", rendered.text);
}

/// **A cook over two layers answers like a cook over the two merged.**
/// The layered starting state (`Over`, and `FileMacros` behind it) replaced a per-file table build and a per-file
/// `extend_from`: the bindings are the same objects either way, and only the *order* decides what wins. That order
/// is the one the flat table had by construction — the newer layer was inserted later, and the lookup takes the
/// last binding in force — so this test pins it down on the one thing that is easy to get backwards: a builtin the
/// compilation defines, and a header's definition of the same name.
#[test]
fn a_cook_over_two_layers_answers_like_the_two_merged() {
    let seed = defines("#define MY_API 1\n#define ONLY_IN_THE_SEED 2\n#define LATE(x) seed_##x\n");
    let environment = defines("#define MY_API 3\n");

    // The two readings being compared: layered, and merged the way the table used to be built.
    let mut merged = seed.clone();
    merged.extend_from(&environment);

    let source = "MY_API + ONLY_IN_THE_SEED + LATE(1)\n";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());

    let layered = cpp_code_analysis::Over::new(&environment, &seed);
    let from_layers = cpp_code_analysis::cook_with(source, &tokens, &layered).render();
    let from_the_merge = cpp_code_analysis::cook_with(source, &tokens, &merged).render();

    assert_eq!(from_layers.text, from_the_merge.text);
    assert_eq!(
        from_layers.text.trim(),
        "3 + 2 + seed_1",
        "the newer layer's `MY_API` wins, the seed answers for what the newer layer never mentions, and both \
         directions expand"
    );
}
/// **A fact whose range is in no file is dropped and counted, never left pointing into a rendering.**
///
/// The mapping answers for the *file*, and a fact it cannot place has to go: a declaration whose range is an
/// offset in the rendering is a declaration that hover, a diagnostic and a jump would all put in the wrong place
/// — with nothing to report the mistake, because the number is a plausible number. So the fact is dropped, the
/// drop is counted, and the caller can say how much of a reading it could use.
#[test]
fn a_fact_that_cannot_be_placed_is_dropped_and_counted() {
    let source = "int declared_here;\n";
    let mut summary = cpp_code_analysis::summarize(
        std::path::Path::new("/p/a.h"),
        source,
        cpp_code_analysis::SummaryKey::new(0, 0),
    );
    assert_eq!(summary.declarations.len(), 1, "the fixture declares one thing");

    // A rendering with **no tokens at all**: every range in the summary is unplaceable, which is what a rendering
    // whose stream is empty looks like from the map's side.
    let empty = cpp_code_analysis::RenderedCooked::default();
    let report = summary.map_into_the_file(&empty);

    assert!(
        summary.declarations.is_empty(),
        "nothing is left pointing into a rendering"
    );
    assert_eq!(report.placed, 0);
    assert_eq!(
        report.dropped, 2,
        "the declaration's range and its name's range: the report counts ranges"
    );
}