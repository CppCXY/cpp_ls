//! `readability-else-after-return`, from clang-tidy: **an `else` whose `if` branch returns does not need to be an `else`.**
//!
//! ```cpp
//! if (!ready) return;      // the early return
//! else      { start(); }   // this check says: drop the `else` and unindent
//! ```
//!
//! Flat is easier to read than nested, and the early return exists precisely to make the rest of the function flat. The rule
//! is a hint because a deliberate symmetry (`if (x) return a; else return b;`) is a legitimate style.
//!
//! # How it is found
//!
//! Over an `if` whose then-branch **contains** a `return` and which has an `else`. Containment rather than "ends in" for the
//! first version: a branch with a return in it and an `else` after it is the shape the rule is about, and the stricter
//! reading (the *last* statement) is a refinement that can land without changing the finding's shape.

use cpp_parser::CppAstNode;

use super::{Check, CheckContext, DiagnosticCode};

pub struct ElseAfterReturn;

impl Check for ElseAfterReturn {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::ElseAfterReturn];

    fn check(context: &mut CheckContext<'_>) {
        for statement in context.own::<cpp_parser::CppIfStat>().collect::<Vec<_>>() {
            let Some(else_branch) = statement.get_else_branch() else {
                continue;
            };
            let Some(then_branch) = statement.get_then_branch() else {
                continue;
            };
            // **Text, not the tree.** `descendants()` is untyped in this parser, and the question here is a sentence's
            // shape: the branch says `return`. A `return` inside a nested lambda would be a false positive, which is the
            // refinement a typed walk would make; the finding's shape does not change.
            if !context.node_text(then_branch.syntax()).contains("return") {
                continue;
            }
            context.add(
                DiagnosticCode::ElseAfterReturn,
                cpp_parser::source_range(else_branch.syntax().text_range()),
                "else".to_string(),
                "this branch returns, so the `else` nests what does not need nesting — drop it and unindent".to_string(),
            );
        }
    }
}