//! **`pure-virtual-not-overridden`** — a class that implements some of its base's pure virtuals but not all.
//!
//! ```cpp
//! struct Shape {
//!     virtual void draw() = 0;
//!     virtual double area() = 0;
//! };
//!
//! struct Circle : Shape {
//!     void draw() override;      // written
//! };                             // `area` is not: `Circle` is still abstract, and `Circle c;` will not compile
//! ```
//!
//! Being abstract is legal, and this rule does **not** report an abstract class — a base that declares `= 0` and
//! nothing else is exactly how an interface is spelled. What it reports is the shape that reads as an oversight: a
//! class that wrote `override` for some of a base's pure virtuals and left the rest, so a reader believes it is
//! concrete and the next object of it does not compile. Hence `Warning` rather than `Error`, and hence the condition
//! that the class **overrides at least one** — a derived class that overrides none is a declaration of a new abstract
//! class, and that is a decision, not a mistake.
//!
//! # Only what is certain
//!
//! ```text
//! the class overrides something, and a pure virtual is left unoverridden  -> report
//! every pure virtual is overridden                                        -> silent
//! the class overrides nothing                                             -> silent (a deliberate abstract class)
//! a base nobody read                                                      -> silent
//! ```
//!
//! The override is matched by **name and depth**, the same comparison [`super::abstract_class_instantiated`] makes and
//! for the same reason: the summary records no parameter list for a function, so a signature cannot be compared.

use super::{Check, CheckContext, DiagnosticCode};

/// **`pure-virtual-not-overridden`** — see the module documentation.
pub struct PureVirtualNotOverridden;

impl Check for PureVirtualNotOverridden {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::PureVirtualNotOverridden];

    fn check(context: &mut CheckContext<'_>) {
        // **The classes that wrote an `override`**, which is the sign that the class is meant to be concrete. Cloned
        // because a finding needs `&mut context` while the declarations borrow it.
        let mut tried: Vec<(String, cpp_parser::SourceRange)> = Vec::new();
        for fact in &context.summary.declarations {
            if fact.kind != crate::DeclKind::Function
                || !fact.modifiers.contains(crate::DeclModifiers::OVERRIDE)
            {
                continue;
            }
            let Some(class) = fact.scope.as_deref() else {
                continue;
            };
            let Some(class_fact) = context.summary.declarations.iter().find(|declared| {
                declared.kind == crate::DeclKind::Type && declared.name == class
            }) else {
                continue;
            };
            if !tried.iter().any(|(name, _)| name == class) {
                tried.push((class.to_string(), class_fact.name_range));
            }
        }

        for (class, name_range) in tried {
            let crate::Known::Yes(list) = context.model.members_of(&class) else {
                continue;
            };
            // **Every pure virtual the hierarchy shows**, minus the ones something below already implemented.
            let mut missing: Vec<String> = Vec::new();
            for member in &list.members {
                if member.fact.kind != crate::DeclKind::Function
                    || !member
                        .fact
                        .modifiers
                        .contains(crate::DeclModifiers::PURE)
                {
                    continue;
                }
                let overridden = list.members.iter().any(|other| {
                    other.fact.kind == crate::DeclKind::Function
                        && other.fact.name == member.fact.name
                        && other.depth < member.depth
                        && !other.fact.modifiers.contains(crate::DeclModifiers::PURE)
                });
                if !overridden && !missing.contains(&member.fact.name) {
                    missing.push(member.fact.name.clone());
                }
            }
            if missing.is_empty() {
                continue;
            }
            let names = missing
                .iter()
                .take(3)
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let more = if missing.len() > 3 {
                format!(" and {} more", missing.len() - 3)
            } else {
                String::new()
            };
            context.add_in_the_file(
                DiagnosticCode::PureVirtualNotOverridden,
                name_range,
                class.clone(),
                t!(
                    "`%{class}` overrides a pure virtual function of its base but not %{names}%{more} — it is still abstract, so no object of it can be constructed",
                    class = class,
                    names = names,
                    more = more
                ),
            );
        }
    }
}
