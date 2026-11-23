//! **A condition that cannot be decided on one side is still decided by the other.**
//!
//! `#if A || B` is true when `B` is true whatever `A` turns out to be, and `#if A && B` is false when `B` is false
//! whatever `A` turns out to be. Answering `Unknown` for the whole expression is *not* the conservative choice: the
//! cook treats an undecidable region as not compiled, so a definition inside it is lost, and every `#if` in the wild
//! that tests a compiler builtin next to a version check — hedley's
//!
//! ```c
//! #if (JSON_HEDLEY_HAS_BUILTIN(__builtin_expect_with_probability) && !defined(JSON_HEDLEY_PGI_VERSION)) \
//!     || JSON_HEDLEY_GCC_VERSION_CHECK(9,0,0) || JSON_HEDLEY_MCST_LCC_VERSION_CHECK(1,25,10)
//! ```
//!
//! — sent the walk down the branch the compiler does not take. Measured on nlohmann/json: `JSON_HEDLEY_LIKELY` and
//! `JSON_HEDLEY_UNLIKELY` were never defined, 95 of its 126 test files reported syntax errors at every `CHECK` in a
//! `TEST_CASE`, and no amount of parser work could have fixed them (`docs/worklog.md`).
//!
//! `__has_builtin(never_a_builtin)` is the unknown side here: the cook has no table of builtins, so it answers
//! `Unknown`, which is the truth — and it is the same answer the compiler's own builtins get.

use cpp_toolchain::macros::MacroTable;
use cpp_toolchain::preprocess::cooked::cook_with;

/// The significant tokens of a cooked text — what a reader would call "what survived".
fn cooked(source: &str) -> String {
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    cook_with(source, &tokens, &nothing_defined).render().text
}

/// **`Unknown || 1` is 1** — the right side settles it, and a declaration in that branch survives.
#[test]
fn an_unknown_or_a_known_true_takes_the_branch() {
    let text = cooked(
        "#if __has_builtin(never_a_builtin) || 1\nint taken;\n#else\nint skipped;\n#endif\n",
    );
    assert!(text.contains("taken"), "{text}");
    assert!(!text.contains("skipped"), "{text}");
}

/// **`Unknown && 0` is 0** — the same rule with the other operator.
#[test]
fn an_unknown_and_a_known_false_does_not_take_the_branch() {
    let text = cooked(
        "#if __has_builtin(never_a_builtin) && 0\nint taken;\n#else\nint skipped;\n#endif\n",
    );
    assert!(!text.contains("taken"), "{text}");
    assert!(text.contains("skipped"), "{text}");
}

/// **Two unknowns stay unknown**, and the `#else` branch is the one a reader sees — the conservatism that was there
/// before is kept where nothing settles the answer. It is a policy, not a proof: the region *may* be compiled, and the
/// cook says so by leaving it out rather than by guessing a value for a builtin it has no table of.
#[test]
fn unknown_with_unknown_takes_the_else_branch() {
    let text = cooked(
        "#if __has_builtin(never_a_builtin) && __has_builtin(also_never_a_builtin)\nint taken;\n#else\nint skipped;\n#endif\n",
    );
    assert!(!text.contains("taken"), "{text}");
    assert!(text.contains("skipped"), "{text}");
}

/// **The hedley shape**, which is what the rule was written for: a builtin question, a version check as a known
/// truth, and the definitions after it.
#[test]
fn the_hedley_shape_defines_what_it_guards() {
    let text = cooked(
        "#define HAS_BUILTIN(x) __has_builtin(x)\n\
         #define GNUC_AT_LEAST(a,b,c) ((16 * 1000000) >= ((a) * 1000000) + ((b) * 1000) + (c))\n\
         #if HAS_BUILTIN(__builtin_expect_with_probability) || GNUC_AT_LEAST(9,0,0)\n\
         #define LIKELY(x) __builtin_expect(!!(x), 1)\n\
         #endif\n\
         int probe = LIKELY(1);\n",
    );
    assert!(text.contains("__builtin_expect"), "{text}");
    assert!(!text.contains("LIKELY("), "{text}");
}

/// **A probe turns the unknown into the compiler's answer.**
///
/// The tests above state what a cook with no compiler does — `Unknown`, so the else branch. This is the other half:
/// when a probe answers `__has_builtin(__builtin_expect)`, the `#if` takes the branch the compiler takes, without any
/// table in the preprocessor claiming to know what a compiler supports.
#[test]
fn a_probe_answer_decides_the_branch() {
    use cpp_toolchain::FeatureProbe;
    use cpp_toolchain::preprocess::cooked::cook_with_file_and_features;

    struct Answers;
    impl FeatureProbe for Answers {
        fn answer(&self, operator: &str, operand: &str) -> Option<i128> {
            (operator == "__has_builtin" && operand == "__builtin_expect").then_some(1)
        }
    }

    let source = "#if __has_builtin(__builtin_expect)\nint taken;\n#else\nint skipped;\n#endif\n";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    let text = cook_with_file_and_features(source, &tokens, &nothing_defined, None, Some(&Answers))
        .render()
        .text;

    assert!(text.contains("taken"), "{text}");
    assert!(!text.contains("skipped"), "{text}");
}
