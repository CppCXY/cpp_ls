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

use cpp_parser::CppSyntaxKind;

use super::{Check, CheckContext, DiagnosticCode};

/// **`assignment-in-condition`** — see the module documentation.
pub struct AssignmentInCondition;

impl Check for AssignmentInCondition {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::AssignmentInCondition];

    fn check(context: &mut CheckContext<'_>) {
        for node in context.tree().descendants().filter(|node| {
            matches!(
                CppSyntaxKind::from(node.kind()),
                CppSyntaxKind::IfStat | CppSyntaxKind::WhileStat
            )
        }) {
            let text = node.text().to_string();
            let text = text.trim();
            let Some(condition) = between_the_first_parentheses(text) else {
                continue;
            };
            if !is_an_assignment(condition) {
                continue;
            }

            context.add(
                DiagnosticCode::AssignmentInCondition,
                cpp_parser::source_range(node.text_range()),
                String::new(),
                format!(
                    "`{condition}` assigns rather than compares: `=` in a condition is almost always a `==`"
                ),
            );
        }
    }
}

/// The text between the first `(` and its matching `)`.
fn between_the_first_parentheses(text: &str) -> Option<&str> {
    let open = text.find('(')?;
    let mut depth = 0usize;
    for (at, character) in text[open..].char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return text.get(open + 1..open + at).map(str::trim);
                }
            }
            _ => {}
        }
    }
    None
}

/// **Is this condition an assignment?** See the module documentation for the four shapes that are not.
fn is_an_assignment(condition: &str) -> bool {
    // `((x = 1))` — the idiom for "deliberate" — is excused by its own outer parentheses.
    if condition.starts_with('(') && condition.ends_with(')') {
        return false;
    }

    // A top-level `=`: not `==`, `!=`, `<=`, `>=`, and not one inside brackets or a template argument list.
    let bytes: Vec<char> = condition.chars().collect();
    let mut depth = 0i32;
    for (at, character) in bytes.iter().enumerate() {
        match character {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            '=' if depth == 0 => {
                let before = at.checked_sub(1).and_then(|index| bytes.get(index));
                let after = bytes.get(at + 1);
                let part_of_a_comparison = matches!(before, Some('=') | Some('!') | Some('<') | Some('>'))
                    || matches!(after, Some('='));
                if !part_of_a_comparison {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}
