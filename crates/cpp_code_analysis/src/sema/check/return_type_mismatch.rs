//! **`return-type-mismatch`**: a `return` whose expression does not convert to the function's return type.
//!
//! ```cpp
//! int  count() { return "three"; }   // a string where an int is declared
//! void stop()  { return 1; }         // a value from a function that returns nothing
//! int  value() { return; }           // nothing from a function that returns something
//! ```
//!
//! # The three shapes, and which two are reported
//!
//! ```text
//!   reported      an expression that does not convert to a non-void return type
//!   reported      a bare `return;` in a function whose return type is not `void`
//!   not reported  an expression in a `void` function — `return f();` where `f` returns `void` is legal, and
//!                 legal C++ is not this layer's business
//! ```
//!
//! # Why the conversion question is asked of [`Type::convertible_to`]
//!
//! Because that is the crate's single answer to "can a value of this type be used where that one is declared", and
//! a check with its own opinion would be a second answer that drifts from the first. It is `Known`-valued, so a
//! comparison the model cannot make — a type spelled in a header nobody read — reports **nothing** rather than
//! guessing, which is the whole contract of this layer.
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
        for function in context
            .tree()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::FunctionDef)
        {
            // **The return type as it is written**, which is what `type_of` on the declaration's type node would
            // say — and taken from the tree rather than from a fact, because a fact's `type_of` for a function is
            // its *name's* type in some readings and its parameter list in others. See the sibling check's note on
            // `DeclFact::parameters` for what reading the wrong field cost.
            let Some(declared) = return_type_of(&function) else {
                continue;
            };
            if declared == "auto" || declared == "decltype(auto)" {
                // A placeholder is a deduction question, not a conversion one.
                continue;
            }

            for returned in function
                .descendants()
                .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ReturnStat)
            {
                // **The returned expression, if any**: `return;` has none, and that is its own answer.
                let expression = returned
                    .children()
                    .find(|child| CppSyntaxKind::from(child.kind()) != CppSyntaxKind::ReturnKeyword)
                    .filter(|child| !is_a_semicolon(child));

                match (expression, declared.as_str()) {
                    (None, "void") => {}
                    (None, _) => context.add(
                        DiagnosticCode::ReturnTypeMismatch,
                        cpp_parser::source_range(returned.text_range()),
                        String::new(),
                        format!("`return;` in a function declared to return `{declared}`"),
                    ),
                    (Some(_), "void") => {}
                    (Some(expression), _) => {
                        let known = context.type_of(&expression);
                        let crate::Known::Yes((from, _)) = known else {
                            // The model could not tell what the expression is — a name from a header nobody read,
                            // an overload set, a deduction. Asking a conversion question about an unknown type is
                            // how a check invents a problem.
                            continue;
                        };
                        if from
                            .convertible_to(&crate::sema::types::parse_type_spelling(&declared))
                            .is_yes()
                        {
                            continue;
                        }
                        // **Only a definite "no" is reported.** `convertible_to` answers `Known::Unknown` for a
                        // pair whose relation needs a conversion the model does not model, and a class with a
                        // constructor taking the argument is exactly that shape.
                        if !matches!(
                            from.convertible_to(&crate::sema::types::parse_type_spelling(&declared)),
                            crate::Known::No
                        ) {
                            continue;
                        }
                        context.add(
                            DiagnosticCode::ReturnTypeMismatch,
                            cpp_parser::source_range(expression.text_range()),
                            String::new(),
                            format!(
                                "this returns `{}` where the function is declared to return `{declared}`",
                                from.spelling()
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
/// `None` for a definition with no written return type, which a constructor has — and a constructor has no
/// `return` statements either, so nothing is lost.
fn return_type_of(function: &CppSyntaxNode) -> Option<String> {
    let parameters = function
        .children()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ParameterList)?;
    let head: Vec<CppSyntaxNode> = function
        .children()
        .take_while(|child| child != &parameters)
        .collect();
    // The name is the last declarator in the head, and the type is everything before it — a shape this reads as
    // text rather than as nodes, because the grammar's recovery around a declarator is exactly what makes a
    // node-shaped answer unreliable here.
    let name = head.last()?;
    let text = function.text().to_string();
    let before = text.get(..(name.text_range().start() - function.text_range().start()).into())?;
    let before = before.trim();
    // A trailing `*`, `&` or `const` belongs to the type.
    let before = before.trim_start_matches(['(', ' ']);
    if before.is_empty() {
        return None;
    }
    Some(before.to_string())
}

/// `;` is a child of some statements and not of others — a token, never an expression.
fn is_a_semicolon(node: &CppSyntaxNode) -> bool {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .all(|token| token.kind() == cpp_parser::CppTokenKind::Semicolon.into())
}
