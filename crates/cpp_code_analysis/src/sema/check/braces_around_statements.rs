//! `readability-braces-around-statements`, from clang-tidy: **a body that is one statement should be a block.**
//!
//! ```cpp
//! if (ready) start();            // this check says: { start(); }
//! if (ready) { start(); }        // …and this is what it wants
//! ```
//!
//! The rule the C++ world argues about, and the reason it is worth a diagnostic rather than a taste: `if (a) if (b) f();
//! else g();` binds the `else` to the *inner* `if`, which is the shape of more real bugs than any other one-line style
//! question. clang-tidy's name is kept so a reader can look the rule up.
//!
//! # How it is found
//!
//! Over an `if` whose then-branch text does not begin with `{`. The `for`/`while`/`do`/`else` bodies are the same question
//! and the same walk, and the first version covers the `if` because that is the one the dangling-`else` bug lives in.

use cpp_parser::CppAstNode;

use super::{Check, CheckContext, DiagnosticCode};

pub struct BracesAroundStatements;

impl Check for BracesAroundStatements {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::BracesAroundStatements];

    fn check(context: &mut CheckContext<'_>) {
        for statement in context.own::<cpp_parser::CppIfStat>().collect::<Vec<_>>() {
            let Some(then_branch) = statement.get_then_branch() else {
                continue;
            };
            let written = then_branch.syntax().text().to_string();
            if written.trim_start().starts_with('{') {
                continue;
            }
            context.add(
                DiagnosticCode::BracesAroundStatements,
                cpp_parser::source_range(then_branch.syntax().text_range()),
                written.trim().to_string(),
                "a body that is one statement should be a block — `if (…) { … }` — because `if (a) if (b) f(); else g();` \
                 binds the `else` to the inner `if`"
                    .to_string(),
            );
        }
    }
}