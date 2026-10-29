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
        //
        // **The two halves are timed apart, because they fail for different reasons.** Measured on
        // `LuaSyntaxTree.cpp`: collecting 74 accesses takes **262 µs** and the loop over them **98 ms** — so this
        // check is not a tree walk that got slow, it is 74 type questions (`type_of` per access, 6 inference
        // questions each), and the work is in the engine. Cheap when off; see `docs/interactive-cost.md`.
        let traced = crate::probes::timing();
        let started = traced.then(std::time::Instant::now);
        let accesses = context.member_accesses();
        let collected = started.map(|started| started.elapsed());
        let mut judged = 0usize;
        for (_node, access) in accesses {
            // **Nothing after the operator** is a file being typed at — the completion path's business, not this
            // layer's: there is no name to have an opinion about yet.
            if access.member.is_empty() {
                continue;
            }

            let crate::Known::Yes((object, _)) = context.type_of(&access.object) else {
                continue;
            };

            // **A dependent type has no member list to check against, and asking for one invents an answer.**
            //
            // `template <class C, class Fn> constexpr int f(Fn op) { C x{42}; op(x); return x.n; }` — `x` has type
            // `C`, and `C` is a template parameter. Every step below is individually correct and the conclusion is
            // wrong: `members_of("C")` finds nothing written in `C` (a parameter declares no members), then finds a
            // declaration whose qualified name is `C` — **the parameter itself**, recorded as `DeclKind::Type`, the
            // kind a class gets — and answers "that class was read and has nothing in it". `x.n` is then reported as
            // a member of a class with no `n`.
            //
            // That is the false positive this layer is written against, on the construct a compiler most refuses to
            // diagnose: what `C` is, and therefore whether it has an `n`, is decided at instantiation. See
            // [`crate::sema::model::SemanticModel::names_a_template_parameter`] for why the question is asked of the
            // scope tree rather than of the index.
            if object.class_name().is_some_and(|class| {
                context
                    .model
                    .names_a_template_parameter(class, access.object.text_range().start().into())
            }) {
                continue;
            }

            // **An arrow is followed, not skipped.**
            //
            // Measured on EmmyLuaCodeStyle: `_session->Send(std::move(message))` was reported as "`_session` has no
            // member `Send`" — the receiver is a `std::shared_ptr<Session>`, whose member list is `shared_ptr`'s, and
            // the member the program means is behind `operator->`. That measurement is why an arrow used to say
            // nothing at all. Following it needs the pointee's type, which the engine now answers for completion
            // (`ptr->` on a pointer, a smart pointer's `operator->`, a class's `element_type`), so the same rule is
            // asked here.
            //
            // **Only when a pointer was actually followed.** A class that is not pointer-like and declares no
            // `operator->` leaves `followed_a_pointer` false — `p->size` on a plain `Widget` is already ill-formed
            // code, and a second finding about its members would be noise.
            let class = if access.arrow {
                match context.model.the_class_after_an_arrow(&object) {
                    Some((reached, true)) => reached,
                    _ => continue,
                }
            } else {
                object.class_name().unwrap_or_default().to_string()
            };

            let crate::Known::Yes(members) = context.model.members_of(&class) else {
                // Not known to be a class, or known and never read — see the module documentation.
                continue;
            };

            judged += 1;
            if members
                .members
                .iter()
                .any(|member| member.fact.name == access.member)
            {
                continue;
            }

            // **A partial list is not evidence.** `MemberList::unlisted` names the bases whose members could not be
            // read — a base out of the index, a base reached through a macro, a base whose own summary was not built —
            // and a member inherited from one of those is *missing from the list* without being absent from the class.
            //
            // Measured on a real project: `std::cin.read(…)` was reported as "`std::cin` has no member `read`", while
            // the hover, which asks the same index a different way, found `basic_istream::read` at once. The two
            // answered differently because this check treated "not in the part I read" as "not there". The list is
            // only an answer when it is complete.
            if !members.unlisted.is_empty() {
                continue;
            }

            // **What the list held, and what the index holds about the name** — the pair that separates "the class
            // has no such member" from "the index never recorded it". The second number is what showed that
            // `std::vector::erase` was missing from the *facts*, not from the check.
            if crate::probes::members() {
                let index = context.model.index();
                eprintln!(
                    "[undefined-field] `{}` -> `{}` ({} member(s), {} unlisted) — index: {} posting(s) of \
                     `{}`, {} in scope `{}`",
                    object.class_name().unwrap_or("?"),
                    access.member,
                    members.members.len(),
                    members.unlisted.len(),
                    index.name_postings(&access.member),
                    access.member,
                    index.scope_postings(object.class_name().unwrap_or("")),
                    object.class_name().unwrap_or("?"),
                );
            }
            // The message names what the reader wrote rather than a rendering of the type: `w.weight` is what is on
            // their screen, and a type pretty-printed by this layer would be a second spelling to disagree with.
            context.add(
                DiagnosticCode::UndefinedField,
                access.member_range,
                access.member.clone(),
                t!(
                    "`%{object}` has no member `%{member}`",
                    object = access.object.text().to_string().trim(),
                    member = access.member
                ),
            );
        }
        if let (true, Some(started), Some(collected)) = (traced, started, collected) {
            eprintln!(
                "[undefined-field] collected {collected:?}, judged {judged} access(es) with a list, \
                 loop took {:?}",
                started.elapsed() - collected
            );
        }
    }
}
