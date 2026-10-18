//! `readability-braces-around-statements`, from clang-tidy: **a body that is one statement should be a block.**
//!
//! ```cpp
//! if (ready) start();            // this check says: { start(); }
//! if (ready) { start(); }        // …and this is what it wants
//! ```
//!
//! The rule the C++ world argues about, and the reason it is worth a diagnostic rather than a taste: `if (a) if (b) f();
//! else g();` binds the `else` to the *inner* `if`, which is the shape of more real bugs than any other one-line style
//! question. clang-tidy's name is kept so a reader can look the rule up.
//!
//! **Not implemented yet.** The file exists so the rule has a name, a code, and a test stating what it should say; the walk
//! is over `if`/`for`/`while`/`do`/`else` nodes and asks whether the body is a `CompoundStmt`.

use super::{Check, CheckContext, DiagnosticCode};

pub struct BracesAroundStatements;

impl Check for BracesAroundStatements {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::BracesAroundStatements];

    fn check(_context: &mut CheckContext<'_>) {
        // See the module documentation: the rule is named and tested, and this is where its walk will live.
    }
}