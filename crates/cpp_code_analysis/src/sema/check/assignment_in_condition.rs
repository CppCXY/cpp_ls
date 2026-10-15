//! **`assignment-in-condition`**: `=` where the reader almost certainly meant `==`.
//!
//! ```cpp
//! if (x = 1) { … }          // assigns, and the condition is then 1 — always true
//! while (line = read()) { } // …and this one is deliberate, which is why the rule is about the *shape* below
//! ```
//!
//! # The shape, and the one that is excused
//!
//! ```text
//!   reported      `if (x = 1)`, `while (x = y)`, `for (; x = y;)`   an assignment directly in the condition
//!   not reported  `if ((x = 1))`            the extra parentheses are the idiom that says "I meant this", and
//!                                           every compiler warns about it in the same way — a second pair is a
//!                                           deliberate choice rather than a typo
//!   not reported  `if (x == 1)`             a comparison
//!   not reported  `if (f(x = 1))`           the assignment is an argument, not the condition
//!   not reported  `if (x = y, x > 0)`       the condition is the comma expression, whose *last* operand is what is
//!                                           tested — and that operand is not an assignment
//! ```
//!
//! # Read as text, and why that is the honest reading here
//!
//! The condition of an `if` is taken between its first `(` and the matching `)`, and the question is whether that
//! text holds a top-level `=`. A tree walk would have to decide which of the recovered nodes is the condition —
//! and a condition with a mismatched parenthesis is exactly where the grammar recovers hardest. The text is what
//! the reader wrote, and the question is about what they wrote.

use cpp_parser::CppAstNode;

use super::{Check, CheckContext, DiagnosticCode};

/// **`assignment-in-condition`** — see the module documentation.
pub struct AssignmentInCondition;

/// **The condition of an `if`/`while`, taken from the node and never from its text.**
///
/// This was a text scan — the first `(` and its match — and that is exactly how a real session got a false positive:
/// `if (arg == "--port" && i + 1 < argc) { const auto message = readMessage(); … }` was reported as *an assignment
/// in a condition*, with the body's own `=` in the message. Counting parentheses through text means deciding what a
/// `<` is, and a `<` is a comparison far more often than it is a bracket.
///
/// A node's child is what the parser decided the condition is, and this takes **the one expression before the
/// body**: no scanning, no ambiguity. A condition written as `(x = y)` arrives as a parenthesised expression, whose
/// text is then trimmed — the trimming is safe because the node is already the whole condition.
#[allow(dead_code)]
fn condition_of(node: &cpp_parser::CppSyntaxNode) -> Option<cpp_parser::CppSyntaxNode> {
    // The body is the one child that is never the condition, and it is the only one that has to be named: the
    // keywords are **tokens**, so `children()` does not offer them at all.
    node.children().find(|child| {
        cpp_parser::CppSyntaxKind::from(child.kind()) != cpp_parser::CppSyntaxKind::CompoundStat
    })
}

impl Check for AssignmentInCondition {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::AssignmentInCondition];

    fn check(context: &mut CheckContext<'_>) {
        // **One walk of this file's own nodes**, dispatched by the sum type rather than by two kind comparisons: the
        // two statements this check is about both hold their condition in a `CppParenExpr`, and asking for that is
        // the parser's answer rather than this layer's guess. Collected before the loop because a finding needs
        // `&mut context` while the iterator holds `&context`.
        for node in context.own::<cpp_parser::CppAst>().collect::<Vec<_>>() {
            let parenthesised = match &node {
                cpp_parser::CppAst::Stat(cpp_parser::CppStat::IfStat(statement)) => {
                    statement.children::<cpp_parser::CppParenExpr>().next()
                }
                cpp_parser::CppAst::Stat(cpp_parser::CppStat::WhileStat(statement)) => {
                    statement.children::<cpp_parser::CppParenExpr>().next()
                }
                _ => None,
            };
            let Some(condition) = parenthesised.map(|p| p.syntax().clone()) else {
                continue;
            };
            if !is_an_assignment(&condition) {
                continue;
            }
            let text = condition.text().to_string();
            let text = text.trim();
            let text = text.trim_start_matches('(').trim_end_matches(')').trim();

            context.add(
                DiagnosticCode::AssignmentInCondition,
                cpp_parser::source_range(condition.text_range()),
                String::new(),
                format!(
                    "`{text}` assigns rather than compares: `=` in a condition is almost always a `==`"
                ),
            );
        }
    }
}

/// **Is this condition nothing but an assignment?**
///
/// Decided from the **tokens**, which is what makes it trustworthy: `=` is an `Assign` token and `==` is a different
/// one, so no amount of punctuation reading can confuse the two — the character scan this replaces had to look at
/// both sides of every `=` and read the first character of `==` as an assignment, which is how `if (x == 1)` came to
/// be reported as *`=` in a condition is almost always a `==`*. A string literal is one token as well, so an `=` in
/// `if (s == "a=b")` cannot be seen at all.
///
/// **The whole condition has to be the assignment.** A **compound** — `&&`, `||`, a comma — is not this rule's shape
/// whatever it holds inside: `if (x = f() && ok)` is a rarer and different mistake than `if (x = f())`, and
/// reporting the first as the second is what put a finding on a line whose condition is two comparisons.
fn is_an_assignment(condition: &cpp_parser::CppSyntaxNode) -> bool {
    let mut assigns = false;
    for token in condition
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        match token.kind().into() {
            // **A compound condition is not this question**, refused before the assignment is even looked for.
            cpp_parser::CppTokenKind::LogicalAnd
            | cpp_parser::CppTokenKind::LogicalOr
            | cpp_parser::CppTokenKind::Comma => return false,
            cpp_parser::CppTokenKind::Assign => assigns = true,
            _ => {}
        }
    }
    assigns
}
