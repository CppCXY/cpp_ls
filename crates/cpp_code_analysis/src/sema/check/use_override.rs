//! `modernize-use-override`, from clang-tidy: **a function that overrides should say so.**
//!
//! ```cpp
//! struct Base { virtual void f(); };
//! struct Derived : Base { void f(); };       // this check says: void f() override;
//! ```
//!
//! `override` is not decoration: it turns a silent non-override into a compile error. Rename `Base::f`, or change its
//! parameter list, and `Derived::f` stops overriding — with no diagnostic under the old spelling, and a call that
//! silently runs the base's version under the new one.
//!
//! # How it is found, and what it deliberately does not do
//!
//! Over a class **with bases**, a member function whose declaration writes `virtual` and neither `override` nor `final`, and
//! **whose name a base actually declares** — the last clause is what keeps a freshly written virtual that overrides nothing
//! out of the report. `members_of` is the query that answers it, and it is why this rule needs the index rather than the
//! file's own text.

use super::{Check, CheckContext, DiagnosticCode};

pub struct UseOverride;

impl Check for UseOverride {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UseOverride];

    fn check(context: &mut CheckContext<'_>) {
        // Cloned because a finding needs `&mut context` while the declarations borrow it.
        let declarations: Vec<(String, Vec<String>)> = context
            .summary
            .declarations
            .iter()
            .filter(|fact| fact.kind == crate::DeclKind::Type && !fact.bases.is_empty())
            .map(|fact| (fact.name.clone(), fact.bases.clone()))
            .collect();

        // **One `members_of` per base, not one per member.** The first version asked for a base's members inside the member
        // loop — `members × bases` lookups per class, and the same base re-asked for every class that derives from it. On a
        // project of 1191 files that is the difference between a cache and a query storm; the census's 586 s of checks is the
        // number this addresses.
        let mut base_members: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();

        for (class, bases) in declarations {
            let members: Vec<(String, String, usize)> = context
                .summary
                .declarations
                .iter()
                .filter(|fact| {
                    fact.kind == crate::DeclKind::Function && fact.scope.as_deref() == Some(class.as_str())
                })
                .filter_map(|fact| {
                    let written = context
                        .source
                        .get(fact.range.start_offset..fact.range.end_offset())?
                        .to_string();
                    Some((fact.name.clone(), written, fact.name_range.start_offset))
                })
                .collect();

            for (name, written, offset) in members {
                // **The member does not have to write `virtual`** — an override need not repeat it, and requiring it was
                // the first version's bug. What matters is that it says neither `override` nor `final`.
                if written.contains("override") || written.contains("final") {
                    continue;
                }
                // **Does a base actually declare it?** Only then is this an override that forgot to say so. Answered from the
                // cache: a base's member names are the same for every class that derives from it.
                let mut overrides = false;
                for base in &bases {
                    if !base_members.contains_key(base) {
                        let names = match context.model.members_of(base) {
                            crate::Known::Yes(list) => list
                                .members
                                .iter()
                                .map(|member| member.fact.name.clone())
                                .collect(),
                            _ => Vec::new(),
                        };
                        base_members.insert(base.clone(), names);
                    }
                    if base_members
                        .get(base)
                        .is_some_and(|names| names.iter().any(|declared| declared == &name))
                    {
                        overrides = true;
                        break;
                    }
                }
                if !overrides {
                    continue;
                }
                let range = cpp_parser::SourceRange::new(offset, name.len());
                let _ = range;
                context.add(
                    DiagnosticCode::UseOverride,
                    cpp_parser::SourceRange::new(offset, name.len()),
                    name.clone(),
                    format!(
                        "`{name}` overrides a base's virtual and does not say so — write `override`, which turns a \
                         silent non-override into a compile error"
                    ),
                );
            }
        }
    }
}