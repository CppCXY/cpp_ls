//! **`ue-specifier`** — a specifier Unreal's reflection macros do not know.
//!
//! ```cpp
//! UPROPERTY(EditAnywhere, BlueprintReadWrit)   // this rule: `BlueprintReadWrit` is not a specifier
//! bool bIsReady = false;
//! ```
//!
//! The build tool **ignores a specifier it does not know**. `BlueprintReadWrit` compiles, the property is not readable
//! from Blueprint, and nothing anywhere says why — the annotation looks like it worked. That silence is the defect, and a
//! warning is the right size for it: the program builds, and what is wrong is the expectation.
//!
//! # Where the answer comes from
//!
//! Each annotation's specifiers are recorded on the declaration's fact ([`crate::DeclFact::reflection`]), read from the
//! file's text because the macros expand to nothing. The vocabulary is
//! [`crate::project::ue::known_specifiers`], a table this server maintains — Rider runs the real UnrealHeaderTool in the
//! background and shows *its* diagnostics; an analysis with no engine to run has to write the list down. It is therefore
//! the common set, and the rule is a warning rather than an error: a specifier a newer engine added may be reported, and
//! a project that hits that silences the rule by name, like every other rule.
//!
//! # What it does not judge
//!
//! A specifier's **value**: `Category = "Game"` and `meta=(DisplayName="…")` are checked by name and their contents are
//! the engine's business. Nor whether two specifiers conflict (`EditAnywhere` with `VisibleAnywhere`), which is a
//! different question with a different table.

use super::{Check, CheckContext, DiagnosticCode};

pub struct UeSpecifier;

impl Check for UeSpecifier {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UeSpecifier];

    fn check(context: &mut CheckContext<'_>) {
        // **The facts, not a walk of the tree**: the annotation was read once when the file was summarised, and it is
        // stored on the declaration it was written for. A check that re-read the text would be a second answer to the
        // same question, and the two could drift.
        let mut findings = Vec::new();
        for fact in &context.summary.declarations {
            let Some(reflection) = &fact.reflection else {
                continue;
            };
            let Some(known) = crate::project::ue::known_specifiers(&reflection.macro_name) else {
                continue;
            };
            for specifier in &reflection.specifiers {
                let name = specifier
                    .split_once('=')
                    .map(|(name, _)| name.trim())
                    .unwrap_or(specifier.trim());
                if name.is_empty() || known.contains(&name) {
                    continue;
                }
                findings.push((
                    fact.range,
                    name.to_string(),
                    format!(
                        "`{name}` is not a `{}` specifier — the build tool ignores specifiers it does not know, so the \
                         declaration silently loses what this one asked for",
                        reflection.macro_name
                    ),
                ));
            }
        }
        // Reported after the walk so the borrow of the summary has ended — `add` needs the context mutably.
        for (range, name, message) in findings {
            context.add_in_the_file(DiagnosticCode::UeSpecifier, range, name, message);
        }
    }
}
