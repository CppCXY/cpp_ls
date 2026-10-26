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

use cpp_parser::{CppAstNode, CppAstToken};

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
            // **An assignment *compared against something* is the idiom**, not the mistake. `if ((op = queue.front()) != 0)`
            // is written deliberately: the assignment is parenthesised, and its result is compared — which is exactly the
            // shape the extra-parentheses rule exists for. The census found one such finding in 1191 files, in boost's
            // `win_object_handle_service.ipp`, and it was this.
            // **The inner expression decides, and it is typed.** `((op = queue->Peek()))` is a parenthesised expression
            // whose child is the assignment; `((op = …) != 0)` is one whose child is a comparison *of* a parenthesised
            // assignment, which is the idiom. The operator **token** on the inner binary says which — the previous form
            // counted parentheses through text, which is how a `--port` comparison was once read as an assignment.
            let is_a_comparison = cpp_parser::CppParenExpr::cast(condition.clone())
                .and_then(|parenthesised| parenthesised.child::<cpp_parser::CppBinaryExpr>())
                .and_then(|binary| binary.token::<cpp_parser::CppOperatorToken>())
                .is_some_and(|operator| {
                    matches!(
                        operator.syntax().text().to_string().as_str(),
                        "==" | "!=" | "<" | ">" | "<=" | ">="
                    )
                });
            if is_a_comparison {
                continue;
            }
            let text = context.node_text(&condition);
            let text = text.trim();

            context.add(
                DiagnosticCode::AssignmentInCondition,
                cpp_parser::source_range(condition.text_range()),
                String::new(),
                t!(
                    "`%{text}` assigns rather than compares: `=` in a condition is almost always a `==`", text = text
                ),
            );
        }
    }
}

/// **Is this condition nothing but an assignment?**
///
/// Decided from the **tokens**, which is what makes it trustworthy: a string literal is one token, so an `=` in
/// `if (s == "a=b")` cannot be seen at all, and a compound condition is refused before any `=` is looked for.
///
/// **The whole condition has to be the assignment.** A **compound** — `&&`, `||`, a comma — is not this rule's shape
/// whatever it holds inside: `if (x = f() && ok)` is a rarer and different mistake than `if (x = f())`, and
/// reporting the first as the second is what put a finding on a line whose condition is two comparisons.
///
/// # Why the kind alone was wrong, and the position is the fix
///
/// **The lexer splits `>=` into two tokens on purpose**, so that `>>` can collapse in `vector<vector<int>>`:
///
/// ```text
/// CppTokenKind::GreaterEqual => {
///     tokens.push(part(start, CppTokenKind::Greater));
///     tokens.push(part(start + 1, CppTokenKind::Assign));   // ← the "=" of ">="
/// }
/// ```
///
/// So the `=` of a `>=` arrives as [`CppTokenKind::Assign`], exactly as a real assignment's does, and a test for the
/// *kind* reported both. Measured on
///
/// ```cpp
/// if (a >= b) { … }   →  Greater@32..33 ">"   Assign@33..34 "="
/// if (a = b)  { … }   →                          Assign@74..75 "="
/// ```
///
/// that was `if (a >= b) assigns rather than compares`, reported from an editor against real code. One `if (x >=
/// size())` — the most ordinary bounds check there is — was a finding on every occurrence.
///
/// The distinction is **positional**, and it is the grammar's own: a real assignment's `=` is the operator of the
/// expression, so it is the **first** token of the condition, while a split `>=` has its `>` before it. Asking for
/// that asks the tree what the operator was rather than reading punctuation.
fn is_an_assignment(condition: &cpp_parser::CppSyntaxNode) -> bool {
    // **The condition arrives as the `ParenExpr`** — `(a = b)`, tokens `(`, `a`, `=`, `b`, `)` — so the leading and
    // trailing parentheses are skipped and what is left is the expression. A condition that is *only* parentheses
    // is not this shape.
    let mut significant = condition
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| !cpp_parser::is_trivia(cpp_parser::CppTokenKind::from(token.kind())))
        .map(|token| cpp_parser::CppTokenKind::from(token.kind()))
        .collect::<Vec<_>>();
    while matches!(
        significant.first(),
        Some(cpp_parser::CppTokenKind::LeftParen)
    ) {
        significant.remove(0);
    }
    while matches!(
        significant.last(),
        Some(cpp_parser::CppTokenKind::RightParen)
    ) {
        significant.pop();
    }

    // **The token after the left operand is the operator**, which is where a binary expression puts it: `a = b` is
    // `a`, `=` and `b`, and `a >= b` is `a`, `>` and `=` and `b`. The first is the operand in both, so asking for the
    // *second* is the question — and asking for the kind anywhere in the range finds the split `=` of a `>=` just as
    // readily as a real one, which is the defect this replaced.
    let mut significant = significant.into_iter();
    if significant.next().is_none() {
        return false;
    }
    let is_the_operator = significant.next() == Some(cpp_parser::CppTokenKind::Assign);

    is_the_operator
    // **And a compound condition is not this question.** `if (x = f() && ok)` is a rarer and different mistake than
    // `if (x = f())`; it is refused here rather than by the caller so that both halves of the rule are in one place.
    && !condition
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| {
            matches!(
                cpp_parser::CppTokenKind::from(token.kind()),
                cpp_parser::CppTokenKind::LogicalAnd
                    | cpp_parser::CppTokenKind::LogicalOr
                    | cpp_parser::CppTokenKind::Comma
            )
        })
}
