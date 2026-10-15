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

use cpp_parser::{CppSyntaxKind, CppSyntaxNode};

use super::{Check, CheckContext, DiagnosticCode};

/// **`return-type-mismatch`** — see the module documentation.
pub struct ReturnTypeMismatch;

impl Check for ReturnTypeMismatch {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::ReturnTypeMismatch];

    fn check(context: &mut CheckContext<'_>) {
        // **This file's own functions** - see CheckContext::own_syntax_nodes.
        for function in context
            .own_syntax_nodes()
            .filter(|node| super::is_a_function_definition(node))
            .collect::<Vec<_>>()
        {
            let Some(declared) = return_type_of(&function) else {
                continue;
            };
            // A placeholder is a deduction question, not a conversion one.
            if declared == "auto" || declared == "decltype(auto)" {
                continue;
            }

            for returned in function
                .descendants()
                .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ReturnStat)
            {
                let returned_text = returned.text().to_string();
                let returned_text = returned_text.trim();
                let expression = returned
                    .children()
                    .find(|child| child.text().to_string().trim() != "return")
                    .filter(|child| child.text().to_string().trim() != ";");

                match expression {
                    // `return;` — the statement is the keyword and the semicolon and nothing between them.
                    None => {
                        if declared != "void" {
                            context.add(
                                DiagnosticCode::ReturnTypeMismatch,
                                cpp_parser::source_range(returned.text_range()),
                                String::new(),
                                format!("`return;` in a function declared to return `{declared}`"),
                            );
                        }
                    }
                    Some(expression) if declared == "void" => {
                        let _ = expression;
                        // A value from a `void` function is legal when the expression is itself `void` — which is
                        // the common shape `return f();` — and this layer does not distinguish them.
                    }
                    Some(expression) => {
                        let crate::Known::Yes((from, _)) = context.type_of(&expression) else {
                            // The model could not tell what the expression is: a name from a header nobody read, an
                            // overload set, a deduction. Asking a conversion question about an unknown type is how a
                            // check invents a problem.
                            continue;
                        };
                        if !from
                            .convertible_to(&crate::sema::types::parse_type_spelling(&declared))
                            .is_definitely_absent()
                        {
                            // `Yes` is fine; `Unknown` is a conversion this layer cannot see, which is not a
                            // conversion the program does not have.
                            continue;
                        }
                        context.add(
                            DiagnosticCode::ReturnTypeMismatch,
                            cpp_parser::source_range(expression.text_range()),
                            String::new(),
                            format!(
                                "`{returned_text}` returns a value that does not convert to `{declared}`, the type \
                                 this function is declared to return"
                            ),
                        );
                    }
                }
            }
        }
    }
}

/// The return type a function definition writes, as the text before its name.
///
/// Read as **text** rather than as nodes, because the grammar's recovery around a declarator is exactly what makes
/// a node-shaped answer unreliable here: `int* f()`, `const std::string& f()` and `auto f()` all have to come out
/// as the spelling the reader wrote.
///
/// `None` for a definition with nothing before its name, which a constructor has — and a constructor has no
/// `return` statements either, so nothing is lost.
fn return_type_of(function: &CppSyntaxNode) -> Option<String> {
    let parameters = function
        .children()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ParameterList)?;
    let name = function
        .children()
        .take_while(|child| child != &parameters)
        .last()?;

    let whole = function.text().to_string();
    let start: usize = function.text_range().start().into();
    let before_the_name: usize = name.text_range().start().into();
    let head = whole.get(..before_the_name.checked_sub(start)?)?.trim();

    // A pointer or reference written against the name — `int *f()` — belongs to the type.
    let head = head.trim_end_matches(['*', '&']).trim_end();
    if head.is_empty() {
        return None;
    }
    Some(head.to_string())
}
