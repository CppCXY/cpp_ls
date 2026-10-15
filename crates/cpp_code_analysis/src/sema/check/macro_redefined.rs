//! **`macro-redefined`**: a `#define` that changes a name whose definition is already in force.
//!
//! ```cpp
//! #define MAX 10
//! #define MAX 20        // the same name, a different body, no `#undef` between them
//! ```
//!
//! # The four conditions, and why each one is a way to be wrong
//!
//! A name may legally be defined more than once, and every condition below is one of the ways the language allows
//! it. All four have to hold for a finding:
//!
//! ```text
//!   1  the definition is of *this* file      a name defined in a header is the header's business, and reporting it
//!                                            here would be reporting another file's line in this one
//!   2  the definition is **in force**        a `#define` inside `#if _HAS_CXX23` that nobody takes is not a
//!                                            definition at all — `fact_in_force` is `Known`-valued and `Unknown`
//!                                            keeps the fact out
//!   3  no `#undef` between the two            an `#undef` makes the second one the name's first definition again
//!   4  the bodies differ                     an identical redefinition is legal, and it is what a header that
//!                                            includes another header's `#define` twice produces
//! ```
//!
//! Condition 2 is the one that keeps this check off the standard library: a three-valued answer to "was this
//! definition taken" means a condition nobody can decide makes it answer `false` for **both** definitions, so two
//! `#define`s in mutually exclusive branches are not a redefinition of each other.
//!
//! That direction is the one this layer already documents — *"`Unknown` keeps the fact out … this can lose
//! evidence, never invent it"* — and it is why this check reports nothing on a header that reads correctly rather
//! than reporting the standard library's own idiom.

use crate::index::environment::fact_in_force;
use crate::summary::MacroFact;

use super::{Check, CheckContext, DiagnosticCode};

/// **`macro-redefined`** — see the module documentation for the four conditions.
pub struct MacroRedefined;

impl Check for MacroRedefined {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::MacroRedefined];

    /// Every `#define` in the file that is a second, different definition of a name already defined in force.
    ///
    /// Answered from the file's **own** macro facts — a name defined in a header this file includes is not this
    /// file's redefinition, and a check that reported it would be reporting the header's business in the wrong file.
    fn check(context: &mut CheckContext<'_>) {
        let seed = context.index().macros();

        // **The definitions in force, in the order the file writes them** — condition 1 by the filter, condition 2
        // by the guard layer. Everything below reads this list and nothing else.
        let live: Vec<&MacroFact> = context
            .summary
            .macros
            .iter()
            .filter(|fact| fact.kind.is_definition())
            .filter(|fact| fact_in_force(seed, context.summary, fact))
            .collect();

        for (at, fact) in live.iter().enumerate() {
            let Some(first) = live[..at].iter().find(|earlier| earlier.name == fact.name) else {
                // The first definition of a name is what a redefinition would be of. Nothing to report.
                continue;
            };

            // **Condition 3**: an `#undef` between the two makes the second one the name's first definition again.
            // Asked of *all* the file's macro facts rather than of `live`, because an `#undef` is never a definition
            // and so never reaches that list.
            let undefined_between = context.summary.macros.iter().any(|other| {
                !other.kind.is_definition()
                    && other.name == first.name
                    && other.range.start_offset > first.range.start_offset
                    && other.range.start_offset < fact.range.start_offset
            });
            if undefined_between {
                continue;
            }

            if first.function_like == fact.function_like && bodies_are_equal(context, first, fact) {
                continue;
            }

            // **Already in the file''s coordinates**: this range came from the summary - see `CheckContext::written`.
            context.add_in_the_file(
                DiagnosticCode::MacroRedefined,
                fact.range,
                fact.name.clone(),
                format!(
                    "`{}` is redefined here with a different body; the definition that was in force is at offset {}",
                    fact.name, first.range.start_offset
                ),
            );
        }
    }
}

/// Whether two definitions wrote the same replacement list.
///
/// Read out of the file's own text through [`MacroFact::body_range`], which is what that field exists for — its
/// documentation says so: a consumer slices the body's text out of the defining file rather than re-deriving it by
/// searching the directive, "a search is a second implementation of the same rule, free to disagree with the one
/// that assigned the name".
///
/// Two empty bodies are equal, and a body the fact has no range for is empty: `#define NAME` followed by
/// `#define NAME` is the one redefinition the standard permits, and treating "no range" as "unknown, report it"
/// would fire on it.
fn bodies_are_equal(context: &CheckContext<'_>, first: &MacroFact, second: &MacroFact) -> bool {
    let text = |fact: &MacroFact| {
        fact.body_range
            .and_then(|range| context.written.get(range.start_offset..range.end_offset()))
            // **Trimmed**, because the ranges do not end where the replacement lists do: a `#define` at the end of a
            // file has a body range that reaches the newline after it, and one in the middle does not — so two
            // identical definitions compared untrimmed are `"10"` against `"10\n"` and the check reported a
            // redefinition of a macro with itself. Measured: `macro_redefined_is_not_reported_for_an_identical_definition`
            // failed on exactly that pair.
            .unwrap_or("")
            .trim()
    };

    text(first) == text(second)
}
