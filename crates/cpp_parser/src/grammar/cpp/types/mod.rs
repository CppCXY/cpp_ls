//! Types and declarators.
//!
//! C++ splits "the type" from "the thing being declared" in a way no other common language does:
//! `int *a[10]` declares an array of pointers, `int (*a)[10]` a pointer to an array, and the
//! difference lives entirely in how the parentheses nest around the declarator. So the declarator
//! cannot be parsed as a suffix of the type; it has to be parsed on its own and nested as it is
//! read.
//!
//! Everything in this module is *syntactic*. Name lookup is deliberately absent: without a symbol
//! table the parser cannot know whether `T` is a type or a variable, so the grammar accepts the
//! superset and leaves the distinction to the caller. That is why `parse_type_id` stops at the
//! first token it cannot classify rather than reporting an error — stopping early is what lets a
//! caller turn "that was not a type after all" into a backtrack.

use crate::{
    grammar::ParseResult,
    kind::{CppSyntaxKind, CppTokenKind, Dialect},
    parser::{CompleteMarker, CppParser, Marker, MarkerEventContainer},
    parser_error::CppParseError,
    symbols::SymbolKind,
};

use super::expect_token;

/// Keywords that begin a type specifier.
///
/// Includes the C++20 module keywords (`module`, `import`) because they are *contextual*: the lexer
/// hands them over as identifiers, and they only matter in `parse_decl_specifier_seq` when the
/// enclosing declaration says so.
/// Does the cursor start a **type-id** rather than an expression — the payload of C23's `typeof` and GNU's
/// `__typeof__`?
///
/// Only the tokens that *cannot* begin an expression: a builtin type specifier, an elaborated specifier, or a
/// cv-qualifier. A name, a literal and a `(` all begin expressions and are left to the expression rule.
pub(super) fn starts_a_type_id(p: &CppParser) -> bool {
    // **`nullptr` is a value, not a type.** The kind table lists it with the type specifiers because
    // `decltype(nullptr)` exists — that is C++'s spelling of a *value*'s type — so `typeof(nullptr)` is the
    // expression reading, and taking it for a type-id reported `expected primary expression` at the `nullptr`
    // (measured on a nine-file C corpus: one new error the moment this rule was added).
    p.current_token() != CppTokenKind::NullptrKeyword
        && (is_type_specifier_keyword(p.current_token())
            || matches!(
                p.current_token(),
                CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword
            ))
}
mod type_id;

pub(crate) use type_id::*;

mod specifiers;

pub(crate) use specifiers::*;

mod joining;

pub(crate) use joining::*;

mod classes;

pub(crate) use classes::*;

mod names;

pub(crate) use names::*;

mod abstract_declarators;

pub(crate) use abstract_declarators::*;

mod declarators;

pub(crate) use declarators::*;

mod template_arguments;

pub(crate) use template_arguments::*;

mod qualifiers;

pub(crate) use qualifiers::*;

mod attributes;

pub(crate) use attributes::*;
