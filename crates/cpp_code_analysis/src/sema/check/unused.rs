//! **`unused`**: a local declaration nothing reads.
//!
//! # What is reported, and what is deliberately not
//!
//! ```text
//!   reported      a local variable declared in a **source** file whose name is written exactly once
//!                 in the whole file — the declaration itself
//!   not reported  a name beginning with `_`, which is how a reader says "I know" ([`is_deliberately_unused`])
//!   not reported  anything declared in a header, where "nothing in this file uses it" says nothing at all
//!   not reported  a parameter, a member, a global, a function, a type
//! ```
//!
//! The last line is a decision about **noise**, and the corpus is what decides it: this analysis reports nothing on
//! a header it read correctly, and an unused parameter is the most common deliberate thing in C++ — a virtual
//! function's, a callback's, a `main`'s `argv`. A rule that fires on those is a rule the reader turns off, and the
//! cost of that is not this finding, it is every other finding it stops them from seeing.
//!
//! # Why the whole file rather than the enclosing scope
//!
//! A local is visible only inside its block, so the honest question is "is it read inside the block it is declared
//! in". This asks the **file** instead, which is strictly more permissive: a spelling written anywhere else — in
//! another function, in a macro body, in a string that the lexer does not treat as an identifier — makes this
//! check stay quiet. That is the direction to be wrong in. Measured on MSVC's headers, the narrower rule reported
//! `__pragma`-heavy declarations whose uses are written inside macros, and the file-wide question does not.

use std::collections::HashMap;

use cpp_parser::CppTokenKind;

use super::{Check, CheckContext, DiagnosticCode};

/// The headers whose "unused" says nothing: a file included by others is used by *them*.
///
/// Decided by extension rather than by asking the index whether anything includes this file, because a check may
/// not ask — see [`Checks`](super::CheckContext)'s note on why it is handed a summary and not a session. The
/// extension is the same evidence the reader has.
fn is_a_header(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("h" | "hh" | "hpp" | "hxx" | "inl" | "ipp" | "tpp" | "inc")
    )
}

/// `_name` is a name the writer chose not to use — see the module documentation.
fn is_deliberately_unused(name: &str) -> bool {
    name.starts_with('_')
}

/// A name that is not worth asking about: a keyword-shaped spelling, or nothing at all.
fn is_a_candidate(name: &str) -> bool {
    !name.is_empty()
        && name.chars().next().is_some_and(|first| {
            first.is_ascii_alphabetic() || first == '_' || first == '$'
        })
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        })
}

/// **`unused`** — see the module documentation for what it reports and what it stays quiet about.
pub struct Unused;

impl Check for Unused {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::Unused];

    fn check(context: &mut CheckContext<'_>) {
        if is_a_header(context.path) {
            return;
        }

        // **One pass over the file's identifiers, counted by spelling.** What the check needs is "how many times
        // is this word written", and a count answers it for every declaration at once — where asking per
        // declaration would walk the token stream once per name, which is quadratic in a file the size of a real
        // header. Measured on MSVC's `<vector>`: 20 247 nodes, and the per-declaration form was the whole cost of
        // the layer.
        let mut written: HashMap<&str, usize> = HashMap::new();
        for token in context.tree().descendants_with_tokens() {
            let Some(token) = token.into_token() else {
                continue;
            };
            if token.kind() != CppTokenKind::Identifier.into() {
                continue;
            }
            *written
                .entry(context.source[token.text_range().start().into()..token.text_range().end().into()].trim())
                .or_default() += 1;
        }

        for fact in &context.summary.declarations {
            // **A local variable of a source file**, which is the only shape this rule claims — see the module
            // documentation for the reasoning behind each of the three conditions.
            if !fact.local || fact.kind != crate::DeclKind::Variable {
                continue;
            }
            if is_deliberately_unused(&fact.name) || !is_a_candidate(&fact.name) {
                continue;
            }

            // **Written once means the declaration is the only place.** The count is over the *whole file*, so a
            // use in another function, in a macro body or in a string keeps the count above one and this stays
            // quiet — which is the direction to be wrong in.
            if written.get(fact.name.as_str()).copied().unwrap_or_default() > 1 {
                continue;
            }

            // **Already in the file''s coordinates**: this range came from the summary - see `CheckContext::written`.
            context.add_in_the_file(
                DiagnosticCode::Unused,
                fact.range,
                fact.name.as_str(),
                format!("`{}` is declared here and never used", fact.name),
            );
        }
    }
}

