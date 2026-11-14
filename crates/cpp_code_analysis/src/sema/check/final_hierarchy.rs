//! **`final-class-derived`** and **`override-of-final`** — the two ways `final` refuses a hierarchy.
//!
//! ```cpp
//! struct Widget final {
//!     virtual void draw() final;
//! };
//!
//! struct Gadget : Widget {};              // error: `Widget` is final
//! struct Tool : Widget { void draw() override; };   // error: `draw` is final
//! ```
//!
//! `final` on a class means "no base may derive from this" ([class.derived]); `final` on a virtual function means "no
//! derived class may override this" ([class.virtual]). Both make the program ill-formed, so both are reported rather
//! than hinted at.
//!
//! # Why one module for two codes
//!
//! They answer the same question from two sides — what `final` forbids *below* a declaration — and share the one
//! lookup they need: a base's own fact, through the index. Splitting them would duplicate that lookup and the
//! reasoning about when it is unreadable.
//!
//! # What is compared, and what is not
//!
//! ```text
//! a base that is final, class fact read      -> final-class-derived
//! a base member the hierarchy calls final    -> override-of-final
//! a base whose fact was never read           -> silent
//! ```
//!
//! The function half compares **name and parameter count**, because the summary records parameters as spellings and
//! two spellings of one type (`const T&`, `T const&`) must not be a difference. It reports only when **every** base
//! member of that name and arity says `final`: `f(int) final` beside `f(double)` leaves `f(double) override` legal, and
//! a rule that reported it would underline correct code.

use super::{Check, CheckContext, DiagnosticCode};

/// **`final-class-derived` / `override-of-final`** — see the module documentation.
pub struct FinalHierarchy;

impl Check for FinalHierarchy {
    const CODES: &'static [DiagnosticCode] = &[
        DiagnosticCode::FinalClassDerived,
        DiagnosticCode::OverrideOfFinal,
    ];

    fn check(context: &mut CheckContext<'_>) {
        // The classes this file declares, with their bases and the range of their name.
        let classes: Vec<(String, Vec<String>, cpp_parser::SourceRange)> = context
            .summary
            .declarations
            .iter()
            .filter(|fact| fact.kind == crate::DeclKind::Type)
            .map(|fact| {
                (
                    fact.name.clone(),
                    fact.bases.clone(),
                    fact.name_range,
                )
            })
            .collect();

        for (class, bases, name_range) in &classes {
            for base in bases {
                // **The base's own fact**, through the index: the class may be declared in a header this file
                // includes. A base nobody read is silence — "not read" is not "not final".
                let crate::Known::Yes(declared) = context.index().definition(base, context.path) else {
                    continue;
                };
                if !declared.fact.modifiers.contains(crate::DeclModifiers::FINAL) {
                    continue;
                }
                context.add_in_the_file(
                    DiagnosticCode::FinalClassDerived,
                    *name_range,
                    class.clone(),
                    t!(
                        "`%{base}` is final, so `%{class}` cannot derive from it",
                        base = last_segment(base),
                        class = class
                    ),
                );
            }
        }

        // **The overriding members of this file**, each judged against what its bases say about the name.
        let overriders: Vec<(String, String, cpp_parser::SourceRange)> = context
            .summary
            .declarations
            .iter()
            .filter(|fact| {
                fact.kind == crate::DeclKind::Function
                    && fact.modifiers.contains(crate::DeclModifiers::OVERRIDE)
                    && !fact.is_a_destructor()
            })
            .filter_map(|fact| {
                Some((
                    fact.scope.clone()?,
                    fact.name.clone(),
                    fact.name_range,
                ))
            })
            .collect();

        for (class, name, name_range) in &overriders {
            let Some((_, bases, _)) = classes.iter().find(|(declared, _, _)| declared == class) else {
                continue;
            };
            let mut any_overridable = false;
            let mut all_final = true;
            let mut could_not_read_a_base = false;
            for base in bases {
                match context.model.members_of(base) {
                    crate::Known::Yes(list) => {
                        if !list.unlisted.is_empty() {
                            could_not_read_a_base = true;
                        }
                        for member in &list.members {
                            if member.fact.kind != crate::DeclKind::Function
                                || member.fact.name != *name
                            {
                                continue;
                            }
                            any_overridable = true;
                            if !member.fact.modifiers.contains(crate::DeclModifiers::FINAL) {
                                all_final = false;
                            }
                        }
                    }
                    _ => could_not_read_a_base = true,
                }
            }
            if could_not_read_a_base || !any_overridable || !all_final {
                continue;
            }
            context.add_in_the_file(
                DiagnosticCode::OverrideOfFinal,
                *name_range,
                name.clone(),
                t!(
                    "`%{name}` overrides a function `%{class}`'s base declares `final` — the program does not build",
                    name = name,
                    class = class
                ),
            );
        }
    }
}

/// The last `::`-separated segment of a spelling: `ns::Base` → `Base`.
fn last_segment(spelling: &str) -> &str {
    spelling
        .rsplit("::")
        .next()
        .map(str::trim)
        .unwrap_or(spelling)
}
