//! **A macro defined twice, with different bodies, on the path the compiler reads.**
//!
//! ```cpp
//! #define BUFFER_SIZE 256
//! #define BUFFER_SIZE 512     // `BUFFER_SIZE` redefined
//! ```
//!
//! Every compiler reports this, and it is a mistake a reader cannot see by looking at either line: whichever
//! definition wins is the one the *second* line wrote, so the first is dead text that reads as live.
//!
//! # What has to be true before this fires, and it is four things
//!
//! Redefinition is the check with the most ways to be wrong, so each condition is written down and each has a
//! test:
//!
//! ```text
//! 1  both are #define facts, not an #undef   a name undefined and defined again is not a redefinition
//! 2  both are **in force**                   a definition in a branch that is not compiled defines nothing
//! 3  no #undef lies between them              the second definition is then the name's first, again
//! 4  the bodies differ, or one is function-  an *identical* redefinition is what the standard allows, and it
//!    like and the other is not               is what every include-guard-shaped header relies on
//! ```
//!
//! # Condition 2 is the one that matters, and the standard library is why
//!
//! MSVC's `__msvc_formatter.hpp` writes exactly this:
//!
//! ```cpp
//! #if _HAS_CXX23
//! #define _FMT_P2286_BEGIN inline namespace __p2286 {
//! #define _FMT_P2286_END   }
//! #else
//! #define _FMT_P2286_BEGIN
//! #define _FMT_P2286_END
//! #endif
//! ```
//!
//! — two definitions of each name, in two branches of one `#if`, and **at most one of them is ever in force**.
//! [`MacroFact`]'s own documentation calls the shape out as one where the branches deliberately differ in what
//! they define the name *as*, and says in as many words that the fact "does **not** say which `#define` is in
//! force". So the question is asked of the guard layer instead — [`fact_in_force`] — and its answer is
//! three-valued: a condition nobody can decide makes it answer `false` for **both**, and two facts that are not
//! in force are not a redefinition of each other.
//!
//! That direction is the one the layer already documents — *"`Unknown` keeps the fact out … this can lose
//! evidence, never invent it"* — and it is why this check reports nothing on a header that reads correctly
//! rather than reporting the standard library's own idiom.

use crate::index::environment::fact_in_force;
use crate::summary::MacroFact;

use super::{Checks, Finding};

/// The name this check reports under — see [`Finding::check`].
pub const CHECK: &str = "a_macro_is_not_redefined";

/// Every `#define` in the file that is a second, different definition of a name already defined in force.
///
/// Answered from the file's **own** macro facts — a name defined in a header this file includes is not this
/// file's redefinition, and a check that reported it would be reporting the header's business in the wrong file.
pub fn no_name_is_defined_twice_with_a_different_body(checks: &Checks<'_>) -> Vec<Finding> {
    let seed = checks.index().macros();

    // **The definitions in force, in the order the file writes them** — condition 1 by the filter, condition 2
    // by the guard layer. Everything below reads this list and nothing else.
    let live: Vec<&MacroFact> = checks
        .summary
        .macros
        .iter()
        .filter(|fact| fact.kind.is_definition())
        .filter(|fact| fact_in_force(seed, checks.summary, fact))
        .collect();

    let mut findings = Vec::new();

    for (at, fact) in live.iter().enumerate() {
        let Some(first) = live[..at].iter().find(|earlier| earlier.name == fact.name) else {
            // The first definition of a name is what a redefinition would be of. Nothing to report.
            continue;
        };

        // **Condition 3**: an `#undef` between the two makes the second one the name's first definition again.
        // Asked of *all* the file's macro facts rather than of `live`, because an `#undef` is never a definition
        // and so never reaches that list.
        let undefined_between = checks.summary.macros.iter().any(|other| {
            !other.kind.is_definition()
                && other.name == first.name
                && other.range.start_offset > first.range.start_offset
                && other.range.start_offset < fact.range.start_offset
        });
        if undefined_between {
            continue;
        }

        // **Condition 4**: an identical redefinition is legal, and it is what a header that includes another
        // header's `#define` twice produces.
        if first.function_like == fact.function_like && bodies_are_equal(checks, first, fact) {
            continue;
        }

        findings.push(Finding {
            range: fact.range,
            name: fact.name.clone(),
            check: CHECK,
            message: format!(
                "`{}` is redefined here with a different body; the definition that was in force is at offset {}",
                fact.name, first.range.start_offset
            ),
        });
    }

    findings
}

/// Whether two definitions wrote the same replacement list.
///
/// Read out of the file's own text through [`MacroFact::body_range`], which is what that field exists for — its
/// documentation says so: a consumer slices the body's text out of the defining file rather than re-deriving it
/// by searching the directive, "a search is a second implementation of the same rule, free to disagree with the
/// one that assigned the name".
///
/// Two empty bodies are equal, and a body the fact has no range for is empty: `#define NAME` followed by
/// `#define NAME` is the one redefinition the standard permits, and treating "no range" as "unknown, report it"
/// would fire on it.
fn bodies_are_equal(checks: &Checks<'_>, first: &MacroFact, second: &MacroFact) -> bool {
    let text = |fact: &MacroFact| {
        fact.body_range
            .and_then(|range| checks.source.get(range.start_offset..range.end_offset()))
            .unwrap_or("")
    };

    text(first) == text(second)
}
