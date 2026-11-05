//! **`return-type-mismatch`**: a `return` whose expression does not convert to the function's return type.
//!
//! ```cpp
//! int  count() { return "three"; }   // a string where an int is declared
//! int  value() { return; }           // nothing from a function that returns something
//! ```
//!
//! # The shapes, and which of them are reported
//!
//! ```text
//!   reported      an expression that does not convert to a non-void return type
//!   reported      a bare `return;` in a function whose return type is not `void`
//!   not reported  an expression in a `void` function — `return f();` where `f` returns `void` is legal, and legal
//!                 C++ is not this layer's business
//!   not reported  a function whose return type is `auto` — a deduction question, not a conversion one
//! ```
//!
//! # Why the conversion question is asked of [`Type::convertible_to`]
//!
//! Because that is the crate's single answer to "can a value of this type be used where that one is declared", and
//! a check with an opinion of its own would be a second answer that drifts from the first. It is `Known`-valued, so
//! a comparison the model cannot make — a type spelled in a header nobody read, a class with a conversion
//! constructor — reports **nothing** rather than guessing, which is the whole contract of this layer.
//!
//! # Only a function this file defines
//!
//! The body being checked is in this file, and so is its return type. Nothing here looks at another file.

use super::{Check, CheckContext, DiagnosticCode};
use cpp_parser::CppAstNode;

/// **`return-type-mismatch`** — see the module documentation.
pub struct ReturnTypeMismatch;

impl Check for ReturnTypeMismatch {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::ReturnTypeMismatch];

    fn check(context: &mut CheckContext<'_>) {
        // **This file's own functions** - see CheckContext::own_syntax_nodes.
        for function in context
            .own_syntax_nodes()
            .filter(super::is_a_function_definition)
            .collect::<Vec<_>>()
        {
            let Some(declared) = super::return_type_written_by(&function) else {
                continue;
            };
            // A placeholder is a deduction question, not a conversion one.
            if declared == "auto" || declared == "decltype(auto)" {
                continue;
            }

            for returned in super::the_returns_in(&function) {
                let expression = returned.get_expr();
                let returned_text = expression
                    .as_ref()
                    .map(|expression| cpp_parser::subtree_text_spaced(expression.syntax()))
                    .unwrap_or_default();

                match expression {
                    // `return;` — the statement is the keyword and the semicolon and nothing between them.
                    None => {
                        if declared != "void" {
                            context.add(
                                DiagnosticCode::ReturnTypeMismatch,
                                cpp_parser::source_range(returned.get_range()),
                                String::new(),
                                t!(
                                    "`return;` in a function declared to return `%{declared}`",
                                    declared = declared
                                ),
                            );
                        }
                    }
                    Some(_) if declared == "void" => {
                        // A value from a `void` function is legal when the expression is itself `void` — which is
                        // the common shape `return f();` — and this layer does not distinguish them.
                    }
                    Some(expression) => {
                        let crate::Known::Yes((from, _)) = context.type_of(expression.syntax())
                        else {
                            // The model could not tell what the expression is: a name from a header nobody read, an
                            // overload set, a deduction. Asking a conversion question about an unknown type is how a
                            // check invents a problem.
                            continue;
                        };
                        // **The shared facility**, the same one `param-type-check` and `assign-type-check` ask: it normalises
                        // the spelling (a return type written `const std::string&` is a reference, and `class_name()` answers
                        // `None` for one), asks the type relation first and the index only for the class question. Only
                        // `Refused` reports, so an unread class stays quiet — which is the rule the module's own note above
                        // is about.
                        let conversion =
                            crate::sema::convert::conversion(&from, &declared, |class| {
                                super::a_class_can_be_built_from(context, class, &from)
                            });
                        if !conversion.is_refused() {
                            continue;
                        }
                        context.add(
                            DiagnosticCode::ReturnTypeMismatch,
                            cpp_parser::source_range(expression.get_range()),
                            String::new(),
                            t!(
                                "`%{returned_text}` returns a value that does not convert to `%{declared}`, the type this function is declared to return", returned_text = returned_text, declared = declared
                            ),
                        );
                    }
                }
            }
        }
    }
}
