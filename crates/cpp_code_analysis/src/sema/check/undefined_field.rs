//! **`undefined-field`**: a member access on a type that does not declare that member.
//!
//! ```cpp
//! struct Widget { int size; };
//!
//! Widget w;
//! w.size;      // fine
//! w.weight;    // `Widget` has no `weight`
//! ```
//!
//! # Only a definite "no"
//!
//! The question "does this type have this member" has three answers, and only one of them is a complaint:
//!
//! ```text
//!   Known::Yes   the member list was read and holds the name            -> nothing to say
//!   Known::No    the member list was read and does not hold the name    -> report
//!   Known::Unknown  the type is not known, or is known and its members were never read — the ordinary case
//!                   for a type from a header nobody read                -> report NOTHING
//! ```
//!
//! The third is why this check is quiet on almost every file: a member access on `std::string` asks a question
//! whose answer is in a header the index may not hold, and a check that reported it would be reporting that *we*
//! have not read something as though the *program* were wrong. That is the failure mode this layer is written
//! against — see the module documentation of [`super`].
//!
//! # Which accesses are asked about
//!
//! A `.` or `->` whose object has a type this file can see. The object's type comes from
//! [`CheckContext::type_of`], the member list from the model — so a chain like `a.b.c` asks about `a.b` first and
//! is only judged when that answer is a type with a member list.

use cpp_parser::CppSyntaxKind;

use super::{Check, CheckContext, DiagnosticCode};

/// **`undefined-field`** — see the module documentation.
pub struct UndefinedField;

impl Check for UndefinedField {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UndefinedField];

    fn check(context: &mut CheckContext<'_>) {
        for access in context
            .tree()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MemberExpr)
        {
            let Some(access) = crate::sema::resolve::member_access_of(&access) else {
                continue;
            };
            // The name being looked for, as the file writes it. A member access with nothing after the operator is
            // a file being typed at — the completion path's business, not this layer's.
            if access.name.is_empty() {
                continue;
            }

            let known = context.type_of(&access.object);
            let crate::Known::Yes((object, _)) = known else {
                continue;
            };

            let crate::Known::Yes(members) = context.model.members_of(&object.spelling()) else {
                // Not known to be a class, or known and never read — see the module documentation.
                continue;
            };

            // **`Known::No` and nothing else.** `members_of` answering `Yes` with a list is the only state in which
            // "the name is not in it" is a fact about the program.
            if members
                .members
                .iter()
                .any(|member| member.fact.name == access.name)
            {
                continue;
            }

            context.add(
                DiagnosticCode::UndefinedField,
                access.name_range,
                access.name.clone(),
                format!(
                    "`{}` has no member `{}`",
                    object.spelling(),
                    access.name
                ),
            );
        }
    }
}
