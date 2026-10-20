//! **`unnecessary-branch`**: a branch the program cannot take, or a condition that is already decided.
//!
//! ```cpp
//! if (true)  { work(); }        // the `if` decides nothing
//! if (false) { work(); }        // and this body is never run
//! ```
//!
//! # What is reported, and what is emphatically not
//!
//! Only a condition that is **a literal written where the condition goes** — `true`, `false`, `0`, `1`. Nothing is
//! evaluated:
//!
//! ```text
//!   reported      if (true) …   if (false) …   if ((0)) …
//!   not reported  if (MY_FLAG)          a macro: its value is the preprocessor's, and this reads the tree
//!   not reported  if (DEBUG)            a name: folding it is another layer's question
//!   not reported  if (1 == 1)           arithmetic on literals — a constant folder, which this is not
//!   not reported  if (sizeof(T) > 4)    ditto
//!   not reported  #if 0 … #endif        the preprocessor removes it before the parse
//! ```
//!
//! The line between the two lists is the whole design of the check: **a literal is a fact about the text, and
//! everything else is a question**. Constant folding is a feature with a semantics of its own — integer promotion,
//! `constexpr` functions, macros — and a check that did half of it would be wrong in the half it did.
//!
//! # Read as text, deliberately
//!
//! The condition is taken from the statement's **own text**, between the first `(` and its match, rather than from
//! the tree's shape. A condition is where the grammar's recovery is most visible — `if (x` with the paren missing
//! produces a node that holds the rest of the file — and a check asking about literals has no business reading a
//! recovered tree. The text is what the reader wrote.
//!
//! # Why the dead branch is reported rather than the live one
//!
//! A finding is a claim about a place. `if (false) { … }` is a claim about the **whole statement** — nothing in it
//! runs — while `if (true) { … }` is a claim about the **condition**, which is a decision already made. The two
//! messages say which one the reader is looking at.

use cpp_parser::CppAstNode;

use super::{Check, CheckContext, DiagnosticCode};

/// **`unnecessary-branch`** — see the module documentation.
pub struct UnnecessaryBranch;

impl Check for UnnecessaryBranch {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UnnecessaryBranch];

    fn check(context: &mut CheckContext<'_>) {
        // **This file's own nodes, of the one type this check is about** — see `CheckContext::own`, which prunes the
        // inlined headers a rendering splices in and asks the typed layer what a node is. The walk is collected
        // before the loop because a finding needs `&mut context` while the iterator holds `&context`; a `Vec` of node
        // handles is a few words each, not a copy of the tree.
        for statement in context.own::<cpp_parser::CppIfStat>().collect::<Vec<_>>() {
            // The condition from the **typed** layer: the parser decides what a condition is, and this asks for it.
            let Some(condition) = statement
                .children::<cpp_parser::CppParenExpr>()
                .next()
                .map(|parenthesised| parenthesised.syntax().clone())
            else {
                continue;
            };
            let Some(decided) = super::literal_condition_of(&condition) else {
                continue;
            };
            let decided = decided == "true";
            let condition = super::spelling_of(&condition).unwrap_or_default();
            let condition = condition.trim();

            // **The whole statement when the branch cannot run, the condition when it always does** — see the module
            // documentation for why the message is about a different place in each of the two cases.
            let (range, message) = if decided {
                (
                    cpp_parser::source_range(statement.get_range()),
                    t!(
                        "this branch always runs: the condition `%{condition}` is a literal true, so the `if` decides nothing",
                        condition = condition
                    )
                    .to_string(),
                )
            } else {
                (
                    cpp_parser::source_range(statement.get_range()),
                    t!(
                        "this branch cannot run: the condition `%{condition}` is a literal false",
                        condition = condition
                    )
                    .to_string(),
                )
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
