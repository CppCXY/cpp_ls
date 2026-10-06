//! **A variable initialised with something its type cannot hold.**
//!
//! ```cpp
//! int count = "three";        // the first thing anybody writes by mistake
//! ```
//!
//! # Why this is the first type check, and why it is narrow
//!
//! The layer had three checks and none of them was about types: a missing `#include`, a redefined macro, and an
//! `#error` the file asked for. Everything a reader expects an editor to underline about *types* was missing — and
//! the reason it stayed missing is in this module's contract, not in the machinery: [`crate::ProjectIndex`] can
//! answer `type_of_expression` today, but it answers `Known::Unknown` for anything it has not read, and a check that
//! reports on `Unknown` is a check that underlines the whole standard library.
//!
//! So this check reports **one shape**, chosen because it cannot be wrong:
//!
//! > a declaration whose type is arithmetic or `void`, initialised with a **string literal**.
//!
//! No conversion makes `int` hold `"three"`. There is no overload set to consult, no template argument to deduce,
//! no user-defined conversion to consider, and no dependence on any other file: both halves are in the file being
//! checked, and the comparison is between a spelling the summary recorded and a token the tree holds.
//!
//! # What is deliberately *not* reported
//!
//! Everything else. Two integer types are convertible, a class may have a constructor from exactly this argument,
//! a pointer may be initialised from `0`, and a placeholder (`auto`) has no type to compare against. Each of those
//! is a real error in *some* program and an ordinary line in another, and telling them apart needs the overload
//! resolution this layer does not have. Reporting them would be guessing, and the module documentation of
//! [`super`] says what guessing costs: *"an editor that underlines correct code is one the user turns off — and then
//! it reports nothing at all, forever."*
//!
//! A string literal assigned to a `char*` **is** ordinary C++ (`char* p = "x";` is ill-formed in C++11 and later,
//! but it is accepted by every compiler in wide use, and this check does not report it): the declared type is a
//! pointer, and the guard below asks for arithmetic or `void`.

use cpp_parser::CppSyntaxKind;

use super::{Checks, Finding};

/// The name this check reports under — see [`Finding::check`].
pub const CHECK: &str = "a_string_is_not_a_number";

/// Every declaration whose type cannot hold the string literal it was given.
pub fn a_string_literal_is_not_a_number(checks: &Checks<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();

    for fact in &checks.summary.declarations {
        // **A variable with a written type**, which is the only shape this check can judge: a function's
        // `returns` is a different question, and a fact with no `type_of` has nothing to compare against.
        if fact.kind != crate::DeclKind::Variable {
            continue;
        }
        let Some(declared) = fact.type_of.as_deref() else {
            continue;
        };
        if !is_arithmetic_or_void(declared) {
            continue;
        }

        // **The initialiser, from the file's own tree** — never from a search of the text, for the reason
        // `super` gives: a search is a second implementation of the lexer.
        let Some(initializer) = initializer_of(checks.tree, fact.range) else {
            continue;
        };
        if !holds_a_string_literal(&initializer) {
            continue;
        }

        findings.push(Finding {
            // The whole initialiser rather than the literal: the literal is what is wrong, and the `=` is what a
            // reader looks at first. The span a diagnostic underlines is the construct it is about.
            range: cpp_parser::source_range(initializer.text_range()),
            name: fact.name.clone(),
            check: CHECK,
            message: format!(
                "`{}` has type `{declared}` and cannot hold a string literal",
                fact.name
            ),
        });
    }

    findings
}

/// **Is this spelling an arithmetic type or `void`?**
///
/// Written as a match over the last word rather than a list of whole spellings, because a declaration's type is
/// spelled with specifiers in any order: `unsigned long long`, `long unsigned`, `const int`, `int const`. What
/// decides the question is the **type word**, which is the last one that is not a qualifier.
fn is_arithmetic_or_void(declared: &str) -> bool {
    const QUALIFIERS: [&str; 6] = ["const", "volatile", "static", "constexpr", "inline", "register"];

    let word = declared
        .split_whitespace()
        .filter(|word| !QUALIFIERS.contains(word))
        .next_back();

    matches!(
        word,
        Some(
            "void"
                | "bool"
                | "char"
                | "signed"
                | "unsigned"
                | "short"
                | "int"
                | "long"
                | "float"
                | "double"
                | "wchar_t"
                | "char8_t"
                | "char16_t"
                | "char32_t"
        ) | Some("_Bool")
    )
}

/// The `Initializer` node written inside `declaration`, if there is one.
///
/// The same search [`crate::sema::deduce`] makes, and for the same reason: a declaration's initialiser is a node of
/// its own, so a node cannot be confused with the `=` of a default template argument or of an operator name.
fn initializer_of(
    root: &cpp_parser::CppSyntaxNode,
    declaration: cpp_parser::SourceRange,
) -> Option<cpp_parser::CppSyntaxNode> {
    root.descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Initializer)
        .find(|node| {
            let range = cpp_parser::source_range(node.text_range());
            range.start_offset >= declaration.start_offset && range.end_offset() <= declaration.end_offset()
        })
}

/// **Does this initialiser hold a string literal?**
///
/// Asked of the **tokens**, not of the text: `"three"` is a `StringLiteral` to the lexer, while the five characters
/// `three` between quotes in a comment are not. A node's descendants carry the tokens, so this needs no second
/// lexer either.
fn holds_a_string_literal(initializer: &cpp_parser::CppSyntaxNode) -> bool {
    initializer
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| {
            cpp_parser::CppTokenKind::from(token.kind()) == cpp_parser::CppTokenKind::StringLiteral
        })
}
