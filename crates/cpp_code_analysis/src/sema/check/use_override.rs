//! `modernize-use-override`, from clang-tidy: **a function that overrides should say so.**
//!
//! ```cpp
//! struct Base { virtual void f(); };
//! struct Derived : Base { void f(); };       // this check says: void f() override;
//! ```
//!
//! `override` is not decoration: it turns a silent non-override into a compile error. Rename `Base::f`, or change its
//! parameter list, and `Derived::f` stops overriding — with no diagnostic under the old spelling, and a call that
//! silently runs the base's version under the new one.
//!
//! **Not implemented yet.** The walk is over a class's member declarations: a `virtual` function whose name and parameter
//! list match a base's, and whose declaration writes neither `override` nor `final`.

use super::{Check, CheckContext, DiagnosticCode};

pub struct UseOverride;

impl Check for UseOverride {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UseOverride];

    fn check(_context: &mut CheckContext<'_>) {
        // See the module documentation.
    }
}