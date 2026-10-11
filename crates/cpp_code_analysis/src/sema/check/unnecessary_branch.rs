//! **`unnecessary-branch`**: a branch the program cannot take, or a condition that is already decided.
//!
//! ```cpp
//! if (true)  { work(); }        // the `if` decides nothing
//! if (false) { work(); }        // and this body is never run
//! #if 0 … #endif                // not this check's business: the preprocessor removes it before the parse
//! ```
//!
//! # What is reported, and what is emphatically not
//!
//! Only a condition that is **a literal** — `true`, `false`, `0`, `1` — written directly where the condition goes.
//! Nothing is evaluated:
//!
//! ```text
//!   reported      if (true) …   if (false) …   while (false) …   for (; false; ) …   x ? a : b with a literal
//!   not reported  if (MY_FLAG)          a macro — its value is the preprocessor's, and this reads the tree
//!   not reported  if (DEBUG)            a name — constant-folding it is another layer's question
//!   not reported  if (1 == 1)           arithmetic on literals: a constant folder, which this is not
//!   not reported  if (sizeof(T) > 4)    ditto
//! ```
//!
//! The line between the two lists is the whole design of the check: **a literal is a fact about the text, and
//! everything else is a question**. Constant folding is a feature with a semantics of its own — integer promotion,
//! `constexpr` functions, macros — and a check that did half of it would be wrong in the half it did.
//!
//! # Why the dead branch is reported rather than the live one
//!
//! A finding is a claim about a place. `if (false) { … }` is a claim about the **body** — it is never run — while
//! `if (true) { … }` is a claim about the **condition**, which is a decision that has already been made. The two
//! messages say which one the reader is looking at.

use cpp_parser::{CppSyntaxKind, CppSyntaxNode};

use super::{Check, CheckContext, DiagnosticCode};

/// **`unnecessary-branch`** — see the module documentation.
pub struct UnnecessaryBranch;

/// A condition written as a literal, as the two answers it can have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decided {
    /// `true` or a non-zero literal: the condition is already true.
    Always,
    /// `false` or `0`: the body cannot run.
    Never,
}

/// The literal a condition **is**, or `None` for anything that would have to be evaluated.
///
/// Parentheses are stepped through — `if ((true))` is the same condition — and nothing else is: a cast, a macro, a
/// name and an expression all answer `None`, which is this check declining to have an opinion.
fn decided_by(condition: &CppSyntaxNode) -> Option<Decided> {
    let text = condition.text().to_string();
    let text = text.trim();
    let text = text.trim_start_matches('(').trim_end_matches(')').trim();

    match text {
        "true" => Some(Decided::Always),
        "false" => Some(Decided::Never),
        "1" => Some(Decided::Always),
        "0" => Some(Decided::Never),
        _ => None,
    }
}

/// The condition of an `if`/`while`/`for` statement: the first expression before the body.
fn condition_of(statement: &CppSyntaxNode) -> Option<CppSyntaxNode> {
    statement.children().find(|child| {
        !matches!(
            CppSyntaxKind::from(child.kind()),
            CppSyntaxKind::IfKeyword
                | CppSyntaxKind::WhileKeyword
                | CppSyntaxKind::ForKeyword
                | CppSyntaxKind::ElseKeyword
                | CppSyntaxKind::CompoundStat
        ) && !child
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .all(|token| token.kind() == cpp_parser::CppTokenKind::Whitespace.into())
    })
}

impl Check for UnnecessaryBranch {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UnnecessaryBranch];

    fn check(context: &mut CheckContext<'_>) {
        for statement in context.tree().descendants().filter(|node| {
            matches!(
                CppSyntaxKind::from(node.kind()),
                CppSyntaxKind::IfStat
            )
        }) {
            let Some(condition) = condition_of(&statement) else {
                continue;
            };
            let Some(decided) = decided_by(&condition) else {
                continue;
            };

            // **The body when the condition is false, the condition when it is true** — see the module
            // documentation for why the message is about a different place in each of the two cases.
            let (range, message) = match decided {
                Decided::Never => (
                    cpp_parser::source_range(statement.text_range()),
                    "this branch cannot run: the condition is a literal `false`".to_string(),
                ),
                Decided::Always => (
                    cpp_parser::source_range(condition.text_range()),
                    "this condition is a literal `true`, so the branch is decided here rather than by the program"
                        .to_string(),
                ),
            };

            context.add(
                DiagnosticCode::UnnecessaryBranch,
                range,
                String::new(),
                message,
            );
        }
    }
}
