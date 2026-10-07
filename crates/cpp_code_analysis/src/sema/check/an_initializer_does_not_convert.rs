//! **An initialiser that does not convert to the type it is assigned to.**
//!
//! ```cpp
//! int count = "three";        // no conversion from `const char*` to `int`
//! ```
//!
//! # One relation, two checks
//!
//! The question is not "is a string a number" — that was the shape of the first version of this check, and it was
//! the wrong shape: a special case dressed as a rule, which would have needed a sibling for every pair of types.
//! What it asks is [`Type::convertible_to`], the crate's single answer to *can a value of this type initialise one
//! of that type* — and [`super::an_argument_does_not_convert`] asks the same relation at a call site. One question,
//! one implementation, which is the rule this crate keeps having to relearn.
//!
//! # What is reported, and what is refused
//!
//! Only [`Known::No`](crate::Known::No) — a conversion the standard does not provide, between two types whose shapes
//! are both known. Everything else is silence, and the relation's own documentation lists what silence covers:
//! anything named (a class may have a converting constructor, and `std::string s = "x";` is the commonest line in
//! modern C++), anything depending on a template parameter, and any pair of different pointer types.
//!
//! # Where the two types come from
//!
//! The declared one is the summary's fact; the initialiser's is [`type_of_expression`](crate::ProjectIndex), the
//! same engine `auto` deduction is built on. Neither costs a parse: the tree was parsed to produce the summary, and
//! the scopes came with it.

use cpp_parser::CppSyntaxKind;

use crate::Known;
use crate::sema::types::parse_type_spelling;

use super::{Checks, Finding};

/// The name this check reports under — see [`Finding::check`].
pub const CHECK: &str = "an_initializer_does_not_convert";

/// Every declaration whose initialiser's type cannot convert to the declared type.
pub fn an_initializer_does_not_convert(checks: &Checks<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();

    // **Every initialiser in the file, found in one walk of the tree** — not one walk per declaration.
    //
    // The first version asked `root.descendants()` for each fact's own initialiser, which is the whole tree scanned
    // once *per declaration*: quadratic in the size of the file. Measured against MSVC's headers, that check alone
    // cost **6 430 ms over twenty files where the other checks together cost 396 ms** — 94% of the layer, paid per
    // keystroke by a diagnostics channel whose whole contract is that it answers from what it already has.
    //
    // The tree is the same either way, so walking it once costs what walking it once per fact cost once.
    let initializers: Vec<(cpp_parser::SourceRange, cpp_parser::CppSyntaxNode)> = checks
        .tree
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Initializer)
        .map(|node| (cpp_parser::source_range(node.text_range()), node))
        .collect();

    for fact in &checks.summary.declarations {
        // **A variable with a written type and an initialiser.** A function's return type is a different question
        // (`return` statements), and a fact with no `type_of` has nothing to convert to.
        if fact.kind != crate::DeclKind::Variable {
            continue;
        }
        let Some(declared) = fact.type_of.as_deref() else {
            continue;
        };
        // A placeholder has no type to compare against — deduction is the question there, not conversion.
        if matches!(declared, "auto" | "decltype(auto)") {
            continue;
        }

        let Some(initializer) = initializers
            .iter()
            .find(|(range, _)| {
                range.start_offset >= fact.range.start_offset && range.end_offset() <= fact.range.end_offset()
            })
            .map(|(_, node)| node)
        else {
            continue;
        };
        let Some(expression) = initializer.children().last() else {
            continue;
        };
        let Known::Yes((from, _)) = crate::index::project::type_of_expression(
            checks.index,
            &mut |_: &std::path::Path| None,
            checks.scopes,
            checks.tree,
            checks.path,
            &expression,
            0,
        ) else {
            // The initialiser's type is not known — a name from a header nobody read, a call whose return type is
            // deduced, a template parameter. **Nothing is reported**: see the module documentation.
            continue;
        };

        let Known::No = from.convertible_to(&parse_type_spelling(declared)) else {
            continue;
        };

        findings.push(Finding {
            range: cpp_parser::source_range(expression.text_range()),
            name: fact.name.clone(),
            check: CHECK,
            message: format!(
                "`{}` has type `{declared}`, and `{}` does not convert to it",
                fact.name,
                from
            ),
        });
    }

    findings
}
