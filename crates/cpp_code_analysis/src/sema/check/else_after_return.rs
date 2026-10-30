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
            // **The tree, typed.** The question is "does this branch return", and the parser's own AST answers it in one
            // query — `descendants::<CppReturnStat>()`. It used to be `node_text(..).contains("return")`, which is true of a
            // branch that only mentions the word: a comment, a string, a `return` inside a nested lambda, or a variable named
            // `returns`. The typed query cannot see any of those.
            if then_branch
                .descendants::<cpp_parser::CppReturnStat>()
                .next()
                .is_none()
            {
                continue;
            }

            // **A compile-time `if` keeps its `else`.** `if constexpr (…) A else B` and `if consteval A else B` are one
            // construct with two branches chosen by the compiler, and the chain form is how they are read: the
            // `else if constexpr` ladder of a type-dispatch function is a table, not nesting to be flattened, and
            // `if consteval { … } else { … }` is the shape the language gave that question. Dropping the `else` would
            // still compile and would read worse — which is the whole test this hint has to pass. Measured: five
            // findings on `constexpr.cpp`, `containers.cpp` and `templates.cpp` of the C++ corpus, none of them
            // something to act on.
            let opening: String = context
                .source
                .chars()
                .skip(usize::from(statement.syntax().text_range().start()))
                .take(32)
                .collect();
            let compile_time = opening
                .split_whitespace()
                .take(2)
                .any(|word| word == "constexpr" || word == "consteval");
            if compile_time {
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
