//! **`syntax-error`**: the file does not parse, and the parser said where.
//!
//! This is the one check that does not reason about the program at all — it publishes what [`cpp_parser`] already
//! answered. It is a check rather than a separate channel because a client shows one list: the whole point of the
//! diagnostics layer is that *the file does not parse* and *the file does not mean what it says* arrive together,
//! in one order, with one severity each.
//!
//! # Why it is first in `run`
//!
//! Every other check reads the **tree**, and a tree that failed to parse is a tree with recovery nodes in it. A
//! finding from `undefined-field` on a recovery node is a claim about a construct the parser only guessed at, so
//! the parse errors are reported first and the reader is told what to fix before they are told what the fixed text
//! will still be wrong about.
//!
//! # The span, and why it is the error's own
//!
//! The parser's error carries the range it tripped at, which is where an editor puts the squiggle. Widening it to
//! the enclosing statement would be this layer inventing a better answer than the one thing it actually knows.
//!
//! **No filtering.** A check that decided some parse errors were not worth showing would be a second parser, and
//! these are the one set of findings in this layer with no `Known` question behind them — the parser is not
//! uncertain about having failed.

use super::{Check, CheckContext, DiagnosticCode};

/// **`syntax-error`** — see the module documentation.
pub struct SyntaxError;

impl Check for SyntaxError {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::SyntaxError];

    fn check(context: &mut CheckContext<'_>) {
        for error in context.errors {
            context.add(
                DiagnosticCode::SyntaxError,
                cpp_parser::source_range(error.range),
                String::new(),
                error.message.clone(),
            );
        }
    }
}
