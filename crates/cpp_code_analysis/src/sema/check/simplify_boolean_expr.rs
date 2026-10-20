//! `readability-simplify-boolean-expr`, from clang-tidy: **a comparison against `true` is the value itself.**
//!
//! ```cpp
//! if (ready == true)  start();    // this check says: if (ready)
//! if (ready == false) stop();     // …and: if (!ready)
//! return ready ? true : false;    // …and: return ready;
//! ```
//!
//! A judgement rather than a law — the explicit comparison is sometimes the clearer sentence — which is why it is a hint.
//! It is worth a rule because the *inverted* form (`if (!ready == false)`) is the shape of real confusion.
//!
//! # How it is found
//!
//! Over an `if`'s condition whose text ends in `== true` or `== false` (and the mirrored `true ==`). Text rather than a
//! parsed comparison on purpose: the shape is the spelling, and a spelling this simple needs no tree to read. A condition
//! with a comment inside it, or split across lines, is still the same sentence to a reader — the check reads the condition's
//! text with whitespace collapsed by `trim` only at the ends, and reports the whole condition.

use cpp_parser::CppAstNode;

use super::{Check, CheckContext, DiagnosticCode};

pub struct SimplifyBooleanExpr;

impl Check for SimplifyBooleanExpr {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::SimplifyBooleanExpr];

    fn check(context: &mut CheckContext<'_>) {
        for statement in context.own::<cpp_parser::CppIfStat>().collect::<Vec<_>>() {
            let Some(condition) = statement.get_condition() else {
                continue;
            };
            let written = context.node_text(condition.syntax());
            let against_a_literal = ["== true", "== false", "true ==", "false =="]
                .iter()
                .any(|spelling| written.contains(spelling));
            if !against_a_literal {
                continue;
            }
            context.add(
                DiagnosticCode::SimplifyBooleanExpr,
                cpp_parser::source_range(condition.syntax().text_range()),
                written.to_string(),
                format!(
                    "`{written}` compares against a literal that is already the value — write the expression, or its negation: `if ({})` or `if (!({}))`",
                    written
                        .replace("== true", "")
                        .replace("== false", "")
                        .trim(),
                    written.replace("== true", "").replace("== false", "").trim()
                ),
            );
        }
    }
}
