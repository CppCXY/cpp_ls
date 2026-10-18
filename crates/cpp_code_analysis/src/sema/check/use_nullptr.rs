//! `modernize-use-nullptr`, from clang-tidy: **`NULL` is a macro for `0`, and `0` is an `int`.**
//!
//! ```cpp
//! Widget* w = NULL;                        // this check says: nullptr
//! void f(Widget*);
//! f(NULL);                                 // …and picks the wrong overload where one exists
//! ```
//!
//! The bug the rule prevents is not stylistic: with `void f(int)` beside `void f(Widget*)`, `f(NULL)` calls the `int`
//! overload, because the macro expands to `0`. C++11's `nullptr` has the pointer type and does not.
//!
//! **Not implemented yet.** The walk is over the tokens for the identifier `NULL` — the macro spelling is the whole
//! evidence, and it is the spelling the rule is about.

use super::{Check, CheckContext, DiagnosticCode};

pub struct UseNullptr;

impl Check for UseNullptr {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UseNullptr];

    fn check(_context: &mut CheckContext<'_>) {
        // See the module documentation.
    }
}