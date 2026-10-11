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
//!   Known::Yes       the member list was read and holds the name         -> nothing to say
//!   Known::No        the member list was read and does not hold the name -> report
//!   Known::Unknown   the type is not known, or its members were never read — the ordinary case for a type from a
//!                    header nobody read                                 -> report NOTHING
//! ```
//!
//! The third is why this check is quiet on almost every file: a member access on `std::string` asks a question
//! whose answer is in a header the index may not hold, and a check that reported it would be reporting that *we*
//! have not read something as though the *program* were wrong. That is the failure mode this layer is written
//! against — see [`super`]'s module documentation.
//!
//! # What is asked, and of whom
//!
//! A `.` or `->` whose object has a type this file can see: the type comes from [`CheckContext::type_of`], the
//! member list from the model. A chain like `a.b.c` therefore asks about `a.b` first, and is only judged when that
//! answer is a type whose member list was read.


use super::{Check, CheckContext, DiagnosticCode};

/// **`undefined-field`** — see the module documentation.
pub struct UndefinedField;

impl Check for UndefinedField {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UndefinedField];

    fn check(context: &mut CheckContext<'_>) {
        // **Every member access in this file's own text**, by the sema layer's definition — see
        // `CheckContext::member_accesses`. This check used to filter on `MemberExpr`, a kind this grammar does not
        // produce for a dot: measured with `cpp_dump`, `w.secret` is an `IndexExpr`. It had therefore never reported
        // anything about `a.b` since it was written, while compiling and looking correct.
        for (_node, access) in context.member_accesses() {
            // **Nothing after the operator** is a file being typed at — the completion path's business, not this
            // layer's: there is no name to have an opinion about yet.
            if access.member.is_empty() {
                continue;
            }

            let crate::Known::Yes((object, _)) = context.type_of(&access.object) else {
                continue;
            };
            let crate::Known::Yes(members) = context.model.members_of(&object.class_name().unwrap_or_default())
            else {
                // Not known to be a class, or known and never read — see the module documentation.
                continue;
            };

            if members
                .members
                .iter()
                .any(|member| member.fact.name == access.member)
            {
                continue;
            }

            // The message names what the reader wrote rather than a rendering of the type: `w.weight` is what is on
            // their screen, and a type pretty-printed by this layer would be a second spelling to disagree with.
            context.add(
                DiagnosticCode::UndefinedField,
                access.member_range,
                access.member.clone(),
                format!(
                    "`{}` has no member `{}`",
                    access.object.text().to_string().trim(),
                    access.member
                ),
            );
        }
    }
}



