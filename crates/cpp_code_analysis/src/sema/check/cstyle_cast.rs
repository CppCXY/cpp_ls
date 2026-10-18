//! `cppcoreguidelines-pro-type-cstyle-cast`, from clang-tidy: **a C-style cast hides which conversion it is.**
//!
//! ```cpp
//! double d = (double)n;             // this check says: static_cast<double>(n)
//! char* p = (char*)const_pointer;   // …and this one hides a const_cast that a compiler would refuse if written out
//! ```
//!
//! The rule's real content is the second line: a C-style cast tries `const_cast`, `static_cast` and `reinterpret_cast` in
//! that order and takes the first that compiles, so it can remove `const` without saying so. Naming the cast is what makes
//! the conversion reviewable.
//!
//! **Not implemented yet.** The walk is over cast expressions whose type is written in parentheses — the tree already
//! distinguishes them from a function-style cast (`int(x)`), which is the reason this is a tree walk rather than a token
//! scan.

use super::{Check, CheckContext, DiagnosticCode};

pub struct CStyleCast;

impl Check for CStyleCast {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::CStyleCast];

    fn check(_context: &mut CheckContext<'_>) {
        // See the module documentation.
    }
}