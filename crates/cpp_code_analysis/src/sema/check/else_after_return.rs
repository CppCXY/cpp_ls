//! `readability-else-after-return`, from clang-tidy: **an `else` whose `if` branch returns does not need to be an `else`.**
//!
//! ```cpp
//! if (!ready) return;      // the early return
//! else      { start(); }   // this check says: drop the `else` and unindent
//! ```
//!
//! Flat is easier to read than nested, and the early return exists precisely to make the rest of the function flat. The rule
//! is a hint because a deliberate symmetry (`if (x) return a; else return b;`) is a legitimate style.
//!
//! **Not implemented yet.** The walk is over `if` statements whose then-branch ends in a `return` and which have an `else`.

use super::{Check, CheckContext, DiagnosticCode};

pub struct ElseAfterReturn;

impl Check for ElseAfterReturn {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::ElseAfterReturn];

    fn check(_context: &mut CheckContext<'_>) {
        // See the module documentation.
    }
}