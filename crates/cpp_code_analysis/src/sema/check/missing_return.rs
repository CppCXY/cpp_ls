//! **`missing-return`**: a function declared to return something, whose body never returns.
//!
//! ```cpp
//! int count() { }                    // nothing is returned, and the type says something must be
//! int other() { if (x) return 1; }   // …and here only one path does
//! ```
//!
//! # What is reported, and the one shape that is not
//!
//! ```text
//!   reported      a body with **no `return` statement at all** and a written return type that is not `void`
//!   not reported  `void f() { }`             nothing is expected
//!   not reported  `auto f() { … }`           a deduced return type, which this does not read
//!   not reported  a body whose last statement cannot fall through — a `throw`, a call to something that does not
//!                 return, an infinite loop. The last is not checked *syntactically* here: a body ending in
//!                 `while (true)` is recognised by its text, and anything more clever belongs in a control-flow
//!                 analysis rather than in a check that walks a tree.
//!   not reported  a **declaration**            `int f();` is a promise, not a body
//! ```
//!
//! # Why "no `return` at all" rather than "not every path returns"
//!
//! Because the second question is a control-flow question and this layer answers from a tree. A walk that tried to
//! decide whether every path returns would have to model `if`/`else`, `switch`, `goto`, exceptions and loops — and a
//! version of that which is *nearly* right reports functions that are correct, which is the failure this layer is
//! written against. The shape that is left is small, unambiguous, and the one a reader actually writes by accident:
//! a body that was never finished.
//!
//! # Only this file
//!
//! A function's body is in the file being checked, so this is a question about one file by construction.

use cpp_parser::CppSyntaxKind;

use super::{Check, CheckContext, DiagnosticCode};

/// **`missing-return`** — see the module documentation.
pub struct MissingReturn;

impl Check for MissingReturn {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::MissingReturn];

    fn check(context: &mut CheckContext<'_>) {
        // **This file's own functions**, typed - see `CheckContext::own_syntax_nodes`, which prunes the inlined
        // headers a rendering splices in. Collected before the loop because a finding needs `&mut context`.
        for function in super::function_definitions(context) {
            let Some(body) = super::body_of(&function) else {
                continue;
            };

            let Some(declared) = super::return_type_written_by(&function) else {
                continue;
            };
            if declared == "void" || declared == "auto" || declared == "decltype(auto)" {
                continue;
            }
            // A constructor has no return type and a destructor's is not written; both are `None` above. A
            // conversion operator (`operator int()`) writes a type but is not this question either — its body is
            // expected to `return`, so it lands on the reported side, which is right.

            let has_a_return = body
                .descendants()
                .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ReturnStat);
            if has_a_return {
                continue;
            }

            // **A body whose last statement cannot fall through.** Read as text, because "does this end in a
            // `throw` or in a loop that never ends" is a question about the last few tokens rather than about the
            // shape of a tree, and because a wrong "it cannot fall through" here is a *missed* finding rather than
            // an invented one.
            let text = body.text().to_string();
            let tail = text.trim_end_matches(['}', ' ', '\n', '\t', ';']);
            let cannot_fall_through = tail.ends_with("throw")
                || tail.contains("throw ")
                || tail.ends_with("while (true)")
                || tail.ends_with("while(true)")
                || tail.ends_with("for (;;)")
                || tail.ends_with("for(;;)");
            if cannot_fall_through {
                continue;
            }

            context.add(
                DiagnosticCode::MissingReturn,
                cpp_parser::source_range(function.text_range()),
                super::name_declared_by(&function).unwrap_or_default(),
                format!(
                    "this function is declared to return `{declared}` and its body has no `return`"
                ),
            );
        }
    }
}








