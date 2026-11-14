//! **`abstract-class-instantiated`** — a by-value variable whose class type is abstract.
//!
//! ```cpp
//! struct Shape {
//!     virtual void draw() = 0;
//! };
//!
//! void f() {
//!     Shape s;        // error: `Shape` is abstract
//!     Shape *p;       // fine: a pointer to an abstract class is what a base-class pointer is for
//!     Shape &r = s;   // fine: and so is a reference
//! }
//! ```
//!
//! [class.abstract] makes an object of an abstract class ill-formed: the class has a pure virtual function that
//! nothing overrides, so there is no body to call through it. The program does not build, which is why this is an
//! `Error` rather than a hint.
//!
//! # How "abstract" is answered
//!
//! ```text
//! a member with `= 0` that no member of the same name and a smaller depth overrides   -> abstract
//! every pure virtual overridden by a non-pure member of the same name                -> concrete
//! a class the index never read                                                       -> not judged
//! ```
//!
//! The member list is the index's (`members_of`), so an override in a header this file includes counts, and the
//! **name** is what is compared — a signature would need parameter types the summary does not record for a function,
//! and an override with a different signature would be a name that *looks* overridden. That is the direction this rule
//! deliberately errs in: a class it wrongly calls concrete is silent, while a class it wrongly called abstract would
//! report correct code.
//!
//! # What a variable is here
//!
//! A fact whose **type spelling is a class name** — no `*`, `&`, `[` or `(`, and no template arguments, because
//! `Widget<int>` is a different class from `Widget` and the summary stores one spelling for both. A type the index
//! does not know as a class is silence: `auto`, a typedef of a standard type, a type from a header nobody read.

use super::{Check, CheckContext, DiagnosticCode};

/// **`abstract-class-instantiated`** — see the module documentation.
pub struct AbstractClassInstantiated;

impl Check for AbstractClassInstantiated {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::AbstractClassInstantiated];

    fn check(context: &mut CheckContext<'_>) {
        // **One answer per class**, because the same base is instantiated many times in a file and the member list
        // is the expensive part (`members_of` is cached, but the walk over it is not).
        let mut known: std::collections::BTreeMap<String, Option<bool>> =
            std::collections::BTreeMap::new();

        let variables: Vec<(String, String, cpp_parser::SourceRange)> = context
            .summary
            .declarations
            .iter()
            .filter(|fact| fact.kind == crate::DeclKind::Variable)
            .filter_map(|fact| {
                let class = a_class_name(fact.type_of.as_deref()?)?;
                Some((class, fact.name.clone(), fact.name_range))
            })
            .collect();

        for (class, name, name_range) in variables {
            let is_abstract = *known.entry(class.clone()).or_insert_with(|| {
                let crate::Known::Yes(list) = context.model.members_of(&class) else {
                    return None;
                };
                // **Every pure virtual has to be overridden** for the class to be concrete: a class with two and one
                // override is still abstract.
                let mut every_pure_is_overridden = true;
                for member in list.members.iter().filter(|member| {
                    member.fact.kind == crate::DeclKind::Function
                        && member
                            .fact
                            .modifiers
                            .contains(crate::DeclModifiers::PURE)
                }) {
                    let overridden = list.members.iter().any(|other| {
                        other.fact.kind == crate::DeclKind::Function
                            && other.fact.name == member.fact.name
                            && other.depth < member.depth
                            && !other.fact.modifiers.contains(crate::DeclModifiers::PURE)
                    });
                    if !overridden {
                        every_pure_is_overridden = false;
                        break;
                    }
                }
                Some(!every_pure_is_overridden)
            });            if is_abstract == Some(true) {
                context.add_in_the_file(
                    DiagnosticCode::AbstractClassInstantiated,
                    name_range,
                    name.clone(),
                    t!(
                        "`%{name}` has type `%{class}`, which is abstract — a pure virtual function of it is declared `= 0` and nothing overrides it, so the class cannot be instantiated",
                        name = name,
                        class = class
                    ),
                );
            }
        }
    }
}

/// **The class a type spelling names, when an object of it can be constructed.**
///
/// `None` for a pointer, a reference, an array, a function type, a template-id and anything that is not one name:
/// each is either legal for an abstract class (a pointer, a reference) or not a class this rule can be sure of.
fn a_class_name(spelling: &str) -> Option<String> {
    let spelling = spelling.trim();
    if spelling.is_empty()
        || spelling.contains(['*', '&', '[', '(', ')', '<', '>', ',', '.'])
    {
        return None;
    }
    let name = spelling.rsplit("::").next()?.trim();
    if name.is_empty() || !name.starts_with(|first: char| first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    (!spelling.contains(' ')).then(|| name.to_string())
}
