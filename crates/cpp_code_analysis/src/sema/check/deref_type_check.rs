//! **`deref-type-check`**: `*` applied to something that is not a pointer to an object.
//!
//! ```c
//! int value = 1;
//! *value;          // `value` is a number: there is nothing to dereference
//!
//! void *memory = …;
//! *memory = 1;     // `void` is not an object type, so it has no size and nothing to write through
//! ```
//!
//! # Why this is its own rule, and what it took to make it one
//!
//! The layer's other rules judge a **conversion** at the place a value enters a declared type — an initialiser, an
//! assignment, an argument, a return. A dereference is not a conversion: `*p` is an expression whose *operand* has a
//! type, and the question is whether that type is one a `*` can be applied to at all. `assign-type-check` cannot
//! answer it, because the left-hand side of `*memory = 1` is an expression rather than a name.
//!
//! Measured on a C project of twenty-five files with one deliberate defect each: `*value` and `*memory = 1` were
//! both silent, both errors in every compiler, and both shapes of the same question.
//!
//! # What is reported, and what is not
//!
//! ```text
//!   reported      an arithmetic type           `int`, `double`, `size_t` after a `typedef`… no — see below
//!   reported      a pointer to `void`          `void` has no object to read or write
//!   not reported  a pointer to anything else   the ordinary case
//!   not reported  a reference, an array       `*ref` is the referent, `*array` its first element
//!   not reported  a name that is not a builtin `Widget`, `size_t`, `T` — a typedef may be a pointer, and only the
//!                                              file's own declarations would say which
//!   not reported  `Unknown`                   the operand's type is not known: a template, a header nobody read
//! ```
//!
//! The arithmetic case is the one that has to be exact, because it is the one that can be reported without knowing
//! what a name stands for: `size_t` written as a name is a `Named`, not a `Builtin`, and is left alone; `int` is a
//! `Builtin`, and no declaration can make it a pointer.

use crate::Known;
use crate::sema;

use cpp_parser::CppAstNode;

use super::{Check, CheckContext, DiagnosticCode};

/// **`deref-type-check`** — see the module documentation.
pub struct DerefTypeCheck;

impl Check for DerefTypeCheck {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::DerefTypeCheck];

    fn check(context: &mut CheckContext<'_>) {
        for unary in context
            .own::<cpp_parser::CppUnaryExpr>()
            .collect::<Vec<_>>()
        {
            if unary.get_operator_text().as_deref() != Some("*") {
                continue;
            }
            let Some(operand) = unary.get_operand() else {
                continue;
            };
            let Known::Yes((type_of, _)) = context.type_of(operand.syntax()) else {
                continue;
            };
            let Some(why) = nothing_to_dereference(&type_of) else {
                continue;
            };

            // **The message names the operand, the way the reader wrote it.** A `*` on a cast or on a call has no
            // short name, and `node_text` is the rendering's slice — which for a *spelling* is the file's own text
            // whenever the cook wrote it. The check layer's rule is that a message names what the reader wrote, so
            // the operand's own text wins over a type spelling this layer would have to invent.
            let name = context.node_text(operand.syntax()).to_string();
            let message = match why {
                Why::AVoidPointer => t!(
                    "`%{name}` is a `void *`, and `void` is not an object type — there is nothing to dereference",
                    name = name
                ),
                Why::ANumber => t!(
                    "`%{name}` is a number, so `*` has nothing to dereference",
                    name = name
                ),
            };
            context.add(
                DiagnosticCode::DerefTypeCheck,
                cpp_parser::source_range(operand.syntax().text_range()),
                name.clone(),
                message,
            );
        }
    }
}

/// **Why a `*` cannot be applied to a value of this type**, or `None` when it can.
fn nothing_to_dereference(type_of: &sema::types::Type) -> Option<Why> {
    // A reference dereferences to its referent, and a qualifier is not a type: peel both before asking.
    let mut type_of = type_of;
    while let sema::types::Type::Reference { to, .. } = type_of {
        type_of = to;
    }
    while let sema::types::Type::Qualified { of, .. } = type_of {
        type_of = of;
    }

    match type_of {
        sema::types::Type::Pointer { to } => to.is_void().then_some(Why::AVoidPointer),
        // An array dereferences to its first element, which is what a `*` means for one.
        sema::types::Type::Array { .. } => None,
        // **An arithmetic type is certainly not a pointer.** This is the only shape the rule decides from the
        // spelling alone: a `Named` may be a typedef for a pointer, so it is left to the answers above.
        other if other.is_arithmetic() => Some(Why::ANumber),
        _ => None,
    }
}

/// **Which of the two shapes of "nothing to dereference" this is** — they read differently, so they say so.
#[derive(Clone, Copy)]
enum Why {
    /// `void *` — an object pointer whose pointee has no size.
    AVoidPointer,
    /// A number: no compiler lets `*` be applied to one.
    ANumber,
}
