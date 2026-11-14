//! **`final-misplaced`** — `final` on a function that is neither virtual nor overriding.
//!
//! ```cpp
//! struct Widget {
//!     void size() final;          // error: `final` may only be applied to a virtual function
//!     virtual void draw() final;  // fine: virtual, so `final` is what it says
//! };
//!
//! struct Base { virtual void run(); };
//! struct Derived : Base { void run() final; };   // fine: overrides `Base::run`
//! ```
//!
//! [class.mem.general] allows `virt-specifier-seq` only where the function is virtual — either because it says
//! `virtual` itself or because it overrides one — so a `final` on an ordinary member function is ill-formed.
//!
//! # Only what is certain
//!
//! ```text
//! `virtual` is written                       -> silent: `virtual … final` is legal
//! the class has no base                       -> report: nothing can be overridden
//! a base names it, and one of them is virtual -> silent: this is an override
//! a base could not be read                    -> silent
//! no base names it, every base was read       -> report
//! ```
//!
//! The last row is the one that needs care: a base with a member of the same name that is **not** virtual could be a
//! different overload, and the signature is not compared here (see [`super::override_nonexistent`] for why), so only
//! the definite shape — no base names it at all — is reported.

use super::{Check, CheckContext, DiagnosticCode};

/// **`final-misplaced`** — see the module documentation.
pub struct FinalMisplaced;

impl Check for FinalMisplaced {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::FinalMisplaced];

    fn check(context: &mut CheckContext<'_>) {
        let classes: Vec<(String, Vec<String>)> = context
            .summary
            .declarations
            .iter()
            .filter(|fact| fact.kind == crate::DeclKind::Type)
            .map(|fact| (fact.name.clone(), fact.bases.clone()))
            .collect();

        let finals: Vec<(String, Option<String>, cpp_parser::SourceRange)> = context
            .summary
            .declarations
            .iter()
            .filter(|fact| {
                fact.kind == crate::DeclKind::Function
                    && fact.modifiers.contains(crate::DeclModifiers::FINAL)
                    // **`virtual` written is the legal spelling**, and the fact records it — so the rule has nothing
                    // to say about `virtual void f() final;` without asking a base anything.
                    && !fact.modifiers.contains(crate::DeclModifiers::VIRTUAL)
                    // A destructor is the one function whose name cannot be compared across the hierarchy — see
                    // [`super::override_nonexistent`] — so `~B() final` is not judged by name.
                    && !fact.is_a_destructor()
            })
            .map(|fact| (fact.name.clone(), fact.scope.clone(), fact.name_range))
            .collect();

        for (name, scope, name_range) in &finals {
            let Some(class) = scope.as_deref() else {
                // A free function cannot be virtual, so `final` on one is always misplaced.
                context.add_in_the_file(
                    DiagnosticCode::FinalMisplaced,
                    *name_range,
                    name.clone(),
                    t!(
                        "`%{name}` says `final` outside a class definition — `final` may only be applied to a virtual function",
                        name = name
                    ),
                );
                continue;
            };

            let Some((_, bases)) = classes.iter().find(|(declared, _)| declared == class) else {
                continue;
            };
            if bases.is_empty() {
                context.add_in_the_file(
                    DiagnosticCode::FinalMisplaced,
                    *name_range,
                    name.clone(),
                    t!(
                        "`%{name}` says `final` but `%{class}` has no base class — `final` may only be applied to a virtual function",
                        name = name,
                        class = class
                    ),
                );
                continue;
            }

            let mut a_base_names_it = false;
            let mut could_not_read_a_base = false;
            for base in bases {
                match context.model.members_of(base) {
                    crate::Known::Yes(list) => {
                        if !list.unlisted.is_empty() {
                            could_not_read_a_base = true;
                        }
                        if list.members.iter().any(|member| member.fact.name == *name) {
                            a_base_names_it = true;
                            break;
                        }
                    }
                    _ => could_not_read_a_base = true,
                }
            }
            if could_not_read_a_base || a_base_names_it {
                continue;
            }
            context.add_in_the_file(
                DiagnosticCode::FinalMisplaced,
                *name_range,
                name.clone(),
                t!(
                    "`%{name}` says `final` but no base of `%{class}` declares it — `final` may only be applied to a virtual function",
                    name = name,
                    class = class
                ),
            );
        }
    }
}
