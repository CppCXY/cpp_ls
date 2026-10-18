//! `modernize-use-nullptr`, from clang-tidy: **`NULL` is a macro for `0`, and `0` is an `int`.**
//!
//! ```cpp
//! Widget* w = NULL;                        // this check says: nullptr
//! void f(Widget*);
//! f(NULL);                                 // …and picks the wrong overload where one exists
//! ```
//!
//! The bug the rule prevents is not stylistic: with `void f(int)` beside `void f(Widget*)`, `f(NULL)` calls the `int`
//! overload, because the macro expands to `0`. C++11's `nullptr` has the pointer type and does not.
//!
//! # How it is found
//!
//! Over the **expressions** whose text is exactly `NULL`. The spelling is the whole evidence: `NULL` is the standard
//! library's macro for `0`, and a project that has its own `NULL` has a different problem. Text rather than a resolved
//! macro on purpose — resolving it would answer "what does `NULL` expand to", which is `0` in every translation unit that
//! includes a standard header, and the finding is about the spelling a reader typed.

use cpp_parser::CppAstNode;

use super::{Check, CheckContext, DiagnosticCode};

pub struct UseNullptr;

impl Check for UseNullptr {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UseNullptr];

    fn check(context: &mut CheckContext<'_>) {
        for expression in context.own::<cpp_parser::CppExpr>().collect::<Vec<_>>() {
            if context.node_text(expression.syntax()) != "NULL" {
                continue;
            }
            let range = expression.syntax().text_range();
            let offset = usize::from(range.start());
            context.add(
                DiagnosticCode::UseNullptr,
                cpp_parser::source_range(range),
                "NULL".to_string(),
                format!(
                    "`NULL` is `0` in every standard header, so this picks an `int` overload where one exists — write \
                     `nullptr`, which has the pointer type.{}{}",
                    context
                        .source
                        .get(offset..usize::from(range.end()))
                        .map(|_| "")
                        .unwrap_or_default(),
                    ""
                ),
            );
        }
    }
}