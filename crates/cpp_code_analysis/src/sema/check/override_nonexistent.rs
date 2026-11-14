//! **`override-nonexistent`** — `override` written on a function that no base class declares.
//!
//! ```cpp
//! struct Base { virtual void run(); };
//! struct Derived : Base { void walk() override; };   // error: `Base` has no `walk`
//!
//! struct Alone { void run() override; };             // error: no base at all
//! ```
//!
//! `override` is not decoration: [class.virtual] makes a function that overrides nothing and says `override`
//! **ill-formed**. That is what the keyword is *for* — it turns a silent non-override into a compile error — and it is
//! also why a rule can report it: the program does not build.
//!
//! # Only what is certain
//!
//! The question is "does a base declare a function this one overrides", and this layer answers it in one direction
//! only:
//!
//! ```text
//! no base at all                     -> report: `override` requires a base
//! a base exists, none names it       -> report: nothing can be overridden
//! a base names it                    -> silent: the *signature* is not compared here (see below)
//! a base could not be read           -> silent: the name may be in the header nobody read
//! ```
//!
//! **The signature is deliberately not compared.** `void f(int)` and `void f(double)` are different overriders, and
//! the summary records parameters as *spellings* — `const T&` in a base and `T const&` in a derivation name the same
//! type. A comparison that normalised them would still be wrong about typedefs and templates, so the rule stays with
//! the half it can prove: a name no base declares at all.

use super::{Check, CheckContext, DiagnosticCode};

/// **`override-nonexistent`** — see the module documentation.
pub struct OverrideNonexistent;

impl Check for OverrideNonexistent {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::OverrideNonexistent];

    fn check(context: &mut CheckContext<'_>) {
        // The classes and their bases, cloned out of the summary because a finding needs `&mut context`.
        let classes: Vec<(String, Vec<String>)> = context
            .summary
            .declarations
            .iter()
            .filter(|fact| fact.kind == crate::DeclKind::Type)
            .map(|fact| (fact.name.clone(), fact.bases.clone()))
            .collect();

        // **The overriding members of this file**, with the class they belong to.
        let overriders: Vec<(String, Option<String>, String, cpp_parser::SourceRange)> = context
            .summary
            .declarations
            .iter()
            .filter(|fact| {
                fact.kind == crate::DeclKind::Function
                    && fact.modifiers.contains(crate::DeclModifiers::OVERRIDE)
                    // **A destructor overrides a different name**: `~B` overrides `~A`, and the two spellings never
                    // match. Comparing names would report every legal `~B() override {}` in a class with a base,
                    // which is what this rule did to a fixture whose destructor is exactly that.
                    && !fact.is_a_destructor()
            })
            .map(|fact| {
                (
                    fact.name.clone(),
                    fact.scope.clone(),
                    fact.name.clone(),
                    fact.name_range,
                )
            })
            .collect();

        for (name, scope, _, name_range) in &overriders {
            let Some(class) = scope.as_deref() else {
                // **`override` outside a class definition is ill-formed too**, and the fact says so by having no
                // scope: a free function cannot override anything.
                context.add_in_the_file(
                    DiagnosticCode::OverrideNonexistent,
                    *name_range,
                    name.clone(),
                    t!(
                        "`%{name}` says `override` outside a class definition — there is nothing it could override",
                        name = name
                    ),
                );
                continue;
            };

            let Some((_, bases)) = classes.iter().find(|(declared, _)| declared == class) else {
                // The class's own fact is not in this file (a member defined out of line in a `.cpp`). Its bases are
                // not knowable from here, so the rule says nothing — the declaration in the header is the one asked
                // about, and it has the bases.
                continue;
            };

            if bases.is_empty() {
                context.add_in_the_file(
                    DiagnosticCode::OverrideNonexistent,
                    *name_range,
                    name.clone(),
                    t!(
                        "`%{name}` says `override` but `%{class}` has no base class, so there is nothing to override",
                        name = name,
                        class = class
                    ),
                );
                continue;
            }

            // **Does any base declare the name?** A base that could not be read makes the answer unknown, and the
            // rule stays silent — the name may be in the header nobody read.
            let mut declared_somewhere = false;
            let mut could_not_read_a_base = false;
            for base in bases {
                match context.model.members_of(base) {
                    crate::Known::Yes(list) => {
                        if !list.unlisted.is_empty() {
                            could_not_read_a_base = true;
                        }
                        if list.members.iter().any(|member| member.fact.name == *name) {
                            declared_somewhere = true;
                            break;
                        }
                    }
                    _ => could_not_read_a_base = true,
                }
            }
            if could_not_read_a_base || declared_somewhere {
                continue;
            }
            context.add_in_the_file(
                DiagnosticCode::OverrideNonexistent,
                *name_range,
                name.clone(),
                t!(
                    "`%{name}` says `override` but no base of `%{class}` declares it — the program does not build",
                    name = name,
                    class = class
                ),
            );
        }
    }
}
