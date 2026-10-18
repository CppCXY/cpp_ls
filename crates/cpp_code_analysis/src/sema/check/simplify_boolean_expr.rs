//! `readability-simplify-boolean-expr`, from clang-tidy: **a comparison against `true` is the value itself.**
//!
//! ```cpp
//! if (ready == true)  start();    // this check says: if (ready)
//! if (ready == false) stop();     // …and: if (!ready)
//! return ready ? true : false;    // …and: return ready;
//! ```
//!
//! A judgement rather than a law — the explicit comparison is sometimes the clearer sentence — which is why it is a hint.
//! It is worth a rule because the *inverted* form (`if (!ready == false)`) is the shape of real confusion.
//!
//! **Not implemented yet.** The walk is over comparison expressions whose left or right operand is the literal `true` or
//! `false`, and over conditional expressions whose two branches are those literals.

use super::{Check, CheckContext, DiagnosticCode};

pub struct SimplifyBooleanExpr;

impl Check for SimplifyBooleanExpr {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::SimplifyBooleanExpr];

    fn check(_context: &mut CheckContext<'_>) {
        // See the module documentation.
    }
}