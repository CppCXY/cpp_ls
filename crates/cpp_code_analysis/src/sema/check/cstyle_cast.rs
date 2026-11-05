//! **`cstyle-cast`** — a cast written in the C style, `(Widget*)p`, where C++ has four named casts to say which conversion
//! is meant.
//!
//! # Why this reads the tree
//!
//! A C-style cast is a `CastExpr` that **opens with its `(`** and names a type: `(Widget*)p`. The named casts are different
//! shapes (`static_cast<…>(…)` opens with its keyword and carries a template-argument list), and a functional cast (`int(x)`)
//! is a call. So the question is structural, and it cannot produce a finding that is not a cast.
//!
//! Before this, the rule paired `(` with `)` in the file's raw text. That needed three separate defences against things that
//! look like a cast and are not — a preprocessor condition, a parameter list, a template-argument list — and each of those
//! defences was added because the census found the false positive it fixes (813, then 376, then 285 findings on one real
//! project). The tree has none of those shapes: a cast the preprocessor skipped is not in the tree, and a `(` in a parameter
//! list is a `ParameterList` node.
//!
//! # Why the spelling is the last thing computed
//!
//! Measured on a 4000-function file with five casts each: the rule took **35 ms** of a ~600 ms pull, and the two string
//! operations in it were the reason — the type's spelling was built for **every** cast (to test it against `void` and to fill
//! the message), and the opening token was found by trimming each token's text. `CppCastExpr::is_a_c_style_cast` and
//! `CppTypeId::is_void` are the typed questions, and the spelling is built once, after both, only when the finding is reported.
//!
//! The walk is `context.own::<CppCastExpr>()` — **this file's own text** — where `root.syntax().descendants()` also visited the
//! headers inlined into the rendering: a cast in `<string>` is not this file's code to report on.

use cpp_parser::CppAstNode;

use super::{Check, CheckContext, DiagnosticCode};

/// **`cstyle-cast`** — see the module documentation.
pub struct CStyleCast;

impl Check for CStyleCast {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::CStyleCast];

    fn check(context: &mut CheckContext<'_>) {
        // **Not a rule in C**, and the answer lives with the code rather than here: `DiagnosticCode::is_cpp_only` marks
        // this one, `run_check` skips it before it walks anything, and `CheckContext::push` would drop it anyway. A
        // second early return here would be a second place to forget when the next C++-only rule is added.
        for cast in context.own::<cpp_parser::CppCastExpr>().collect::<Vec<_>>() {
            // **The `(` is what makes it the C style**: a named cast opens with its keyword, and a functional cast is a call.
            if !cast.is_a_c_style_cast() {
                continue;
            }
            // **The `TypeId` is the type the cast names.** A named cast carries a template-argument list instead, and a
            // functional cast is a call — neither has one.
            let Some(type_id) = cast.get_type_id() else {
                continue;
            };
            // **A cast to `void` is not a conversion**, and the message would name a spelling that does not exist:
            // `(void)expression` is how C++ says "evaluate this and discard the result", and there is no
            // `static_cast<void>(…)` in the language this rule was borrowed from. Every C++ codebase writes it, including
            // the macro that made this finding on `preprocessor.cpp` of the C++ corpus.
            if type_id.is_void() {
                continue;
            }
            // **The spelling, once**, for the range that is reported and for the two sentences.
            let written = cpp_parser::subtree_text_spaced(type_id.syntax());
            let written = written.trim();
            if written.is_empty() {
                continue;
            }
            context.add(
                DiagnosticCode::CStyleCast,
                cpp_parser::source_range(cast.syntax().text_range()),
                t!("(%{written})", written = written),
                t!(
                    "this is a C-style cast to `%{written}` — the four named casts say which conversion is meant, and \
                     `static_cast<%{written}>(…)` is almost always the one",
                    written = written
                ),
            );
        }
    }
}
