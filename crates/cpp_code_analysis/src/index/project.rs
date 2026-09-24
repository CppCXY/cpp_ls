//! Many files at once: the summaries, the edges between them, and the queries that need both.
//!
//! [`crate::index`] turns one file into a [`FileSummary`]. This module is what holds a project's worth of them
//! and answers the questions that cross a file boundary — which is the whole reason an index exists, and the
//! layer `docs/index-design.md` calls `resolve`.
//!
//! # Why the reverse map is built here and not stored
//!
//! A summary stores the includes its own file *writes* — direct edges, one per `#include`. The map from a header
//! back to the files that include it is therefore **derived**, and it is derived here, in memory, from the
//! summaries as they are loaded. Writing it to disk would be storing a conclusion: it is a fact about the graph
//! rather than about any file, so it would have no single file to go stale with, and the first inconsistency
//! between it and the summaries would be invisible. Rebuilding it costs one pass over the includes, which is
//! what the summaries are for.
//!
//! # What visible means here
//!
//! ```text
//! a.h  declares Widget
//! b.h  #include "a.h"
//! c.cpp #include "b.h"      -> Widget is visible in c.cpp
//! d.cpp (nothing)           -> Widget is not visible in d.cpp, even though it is in the project
//! ```
//!
//! So a name is visible in a file when the file **transitively includes** the file that declares it. That is a
//! graph reachability question, and it is answered without re-reading anything: the edges are in the summaries.
//!
//! # The one case this cannot decide, and what it says instead
//!
//! An `#include` written inside an `#if` is a fact about the text, not about a compilation: whether the compiler
//! took that branch depends on macros the index does not have. So a declaration reached only through a
//! **guarded** include is reported as [`Known::Unknown`] rather than as visible or invisible — the same rule the
//! rest of the crate follows, and the reason [`IncludeFact`](crate::summary::IncludeFact) carries a guard at all.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::include::paths::normalize_path;
use crate::summary::{DeclFact, FactGuard, FileSummary, MacroFact};
use crate::symbol::{Known, UnknownReason};

/// How many files a visibility walk will cross before giving up.
///
/// A backstop rather than a policy: real include graphs are shallow (the walker's own limit is
/// [`crate::MAX_INCLUDE_DEPTH`]), and a corpus that exceeds this is one whose summaries disagree with its
/// includes — which the visited set already handles. The number is here so that the *round* count of a
/// pathological graph cannot become an unbounded amount of work on a keystroke.
const MAX_VISIBILITY_DEPTH: usize = 128;

/// Which declaration something refers to, using **both** layers.
///
/// The entry point a feature should call, and the reason it exists rather than each caller composing the two:
/// C++ resolves a name in a fixed order, and getting that order wrong is a jump to the wrong file rather than an
/// error. The order is:
///
/// ```text
/// 1. this file's scopes        — a local shadows a header's declaration, always
/// 2. this file's own top-level — a declaration written here is not the header's
/// 3. the headers it includes   — reached through the include graph
/// ```
///
/// # What it takes, and why each half is a reference
///
/// `scopes` and `root` are the file's own analysis, which [`crate::build_scopes`] and the parser produce — the
/// caller has them because it just parsed the file. The index holds the *other* files. Neither is derivable from
/// the other: the index has no scopes for the open buffer, and the scopes know nothing outside it.
///
/// # The two reasons that reach step 3, and the one that does not
///
/// [`UnknownReason::NotDeclaredHere`] means the name is not in this file, which is exactly what step 3 is for.
/// [`UnknownReason::Ambiguous`] does **not** mean that: the name *is* here, more than once, and looking in the
/// headers would be answering a different question. So it stops.
///
/// Everything else stops too — a name that could not be read, a conditional the analysis cannot evaluate — for
/// the same reason: the failure is not "not here".
pub fn definition_across_files(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectDefinition> {
    match crate::sema::resolve::definition_at(scopes, root, offset) {
        Known::Yes(binding) => {
            // The scope the answer was reached *through*, when the cursor wrote one: `ns` for `ns::Widget`. A bare
            // name has none, and a leading `::` means the global name space, which is no scope at all.
            let scope = crate::sema::resolve::qualified_name_at(root, offset).and_then(|(written, _)| {
                written
                    .rsplit_once("::")
                    .map(|(scope, _)| scope.trim_start_matches("::").to_string())
                    .filter(|scope| !scope.is_empty())
            });

            // A definition answer can be a *local*: the cursor is inside the same body the name was declared in,
            // which is exactly when `definition_at` resolves it here rather than sending it to the index. Saying
            // so matters because the caller may hand this answer on to a consumer asking whether the name is
            // reachable elsewhere. Asked before the binding is moved, and from the tree that can answer it.
            let local = scopes.declares_a_local(binding.scope);

            return Known::Yes(ProjectDefinition::from_binding(path, binding, scope, local));
        }
        Known::Unknown(UnknownReason::NotDeclaredHere(name)) => {
            // The single-file layer has already established the spelling, so the project layer is asked about
            // exactly that name rather than re-reading the cursor.
            return index.definition(&name, path);
        }
        Known::Unknown(reason) => return Known::Unknown(reason),
        Known::No => {}
    }

    // `No` from the single-file layer means the offset is not in a name the scopes could place at all — on
    // punctuation, or past the end — so there is nothing to look up anywhere.
    Known::Unknown(UnknownReason::UnparsableName)
}

/// The answer to a cross-file question about a macro name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMacro {
    pub file: PathBuf,
    /// The `#define` or the `#undef` that settles what the name is at the point asked about.
    pub fact: MacroFact,
}

/// Where the macro name written at `offset` is defined — or that it is not a macro there.
///
/// The entry point for "go to definition" on a macro, and the counterpart of [`definition_across_files`]. It needs
/// no scope tree and no second layer, because a macro query is not a name lookup at all: it is a question about
/// **translation order** — which of the `#define`s and `#undef`s written before this point is the last one — and
/// the index already stores every one of them with the offset it was written at.
///
/// [`UnknownReason::UnparsableName`] when the offset is not on a name at all, which is the ordinary answer for
/// most cursor positions.
pub fn macro_across_files(
    index: &ProjectIndex,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectMacro> {
    match crate::sema::resolve::name_at(root, offset) {
        Some((name, _)) => index.macro_definition(&name, path, offset),
        None => Known::Unknown(UnknownReason::UnparsableName),
    }
}

/// A project's summaries, and the queries that need more than one of them.
///
/// Built incrementally: [`ProjectIndex::insert`] takes a summary that some other layer produced, which is what
/// keeps this type free of any opinion about parsing, caching or the filesystem.
#[derive(Debug, Default)]
pub struct ProjectIndex {
    /// The summaries, by normalized path.
    summaries: HashMap<String, FileSummary>,
    /// The paths in insertion order, so that a query over all of them is deterministic rather than
    /// `HashMap`-ordered. A definition jump that returned a different file on each run would be a bug that only
    /// shows up in a test that runs twice.
    order: Vec<String>,
    /// For each file, the files that include it. Derived from the summaries; see the module documentation.
    included_by: HashMap<String, BTreeSet<String>>,
}

    /// Which member a `obj.member` or `ptr->member` at `offset` names.
///
/// The first query in this crate that needs a **type**: `size` in `widget.size` is not looked up among the names
/// in scope, it is looked up *in the type of `widget`*. So the answer is built in three steps, each of which
/// already existed:
///
/// ```text
/// 1. read the shape          — the object expression and the member's spelling (sema::resolve)
/// 2. infer the object's type — its declaration's `type_of`, in this file or through the index
/// 3. look the member up      — as the qualified name `<type>::<member>`, which the index already matches
/// ```
///
/// Step 3 is why this was cheap to add: a declaration fact records the qualified spelling of the scope it was
/// written in, so `Widget::size` is a question the existing lookup answers. What is new is step 2, and it is the
/// beginning of the `infer` layer `docs/index-design.md` describes — deliberately narrow: the object has to be a
/// **name**, because inferring the type of an arbitrary expression is a different and much larger problem.
///
/// # The four answers
///
/// * `Yes` — the member, declared in the class the object's type names.
/// * `Unknown(UnknownType)` — the object's type could not be worked out: the object is not a plain name, or its
///   declaration says nothing about its type, or the type's name resolves to nothing.
/// * `Unknown(NotDeclaredHere)` — the type is known and the member is not in it. Not a definite no, for the
///   reason every other answer here is not: the class may be a base class, a template, or declared in a header
///   nobody indexed.
/// * `Unknown(UnparsableName)` — the offset is not on a member access at all.
pub fn member_across_files(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectDefinition> {
    let Some(access) = crate::sema::resolve::member_access_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    // `w.` with nothing after the operator is a member access with no member, and a *jump* needs something to jump
    // to. The question this answers is which declaration the written name refers to, and there is no written name —
    // so this is the same "nothing to look up" as a cursor on punctuation. That the shape is readable at all is
    // what the completion query needs, and it is why the tolerance lives in the shape reader rather than here.
    if access.member.is_empty() {
        return Known::Unknown(UnknownReason::UnparsableName);
    }

    // The type of the object, which is what decides which class the member is looked for in.
    let Known::Yes((written, _)) = type_of_expression(index, scopes, root, path, &access.object)
    else {
        let Known::Unknown(reason) =
            type_of_expression(index, scopes, root, path, &access.object)
        else {
            unreachable!("the first match established that this is an `Unknown`")
        };
        return Known::Unknown(reason);
    };

    let class = base_type_name(&written);
    if class.is_empty() {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    }

    let found = member_fact(index, scopes, root, path, class, &access.member);
    let Known::Yes((fact, file)) = found else {
        let Known::Unknown(reason) = found else {
            unreachable!("the first match established that this is an `Unknown`")
        };
        return Known::Unknown(reason);
    };

    Known::Yes(ProjectDefinition { file, fact })
}

/// Every member a type has: its own, and the ones it inherits.
///
/// The query a completion after `.` or `->` is built on, and the second one in this module that needs a **type**.
/// It is the *list* form of [`member_across_files`]: that one is given a member's name and answers where it is
/// declared, this one is given nothing but the type and answers what there is to name at all.
///
/// # The base chain is walked here, and never stored
///
/// The one design decision this query makes, and the reason it is a query rather than a field.
///
/// A summary keeps what each file **says**: `struct D : public B` is stored as the spelling `B`, and `D`'s
/// summary says nothing about `B`'s members. The alternative — resolving the bases when the summary is built and
/// writing `D`'s inherited members onto `D` — is what an index-shaped instinct reaches for, and it is wrong for
/// three reasons that all end in a stale answer with nothing on disk to contradict it:
///
/// ```text
/// 1. B gains and loses members          -> D's text and D's key are unchanged, and D's stored list is now wrong
/// 2. D's base list changes              -> caught, because D's text changed
/// 3. *which* B the name `B` means       -> unchanged in D's text, changed by a macro, an include or an
///                                          `#undef`, so D's stored list is wrong with nothing to catch it
/// ```
///
/// The third is the one that settles it: it leaves `D`'s text identical, so no per-file invalidation can see it.
/// Walking the chain at query time costs one lookup per base per query and cannot go stale, because nothing is
/// kept. `a_member_added_to_a_base_appears_without_reindexing_the_derived_class` is that argument as a test.
///
/// # The three things a list can be, and none of them is "these are all the members"
///
/// * `Yes(list)` — the members this analysis can see. `list.unlisted` names the bases that could not be listed
///   at all, so a consumer can tell a complete answer from a truncated one instead of guessing.
/// * `Unknown(NotDeclaredHere)` — nothing visible declares the type. Not `No`: the index holds a subset of the
///   translation unit, so a type from a header nobody indexed looks exactly like a type that does not exist.
/// * `Unknown(ConditionalCompilation)` — the type is only reachable through a guarded `#include`, so whether it
///   is here at all is not known.
///
/// # What it does with a name two bases declare
///
/// Both entries are listed and both are marked [`ProjectMember::ambiguous`]. Dropping one would be choosing, and
/// choosing is the answer the language refuses to give — the same fact [`member_across_files`] states as
/// `Unknown(Ambiguous)` for a single name, stated here as a property of a listed member, because a list with a
/// hole in it would be a worse answer than a list that says which entries are contested.
///
/// # What it deliberately does not do
///
/// * No `using` declarations and no virtual/override resolution. A `using Base::f;` in a derived class brings a
///   name in without declaring a member of its own, and nothing here models that yet.
/// * No access check. A `private` base's members are listed, because access is not in the facts — see
///   [`DeclFact::bases`] — and filtering on a guess would hide members a consumer can legitimately see.
/// * No instantiation. A template class lists the members it was written with; a base written `Base<int>` is
///   looked up as the class `Base`.
/// * No conditional region. A member of a class in the buffer comes back with [`FactGuard::Unconditional`]
///   whatever `#if` it is really in — see `fact_from_binding`. A class from the index does carry its regions.
pub fn members_of(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<MemberList> {
    // The spelling is normalized once, at the entry, for the same reason a base's is: a consumer feeding this from
    // `DeclFact.type_of` hands over what the file wrote — `const Widget&`, `::Widget`, `Base<int>` — and every one
    // of those parts is about the type's shape rather than about which class declares the members. Normalizing
    // here also means `declared_in` is a qualified spelling from the first level on, and levels cannot disagree.
    let class = base_type_name(class);

    let own = match direct_members(index, scopes, root, path, class) {
        Known::Yes(members) => members,
        Known::Unknown(reason) => return Known::Unknown(reason),
        // `direct_members` reports a name nothing declares as `Unknown(NotDeclaredHere)` rather than as `No` —
        // the index is a subset of the translation unit — so this arm exists for totality, not for a case.
        Known::No => return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class))),
    };

    let mut list = MemberList::default();
    let mut hidden: HashSet<String> = HashSet::new();
    let mut visited: HashSet<String> = HashSet::from([class.to_string()]);

    // Level 0 is the type's own body. It is not part of the walk below because its members are the ones that
    // *hide*, and a base is only reached after them.
    let mut own: Vec<ProjectMember> = own
        .into_iter()
        .map(|(file, fact)| ProjectMember {
            file,
            fact,
            declared_in: class.to_string(),
            depth: 0,
            ambiguous: false,
        })
        .collect();
    own.sort_by(|one, other| one.fact.name.cmp(&other.fact.name));
    mark_ambiguity(&mut own);
    hidden.extend(recorded_names(&own));
    list.members.extend(own);

    // Then outward, one level of bases at a time — which is the order C++ hides in: a base's member is hidden by
    // a same-named member of anything nearer, so a level that has already contributed a name removes it from
    // every level below.
    let mut level = bases_of(index, scopes, root, path, class)
        .value()
        .unwrap_or_default();
    let mut depth = 1;

    while !level.is_empty() {
        let mut found: Vec<ProjectMember> = Vec::new();
        let mut next: Vec<String> = Vec::new();

        for base in level {
            if !visited.insert(base.clone()) {
                continue;
            }

            let members = match direct_members(index, scopes, root, path, &base) {
                Known::Yes(members) => members,
                // A base nothing here can resolve. Its members are **missing from the list** rather than absent
                // from the type, and naming the base is what makes the gap actionable — the fix is an include
                // path or a file that was never indexed, not a different query.
                Known::Unknown(reason) => {
                    list.unlisted.push(UnlistedBase {
                        spelling: base,
                        reason,
                    });
                    continue;
                }
                Known::No => continue,
            };

            found.extend(members.into_iter().map(|(file, fact)| ProjectMember {
                file,
                fact,
                declared_in: base.clone(),
                depth,
                ambiguous: false,
            }));

            match bases_of(index, scopes, root, path, &base) {
                Known::Yes(further) => next.extend(further),
                Known::Unknown(reason) => list.unlisted.push(UnlistedBase {
                    spelling: base,
                    reason,
                }),
                Known::No => {}
            }
        }

        found.retain(|member| !hidden.contains(&member.fact.name));
        found.sort_by(|one, other| one.fact.name.cmp(&other.fact.name));
        mark_ambiguity(&mut found);
        hidden.extend(recorded_names(&found));
        list.members.extend(found);

        level = next;
        depth += 1;
    }

    Known::Yes(list)
}

/// Every declaration written **directly in** the class or namespace `class` names.
///
/// The class's own body first, from the file being edited, and then the index — the same two-layer split
/// [`direct_member`] makes, and for the same reason: a buffer that has never been saved has no summary, and a
/// class the buffer does not mention is only in the index.
///
/// [`Known::Yes`] with an empty list is a real answer — a class with nothing in it — and is deliberately not the
/// same as [`Known::Unknown`], which is what a name nothing declares produces. The two are told apart by asking
/// whether the *name* is declared, which is the one question that distinguishes "nothing written in it" from
/// "nothing here knows what it is".
///
/// A name that is not an identifier — a destructor, an operator, a conversion function — comes back with an empty
/// [`DeclFact::name`], exactly as the index stores it. It is a declaration that exists, so it is not dropped here;
/// a consumer that shows a list filters on the name it can print and reads the spelling from the source, which is
/// where it lives.
fn direct_members(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<(PathBuf, DeclFact)>> {
    if let Some(scope) = scopes.scope_with_qualified_name(class)
        && let Some(data) = scopes.scope(scope)
    {
        return Known::Yes(
            data.bindings
                .iter()
                .map(|binding| (path.to_path_buf(), fact_from_binding(root, class, binding)))
                .collect(),
        );
    }

    let found = index.declarations_in(class, path);
    if !found.is_empty() {
        return Known::Yes(
            found
                .into_iter()
                .map(|declaration| (declaration.file.clone(), declaration.fact.clone()))
                .collect(),
        );
    }

    // Nothing is written *in* it, so the name is either an empty class or no class at all. Which one is decided by
    // the declaration itself rather than by the absence of members: a fact whose own qualified name is the
    // spelling asked about is the class, and everything else that matched did so on its bare name.
    match index.definition(class, path) {
        Known::Yes(found) if found.fact.qualified_name() == class => Known::Yes(Vec::new()),
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::Yes(_) | Known::No => Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class))),
    }
}

/// The names in a level that take part in **hiding**: every member except the ones whose name is not recorded.
///
/// A destructor, an operator and a conversion function come back with an empty [`DeclFact::name`], because a fact
/// stores a lookup key rather than a spelling — the spelling lives in the source, and `~D` has no identifier in it.
/// There is therefore nothing to compare them by, and two of them are *not* one name: `~D` and `~B` differ by the
/// class they name. Leaving them out of the hiding rule is what keeps `~D` from hiding `~B` — a wrong answer
/// arrived at by treating a missing spelling as if it were a spelling. They are still **listed**, because they are
/// declarations that exist; see [`direct_members`].
fn recorded_names(members: &[ProjectMember]) -> impl Iterator<Item = String> + '_ {
    members
        .iter()
        .map(|member| member.fact.name.clone())
        .filter(|name| !name.is_empty())
}

/// Flag the members whose name another class at the same level also declares.
///
/// Level-scoped rather than list-scoped, and the difference is the language's: a base's member that a *derived*
/// class redeclares is hidden, so it never reaches this function, while two bases at the same level genuinely
/// leave the name unresolved — the same finding [`member_across_files`] reports as `Unknown(Ambiguous)`.
///
/// **Overloads are not ambiguity.** `void f(); void f(int);` declares one name once, in one class, and a consumer
/// that flagged it would refuse to complete a name the language resolves perfectly well. So what is counted is
/// the number of distinct declaring classes, not the number of declarations.
///
/// A member with no recorded name is skipped for the same reason it takes no part in hiding — see
/// [`recorded_names`] — and skipping it is the conservative direction: claiming two declarations are one contested
/// name would be a definite statement about a name this layer cannot read.
fn mark_ambiguity(members: &mut [ProjectMember]) {
    let mut declaring: HashMap<String, HashSet<String>> = HashMap::new();

    for member in members.iter().filter(|member| !member.fact.name.is_empty()) {
        declaring
            .entry(member.fact.name.clone())
            .or_default()
            .insert(member.declared_in.clone());
    }

    for member in members.iter_mut() {
        member.ambiguous = !member.fact.name.is_empty()
            && declaring
                .get(&member.fact.name)
                .is_some_and(|classes| classes.len() > 1);
    }
}

/// What a completion at a member access should offer: the type, its members, and where to put the chosen one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberCompletions {
    /// The type of the object, as the **lookup key** the members were found under — `Widget`, `ns::Widget`,
    /// `Base` for `Base<int>`.
    ///
    /// The key rather than the spelling the file wrote, because a consumer showing "members of `…`" has to name
    /// something the user can find, and `Base<int>` is a type no member is declared in. It is also the fastest
    /// thing to look at when the list is wrong.
    pub class: String,
    pub members: MemberList,
    /// The range the member's name occupies, which is what a client **replaces** with the chosen one.
    ///
    /// Empty and just past the operator for `w.`, which is the state this query exists for. One field for both
    /// states on purpose: an insert-at-a-point and a replace-a-prefix are the same edit, and a consumer that had
    /// to branch on which one it is would get it wrong exactly once — on the keystroke right after the dot.
    pub member_range: cpp_parser::SourceRange,
    /// What of the name is already written **before the cursor**: `si` for `w.si|`, and empty for `w.|`.
    ///
    /// Not the whole written member, which is why it is computed here rather than taken from the shape: a cursor
    /// in the *middle* of a name — `w.si|ze` — filters by `si` while [`MemberCompletions::member_range`] covers
    /// all of `size`, because the range is what gets replaced and the prefix is what gets matched. A consumer that
    /// used the whole spelling for both would offer nothing while the user is plainly typing.
    pub prefix: String,
}

/// The members a completion at `offset` should offer: `w.` and `w.si`, answered with `Widget`'s members.
///
/// The third query that needs a **type**, and the one the other two were building towards: [`members_of`] answers
/// "what does this type have" given a type, and this answers "what should be offered here" given a cursor. The
/// steps are all ones that already existed, which is the whole reason it is short:
///
/// ```text
/// 1. read the shape   — the object expression and what of the member is written (sema::resolve::member_access_at)
/// 2. infer the type   — type_of_expression, the recursive inference the member *lookup* already uses
/// 3. list the members — members_of, including the ones the bases declare
/// ```
///
/// # The state it exists for
///
/// `w.` — the operator typed and nothing after it. That is the keystroke that *asks* the question, and the parser
/// reads it as a member access with an empty member rather than as some other construct, so nothing here needs a
/// second shape reader or a re-parse of the line. The same answer covers `w.si`, where the work is only that
/// [`MemberCompletions::member_range`] is the written prefix instead of a point.
///
/// # The answers, and the one that matters most
///
/// * `Yes(completions)` — the members of the object's type.
/// * `Unknown(UnknownType)` — the object's type could not be worked out. **The common case in real code**, and
///   the honest one: `f().size` needs a return type computed, `(*p).size` needs a dereference followed, and
///   `arr[i].size` needs a subscript — see [`members_of`]'s note and `docs/index-design.md`. A consumer shows
///   nothing rather than offering the members of some other type.
/// * `Unknown(NotDeclaredHere)` — the object's type is a class nothing visible declares.
/// * `Unknown(ConditionalCompilation)` — that class is only reachable through a guarded `#include`.
/// * `Unknown(UnparsableName)` — the cursor is not on a member access at all.
pub fn member_completions_at(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<MemberCompletions> {
    let Some(access) = crate::sema::resolve::member_access_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    let written = match type_of_expression(index, scopes, root, path, &access.object) {
        Known::Yes((written, _)) => written,
        Known::Unknown(reason) => return Known::Unknown(reason),
        // The only `No` the type layer produces is "nothing says what this is", which is the same answer as an
        // expression it cannot type: there is nothing to list members of.
        Known::No => {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(
                access.object.text().to_string().trim(),
            )));
        }
    };

    let class = base_type_name(&written);
    if class.is_empty() {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    }

    match members_of(index, scopes, root, path, class) {
        Known::Yes(members) => Known::Yes(MemberCompletions {
            class: class.to_string(),
            members,
            member_range: access.member_range,
            prefix: written_before_the_cursor(&access, offset),
        }),
        Known::Unknown(reason) => Known::Unknown(reason),
        // `members_of` reports a name nothing declares as `Unknown(NotDeclaredHere)` rather than `No`, so this arm
        // is for totality and says the same thing it would.
        Known::No => Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class))),
    }
}

/// The part of the member's spelling that lies **before** the cursor.
///
/// Separate from the range because the two answer different halves of the same edit: the range is what the client
/// **replaces** with the chosen name — all of `size`, so nothing of the old spelling is left behind — while this
/// is what a server **matches** against the candidates, and a cursor inside a name has only typed the part before
/// it. Taking the whole spelling for both is what makes `w.si|ze` offer nothing.
///
/// The slice is by byte offset into the member's own text, so it is exactly the written prefix; a cursor that is
/// not on a character boundary (possible in an identifier with non-ASCII letters) falls back to the whole
/// spelling, which is the safe direction — a filter that matches too much still shows something.
fn written_before_the_cursor(access: &crate::sema::resolve::MemberAccess, offset: usize) -> String {
    let start = access.member_range.start_offset;
    let end = access.member_range.end_offset().min(offset);

    if end <= start {
        return String::new();
    }

    access
        .member
        .get(..end - start)
        .unwrap_or(&access.member)
        .to_string()
}

/// One name a completion can offer: where it is declared, and how far out it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferedName {
    pub file: PathBuf,
    pub fact: DeclFact,
    /// How many scopes out from the cursor the declaration was found: `0` is a name declared in the scope the
    /// cursor is in — a local, or the scope the `::` named — and larger numbers are enclosing scopes, which C++
    /// only considers once the inner ones have been searched. The order of [`NameCompletions::names`].
    ///
    /// Recorded rather than left to be recomputed, for the reason [`ProjectMember::depth`] is: a consumer showing
    /// locals above globals, or grouping by where a name came from, would otherwise have to reconstruct the walk it
    /// was just handed the result of.
    pub depth: usize,
}

/// What a completion **in a name** should offer: the scope that was listed, and its names.
///
/// The sibling of [`MemberCompletions`], and the other half of "what can be typed here": that one answers after a
/// `.` or `->`, where the answer is a type's members; this one answers in a name position, where the answer is
/// what is visible — after a `::` (one scope), or with no qualifier at all (the scopes the cursor is inside, from
/// the innermost outward).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameCompletions {
    /// The scope the names were listed from, as the **lookup key** rather than as the file spelled it: `ns`,
    /// `ns::Widget`, `::` for the global name space, or empty when the cursor wrote no qualifier — there the names
    /// come from the scopes the cursor is inside rather than from one scope.
    pub scope: String,
    /// The names, innermost first and **deduplicated by name**: a declaration in an inner scope hides the same
    /// name in an outer one, which is what C++ does and what keeps a local `count` from being offered beside the
    /// header's. Two declarations of one name in the *same* scope are both kept — that is an overload, not a
    /// choice.
    pub names: Vec<OfferedName>,
    /// What of the name is already written **before the cursor**: `Wid` for `ns::Wid|`, empty for `ns::|`.
    ///
    /// The counterpart of [`MemberCompletions::prefix`], and separate from the range for the same reason.
    pub prefix: String,
    /// The range a client **replaces** with the chosen name: the written segment, or an empty range at the cursor
    /// when the segment has only just begun.
    pub name_range: cpp_parser::SourceRange,
}

/// The names a completion at `offset` should offer: `ns::`, `Widget::`, `::`, and a bare name.
///
/// # The four positions, and one query for all of them
///
/// ```text
/// ns::            one scope: the names written directly in `ns`
/// Widget::        one scope, and its **bases** — a typedef a base declares is nameable through the derived class
/// ::Widget        the global name space only, which is what the leading `::` asks for
/// loc             no qualifier: every name visible from the cursor, innermost scope first
/// ```
///
/// The first three are one question — list the declarations written in a scope — and the fourth is the same
/// question asked of a *chain* of scopes. What differs is only where the scopes come from, which is why this is one
/// query rather than four.
///
/// # The two layers, and when they are merged rather than chosen between
///
/// The file's own scope tree is asked first, because a buffer that has never been saved has no summary — the split
/// every cross-file query in this crate makes. What happens next depends on what the scope *is*, and the difference
/// is the language's:
///
/// * a **class** is defined once, so its members come from one place: the buffer's scope if it has one, the index
///   otherwise. Merging would list the same member twice;
/// * a **namespace** can be reopened, and a file that writes `namespace ns { … }` and includes a header that does
///   the same has both sets of names. So those are merged, the buffer winning a name they both declare;
/// * the **global name space** is a namespace, and is the namespace every included header writes into — the same
///   merge, with the file scope of the buffer as the first contributor.
///
/// # What it will not offer
///
/// A **local** declaration of another file is never in the list. The index cannot place a local in the function it
/// belongs to, so a name lookup there could only guess — and the guess would be wrong in the common case rather
/// than the rare one: the standard library's headers alone declare thousands of locals called `__first` and `n`.
/// See [`DeclFact::local`]. Locals of the file being edited *are* offered, from its own scope tree, which is the
/// only thing that knows which body they are in.
///
/// # The answers
///
/// * `Yes(completions)` — the names, which may be an empty list for a scope that is declared and empty.
/// * `Unknown(UnparsableName)` — the cursor is not in a name or a qualifier: on punctuation, on a keyword, or past
///   the end of what could be one.
/// * `Unknown(NotDeclaredHere)` — the `::` named a scope nothing here declares, so there is nothing to list and no
///   way to say it is empty.
/// * `Unknown(ConditionalCompilation)` — the scope is declared, but only in a file reached through a guarded
///   `#include`, so whether these names are visible depends on macros this layer does not have.
pub fn name_completions_at(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<NameCompletions> {
    let Some(position) = crate::sema::resolve::name_position_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    let names = if position.scope.is_empty() {
        match visible_names(index, scopes, root, path, offset) {
            Known::Yes(names) => names,
            Known::Unknown(reason) => return Known::Unknown(reason),
            Known::No => return Known::Unknown(UnknownReason::UnparsableName),
        }
    } else {
        match names_in_a_scope(index, scopes, root, path, &position.scope) {
            Known::Yes(names) => names,
            Known::Unknown(reason) => return Known::Unknown(reason),
            Known::No => return Known::Unknown(UnknownReason::UnparsableName),
        }
    };

    Known::Yes(NameCompletions {
        scope: position.scope,
        names,
        prefix: position.written,
        name_range: position.range,
    })
}

/// Every name visible from a cursor with no qualifier written: the scope chain, then the index.
///
/// The order is C++'s: the scope the cursor is in, then each enclosing scope outward, and only then the names other
/// files contribute. Each step's own answer is already a list — the tree's bindings, a class's members including
/// its bases, a namespace's declarations across the project — so this walks the chain and hands each depth to the
/// function that knows how to list it.
fn visible_names(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<Vec<OfferedName>> {
    let Some(innermost) = scopes.scope_at(offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    let chain = scopes.scope_chain(innermost);
    let mut names: Vec<OfferedName> = Vec::new();

    for (depth, scope) in chain.iter().copied().enumerate() {
        let Some(data) = scopes.scope(scope) else {
            continue;
        };

        // The file scope is the global name space, and it is the one scope whose names other files contribute to
        // as well — every included header is written there. It is listed below, once, rather than here.
        if data.kind == crate::ScopeKind::TranslationUnit {
            continue;
        }

        // A **class** in the chain — the cursor is inside a member function of it — brings its members, bases
        // included: `size` is nameable inside `Widget::g` without a qualifier, and so is anything a base declares.
        if matches!(data.kind, crate::ScopeKind::Class | crate::ScopeKind::Enum)
            && let Some(class) = scopes.qualification_prefix_of(scope)
        {
            let members = match names_of_a_class(index, scopes, root, path, &class) {
                Known::Yes(members) => members,
                Known::Unknown(reason) => return Known::Unknown(reason),
                Known::No => Vec::new(),
            };
            names.extend(offered(members, depth));
            continue;
        }

        // Anything else in the chain is a body, a block or a lambda: its own bindings, which the tree has.
        names.extend(offered(
            bindings_of(root, path, &data.bindings, None),
            depth,
        ));

        // …and for a **namespace**, the names other files write into it. A namespace is reopened by every file
        // that mentions it, so the buffer's list is never the whole answer.
        if let Some(prefix) = scopes.qualification_prefix_of(scope) {
            names.extend(offered(declarations_in(&prefix, index, path), depth));
        }
    }

    // The global name space: the buffer's own file scope, and every file it includes. One step past the chain, so
    // that the depth stays an ordinary count of how far the lookup walked — the file scope is a scope like any
    // other, it is just not one the chain walked through.
    names.extend(global_offers(index, scopes, root, path, chain.len()));

    sort_and_hide(&mut names);
    Known::Yes(names)
}

/// The names at **file scope**, ready to offer: the buffer's own and every included file's, deduplicated.
///
/// A helper rather than two lines at each call site because the deduplication is the part that is easy to forget
/// and impossible to notice: the buffer's file-scope names and the index's are the *same* declarations whenever
/// the file being edited has been saved, so a list that skipped this step offers every global name twice.
fn global_offers(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    depth: usize,
) -> Vec<OfferedName> {
    let mut names = offered(global_names(index, scopes, root, path), depth);
    sort_and_hide(&mut names);
    names
}

/// The names written directly in one `::`-qualified scope.
fn names_in_a_scope(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    scope: &str,
) -> Known<Vec<OfferedName>> {
    // The global name space has no spelling of its own: `::` is what the file writes, and `None` is what a
    // declaration at file scope records.
    if scope == "::" {
        return Known::Yes(global_offers(index, scopes, root, path, 0));
    }

    // A leading `::` on a longer path — `::ns::Widget` — asks for the *global* one, and the spelling to look up is
    // the rest: a fact records `ns::Widget` and never `::ns::Widget`, for the same reason.
    let spelling = scope.trim_start_matches("::");

    let in_the_tree = scopes.scope_with_qualified_name(spelling);
    let kind = in_the_tree.and_then(|id| scopes.scope(id)).map(|scope| scope.kind);

    let mut names: Vec<OfferedName> = Vec::new();

    match kind {
        // A class is defined once, so its members come from one place — and the walk over its bases is
        // `members_of`'s job rather than this query's.
        Some(crate::ScopeKind::Class | crate::ScopeKind::Enum) => {
            let members = match names_of_a_class(index, scopes, root, path, spelling) {
                Known::Yes(members) => members,
                Known::Unknown(reason) => return Known::Unknown(reason),
                Known::No => Vec::new(),
            };
            names.extend(offered(members, 0));
        }
        // A namespace can be reopened, so the buffer's names and the project's are both part of the answer.
        Some(_) => {
            if let Some(data) = in_the_tree.and_then(|id| scopes.scope(id)) {
                names.extend(offered(
                    bindings_of(root, path, &data.bindings, Some(spelling)),
                    0,
                ));
            }
            names.extend(offered(declarations_in(spelling, index, path), 0));
        }
        // Not in the buffer at all: the project's answer, or nothing.
        None => {
            let found = declarations_in(spelling, index, path);
            if found.is_empty() {
                // Empty is either "declared and empty" or "no such scope", and the difference is the *name* — the
                // same distinction `direct_members` makes, for the same reason.
                return match index.definition(spelling, path) {
                    Known::Yes(found) if found.fact.qualified_name() == spelling => Known::Yes(Vec::new()),
                    Known::Unknown(reason) => Known::Unknown(reason),
                    _ => Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(spelling))),
                };
            }
            names.extend(offered(found, 0));
        }
    }

    sort_and_hide(&mut names);
    Known::Yes(names)
}

/// The bindings one of the file's own scopes holds, as names to offer.
///
/// `scope` is the qualified spelling the binding was written in — `ns` for `namespace ns { … }`, a class's name for
/// its body — and `None` for a scope that has none: a function body, a block, a lambda. The distinction is the one
/// [`DeclFact::scope`] documents, and it is passed rather than derived because the caller is the one that knows
/// *why* it is listing these bindings.
fn bindings_of(
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    bindings: &[crate::Binding],
    scope: Option<&str>,
) -> Vec<(PathBuf, DeclFact, usize)> {
    let spelling = scope.unwrap_or_default();

    bindings
        .iter()
        .map(|binding| {
            (
                path.to_path_buf(),
                fact_from_binding(root, spelling, binding),
                0,
            )
        })
        .collect()
}

/// The names written at **file scope**: this buffer's, and every included file's.
fn global_names(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
) -> Vec<(PathBuf, DeclFact, usize)> {
    let mut names: Vec<(PathBuf, DeclFact, usize)> = scopes
        .root()
        .and_then(|root_scope| scopes.scope(root_scope))
        .map(|data| bindings_of(root, path, &data.bindings, None))
        .unwrap_or_default();

    names.extend(declarations_in_scope(index, path, None));
    names
}

/// The facts the index holds for a scope spelling, or for the global name space when it is `None`.
///
/// The global case is the *absence* of a spelling rather than an empty one, which is why it cannot go through
/// [`ProjectIndex::declarations_in`]: that one compares a spelling, and a declaration at file scope records
/// `None`. It is the same distinction `matches` makes for a leading `::`. A **local** is left out here for the
/// reason [`DeclFact::local`] gives — a local also records `None`, and offering another file's would be the wrong
/// answer rather than a missing one.
fn declarations_in_scope(
    index: &ProjectIndex,
    visible_from: &Path,
    scope: Option<&str>,
) -> Vec<(PathBuf, DeclFact, usize)> {
    let found = match scope {
        Some(spelling) => index.declarations_in(spelling, visible_from),
        None => index.visible_declarations(visible_from, |fact| fact.scope.is_none() && !fact.local),
    };

    found
        .into_iter()
        .map(|declaration| (declaration.file.clone(), declaration.fact.clone(), 0))
        .collect()
}

/// [`declarations_in_scope`] for a named scope, spelled the way this module spells lookups.
fn declarations_in(
    spelling: &str,
    index: &ProjectIndex,
    visible_from: &Path,
) -> Vec<(PathBuf, DeclFact, usize)> {
    declarations_in_scope(index, visible_from, Some(spelling))
}

/// The members of a class, bases included, as names to offer.
fn names_of_a_class(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<(PathBuf, DeclFact, usize)>> {
    match members_of(index, scopes, root, path, class) {
        Known::Yes(members) => Known::Yes(
            members
                .members
                .into_iter()
                .map(|member| (member.file, member.fact, member.depth))
                .collect(),
        ),
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class))),
    }
}
/// Wrap `(file, fact, steps)` as offers, adding the depth of the scope they were found in and dropping the
/// declarations that have no name to type.
///
/// `steps` is how far *inside* one answer the name was — a base class's members are one step further than the
/// class's own — and the two are added because they mean the same thing to a consumer: how far the lookup had to
/// walk before it found this name, which is the order C++ walks in too.
fn offered(found: Vec<(PathBuf, DeclFact, usize)>, depth: usize) -> Vec<OfferedName> {
    found
        .into_iter()
        .filter(|(_, fact, _)| !fact.name.is_empty())
        .map(|(file, fact, steps)| OfferedName {
            file,
            fact,
            depth: depth.saturating_add(steps),
        })
        .collect()
}

/// Put the offers in the order a completion shows them: nearest scope first, then by name; and drop every name an
/// inner scope has already offered.
///
/// The hiding is what makes a *list* an answer rather than a pile: a local `count` and a header's `count` are two
/// declarations of one name, and C++ reaches only the first. Both would be wrong to show — one of them is not what
/// the user gets if they pick it — and the inner one is the one they get.
fn sort_and_hide(names: &mut Vec<OfferedName>) {
    // Stable, so that two declarations of one name at one depth keep the order they were found in: that is an
    // overload, and reordering it would be inventing a preference.
    names.sort_by(|one, other| {
        one.depth
            .cmp(&other.depth)
            .then_with(|| one.fact.name.cmp(&other.fact.name))
    });

    let mut seen: HashSet<String> = HashSet::new();
    names.retain(|name| seen.insert(name.fact.name.clone()));
}

/// The type of an expression, as far as this layer can tell, and the file that declared it.
///
/// The core of the `infer` layer, and it is **recursive** because that is what an expression is: `a.b.size` is a
/// member access whose object is a member access, and the type of the inner one is the type recorded on the
/// declaration the outer one starts from.
///
/// # The three shapes it can type, and the boundary
///
/// ```text
/// a name          `widget`      — its declaration's `type_of`, from this file or through the index
/// `this`          `this->size`  — the class whose scope encloses the expression, which needs no inference
/// a member access `a.b`         — recursively: find `b` in the type of `a`, then read `b`'s own `type_of`
/// ```
///
/// Everything else is [`UnknownReason::UnknownType`] carrying the expression's spelling: a call (`f().size`), a
/// dereference (`(*p).size`), a subscript, an arithmetic expression. Each of those needs a type *computed* rather
/// than read off a declaration, which is a different and much larger problem — and answering `Unknown` is what
/// keeps this layer from being wrong in a way a consumer cannot see.
fn type_of_expression(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
) -> Known<(String, PathBuf)> {
    let text = expression.text().to_string();
    let written = text.trim();

    // `this` is the enclosing class, and no inference is involved: the scope chain already knows which class this
    // is, and it is the same answer inside every member function of it.
    if written == "this" {
        let offset = usize::from(expression.text_range().start());
        return match enclosing_class(scopes, offset) {
            Some(class) => Known::Yes((class, path.to_path_buf())),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A name: its declaration says what type it has. The file being edited is asked first, because a buffer that
    // has never been saved has no summary — the two-layer split the name query uses, for the same reason.
    if !written.is_empty() && written.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return match declaration_of_expression(index, scopes, root, path, expression) {
            Known::Yes(named) => match named.type_of(root) {
                Some(type_of) => Known::Yes((type_of, named.file(path))),
                // The declaration is a function, a class or an alias: none of them has a type *as a name*, which
                // is the distinction `DeclFact::returns` exists for.
                None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
            },
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A **call**: what the callee returns, or a temporary of the class it names.
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::CallExpr {
        return type_of_a_call(index, scopes, root, path, expression);
    }

    // A member access: the type of the member, which is a fact on its declaration.
    if let Some(inner) = crate::sema::resolve::member_access_of(expression) {
        let Known::Yes((inner_type, _)) =
            type_of_expression(index, scopes, root, path, &inner.object)
        else {
            let Known::Unknown(reason) =
                type_of_expression(index, scopes, root, path, &inner.object)
            else {
                unreachable!("the first match established that this is an `Unknown`")
            };
            return Known::Unknown(reason);
        };

        let class = base_type_name(&inner_type);
        if class.is_empty() {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
        }

        let found = member_fact(index, scopes, root, path, class, &inner.member);
        let Known::Yes((fact, file)) = found else {
            let Known::Unknown(reason) = found else {
                unreachable!("the first match established that this is an `Unknown`")
            };
            return Known::Unknown(reason);
        };

        return match fact.type_of {
            Some(type_of) => Known::Yes((type_of, file)),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    Known::Unknown(UnknownReason::UnknownType(Box::from(written)))
}

/// The declaration an expression **names**, from the file being edited or from the index.
///
/// The reconciliation the two-layer split needs, in one place: the buffer answers with a [`crate::Binding`] whose
/// spellings are read out of the tree, while an indexed file answers with a [`DeclFact`] that carries them as
/// fields. Both are needed for the same two questions — what type does this name have, what does a call of it
/// give — and a second reconciliation would be a second rule book for the same question.
///
/// The answers are the ones the name query gives, and for the same reason: a name this file cannot place is asked
/// of the index, and a name *nothing* declares is [`UnknownType`] carrying the spelling rather than
/// [`UnknownReason::NotDeclaredHere`] — what is missing here is the type of an expression, which is a different
/// thing for a consumer to be told.
///
/// [`UnknownType`]: UnknownReason::UnknownType
enum NamedDeclaration {
    /// A binding of the file being edited. Its spellings come from the tree the caller has.
    Here(crate::Binding),
    /// A declaration in an indexed file, with the file it is in.
    Indexed(DeclFact, PathBuf),
}

impl NamedDeclaration {
    /// Where the declaration is — the answer's own file, for a consumer that has to jump or to look further.
    fn file(&self, here: &Path) -> PathBuf {
        match self {
            NamedDeclaration::Here(_) => here.to_path_buf(),
            NamedDeclaration::Indexed(_, file) => file.clone(),
        }
    }

    /// The type this declaration was written with.
    ///
    /// `None` for a function — `make` is not a `Widget` and has no members — which is the distinction
    /// [`DeclFact::type_of`] documents.
    fn type_of(&self, root: &cpp_parser::CppSyntaxNode) -> Option<String> {
        match self {
            NamedDeclaration::Here(binding) => {
                crate::sema::declarations::declared_type_of(root, binding)
            }
            NamedDeclaration::Indexed(fact, _) => fact.type_of.clone(),
        }
    }

    /// What a **call** of this declaration has: the type it returns, or the class it declares.
    ///
    /// Two answers because C++ has two, and the tokens do not separate them: `make()` is a call of a function and
    /// has what it returns, while `Widget()` — the same shape — is a *temporary* of the class. Only the
    /// declaration says which, which is why this is asked here rather than of the shape.
    fn what_a_call_has(&self, root: &cpp_parser::CppSyntaxNode) -> Option<String> {
        match self {
            NamedDeclaration::Here(binding) => {
                if binding.kind == crate::BindingKind::Class {
                    return binding
                        .name
                        .identifier_text()
                        .map(|name| name.to_string());
                }
                crate::sema::declarations::declared_returns_of(root, binding)
            }
            NamedDeclaration::Indexed(fact, _) => what_a_call_has_in(fact),
        }
    }
}

/// [`NamedDeclaration::what_a_call_has`] for a declaration the index (or a member lookup) produced.
fn what_a_call_has_in(fact: &DeclFact) -> Option<String> {
    match fact.kind {
        // `Widget()` is a temporary of `Widget`, so a call of a class has the class. `DeclKind::Type` is exactly
        // "this declaration declares a class-like type", which is the case a call whose callee is a *type* lands in.
        crate::DeclKind::Type => Some(fact.qualified_name()),
        _ => fact.returns.clone(),
    }
}

/// The declaration an expression names, or why it names none. See [`NamedDeclaration`].
fn declaration_of_expression(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
) -> Known<NamedDeclaration> {
    let offset = usize::from(expression.text_range().start());
    let written = expression.text().to_string();
    let written = written.trim();

    match crate::sema::resolve::definition_at(scopes, root, offset) {
        Known::Yes(binding) => Known::Yes(NamedDeclaration::Here(binding)),
        Known::Unknown(UnknownReason::NotDeclaredHere(name)) => match index.definition(&name, path) {
            Known::Yes(found) => Known::Yes(NamedDeclaration::Indexed(found.fact, found.file)),
            // The declaration is nowhere this analysis can see, so there is no type to read. Reporting the *name*
            // reason would say "the owner is missing" where what is missing is the type of an expression — a
            // different answer for a consumer deciding what to tell the user.
            Known::Unknown(_) | Known::No => {
                Known::Unknown(UnknownReason::UnknownType(Box::from(written)))
            }
        },
        // A name this layer cannot place at all is not a type it can read. `No` means the offset is not on a name;
        // any other `Unknown` is already the most specific answer available and is passed through.
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
    }
}

/// The type of a **call**: `make().size` needs this, and so does `fac.build().size`.
///
/// # The two callees, and the one lookup behind them
///
/// ```text
/// make()          a name: what it returns
/// Widget()        a name, and the same tokens — a temporary of the class it names
/// fac.build()     a member access: what the member returns
/// ns::make()      a qualified name: what it returns
/// ```
///
/// The first two are one case because only the *declaration* separates them, and the last two go through the
/// lookups that already exist — the member lookup for a member call, the name lookup for everything else. What
/// this adds is the last step: the callee's declaration says what a call of it has ([`what_a_call_has_in`]).
///
/// Everything else stays [`UnknownReason::UnknownType`]: a call of a function pointer, of a lambda, of a template
/// parameter whose type is not known here. Each of those needs a type *computed* rather than read off a
/// declaration, which is the same boundary the rest of this function has.
fn type_of_a_call(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    call: &cpp_parser::CppSyntaxNode,
) -> Known<(String, PathBuf)> {
    let written = call.text().to_string();
    let written = written.trim();

    let Some(callee) = call.children().next() else {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    };

    // A member call: the object's type, then the member's declaration, then what a call of it has.
    if let Some(access) = crate::sema::resolve::member_access_of(&callee) {
        let object = match type_of_expression(index, scopes, root, path, &access.object) {
            Known::Yes((type_of, _)) => type_of,
            Known::Unknown(reason) => return Known::Unknown(reason),
            Known::No => return Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };

        let class = base_type_name(&object);
        if class.is_empty() {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
        }

        let found = member_fact(index, scopes, root, path, class, &access.member);
        let Known::Yes((fact, file)) = found else {
            let Known::Unknown(reason) = found else {
                unreachable!("the first match established that this is an `Unknown`")
            };
            return Known::Unknown(reason);
        };

        return match what_a_call_has_in(&fact) {
            Some(type_of) => Known::Yes((type_of, file)),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // Everything else is a name — `make()`, `Widget()`, `ns::make()` — and the name lookup is the one every other
    // question about a name goes through.
    match declaration_of_expression(index, scopes, root, path, &callee) {
        Known::Yes(named) => match named.what_a_call_has(root) {
            Some(type_of) => Known::Yes((type_of, named.file(path))),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        },
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
    }
}

/// The declaration of `member` in the class `class` names, and the file it is in.
///
/// The lookup is the qualified name `<class>::<member>`, which is why nothing here needs to know what a class
/// *is*: the file being edited is asked first through its scope tree, and the index second — the same two-layer
/// split [`definition_across_files`] makes, and for the same reason.
fn member_fact(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    member: &str,
) -> Known<(DeclFact, PathBuf)> {
    if let Some(found) = direct_member(index, scopes, root, path, class, member) {
        return Known::Yes(found);
    }

    // Inherited members, **level by level**: a member of a direct base hides a member of that base's own base,
    // which is what C++ does, so the search stops at the first level that has any answer. Two answers at the same
    // level are ambiguous — a diamond where both sides declare the name — and reporting that is the honest
    // outcome: picking one would be a jump to an entity the language says is not uniquely named.
    let mut level = bases_of(index, scopes, root, path, class)
        .value()
        .unwrap_or_default();
    let mut visited: Vec<String> = vec![class.to_string()];

    while !level.is_empty() {
        let mut found: Vec<(DeclFact, PathBuf)> = Vec::new();
        let mut next: Vec<String> = Vec::new();

        for base in level {
            if visited.contains(&base) {
                continue;
            }
            visited.push(base.clone());

            // A base that cannot be resolved contributes nothing *and* stops nothing: whatever it might inherit
            // from is unknown, not absent, and the levels below it are still worth asking.
            if let Some(member_found) = direct_member(index, scopes, root, path, &base, member) {
                found.push(member_found);
            }
            next.extend(
                bases_of(index, scopes, root, path, &base)
                    .value()
                    .unwrap_or_default(),
            );
        }

        match found.len() {
            0 => level = next,
            1 => return Known::Yes(found.remove(0)),
            _ => return Known::Unknown(UnknownReason::Ambiguous(Box::from(member))),
        }
    }

    Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(format!(
        "{class}::{member}"
    ))))
}

/// The declaration of `member` written **directly** in `class`, or `None`.
///
/// The qualified name `<class>::<member>`, which is why nothing here needs to know what a class *is*: the file
/// being edited is asked first through its scope tree, and the index second — the two-layer split
/// [`definition_across_files`] makes, for the same reason.
fn direct_member(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    member: &str,
) -> Option<(DeclFact, PathBuf)> {
    // A class declared in this file is looked up here first, which is what makes the whole query work on a buffer
    // that has never been written to disk. The declared type is filled in from the tree, because a member of a
    // member is exactly what a nested access asks for next.
    if let Some(scope) = scopes.scope_with_qualified_name(class)
        && let Some(scope_data) = scopes.scope(scope)
        && let Some(binding) = scope_data
            .bindings
            .iter()
            .find(|binding| binding.name.identifier_text() == Some(member))
    {
        return Some((fact_from_binding(root, class, binding), path.to_path_buf()));
    }

    match index.definition(&format!("{class}::{member}"), path) {
        Known::Yes(found) => Some((found.fact, found.file)),
        Known::Unknown(_) | Known::No => None,
    }
}

/// One binding as a [`DeclFact`], under the qualified name of the scope it was written in.
///
/// The single reader of a binding's fact-shaped fields, shared by the single-member query and the member-list
/// query so that `Widget::size`'s type and `Widget`'s member `size` cannot come out differently: both are read
/// out of the tree through [`declared_type_of`](crate::sema::declarations::declared_type_of) and
/// [`declared_bases_of`](crate::sema::declarations::declared_bases_of), and a second call site would be a second
/// answer to "what does this declaration say".
///
/// The guard is [`FactGuard::Unconditional`] because a binding carries none: the region a declaration sits in is
/// a fact about the text, and the layer that sweeps the directives fills it in when the summary is built — see
/// [`build_facts`](crate::sema::declarations::build_facts).
///
/// # The two fields that are not answered on this path
///
/// A fact built from the **buffer's** scope tree has had no directive sweep and has no diagnostic list, so two of
/// its fields are defaults rather than answers: `guard` says `Unconditional` even for a member written inside an
/// `#if`, and `clean` says `true` even in a buffer that does not parse. Both are gaps rather than decisions, and
/// they are stated here rather than left to be discovered: this path is handed a **node** and the tree's
/// diagnostics are not on it, so a caller that needs either field has to ask the summary — which does have them —
/// and a consumer showing a member list has nothing to gain from either, which is why neither was worth
/// threading the tree through every query for. The same applies to the facts [`member_across_files`] returns
/// from this path.
fn fact_from_binding(root: &cpp_parser::CppSyntaxNode, class: &str, binding: &crate::Binding) -> DeclFact {
    DeclFact {
        name: binding
            .name
            .identifier_text()
            .unwrap_or_default()
            .to_string(),
        // `None` for an empty spelling, which is what a caller listing the names of a **body** passes: a local has
        // no qualified scope at all, and `Some("")` would be a scope that matches nothing while looking like one.
        // Every other caller passes the class or namespace the binding is in.
        scope: (!class.is_empty()).then(|| class.to_string()),
        // Answered rather than defaulted, and the answer is always `false`: this path exists for **members**, and a
        // member is declared in a class body by construction — the `class` spelling it is keyed by has no meaning
        // inside a function. A local class's members are members of that class, and the caller that asked for them
        // asked by name. See [`DeclFact::local`].
        local: false,
        kind: crate::DeclKind::from_binding_kind(binding.kind),
        type_of: crate::sema::declarations::declared_type_of(root, binding),
        // Answered here as well, because **members** are exactly where a call happens: `fac.build().size` needs the
        // return type of a member function that the file being edited declares. See [`DeclFact::returns`].
        returns: crate::sema::declarations::declared_returns_of(root, binding),
        bases: crate::sema::declarations::declared_bases_of(root, binding),
        range: binding.range,
        name_range: binding.name_range,
        clean: true,
        guard: FactGuard::Unconditional,
    }
}

/// The base classes `class` was written with, from this file or from the index.
///
/// # Names, not spellings
///
/// What comes back is normalized for **lookup**: `public Base<int>` is the class `Base`. The spelling the file
/// wrote stays in [`DeclFact::bases`], which is a fact about the text; a base here is a name to find a class by,
/// and the template arguments say which *type* is inherited rather than which class declares the members. The
/// members of `Base<int>` are the members of `Base`'s primary template, which is the most useful answer available
/// without instantiating anything — and the same rule [`base_type_name`] applies to a declared type.
///
/// # Why this is `Known` and not a list
///
/// "This class has no bases" and "nothing here says what this class inherits from" are different statements, and
/// an empty `Vec` would merge them. The member **lookup** treats them alike — a base it cannot reach contributes
/// no members either way, so it walks on — while the member **list** reports the gap, because a list is a claim
/// about what a type has.
fn bases_of(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<String>> {
    // In this file: the class scope's parent holds the binding of the class's *name*, which is the declaration the
    // bases were written on. Asking the tree through that binding is the same walk the fact builder makes, so the
    // two cannot disagree about what a class inherits from.
    if let Some(scope) = scopes.scope_with_qualified_name(class)
        && let Some(data) = scopes.scope(scope)
        && let Some(parent) = data.parent
        && let Some(name) = class.rsplit("::").next()
        && let Some(binding) = scopes
            .scope(parent)
            .and_then(|scope| {
                scope
                    .bindings
                    .iter()
                    .find(|binding| binding.name.identifier_text() == Some(name))
            })
    {
        return Known::Yes(lookup_names(&crate::sema::declarations::declared_bases_of(
            root, binding,
        )));
    }

    match index.definition(class, path) {
        Known::Yes(found) => Known::Yes(lookup_names(&found.fact.bases)),
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::No,
    }
}

/// Base spellings as names a class can be looked up by, in the order they were written.
fn lookup_names(bases: &[String]) -> Vec<String> {
    bases
        .iter()
        .map(|base| base_type_name(base).to_string())
        .filter(|base| !base.is_empty())
        .collect()
}

/// The class whose scope encloses `offset`, for `this`.
///
/// The nearest class-like scope on the chain, which is the same answer in a member function, in a nested class's
/// member function (the nested class wins, correctly) and in a default member initialiser.
fn enclosing_class(scopes: &crate::ScopeTree, offset: usize) -> Option<String> {
    let innermost = scopes.scope_at(offset)?;

    scopes
        .scope_chain(innermost)
        .into_iter()
        .find_map(|scope| {
            let data = scopes.scope(scope)?;
            // One variant for class, struct and union: the difference is access, which is a member's property
            // rather than the scope's. See `ScopeKind`.
            (data.kind == crate::ScopeKind::Class)
                .then(|| scopes.qualified_name_of(scope))
                .flatten()
        })
}

/// The name of the class a written type names, with the parts that do not affect *which* class it is removed.
///
/// `const Widget&` → `Widget`, `std::vector<int>` → `std::vector`, `struct Widget` → `Widget`, `::Widget` →
/// `Widget`. The goal is a spelling that can be looked up as a qualified name, and every one of those parts is
/// about the type's shape or about which name space it is in rather than about its name. `unsigned long` is left
/// alone: there the words *are* the type, and it names no class anyway.
///
/// # Why the leading `::` comes off
///
/// A leading `::` asks about the **global** name space — a real distinction for a *name* lookup, where dropping it
/// would also match `ns::Widget`, and [`matches`] honours it for exactly that reason. It is not a distinction for
/// this walk, because the walk does not ask "which name space"; it asks for the qualified spelling the index keys
/// on, and a global declaration's spelling is its bare name. Keeping the prefix made `::Widget w;` — the type
/// spelling a consumer hands over verbatim from `DeclFact.type_of` — fail to find a class sitting in the buffer.
fn base_type_name(written: &str) -> &str {
    let mut name = written.trim();

    // Template arguments: the members of `std::vector<int>` are the members of `std::vector`'s primary template,
    // which is the most useful answer available without instantiating anything.
    if let Some(position) = name.find('<') {
        name = name[..position].trim();
    }

    // Declarators written after the type: `Widget*`, `Widget&`, `Widget&&`.
    name = name.trim_end_matches(['*', '&']).trim();

    // The global name space, which for a qualified spelling is no prefix at all.
    name = name.strip_prefix("::").unwrap_or(name).trim();

    // An elaborated specifier: `struct Widget` and `Widget` name one class, and only the second is a spelling the
    // index matches.
    for keyword in ["struct ", "class ", "union ", "enum "] {
        if let Some(rest) = name.strip_prefix(keyword) {
            name = rest.trim();
            break;
        }
    }

    name
}


impl ProjectIndex {
    pub fn new() -> Self {
        ProjectIndex::default()
    }

    /// Add or replace one file's summary.
    ///
    /// The reverse edges are updated rather than rebuilt: an edit to one file changes only its own out-edges,
    /// and rebuilding the whole map on every keystroke is the cost the per-file design exists to avoid.
    pub fn insert(&mut self, summary: FileSummary) {
        let path = summary.path.clone();
        self.insert_at(&path, summary);
    }

    /// [`ProjectIndex::insert`] for a summary that was **read from the cache**.
    ///
    /// `path` is the file it is being filed under, and it has to be passed rather than taken from the summary
    /// because the two can legitimately differ: a cache entry is keyed on a file's *contents* and the directory it
    /// was compiled in, so two files with identical text side by side share an entry, and the entry's own `path`
    /// records whichever of them was written first. Filing the second under the first's name makes every fact in
    /// it point at the wrong file, and a definition jump into a file the user never mentioned.
    ///
    /// The facts themselves are right either way: a summary's contents are a function of the text *and its
    /// directory* — which is exactly what the key names, and the reason the directory is part of it. Two files
    /// whose keys match have the same declarations, at the same offsets, with the same ranges, *and* the same
    /// resolved includes.
    pub fn insert_at(&mut self, path: &Path, summary: FileSummary) {
        let mut summary = summary;
        summary.path = path.to_path_buf();

        let path = normalize(path);

        // Remove the edges the previous version of this file contributed, so that a deleted `#include` stops
        // making its target reachable. Without this, an edge would outlive the line that wrote it.
        if let Some(previous) = self.summaries.get(&path) {
            for target in include_targets(previous) {
                if let Some(includers) = self.included_by.get_mut(&target) {
                    includers.remove(&path);
                }
            }
        } else {
            self.order.push(path.clone());
        }

        for target in include_targets(&summary) {
            self.included_by
                .entry(target)
                .or_default()
                .insert(path.clone());
        }

        self.summaries.insert(path, summary);
    }

    /// Forget the file at `path`, and the edges its summary contributed.
    ///
    /// The one operation that makes a *deleted* file stop answering. Without it a query would go on finding
    /// declarations in a file that is no longer on disk — the summary is in memory, and nothing about it is
    /// wrong except that it describes something that is gone. The reverse edges go with it, for the same reason
    /// they are removed when a file is re-indexed: an edge outliving the line that wrote it would keep a header
    /// reachable from a file that no longer includes it.
    ///
    /// Returns whether there was anything to forget, which is what lets a watcher report "this event changed
    /// nothing" rather than counting every ignored path as work.
    pub fn forget(&mut self, path: &Path) -> bool {
        let path = normalize(path);
        let Some(summary) = self.summaries.remove(&path) else {
            return false;
        };

        for target in include_targets(&summary) {
            if let Some(includers) = self.included_by.get_mut(&target) {
                includers.remove(&path);
            }
        }
        self.order.retain(|held| held != &path);

        true
    }

    pub fn len(&self) -> usize {
        self.summaries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.summaries.is_empty()
    }

    /// The summary of the file at `path`, if it has been indexed.
    pub fn summary(&self, path: &Path) -> Option<&FileSummary> {
        self.summaries.get(&normalize(path))
    }

    /// Every summary, in insertion order.
    pub fn summaries(&self) -> impl Iterator<Item = &FileSummary> {
        self.order.iter().filter_map(|path| self.summaries.get(path))
    }

    /// The files that include `path`, directly.
    pub fn includers_of(&self, path: &Path) -> Vec<PathBuf> {
        self.included_by
            .get(&normalize(path))
            .map(|includers| includers.iter().map(PathBuf::from).collect())
            .unwrap_or_default()
    }

    /// The files in which `name` is visible, in insertion order.
    ///
    /// `name` is matched against a declaration's **qualified** name first — `ns::Widget` — and against its bare
    /// name only as a fallback, because a qualified spelling is a much stronger claim than a name that happens to
    /// appear in some scope. A caller that gets one answer from the qualified match should prefer it to any
    /// number from the bare one.
    pub fn files_declaring(&self, name: &str, visible_from: &Path) -> Vec<VisibleDeclaration<'_>> {
        self.visible_declarations(visible_from, |fact| matches(fact, name))
    }

    /// Every declaration written **directly in** the scope `scope`, visible from `visible_from`.
    ///
    /// The whole-scope counterpart of [`ProjectIndex::definition`]: that asks about one name, this asks about
    /// every name one scope holds, which is what a member list is made of. `scope` is a **qualified** spelling —
    /// `Widget`, `ns::Widget` — because that is what a [`DeclFact`] records; see [`DeclFact::scope`].
    ///
    /// Note what "directly in" excludes, because it is the whole reason this is not a name search: a local
    /// variable inside a member function is a fact whose scope is `None` — a function body contributes no segment
    /// to a qualified name — so `C`'s members are `C`'s bindings and not everything written between its braces.
    pub fn declarations_in(&self, scope: &str, visible_from: &Path) -> Vec<VisibleDeclaration<'_>> {
        self.visible_declarations(visible_from, |fact| fact.scope.as_deref() == Some(scope))
    }

    /// The declarations some predicate accepts, in the files `visible_from` can see.
    ///
    /// The one place the visibility walk is applied to the declaration list, so that a new query over facts
    /// cannot forget it and quietly answer with a declaration in a file the querying file does not include —
    /// which is a jump to something it cannot compile against. `includers_of` and [`ProjectIndex::visible_files`]
    /// are the graph; this is the graph applied to a question.
    ///
    /// # Why the graph is walked once, and not once per file
    ///
    /// This used to ask `visibility_of` for **each summary** in the index, and each of those answers walked the
    /// include graph from scratch: a query over a standard-library closure therefore did three hundred
    /// breadth-first searches to answer one question. Measured, on a closure of 308 files: **573 ms** for a
    /// completion at `std::`, which is a query a client asks on every keystroke. One walk and a map lookup is the
    /// same answer in under a millisecond.
    fn visible_declarations<'a>(
        &'a self,
        visible_from: &Path,
        accepts: impl Fn(&DeclFact) -> bool,
    ) -> Vec<VisibleDeclaration<'a>> {
        // Keyed by the **normalized** path, which is what the summary map is keyed by too: the walk produces
        // normalized spellings, and normalizing three hundred paths again per query is work with no answer in it.
        let visible: HashMap<String, IncludeVisibility> = self.visible_files(visible_from).into_iter().collect();

        let mut found = Vec::new();

        for summary in self.summaries() {
            let Some(visibility) = visible.get(&normalize(&summary.path)) else {
                continue;
            };

            for fact in summary.declarations.iter().filter(|fact| accepts(fact)) {
                found.push(VisibleDeclaration {
                    file: summary.path.clone(),
                    fact,
                    visibility: *visibility,
                });
            }
        }

        found
    }

    /// Every file `from` can see, and how — one walk of the include graph.
    ///
    /// The whole-graph answer to what used to be a per-file question: "can this file see that one" was asked once
    /// per summary, and each answer walked the include graph. This returns the answer for every file at once, so
    /// one walk serves a whole query — measured on a closure of 308 files, that turned a 573 ms completion into a
    /// 3 ms one, and every cross-file query over declarations goes through it.
    ///
    /// The visibility recorded is the **best** one found: a file reached both unconditionally and through a
    /// guarded `#include` is unconditional, because the unconditional path is the one that is always there. That
    /// is why a file may be relaxed rather than only visited — the first path found is not necessarily the best,
    /// and a graph with a diamond in it (which every header guard produces) has exactly that shape.
    pub fn visible_files(&self, from: &Path) -> Vec<(String, IncludeVisibility)> {
        let from = normalize(from);

        // The file itself is visible to itself, and unconditionally: a question asked in a file is about what
        // that file says before anything it includes.
        let mut best: HashMap<String, IncludeVisibility> =
            HashMap::from([(from.clone(), IncludeVisibility::Unconditional)]);
        let mut order: Vec<String> = vec![from.clone()];
        let mut pending: Vec<(String, IncludeVisibility, usize)> =
            vec![(from, IncludeVisibility::Unconditional, 0)];

        while let Some((current, so_far, depth)) = pending.pop() {
            if depth > MAX_VISIBILITY_DEPTH {
                continue;
            }

            let Some(summary) = self.summaries.get(&current) else {
                continue;
            };

            for include in &summary.includes {
                let Some(resolved) = &include.resolved else {
                    continue;
                };
                let next = normalize(resolved);

                let step = match include.guard {
                    FactGuard::Unconditional => so_far,
                    FactGuard::Region(_) => IncludeVisibility::Conditional,
                };

                match best.get(&next) {
                    // Already known, and no worse: nothing new to explore through it.
                    Some(known) if *known <= step => continue,
                    Some(_) => {}
                    None => order.push(next.clone()),
                }

                best.insert(next.clone(), step);
                pending.push((next, step, depth + 1));
            }
        }

        order
            .into_iter()
            .filter_map(|path| best.get(&path).map(|visibility| (path.clone(), *visibility)))
            .collect()
    }

    /// Which declaration a name written in `visible_from` refers to, across the project.
    ///
    /// The cross-file half of [`crate::sema::resolve::definition_at`], and the answer to the
    /// the single-file query returns when a name is somewhere else.
    ///
    /// # The four answers
    ///
    /// * `Yes` — exactly one visible declaration, unconditionally reachable.
    /// * `Unknown(NotDeclaredHere)` — nothing in the project declares it, or nothing that is visible. **Not**
    ///   `No`: the project's index is a subset of what a compiler would see (the standard library, a header
    ///   outside every include path), so "not here" and "nowhere" are still different claims.
    /// * `Unknown(Ambiguous)` — several declarations are visible and nothing chooses between them. Overloads, a
    ///   name declared in two headers the file includes, a bare name declared in two namespaces.
    /// * `Unknown(ConditionalCompilation)` — the only match is reached through an `#include` inside an `#if`, so
    ///   whether it is in scope depends on macros this layer does not have.
    pub fn definition(&self, name: &str, visible_from: &Path) -> Known<ProjectDefinition> {
        let candidates = self.files_declaring(name, visible_from);

        if candidates.is_empty() {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(name)));
        }

        // A declaration the file itself writes wins over one it includes, and is the one a reader means: a
        // header's `Widget` and this file's `Widget` are different entities, and C++ resolves to the local one.
        let own = normalize(visible_from);
        if let Some(local) = candidates.iter().find(|found| normalize(&found.file) == own) {
            return Known::Yes(ProjectDefinition {
                file: local.file.clone(),
                fact: local.fact.clone(),
            });
        }

        // Prefer the unambiguous ones: a declaration reachable without any conditional include is visible
        // whatever the macros are, so it is a better answer than one that might not be there.
        let unconditional: Vec<&VisibleDeclaration<'_>> = candidates
            .iter()
            .filter(|found| found.visibility == IncludeVisibility::Unconditional)
            .collect();
        let guarded: Vec<&VisibleDeclaration<'_>> = candidates
            .iter()
            .filter(|found| found.visibility == IncludeVisibility::Conditional)
            .collect();

        if unconditional.len() == 1 {
            let found = unconditional[0];
            return Known::Yes(ProjectDefinition {
                file: found.file.clone(),
                fact: found.fact.clone(),
            });
        }
        if !unconditional.is_empty() {
            return Known::Unknown(UnknownReason::Ambiguous(Box::from(name)));
        }

        if guarded.is_empty() {
            // Reachable only through a missing file, which `visibility_of` reports as not visible — so this arm
            // is unreachable in practice and exists so that a future visibility answer has to be handled here
            // rather than silently falling through to `Yes`.
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(name)));
        }

        Known::Unknown(UnknownReason::ConditionalCompilation)
    }

    /// What the macro name `name` is at `offset` in the file at `visible_from`.
    ///
    /// # Translation order is the whole answer
    ///
    /// A preprocessor reads a translation unit as one stream: the file's own lines, with each `#include` replaced
    /// by the whole of the file it names — recursively — and the macros in force at a point are decided by the
    /// **last** `#define` or `#undef` of that name to have gone past. So the position of a fact is not an offset
    /// but a **chain** of them: the `#include` that pulled its file in, then the `#include` inside *that* file, and
    /// so on, ending with the fact's own offset. Comparing two chains lexicographically *is* comparing where they
    /// came in the stream, which is why this needs no separate rules for "written here" and "written in a header":
    ///
    /// ```text
    /// a.cpp:   #define MAX 1        position [0]         -> the header's wins
    /// a.cpp:   #include "x.h"       position [10, 4]
    /// a.cpp:   #include "x.h"                          -> the local one wins
    /// a.cpp:   #define MAX 1         position [0]
    /// ```
    ///
    /// # The four answers
    ///
    /// * `Yes` — the last fact in that order is a `#define`.
    /// * `Unknown(UndefinedHere)` — it is an `#undef`. Not `No`, and not a pointer at the `#define` it used to
    ///   have: a name that was undefined above the cursor is an ordinary identifier, and a jump to a definition
    ///   that is no longer in force would be a wrong answer rather than a missing one.
    /// * `Unknown(ConditionalCompilation)` — every fact that could decide it is inside an `#if`, or reached
    ///   through an `#include` that is. See below for the rule that keeps this from swallowing the common case.
    /// * `Unknown(NotDeclaredHere)` — nothing in the index touches the name. **Not** "there is no such macro":
    ///   the index holds the files it has been asked about, and a macro defined in a header nobody indexed looks
    ///   exactly like a name that is not a macro at all.
    ///
    /// # Why the answer prefers the unconditional fact
    ///
    /// The same rule [`ProjectIndex::definition`] uses, for the same reason. An unconditional `#define` above a
    /// *guarded* one is what the name certainly is; reporting `ConditionalCompilation` instead would refuse to
    /// answer a question that has an answer. Only when nothing unconditional is in the running does the answer
    /// become `Unknown` — and the residual uncertainty is stated rather than hidden: a guarded `#undef` *after* an
    /// unconditional `#define` is treated as not having happened.
    pub fn macro_definition(&self, name: &str, visible_from: &Path, offset: usize) -> Known<ProjectMacro> {
        let mut candidates = Vec::new();
        let mut visited = HashSet::new();

        self.macro_candidates(
            visible_from,
            &mut Vec::new(),
            Some(offset),
            false,
            &mut visited,
            name,
            &mut candidates,
        );

        // Translation order, and `Vec`'s order is the lexicographic one the chains are meant to be compared by.
        // The unconditional ones are what settles it; see above.
        let certain = candidates
            .iter()
            .filter(|candidate| !candidate.conditional)
            .max_by(|one, other| one.position.cmp(&other.position));
        let best = certain.or_else(|| {
            candidates
                .iter()
                .max_by(|one, other| one.position.cmp(&other.position))
        });

        let Some(best) = best else {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(name)));
        };

        if best.conditional {
            return Known::Unknown(UnknownReason::ConditionalCompilation);
        }

        if best.fact.kind.is_definition() {
            return Known::Yes(ProjectMacro {
                file: best.file.clone(),
                fact: best.fact.clone(),
            });
        }

        Known::Unknown(UnknownReason::UndefinedHere(Box::from(name)))
    }

    /// Every fact about `name` in this file and everything it includes, with where each one sits.
    ///
    /// `limit` is the offset the query is asked at, and applies **only to the file the query started from**: a
    /// line after the cursor cannot be in force yet, while everything in an included file is pasted at the
    /// `#include` and therefore all of it counts.
    ///
    /// A file is expanded once per query. That is enough for the answer — the facts are the same however many
    /// paths reach them — and it is what makes a cycle of includes terminate. What it costs is the ordering *among
    /// facts reached through the same top-level include*, which is the one case where a header reached twice by
    /// different routes could be placed at the earlier of its two positions rather than the later; the fact it
    /// reports is the same either way.
    #[allow(clippy::too_many_arguments)]
    fn macro_candidates(
        &self,
        path: &Path,
        chain: &mut Vec<usize>,
        limit: Option<usize>,
        conditional: bool,
        visited: &mut HashSet<String>,
        name: &str,
        out: &mut Vec<MacroCandidate>,
    ) {
        let path = normalize(path);
        if !visited.insert(path.clone()) {
            return;
        }

        let Some(summary) = self.summaries.get(&path) else {
            return;
        };

        for fact in summary.macros.iter().filter(|fact| fact.name == name) {
            if limit.is_some_and(|limit| fact.range.start_offset > limit) {
                continue;
            }

            let mut position = chain.clone();
            position.push(fact.range.start_offset);
            out.push(MacroCandidate {
                position,
                file: summary.path.clone(),
                fact: fact.clone(),
                conditional: conditional || fact.guard != FactGuard::Unconditional,
            });
        }

        for include in &summary.includes {
            if limit.is_some_and(|limit| include.range.start_offset > limit) {
                continue;
            }
            let Some(target) = &include.resolved else {
                continue;
            };

            chain.push(include.range.start_offset);
            self.macro_candidates(
                target,
                chain,
                None,
                conditional || include.guard != FactGuard::Unconditional,
                visited,
                name,
                out,
            );
            chain.pop();
        }
    }
}

/// One fact about a macro name, and where it sits in the translation unit's stream.
struct MacroCandidate {
    /// The chain of offsets that pastes this fact in: the top-level `#include`, each nested one, and the fact's
    /// own offset. Compared lexicographically, which is what makes it a position.
    position: Vec<usize>,
    file: PathBuf,
    fact: MacroFact,
    /// Is it inside an `#if` on the way in, or inside one itself?
    conditional: bool,
}

/// A declaration found in another file, with how the file that asked reaches it.#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleDeclaration<'a> {
    pub file: PathBuf,
    pub fact: &'a DeclFact,
    pub visibility: IncludeVisibility,
}

/// The answer to a cross-file definition question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDefinition {
    pub file: PathBuf,
    /// The declaration, cloned out of the index so that an answer does not borrow the project for as long as a
    /// consumer wants to hold it — a language server hands the location to a client and moves on.
    pub fact: DeclFact,
}

/// A type's members, as [`members_of`] lists them.
///
/// # The order is part of the answer
///
/// Members come **nearest class first**: the type's own, then its direct bases', then theirs, level by level.
/// That is the order C++ hides in, so a consumer that shows the list in this order shows the members in
/// precedence order — and a member that a nearer level also declares is not in the list at all, because the
/// language does not find it by that name.
///
/// Within a level the order is by **name**, and it has to be imposed rather than inherited: the file's own scope
/// tree keeps its bindings name-sorted — see [`ScopeTree::add_binding`](crate::ScopeTree::add_binding) — while
/// the index keeps facts in offset order, so leaving each side as it came would make a list depend on whether the
/// class happened to be in the buffer or in a header. Sorting by name is the one rule both sides can obey, and it
/// is stable, so two declarations of one name keep the order they were written in.
///
/// # It is never a claim of completeness
///
/// [`MemberList::unlisted`] names the bases this walk could not open. An empty `unlisted` means "no base this walk
/// reached was left unread" — not "this is everything the compiler would see". The index holds a subset of the
/// translation unit, and the standard library is not in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberList {
    pub members: Vec<ProjectMember>,
    /// The bases whose own members are **not** in this list, with why.
    ///
    /// Separate from `members` rather than folded into it, because the two say different things: `members` is
    /// what the type has, and this is where the answer stops. A consumer that shows a truncated list without
    /// saying so is the failure this field exists to prevent.
    pub unlisted: Vec<UnlistedBase>,
}

impl MemberList {
    /// The members declared by the type itself, as opposed to the ones it inherits.
    pub fn own(&self) -> impl Iterator<Item = &ProjectMember> {
        self.members.iter().filter(|member| member.depth == 0)
    }

    /// The members reached `depth` base steps away: `0` for the type's own, `1` for a direct base's.
    pub fn at_depth(&self, depth: usize) -> impl Iterator<Item = &ProjectMember> {
        self.members.iter().filter(move |member| member.depth == depth)
    }
}

/// One member of a type: where it is declared, and which class in the chain declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMember {
    pub file: PathBuf,
    pub fact: DeclFact,
    /// The **resolved** qualified name of the class that declares it — the type asked about, or one of its bases.
    ///
    /// The qualified name rather than the spelling the derived class wrote, because that is what makes the member
    /// a thing a second query can be asked about: `Base` in `struct D : public Base` is a spelling, and
    /// `ns::Base` is the class. The spelling is in [`DeclFact::bases`] on the derived class's own fact.
    pub declared_in: String,
    /// How many base steps away it is: `0` for the type's own members, `1` for a direct base's, and so on.
    ///
    /// Recorded rather than left to be recomputed from `members`, because a consumer grouping by it — an outline,
    /// a completion that shows inherited members separately — would otherwise have to reconstruct the walk it was
    /// just handed the result of.
    pub depth: usize,
    /// Another class **at the same level** also declares this name, so the name is not uniquely resolved here.
    ///
    /// The list keeps both declarations, because dropping either would be choosing. See [`members_of`].
    pub ambiguous: bool,
}

/// A base whose members are not in the list, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlistedBase {
    /// The base as a **name to look up**, not as the file spelled it: `Base` for `public Base<int>` — a base is
    /// found by name, and the template arguments say which type is inherited rather than which class declares the
    /// members.
    pub spelling: String,
    /// [`UnknownReason::NotDeclaredHere`] for a base nothing visible declares, and
    /// [`UnknownReason::ConditionalCompilation`] for one reachable only through a guarded `#include`.
    pub reason: UnknownReason,
}

impl ProjectDefinition {
    /// The same shape, from a binding the file's own scopes produced.
    ///
    /// [`crate::sema::resolve::definition_at`] answers with a [`Binding`], which carries a `Name` rather than a plain
    /// string and no qualified scope — so the two answers are made to look alike here, in one place, rather than
    /// at every call site.
    ///
    /// `scope` is what the *caller* knows and this does not: the qualified name a member access or a qualifier
    /// went through (`Widget` for `widget.size`, `ns` for `ns::Widget`), or `None` for a name written bare. It is
    /// passed rather than derived because deriving it here would mean re-reading the cursor, and left empty it
    /// would make `widget.size` and a free `size` indistinguishable to a consumer showing the answer.
    ///
    /// `local` is passed for the same reason, one step further out: whether the name can be reached from another
    /// file is a fact about the **scope chain** the binding was made in, and a `Binding` carries a [`crate::ScopeId`]
    /// rather than the tree that id belongs to. The caller has the tree; see [`ScopeTree::declares_a_local`].
    ///
    /// [`Binding`]: crate::Binding
    /// [`ScopeTree::declares_a_local`]: crate::ScopeTree::declares_a_local
    pub fn from_binding(
        path: &Path,
        binding: crate::Binding,
        scope: Option<String>,
        local: bool,
    ) -> Self {
        ProjectDefinition {
            file: path.to_path_buf(),
            fact: DeclFact {
                name: binding
                    .name
                    .identifier_text()
                    .unwrap_or_default()
                    .to_string(),
                scope,
                local,
                kind: crate::DeclKind::from_binding_kind(binding.kind),
                // No type, no return type and no bases, because this answer is a *place to jump to* and the binding
                // it comes from carries none of them: they are facts about the file's text, and the file they
                // belong to has the summary that holds them. A consumer asking what a name *is* asks the index,
                // not this answer.
                type_of: None,
                returns: None,
                bases: Vec::new(),
                range: binding.range,
                name_range: binding.name_range,
                // And no answer about diagnostics either, for the same reason `guard` has none: the binding came
                // from the buffer's own scopes, which are built from a node rather than from a tree with a
                // diagnostic list. See [`fact_from_binding`].
                clean: true,
                guard: FactGuard::Unconditional,
            },
        }
    }
}

/// Is the path to a declaration always taken, or only under conditions the index cannot evaluate?
///
/// Ordered by **how good the answer is**, which is what lets [`ProjectIndex::visible_files`] keep the best path to
/// a file rather than the first one it happens to find: `Unconditional < Conditional`, so "is the known answer no
/// worse than this one" is a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IncludeVisibility {
    /// Every `#include` on the path is outside any `#if`, so the declaration is in scope whatever the macros are.
    Unconditional,
    /// At least one `#include` on the path is inside an `#if` this layer cannot evaluate.
    Conditional,
}

/// Does this declaration answer to `name`?
///
/// The qualified name first, then the bare one. Both are needed and the order matters: `ns::Widget` written in
/// the query is a claim that the reader knows where the name lives, and honouring it must not be diluted by
/// every unrelated `Widget` elsewhere in the project.
///
/// A **leading `::`** is a third spelling and not a decoration: `::Widget` asks about the global name space, so
/// only a declaration written at file scope answers it. Matching it by dropping the `::` would find `ns::Widget`
/// as well — the exact wrong answer the spelling exists to avoid — so the prefix is honoured rather than
/// stripped.
///
/// A **local** never answers, whatever it is called. See [`DeclFact::local`]: the index is a per-file list and
/// cannot say which function a local belongs to, so a name lookup here has no way to know whether the reader is
/// inside that function — and the answer it would give instead is a *wrong* one, not a missing one, because
/// thousands of locals in the standard library's headers are called `__first` and `n`. Resolving one is the
/// scope tree's job, and it is done there: [`crate::sema::resolve::definition_at`] walks the scopes of the file
/// being edited, which is the only place a local can be placed.
fn matches(fact: &DeclFact, name: &str) -> bool {
    if fact.local {
        return false;
    }

    if let Some(global) = name.strip_prefix("::") {
        return fact.scope.is_none() && fact.name == global;
    }

    fact.qualified_name() == name || fact.name == name
}

/// The resolved include targets of a summary, as normalized path strings.
fn include_targets(summary: &FileSummary) -> Vec<String> {
    summary
        .includes
        .iter()
        .filter_map(|include| include.resolved.as_ref())
        .map(|path| normalize(path))
        .collect()
}

/// A path as the index keys on it.
fn normalize(path: &Path) -> String {
    // Case-insensitive on Windows, where two spellings of one path are one file. The rest of the crate reads the
    // same flag from the compiler configuration; here it is the platform, because the *index* has to agree with
    // the filesystem about identity and nothing else.
    normalize_path(path, cfg!(windows))
}

#[cfg(test)]
mod tests {
    use super::{IncludeVisibility, ProjectIndex};
    use crate::cache::SummaryKey;
    use crate::index::summarize;
    use crate::symbol::{Known, UnknownReason};
    use std::path::Path;

    fn index(files: &[(&str, &str)]) -> ProjectIndex {
        let mut index = ProjectIndex::new();

        for (path, source) in files {
            // The includes are resolved by hand here rather than through a `FileProvider`, because what these
            // tests are about is the graph that results — not the search that produced it.
            let mut summary = summarize(Path::new(path), source, SummaryKey::new(0, 0));
            for include in &mut summary.includes {
                include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
            }
            index.insert(summary);
        }

        index
    }

    #[test]
    fn a_declaration_in_an_included_header_is_found() {
        let index = index(&[
            ("/p/widget.h", "struct Widget { int size; };\n"),
            ("/p/main.cpp", "#include \"widget.h\"\nvoid f() { Widget w; }\n"),
        ]);

        let found = index.definition("Widget", Path::new("/p/main.cpp"));
        let Known::Yes(definition) = found else {
            panic!("the class in the included header must be found: {found:?}");
        };

        assert_eq!(definition.file, Path::new("/p/widget.h"));
        assert_eq!(definition.fact.name, "Widget");
    }

    #[test]
    fn a_declaration_in_a_header_the_file_does_not_include_is_not_found() {
        // The distinction the whole visibility walk exists for: `Widget` is in the project, and not in scope
        // here. Answering `Yes` would be a jump to a declaration the file cannot compile against.
        let index = index(&[
            ("/p/widget.h", "struct Widget { int size; };\n"),
            ("/p/other.cpp", "void g() { }\n"),
        ]);

        let found = index.definition("Widget", Path::new("/p/other.cpp"));

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "a name that is not in scope is not a definition: {found:?}"
        );
    }

    #[test]
    fn visibility_follows_a_chain_of_includes() {
        let index = index(&[
            ("/p/deep.h", "struct Deep { int x; };\n"),
            ("/p/middle.h", "#include \"deep.h\"\n"),
            ("/p/main.cpp", "#include \"middle.h\"\nvoid f() { Deep d; }\n"),
        ]);

        let found = index.definition("Deep", Path::new("/p/main.cpp"));
        let Known::Yes(definition) = found else {
            panic!("a transitively included declaration is visible: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/deep.h"));
    }

    #[test]
    fn the_files_own_declaration_wins_over_an_included_one() {
        // C++ resolves to the declaration in the file being compiled, and a jump that went into a header instead
        // would be to a different entity.
        let index = index(&[
            ("/p/widget.h", "struct Widget { int from_header; };\n"),
            (
                "/p/main.cpp",
                "#include \"widget.h\"\nstruct Widget { int from_this_file; };\n",
            ),
        ]);

        let found = index.definition("Widget", Path::new("/p/main.cpp"));
        let Known::Yes(definition) = found else {
            panic!("the local declaration wins: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/main.cpp"));
    }

    #[test]
    fn a_name_declared_in_two_visible_headers_is_ambiguous_rather_than_guessed() {
        let index = index(&[
            ("/p/one.h", "int count;\n"),
            ("/p/two.h", "int count;\n"),
            (
                "/p/main.cpp",
                "#include \"one.h\"\n#include \"two.h\"\nvoid f() { count = 1; }\n",
            ),
        ]);

        let found = index.definition("count", Path::new("/p/main.cpp"));

        assert!(
            matches!(found, Known::Unknown(UnknownReason::Ambiguous(_))),
            "two declarations and nothing to choose between them: {found:?}"
        );
    }

    #[test]
    fn a_qualified_name_prefers_the_declaration_that_matches_it() {
        let index = index(&[
            ("/p/a.h", "namespace a {\n  struct Widget { int x; };\n}\n"),
            ("/p/b.h", "namespace b {\n  struct Widget { int y; };\n}\n"),
            (
                "/p/main.cpp",
                "#include \"a.h\"\n#include \"b.h\"\nvoid f() { a::Widget w; }\n",
            ),
        ]);

        let found = index.definition("a::Widget", Path::new("/p/main.cpp"));
        let Known::Yes(definition) = found else {
            panic!("the qualified name must select one of them: {found:?}");
        };

        assert_eq!(definition.file, Path::new("/p/a.h"));
        assert_eq!(definition.fact.qualified_name(), "a::Widget");
    }

    #[test]
    fn a_guarded_include_makes_the_answer_unknown() {
        // `#include` inside an `#if`: whether it is taken depends on macros the index does not have, so the
        // honest answer is that the name may or may not be in scope.
        let index = index(&[
            ("/p/widget.h", "struct Widget { int size; };\n"),
            (
                "/p/main.cpp",
                "#if defined(USE_WIDGET)\n#include \"widget.h\"\n#endif\nvoid f() { Widget w; }\n",
            ),
        ]);

        let found = index.definition("Widget", Path::new("/p/main.cpp"));

        assert!(
            matches!(found, Known::Unknown(UnknownReason::ConditionalCompilation)),
            "a conditional include cannot be decided here: {found:?}"
        );
    }

    #[test]
    fn an_unconditional_include_beats_a_guarded_one() {
        // The same name reachable two ways: the unguarded path is always there, so it is the answer.
        let index = index(&[
            ("/p/real.h", "struct Widget { int size; };\n"),
            (
                "/p/main.cpp",
                "#include \"real.h\"\n#if defined(X)\n#include \"other.h\"\n#endif\nvoid f() { Widget w; }\n",
            ),
            ("/p/other.h", "struct Widget { int other; };\n"),
        ]);

        let found = index.definition("Widget", Path::new("/p/main.cpp"));
        let Known::Yes(definition) = found else {
            panic!("the unconditional path wins: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/real.h"));
    }

    #[test]
    fn a_file_that_includes_nothing_sees_only_itself() {
        let index = index(&[
            ("/p/a.cpp", "int count;\n"),
            ("/p/b.cpp", "int count;\n"),
        ]);

        for path in ["/p/a.cpp", "/p/b.cpp"] {
            let found = index.definition("count", Path::new(path));
            let Known::Yes(definition) = found else {
                panic!("{path} must find its own count: {found:?}");
            };
            assert_eq!(definition.file, Path::new(path));
        }
    }

    #[test]
    fn a_cycle_of_includes_does_not_hang_the_visibility_walk() {
        let index = index(&[
            ("/p/a.h", "#include \"b.h\"\nstruct A { int x; };\n"),
            ("/p/b.h", "#include \"a.h\"\nstruct B { int y; };\n"),
            ("/p/main.cpp", "#include \"a.h\"\nvoid f() { B b; }\n"),
        ]);

        let found = index.definition("B", Path::new("/p/main.cpp"));
        let Known::Yes(definition) = found else {
            panic!("a cycle must not stop a name being found: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/b.h"));
    }

    #[test]
    fn the_reverse_edges_are_derived_from_the_summaries() {
        let index = index(&[
            ("/p/header.h", "int x;\n"),
            ("/p/one.cpp", "#include \"header.h\"\n"),
            ("/p/two.cpp", "#include \"header.h\"\n"),
        ]);

        let mut includers = index.includers_of(Path::new("/p/header.h"));
        includers.sort();
        assert_eq!(
            includers,
            [Path::new("/p/one.cpp"), Path::new("/p/two.cpp")],
            "an edit to the header invalidates exactly these"
        );
    }

    #[test]
    fn reindexing_a_file_removes_the_edges_its_old_text_had() {
        // An edge that outlived the `#include` that wrote it would keep a header reachable from a file that no
        // longer includes it — a stale conclusion with nothing on disk to contradict it.
        let mut index = index(&[
            ("/p/widget.h", "struct Widget { int size; };\n"),
            ("/p/main.cpp", "#include \"widget.h\"\nvoid f() { Widget w; }\n"),
        ]);

        assert!(index.includers_of(Path::new("/p/widget.h")).len() == 1);

        let mut edited = summarize(Path::new("/p/main.cpp"), "void f() { }\n", SummaryKey::new(1, 0));
        for include in &mut edited.includes {
            include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
        }
        index.insert(edited);

        assert!(
            index.includers_of(Path::new("/p/widget.h")).is_empty(),
            "the edge must go with the line that wrote it"
        );
        assert!(
            matches!(
                index.definition("Widget", Path::new("/p/main.cpp")),
                Known::Unknown(_)
            ),
            "and the name is no longer visible there"
        );
    }

    #[test]
    fn visibility_is_reported_so_a_caller_can_downgrade() {
        let index = index(&[
            ("/p/widget.h", "struct Widget { int size; };\n"),
            ("/p/main.cpp", "#include \"widget.h\"\n"),
        ]);

        let found = index.files_declaring("Widget", Path::new("/p/main.cpp"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].visibility, IncludeVisibility::Unconditional);
    }

    /// The offset of the last `needle`, which is the cursor position in these fixtures.
    fn at(source: &str, needle: &str) -> usize {
        source
            .rfind(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not in {source:?}"))
    }

    /// An index over the fixtures **plus** the analysed querying file, which is what
    /// [`super::definition_across_files`] needs: the index holds the other files, and the scope tree holds this
    /// one.
    fn analysed(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
    ) -> (ProjectIndex, cpp_parser::CppSyntaxTree) {
        let (index, tree) = analysed_while_typing(files, from, source);

        assert!(
            tree.get_errors().is_empty(),
            "the fixture must parse cleanly: {source:?}"
        );

        (index, tree)
    }

    /// The same, for a file that is **being typed** and therefore need not parse cleanly.
    ///
    /// Separate from [`analysed`] rather than a flag on it, because the two assertions mean different things: a
    /// shape assertion is worthless if the tree it reads came from a file with errors, while an incomplete file is
    /// the *state the completion query exists for* — `w.` is not a program, it is a question. What is deliberately
    /// **not** relaxed is that the querying file is indexed: the index and the tree have to describe the same text
    /// or the offsets in the answer mean something else.
    fn analysed_while_typing(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
    ) -> (ProjectIndex, cpp_parser::CppSyntaxTree) {
        let mut index = index(files);

        let tree = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());

        let mut summary = summarize(Path::new(from), source, SummaryKey::new(0, 0));
        for include in &mut summary.includes {
            include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
        }
        index.insert(summary);

        (index, tree)
    }

    #[test]
    fn a_local_declaration_is_answered_without_looking_in_the_index() {
        // The first step of the resolution order, and the one that has to win: a local shadows a header's
        // declaration, so a jump that went into the header would be to a different entity.
        let source = "#include \"widget.h\"\nvoid f() {\n  int count = 0;\n  count = 1;\n}\n";
        let (index, tree) = analysed(&[("/p/widget.h", "int count;\n")], "/p/main.cpp", source);

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "count = 1;"),
        );

        let Known::Yes(definition) = found else {
            panic!("the local must win: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/main.cpp"));
        assert!(
            definition.fact.range.start_offset > at(source, "void f"),
            "the jump goes to the local, not to the header"
        );
    }

    #[test]
    fn another_files_local_is_never_the_answer() {
        // The wrong answer this exists to prevent, and it is the *common* one rather than a corner: a header's
        // function bodies declare thousands of names (`__first`, `__n`, `_Tp`), and every one of them is a
        // declaration in the index with no scope to place it in. A lookup that matched one would answer with a
        // name the reader cannot see — and, worse, a name that is not the entity the cursor is asking about.
        let source = "#include \"widget.h\"\nvoid f() {\n  helper();\n}\n";
        let (index, tree) = analysed(
            &[(
                "/p/widget.h",
                "void g() {\n  int helper = 0;\n}\nstruct Widget { int size; };\n",
            )],
            "/p/main.cpp",
            source,
        );

        // The fact itself, so that a failure says which half broke: the header's declaration is marked local.
        let header = index.summary(Path::new("/p/widget.h")).expect("indexed");
        let helper = header
            .declarations
            .iter()
            .find(|fact| fact.name == "helper")
            .expect("the header declares `helper`");
        assert!(helper.local, "declared inside `g`'s body");
        assert_eq!(helper.scope, None, "…which is why the scope cannot say so");

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "helper();"),
        );

        assert!(
            matches!(
                found,
                Known::Unknown(UnknownReason::NotDeclaredHere(_)) | Known::No
            ),
            "the header's local is not a declaration of the name this file writes: {found:?}"
        );

        // And the other half, so that the skip is a filter rather than a hole: a *file-scope* declaration in the
        // same header is still found through the index.
        let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n}\n";
        let (index, tree) = analysed(
            &[(
                "/p/widget.h",
                "void g() {\n  int helper = 0;\n}\nstruct Widget { int size; };\n",
            )],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "Widget w;"),
        );

        let Known::Yes(definition) = found else {
            panic!("`Widget` is at file scope in the header: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/widget.h"));
        assert!(!definition.fact.local);
    }

    #[test]
    fn a_call_resolves_through_the_return_type_of_a_header() {
        // The whole chain, and three of its links are in the header rather than in the buffer: the class, the
        // member function, and the class its return type names. This is what `DeclFact::returns` was added for,
        // and it is also the path a cached summary goes through — the field is written and read by the codec.
        let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n  w.inner().size = 1;\n}\n";
        let (index, tree) = analysed(
            &[(
                "/p/widget.h",
                "struct Inner {\n  int size;\n};\nstruct Widget {\n  Inner inner();\n};\n",
            )],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::member_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "size = 1;"),
        );

        let Known::Yes(definition) = found else {
            panic!("`w.inner()` returns an `Inner`, which declares `size`: {found:?}");
        };
        assert_eq!(definition.fact.name, "size");
        assert_eq!(
            definition.file,
            Path::new("/p/widget.h"),
            "the member is in the header, like everything else on this chain"
        );
    }

    #[test]
    fn a_name_only_a_header_declares_is_found_through_the_index() {        let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n}\n";
        let (index, tree) = analysed(
            &[("/p/widget.h", "struct Widget { int size; };\n")],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "Widget w;"),
        );

        let Known::Yes(definition) = found else {
            panic!("the header's class must be found: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/widget.h"));
        assert_eq!(definition.fact.name, "Widget");
    }

    #[test]
    fn a_qualified_name_resolves_into_an_included_header() {
        // The two layers together: the qualifier names a namespace that only the *header* declares, so the
        // single-file layer cannot answer it and the spelling goes to the index — which can, because a fact
        // records the scope it was written in.
        let source = "#include \"a.h\"\nvoid f() {\n  a::Widget w;\n}\n";
        let (index, tree) = analysed(
            &[("/p/a.h", "namespace a {\n  struct Widget { int x; };\n}\n")],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "Widget w;"),
        );

        let Known::Yes(definition) = found else {
            panic!("the header's `a::Widget` must be found: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/a.h"));
        assert_eq!(definition.fact.qualified_name(), "a::Widget");
    }

    #[test]
    fn a_qualified_name_is_not_answered_by_a_same_named_declaration_elsewhere() {
        // The wrong answer qualification exists to prevent, one file further out: `b::Widget` is in scope (its
        // header is included), and the cursor asked for `a::Widget`, which is not. Answering with `b`'s would be a
        // jump to a different entity that the user cannot tell apart from the right one.
        let source = "#include \"b.h\"\nvoid f() {\n  a::Widget w;\n}\n";
        let (index, tree) = analysed(
            &[("/p/b.h", "namespace b {\n  struct Widget { int y; };\n}\n")],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "Widget w;"),
        );

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "`a::Widget` is not declared anywhere this file can see: {found:?}"
        );
    }

    #[test]
    fn a_global_spelling_does_not_match_a_namespaced_declaration() {
        // `::Widget` means the global name space. Both declarations exist and both are visible, so a matcher that
        // dropped the `::` would have two candidates and no way to choose — the prefix is what chooses.
        let source = "#include \"both.h\"\nvoid f() {\n  ::Widget w;\n}\n";
        let (index, tree) = analysed(
            &[(
                "/p/both.h",
                "struct Widget { int global; };\nnamespace ns {\n  struct Widget { int nested; };\n}\n",
            )],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "Widget w;"),
        );

        let Known::Yes(definition) = found else {
            panic!("the global `Widget` is the one asked for: {found:?}");
        };
        assert_eq!(definition.fact.scope, None);
        assert_eq!(definition.fact.name, "Widget");
    }

    // -------------------------------------------------------------------------------------------
    // Member access
    //
    // The first query that needs a *type*. Every fixture here declares the object and its type in the same
    // file, because that is where the interesting failure is — the lookup that would otherwise happen is
    // "find some `size` in scope", and a test that only ever has one `size` in the project cannot tell the
    // difference between typing the object and guessing.
    // -------------------------------------------------------------------------------------------

    /// The member a `needle` position names, with the file analysed and indexed the way a real query has it.
    fn member_of(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
        needle: &str,
    ) -> Known<super::ProjectDefinition> {
        let (index, tree) = analysed(files, from, source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);

        super::member_across_files(&index, &scopes, &root, Path::new(from), at(source, needle))
    }

    #[test]
    fn a_member_access_resolves_through_the_objects_type() {
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`Widget::size` is declared in this file: {found:?}");
        };
        assert_eq!(member.fact.name, "size");
        assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
        assert!(
            member.fact.range.start_offset < at(source, "void f"),
            "the jump goes to the member's declaration, not to the use"
        );
    }

    #[test]
    fn a_member_of_another_class_with_the_same_name_is_not_the_answer() {
        // The wrong answer this query exists to avoid: two classes with a `size`, and the object decides which
        // one. A lookup that ignored the object would answer with whichever was declared first.
        let source = "struct Other {\n  int size;\n};\nstruct Widget {\n  int size;\n};\n\
                      void f() {\n  Widget w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`Widget::size` is the one the object names: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
        assert!(
            member.fact.range.start_offset > at(source, "struct Widget"),
            "the jump lands in `Widget`, not in `Other`"
        );
    }

    #[test]
    fn a_member_access_reads_a_pointer_the_same_way() {
        // `ptr->member` names a member of the pointee. The type spelling of `Widget* p` is `Widget*`, and the
        // pointer is a fact about the declarator rather than about which class the name refers to.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget* p;\n  p->size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("the pointee's member is the answer: {found:?}");
        };
        assert_eq!(member.fact.name, "size");
    }

    #[test]
    fn a_member_access_reaches_a_class_in_an_included_header() {
        // The everyday case: the class is in the header, the object is local to the `.cpp`.
        let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n  w.size = 1;\n}\n";
        let (index, tree) = analysed(
            &[("/p/widget.h", "struct Widget {\n  int size;\n};\n")],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::member_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "size = 1;"),
        );

        let Known::Yes(member) = found else {
            panic!("the member declared in the header is the answer: {found:?}");
        };
        assert_eq!(member.file, Path::new("/p/widget.h"));
        assert_eq!(member.fact.name, "size");
    }

    #[test]
    fn a_member_access_through_a_qualified_type_resolves() {
        // `ns::Widget` as a type: the type spelling goes through the qualified-name machinery rather than being
        // treated as a plain name, which is what makes the two features compose instead of each needing its own
        // path.
        let source = "namespace ns {\n  struct Widget {\n    int size;\n  };\n}\n\
                      void f() {\n  ns::Widget w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`ns::Widget::size` is in this file: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("ns::Widget"));
    }

    #[test]
    fn a_member_of_a_class_that_does_not_have_it_is_not_declared_here() {
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.nope = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "nope = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "the class is known and the member is not in it: {found:?}"
        );
    }

    #[test]
    fn a_member_access_on_a_call_is_the_callees_return_type() {
        // `make().size` — the construct `DeclFact::returns` exists for.
        //
        // This test used to assert the *opposite*, and its comment said so: "the boundary of this layer, stated as
        // an answer rather than as a wrong guess: `f().size` has a type, and working it out is the larger problem
        // the type layer will have to take on". That problem is now solved for a call whose callee is a
        // declaration this analysis can find, so the boundary moved — the failure of this test is what said so.
        let source = "struct Widget {\n  int size;\n};\nWidget make();\n\
                      void f() {\n  make().size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(definition) = found else {
            panic!("`make()` returns a `Widget`, so `size` is its member: {found:?}");
        };
        assert_eq!(definition.fact.name, "size");
        assert_eq!(definition.fact.type_of.as_deref(), Some("int"));

        // The other reading of the same tokens: `Widget()` is a **temporary** of the class rather than a call of
        // anything, and the declaration is what says which one this is.
        let constructed = member_of(
            &[],
            "/p/a.cpp",
            "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget().size = 1;\n}\n",
            "size = 1;",
        );
        assert!(
            matches!(constructed, Known::Yes(_)),
            "a temporary of a known class has the class's members: {constructed:?}"
        );
    }

    #[test]
    fn a_member_access_on_an_expression_that_is_not_a_call_is_an_unknown_type() {
        // What is left of the old boundary, and the reason it is still a boundary: a dereference, a subscript and
        // an arithmetic expression each need a type *computed* rather than read off a declaration.
        let source = "struct Widget {\n  int size;\n};\nWidget* make();\n\
                      void f() {\n  (*make()).size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnknownType(_))),
            "the object is a dereference, not a call: {found:?}"
        );
    }

    #[test]
    fn a_call_of_something_with_no_return_type_is_an_unknown_type() {
        // The boundary inside the new capability, and it is the *honest* answer rather than a wrong guess: a
        // deduced `auto` is not a class anything can be looked up in, so the file never says what this call has.
        let source = "struct Widget {\n  int size;\n};\nauto make() { return Widget{}; }\n\
                      void f() {\n  make().size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnknownType(_))),
            "the return type is deduced, so the file does not state it: {found:?}"
        );
    }

    #[test]
    fn a_member_access_on_an_unknown_object_is_an_unknown_type() {
        // The object is a name, but nothing declares it — so there is no type to read, and the answer has to say
        // *that* rather than "the member is missing".
        let source = "void f() {\n  widget.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnknownType(_))),
            "nothing says what `widget` is: {found:?}"
        );
    }

    #[test]
    fn a_cursor_that_is_not_on_a_member_access_has_nothing_to_look_up() {
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "= 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
            "the cursor is not on a member name: {found:?}"
        );
    }

    #[test]
    fn a_nested_member_access_follows_the_types_down() {
        // This test used to assert the opposite — that `a.b.size` is `Unknown`, because inferring a nested
        // object's type was not done. It is done now, and this is what it bought: the inner access is typed by the
        // same query, so the outer one is answered by asking the inner one what it is.
        let source = "struct Inner {\n  int size;\n};\nstruct Outer {\n  Inner b;\n};\n\
                      void f() {\n  Outer a;\n  a.b.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`Inner::size` is what `a.b.size` names: {found:?}");
        };
        assert_eq!(member.fact.name, "size");
        assert_eq!(member.fact.scope.as_deref(), Some("Inner"));
        assert!(
            member.fact.range.start_offset < at(source, "struct Outer"),
            "the jump lands in `Inner`, not in `Outer`"
        );
    }

    #[test]
    fn a_nested_member_access_that_disagrees_with_the_outer_class_is_not_answered_by_it() {
        // `Outer` has a `size` of its own *and* a member whose type has one: the answer is the inner one, because
        // the object is `a.b`. A lookup that fell back to the outer class when the object was not a plain name
        // would land on `Outer::size`, which is the wrong entity and looks plausible.
        let source = "struct Inner {\n  int size;\n};\nstruct Outer {\n  Inner b;\n  int size;\n};\n\
                      void f() {\n  Outer a;\n  a.b.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("the member of `a.b`'s type is the answer: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Inner"));
    }

    #[test]
    fn this_resolves_to_the_enclosing_class() {
        // `this` needs no inference at all: the scope chain already knows which class it is, which is why it is
        // the cheapest case in the type layer and the first one to handle.
        let source = "struct Widget {\n  int size;\n  void grow() {\n    this->size = 1;\n  }\n};\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`this->size` is `Widget::size`: {found:?}");
        };
        assert_eq!(member.fact.name, "size");
        assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
    }

    #[test]
    fn this_resolves_to_the_nearest_class_when_classes_are_nested() {
        let source = "struct Outer {\n  int size;\n  struct Inner {\n    int size;\n    void f() {\n      \
                      this->size = 1;\n    }\n  };\n};\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`this` inside `Inner` is `Inner`: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Outer::Inner"));
    }

    #[test]
    fn a_nested_access_propagates_why_the_inner_one_failed() {
        // The inner access is where the trouble is, and its reason is kept rather than flattened: there is no
        // member `nope` in `Outer`, so the inner one has no type — and saying "`Outer` has no `nope`" tells a
        // consumer what to fix, where "the type of `a.nope` is unknown" would only say that something is wrong.
        // (This test's first expectation was `UnknownType`, which is what flattening would produce.)
        let source = "struct Inner {\n  int size;\n};\nstruct Outer {\n  Inner b;\n};\n\
                      void f() {\n  Outer a;\n  a.nope.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "there is no `nope` to type, and that is the answer worth keeping: {found:?}"
        );
    }

    #[test]
    fn a_member_inherited_from_a_base_class_resolves() {
        // The everyday OO case: the member is not in the class the object's type names, it is in the class that
        // one inherits from. Without the base walk this is `Unknown(NotDeclaredHere)`, which is the answer a user
        // would see for most of their code.
        let source = "struct Base {\n  int size;\n};\nstruct Derived : public Base {\n  int other;\n};\n\
                      void f() {\n  Derived d;\n  d.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`Base::size` is inherited by `Derived`: {found:?}");
        };
        assert_eq!(member.fact.name, "size");
        assert_eq!(member.fact.scope.as_deref(), Some("Base"));
        assert!(
            member.fact.range.start_offset < at(source, "struct Derived"),
            "the jump lands in the base, not in the derived class"
        );
    }

    #[test]
    fn a_member_of_the_class_itself_hides_one_in_a_base() {
        // Level by level, and the class's own level is first: a `size` written in `Derived` wins over `Base`'s,
        // which is what the language does and what a reader expects.
        let source = "struct Base {\n  int size;\n};\nstruct Derived : public Base {\n  int size;\n};\n\
                      void f() {\n  Derived d;\n  d.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("the class's own member hides the one it inherits: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Derived"));
    }

    #[test]
    fn a_member_inherited_through_another_base_resolves() {
        // Two levels: `Derived` has nothing, `Middle` has nothing, `Base` has it.
        let source = "struct Base {\n  int size;\n};\nstruct Middle : public Base {\n  int m;\n};\n\
                      struct Derived : public Middle {\n  int d;\n};\n\
                      void f() {\n  Derived x;\n  x.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("a member two bases down is still inherited: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Base"));
    }

    #[test]
    fn a_member_declared_in_two_bases_at_once_is_ambiguous_rather_than_picked() {
        // A diamond without `virtual`: both sides declare `size`, so the name is not uniquely resolved by the
        // language. Picking the first would be a jump to an entity the user cannot tell from the other, which is
        // exactly the wrong answer this project refuses to give.
        let source = "struct Left {\n  int size;\n};\nstruct Right {\n  int size;\n};\n\
                      struct Both : public Left, public Right {\n  int own;\n};\n\
                      void f() {\n  Both b;\n  b.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::Ambiguous(_))),
            "two bases declare it and nothing chooses: {found:?}"
        );
    }

    #[test]
    fn a_member_inherited_across_a_header_resolves() {
        // The base is in the header and the derived class is in the file being edited: the base *list* comes from
        // this file's tree and the base *class* from the index, which is the two halves meeting.
        let source = "#include \"base.h\"\nstruct Derived : public Base {\n  int d;\n};\n\
                      void f() {\n  Derived x;\n  x.size = 1;\n}\n";
        let (index, tree) = analysed(
            &[("/p/base.h", "struct Base {\n  int size;\n};\n")],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::member_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "size = 1;"),
        );

        let Known::Yes(member) = found else {
            panic!("the inherited member is declared in the header: {found:?}");
        };
        assert_eq!(member.file, Path::new("/p/base.h"));
        assert_eq!(member.fact.name, "size");
    }

    #[test]
    fn a_member_nobody_declares_is_still_not_declared() {
        // The base walk must not turn "not found" into "found somewhere": the honest answer survives it.
        let source = "struct Base {\n  int size;\n};\nstruct Derived : public Base {\n  int d;\n};\n\
                      void f() {\n  Derived x;\n  x.nope = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "nope = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "neither the class nor its base declares it: {found:?}"
        );
    }

    #[test]
    fn a_base_list_is_read_without_its_access_keywords() {
        // `public Base` is one base called `Base`: reading the whole specifier would look for a class named
        // "public Base", which is a name nothing declares.
        let source = "struct Base {\n  int size;\n};\nstruct Derived : private virtual Base {\n  int d;\n};\n\
                      void f() {\n  Derived x;\n  x.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("the access and `virtual` are not part of the base's name: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Base"));
    }

    #[test]
    fn a_base_that_inherits_from_itself_terminates() {
        // A cycle in the base list cannot be written in valid C++, and a malformed file can produce one: the
        // visited set is what keeps the walk from being an infinite loop on a file that is being typed.
        let source = "struct A : public B {\n  int a;\n};\nstruct B : public A {\n  int size;\n};\n\
                      void f() {\n  A x;\n  x.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("a cycle must not stop the member being found: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("B"));
    }

    #[test]
    fn a_local_declaration_still_wins_when_the_header_also_has_the_name() {
        // `count` is declared both here and in the header. The answer is this file's, and it is reached without
        // the index — which is the resolution order, not an optimisation.
        let source = "#include \"widget.h\"\nint count = 0;\nvoid f() {\n  count = 1;\n}\n";
        let (index, tree) = analysed(&[("/p/widget.h", "int count = 7;\n")], "/p/main.cpp", source);

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "count = 1;"),
        );

        let Known::Yes(definition) = found else {
            panic!("the file's own declaration wins: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/main.cpp"));
    }

    // -------------------------------------------------------------------------------------------
    // Member lists
    //
    // The *list* form of the member query, and the fixtures are written to make the one decision it makes
    // visible: which class declares each member, and whether that is the type asked about or a base. The
    // staleness test at the end is the reason none of it is stored.
    // -------------------------------------------------------------------------------------------

    /// The members of `class`, with the file analysed and indexed the way a real query has them.
    fn members_of_class(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
        class: &str,
    ) -> Known<super::MemberList> {
        let (index, tree) = analysed(files, from, source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);

        super::members_of(&index, &scopes, &root, Path::new(from), class)
    }

    /// The member names, in the order the query produced them.
    fn names(list: &super::MemberList) -> Vec<&str> {
        list.members
            .iter()
            .map(|member| member.fact.name.as_str())
            .collect()
    }

    #[test]
    fn a_class_lists_the_members_written_in_it() {
        // Written out of alphabetical order on purpose: a member list is ordered by name rather than by the text,
        // because the file's scope tree keeps its bindings name-sorted while the index keeps facts in offset
        // order, and the two have to agree for a class to list the same way wherever it is declared.
        let source = "struct Widget {\n  void grow();\n  int size;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Widget");

        let Known::Yes(list) = found else {
            panic!("`Widget` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["grow", "size"]);
        assert!(
            list.unlisted.is_empty(),
            "a class with no bases has nothing left unread: {:?}",
            list.unlisted
        );

        let size = &list.members[1];
        assert_eq!(size.declared_in, "Widget");
        assert_eq!(size.depth, 0, "its own member, not an inherited one");
        assert!(!size.ambiguous);
        assert_eq!(size.file, Path::new("/p/a.cpp"));
        assert_eq!(
            size.fact.type_of.as_deref(),
            Some("int"),
            "the type a completion shows beside the name"
        );
        assert_eq!(list.own().count(), 2, "every member here is the class's own");
    }

    #[test]
    fn a_derived_class_lists_its_own_members_before_the_ones_it_inherits() {
        // Nearest first, which is the order C++ hides in — and the base is tagged rather than flattened into the
        // derived class, so a consumer can show an inherited member as inherited and can jump to `Base::count`
        // rather than to a copy of it.
        let source = "struct Base {\n  int inherited_count;\n};\n\
                      struct Derived : public Base {\n  int own_count;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

        let Known::Yes(list) = found else {
            panic!("`Derived` is declared in this file: {found:?}");
        };

        assert_eq!(
            names(&list),
            ["own_count", "inherited_count"],
            "the class's own members come first, and the inherited one is last"
        );
        assert_eq!(list.members[0].declared_in, "Derived");
        assert_eq!(list.members[0].depth, 0);
        assert_eq!(
            list.members[1].declared_in, "Base",
            "the member is the base's, not the derived class's"
        );
        assert_eq!(list.members[1].depth, 1);
        assert_eq!(
            list.members[1].fact.type_of.as_deref(),
            Some("int"),
            "and it carries its own declaration's type"
        );
        assert_eq!(list.at_depth(1).count(), 1);
    }

    #[test]
    fn a_member_the_class_redeclares_hides_the_one_in_its_base() {
        // Hiding is by name, not by signature: `Derived::size` hides `Base::size` whatever the parameters are,
        // so the base's is not in the list. A list that kept both would offer a name the language does not find.
        let source = "struct Base {\n  int size;\n};\n\
                      struct Derived : public Base {\n  double size;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

        let Known::Yes(list) = found else {
            panic!("`Derived` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["size"], "one `size`, and it is the derived class's");
        assert_eq!(list.members[0].declared_in, "Derived");
        assert_eq!(list.members[0].fact.type_of.as_deref(), Some("double"));
    }

    #[test]
    fn a_name_two_bases_declare_is_listed_twice_and_marked_ambiguous() {
        // The list form of the ambiguity the single-member query reports: both declarations are kept, because
        // dropping either would be choosing one for the user, and both are flagged, because a consumer offering
        // the name has to be able to say that the language does not resolve it here.
        let source = "struct Left {\n  int value;\n};\nstruct Right {\n  int value;\n};\n\
                      struct Both : public Left, public Right {\n  int own;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Both");

        let Known::Yes(list) = found else {
            panic!("`Both` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["own", "value", "value"]);

        let values: Vec<&super::ProjectMember> = list
            .members
            .iter()
            .filter(|member| member.fact.name == "value")
            .collect();
        assert_eq!(values.len(), 2, "both declarations survive into the list");
        assert!(
            values.iter().all(|member| member.ambiguous),
            "both are contested: {values:?}"
        );

        let mut declaring: Vec<&str> = values
            .iter()
            .map(|member| member.declared_in.as_str())
            .collect();
        declaring.sort_unstable();
        assert_eq!(declaring, ["Left", "Right"]);

        assert!(
            !list.members[0].ambiguous,
            "`own` is declared once, in one class"
        );
    }

    #[test]
    fn a_member_whose_name_is_not_recorded_does_not_hide_one_in_a_base() {
        // `virtual ~Base();` is what makes this real rather than hypothetical: the specifier sequence is what lets
        // the declaration through the scope walker, and the name it binds is `~Base` — a destructor, whose
        // `identifier_text()` is `None`, so the fact stores an **empty** name. Comparing two empty names made
        // `~Derived` hide `~Base`, which is a wrong answer arrived at by treating a missing spelling as if it were
        // a spelling: the two differ by the class they name, and neither one's spelling is in the fact.
        let source = "struct Base {\n  virtual ~Base();\n  int size;\n};\n\
                      struct Derived : public Base {\n  virtual ~Derived();\n  int d;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

        let Known::Yes(list) = found else {
            panic!("`Derived` is declared in this file: {found:?}");
        };

        let unnamed: Vec<&super::ProjectMember> = list
            .members
            .iter()
            .filter(|member| member.fact.name.is_empty())
            .collect();
        assert_eq!(
            unnamed.len(),
            2,
            "both destructors are declarations that exist, and dropping one would be a silent omission: {:?}",
            names(&list)
        );

        let mut classes: Vec<&str> = unnamed
            .iter()
            .map(|member| member.declared_in.as_str())
            .collect();
        classes.sort_unstable();
        assert_eq!(classes, ["Base", "Derived"]);

        assert!(
            unnamed.iter().all(|member| !member.ambiguous),
            "an empty name is not one name two classes contest: {unnamed:?}"
        );
        assert!(
            list.members
                .iter()
                .any(|member| member.fact.name == "size" && member.declared_in == "Base"),
            "and the named members still cross the inheritance boundary: {:?}",
            names(&list)
        );
    }

    #[test]
    fn a_destructor_without_a_specifier_is_not_a_member_yet() {
        // The boundary, asserted rather than left to be discovered — see "现在答不了什么" in
        // `docs/index-design.md`, and `a_destructor_without_a_specifier_declares_nothing_yet` in
        // `tests/scopes.rs` for the rule and for what landing it needs.
        //
        // What this test is really pinning is the *other* half: the class is found by its own name. Before the fix
        // in `name_from_text`, a class whose body held a destructor was itself named `~Derived` — its scope was
        // `~Derived`, `d` was filed under that, and `Derived` was never bound. So this fixture used to answer
        // `Unknown(NotDeclaredHere("Derived"))`, which is why it is here as well as in `tests/scopes.rs`.
        let source = "struct Derived {\n  ~Derived();\n  int d;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

        let Known::Yes(list) = found else {
            panic!("the class must be found by its own name: {found:?}");
        };
        assert_eq!(
            names(&list),
            ["d"],
            "`~Derived` is not listed yet: the walker binds no declarator that is not an `InitDeclarator`, so the \
             destructor is not a fact for this query to list"
        );
    }

    #[test]
    fn overloads_in_one_class_are_not_ambiguous() {
        // The other half of the rule above, and the case a count-of-declarations implementation gets wrong:
        // `f` is declared twice and is one name in one class, so nothing about it is contested.
        let source = "struct Widget {\n  void f();\n  void f(int);\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Widget");

        let Known::Yes(list) = found else {
            panic!("`Widget` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["f", "f"], "both overloads are listed");
        assert!(
            list.members.iter().all(|member| !member.ambiguous),
            "one class declaring a name twice is an overload set, not an ambiguity: {:?}",
            list.members
        );
    }

    #[test]
    fn a_cycle_of_bases_terminates_and_each_class_contributes_once() {
        // A base list that cannot be written in valid C++ and can be produced by a malformed file. The visited set
        // is what keeps the walk finite; without it this test does not fail, it does not return.
        let source = "struct A : public B {\n  int a_member;\n};\nstruct B : public A {\n  int b_member;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "A");

        let Known::Yes(list) = found else {
            panic!("`A` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["a_member", "b_member"]);
        assert_eq!(list.members[0].depth, 0);
        assert_eq!(
            list.members[1].depth, 1,
            "`A` is reached again as `B`'s base and is not listed a second time"
        );
    }

    #[test]
    fn a_template_class_lists_the_members_it_was_written_with() {
        // No instantiation, and the answer says so by what it contains: `value` has the type `T`, which is what
        // the class wrote. Instantiating would mean picking an argument, and a member list for `Holder<int>` and
        // `Holder<std::string>` would then be two different lists — which is a `sema` question, not this one.
        let source = "template <typename T>\nstruct Holder {\n  T value;\n  int count;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Holder");

        let Known::Yes(list) = found else {
            panic!("`Holder` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["count", "value"]);
        assert_eq!(
            list.members[1].fact.type_of.as_deref(),
            Some("T"),
            "the parameter as written, not an instantiated type"
        );
        assert!(
            !list.members.iter().any(|member| member.fact.name == "T"),
            "the template parameter is declared in the parameter list, not in the class: {:?}",
            names(&list)
        );
    }

    #[test]
    fn a_type_nothing_declares_has_no_member_list_rather_than_an_empty_one() {
        // The distinction the whole crate is built around, asked of a list: an empty list is a claim that the type
        // has no members, and this analysis cannot make it about a type it has never seen.
        let source = "void f() { }\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Nowhere");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "`Nowhere` is not a type this analysis has seen: {found:?}"
        );
    }

    #[test]
    fn a_class_that_is_empty_has_an_empty_member_list() {
        // The other side of the test above, and the one that needs the *name* to be asked about rather than the
        // members: `struct Empty { };` writes no member and no scope either — see `scopes::class_like` — so a
        // query that read "no members found" as "no class found" would report an empty class as a missing one.
        let source = "struct Empty { };\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Empty");

        let Known::Yes(list) = found else {
            panic!("`Empty` is declared in this file and has no members: {found:?}");
        };
        assert!(list.members.is_empty());
    }

    #[test]
    fn a_member_declared_in_a_nested_class_is_not_a_member_of_the_outer_one() {
        // The scope a fact records is the scope it was written in, so `Inner::x` is a member of `Inner` and the
        // member of `Outer` is the class `Inner`. A list built by name rather than by scope would put `x` in both.
        let source = "struct Outer {\n  struct Inner {\n    int x;\n  };\n  int y;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Outer");

        let Known::Yes(list) = found else {
            panic!("`Outer` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["Inner", "y"]);
        assert_eq!(list.members[0].fact.qualified_name(), "Outer::Inner");
    }

    #[test]
    fn a_member_list_crosses_a_header_and_keeps_the_base_tagged() {
        // The everyday layout: the base is in a header, the derived class is in the file being edited. The base
        // list comes from the buffer's tree and the base's members from the index, and the answer has to name the
        // header as the place to jump to rather than the buffer.
        let source = "#include \"base.h\"\nstruct Derived : public Base {\n  int d;\n};\n";
        let (index, tree) = analysed(
            &[("/p/base.h", "struct Base {\n  int size;\n};\n")],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);

        let found = super::members_of(&index, &scopes, &root, Path::new("/p/main.cpp"), "Derived");
        let Known::Yes(list) = found else {
            panic!("`Derived` is declared in the buffer: {found:?}");
        };

        assert_eq!(names(&list), ["d", "size"]);
        assert_eq!(list.members[1].file, Path::new("/p/base.h"));
        assert_eq!(list.members[1].declared_in, "Base");
    }

    #[test]
    fn a_base_that_resolves_to_a_template_is_listed_without_instantiating_it() {
        // `public Base<int>` is the class `Base` for the purpose of finding members: the arguments say which type
        // is inherited, not which class declares what. Reading the spelling literally would look for a class named
        // `Base<int>`, which is a name no declaration has.
        let source = "template <typename T>\nstruct Base {\n  int size;\n};\n\
                      struct Derived : public Base<int> {\n  int d;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

        let Known::Yes(list) = found else {
            panic!("`Derived` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["d", "size"]);
        assert_eq!(list.members[1].declared_in, "Base");
    }

    #[test]
    fn a_base_nothing_declares_is_reported_as_a_gap_rather_than_dropped() {
        // The list is incomplete and says so. Answering with just `d` would be a claim that `Derived` has one
        // member, which is exactly what this analysis cannot know: `Base` is behind an include nobody indexed.
        let source = "#include \"missing.h\"\nstruct Derived : public Base {\n  int d;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

        let Known::Yes(list) = found else {
            panic!("`Derived` itself is in the buffer: {found:?}");
        };

        assert_eq!(names(&list), ["d"], "what could be listed is listed");
        assert_eq!(list.unlisted.len(), 1, "and what could not is named");
        assert_eq!(list.unlisted[0].spelling, "Base");
        assert!(matches!(list.unlisted[0].reason, UnknownReason::NotDeclaredHere(_)),
            "the reason has to say that the base is not here, not that the list ended: {:?}",
            list.unlisted[0].reason
        );
    }

    #[test]
    fn a_type_spelled_from_the_global_name_space_lists_the_same_members() {
        // `::Widget w;` is a declaration whose type spelling carries the leading `::`, and a consumer feeding this
        // query from `DeclFact.type_of` therefore hands it `::Widget`. The index keys a global declaration under
        // its bare name — a file-scope declaration has no scope prefix — so the prefix has to come off before the
        // walk, or a class that is plainly in the buffer answers `Unknown(NotDeclaredHere("::Widget"))`.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  ::Widget w;\n}\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "::Widget");

        let Known::Yes(list) = found else {
            panic!("`::Widget` is the file-scope `Widget`: {found:?}");
        };
        assert_eq!(names(&list), ["size"]);
        assert_eq!(list.members[0].declared_in, "Widget");
    }

    #[test]
    fn a_member_added_to_a_base_appears_without_reindexing_the_derived_class() {
        // The test that decides whether inherited members are *materialized*, and the reason they are not.
        //
        // Only `b.h` is rebuilt below. Nothing about `A`'s text or its key changes, so a summary that stored the
        // members `A` inherits would go on answering with `old_member` for ever — and no invalidation rule could
        // catch it, because there is nothing to invalidate: the edit is in a file `A` never mentions by name.
        // Walking the chain at query time is what makes the second assertion true.
        let a_header = "#include \"b.h\"\nstruct A : public B {\n  int a_member;\n};\n";
        let before = "struct B {\n  int old_member;\n};\n";
        let after = "struct B {\n  int new_member;\n};\n";
        let main = "#include \"a.h\"\n";

        let mut index = index(&[
            ("/p/main.cpp", main),
            ("/p/a.h", a_header),
            ("/p/b.h", before),
        ]);

        let tree = cpp_parser::CppParser::parse(main, cpp_parser::ParserConfig::default());
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);

        let listed = |index: &ProjectIndex| {
            match super::members_of(index, &scopes, &root, Path::new("/p/main.cpp"), "A") {
                Known::Yes(list) => list
                    .members
                    .iter()
                    .map(|member| member.fact.name.clone())
                    .collect::<Vec<_>>(),
                other => panic!("`A` is declared in an included header: {other:?}"),
            }
        };

        assert_eq!(listed(&index), ["a_member", "old_member"]);

        // `b.h` and nothing else. `a.h` keeps the summary it was built with, which is the whole point.
        let mut rebuilt = summarize(Path::new("/p/b.h"), after, SummaryKey::new(1, 0));
        for include in &mut rebuilt.includes {
            include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
        }
        index.insert(rebuilt);

        assert_eq!(
            listed(&index),
            ["a_member", "new_member"],
            "the derived class's members are read from its bases at query time, so the edit is seen without \
             anything of `A`'s being rebuilt"
        );

        // And why that worked, asserted rather than implied: `A`'s own summary holds its own member and nothing it
        // inherits. The day something starts writing inherited members into a derived class's summary, this is
        // where it shows up — before the stale answers do.
        let a = index.summary(Path::new("/p/a.h")).expect("`a.h` is indexed");
        let a_members: Vec<&str> = a
            .declarations
            .iter()
            .filter(|fact| fact.scope.as_deref() == Some("A"))
            .map(|fact| fact.name.as_str())
            .collect();
        assert_eq!(
            a_members,
            ["a_member"],
            "a fact stores the spelling `B`, never the members `B` happens to have"
        );
    }

    // -------------------------------------------------------------------------------------------
    // Completion: the cursor-to-members query
    //
    // Every fixture here is an **incomplete program**, which is the only honest way to test this: `w.` does not
    // parse, and a test that made it parse would be testing a question nobody asks. The helper below therefore
    // does not assert a clean parse — see `analysed_while_typing`.
    // -------------------------------------------------------------------------------------------
    /// The offset just **past** the last `needle`, which is where the cursor is after that much has been typed.
    fn after(source: &str, needle: &str) -> usize {
        source
            .rfind(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not in {source:?}"))
            + needle.len()
    }

    /// The members offered at the cursor just past `needle`.
    fn completions_at(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
        needle: &str,
    ) -> Known<super::MemberCompletions> {
        let (index, tree) = analysed_while_typing(files, from, source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);

        super::member_completions_at(
            &index,
            &scopes,
            &root,
            Path::new(from),
            after(source, needle),
        )
    }

    #[test]
    fn a_member_access_with_nothing_typed_offers_the_types_members() {
        // The keystroke that asks the question. `w.` is not a program and does not parse, which is exactly why
        // this is a query over a damaged tree rather than over a compiled one.
        let source = "struct Widget {\n  int size;\n  void grow();\n};\n\
                      void f() {\n  Widget w;\n  w.\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "w.");

        let Known::Yes(completions) = found else {
            panic!("`w` is a `Widget` declared two lines up: {found:?}");
        };

        assert_eq!(completions.class, "Widget");
        assert_eq!(
            completions
                .members
                .members
                .iter()
                .map(|member| member.fact.name.as_str())
                .collect::<Vec<_>>(),
            ["grow", "size"]
        );
        assert_eq!(completions.prefix, "", "nothing of the name is written yet");
        assert_eq!(
            completions.member_range.start_offset,
            after(source, "w."),
            "the empty range sits just past the operator, which is where the name goes"
        );
        assert_eq!(completions.member_range.length, 0);
    }

    #[test]
    fn a_half_typed_member_offers_the_same_members_and_names_the_prefix() {
        // The other half of the same question: the list is the *whole* list — filtering is the client's job and
        // it has the document — while the range is what the client replaces with the chosen member.
        let source = "struct Widget {\n  int size;\n  void grow();\n};\n\
                      void f() {\n  Widget w;\n  w.si\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "w.si");

        let Known::Yes(completions) = found else {
            panic!("the cursor is on the member being typed: {found:?}");
        };

        assert_eq!(completions.class, "Widget");
        assert_eq!(completions.prefix, "si");
        assert_eq!(
            &source[completions.member_range.start_offset..completions.member_range.end_offset()],
            "si",
            "the range covers what is written, so replacing it does not leave a doubled name"
        );
    }

    #[test]
    fn a_pointer_member_access_offers_the_pointees_members() {
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget* p;\n  p->\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "p->");

        let Known::Yes(completions) = found else {
            panic!("`Widget* p` is a pointer to a `Widget`: {found:?}");
        };
        assert_eq!(completions.class, "Widget");
        assert_eq!(completions.members.members.len(), 1);
    }

    #[test]
    fn a_completion_on_this_offers_the_enclosing_classs_members() {
        // `this` needs no inference — the scope chain already knows which class it is — which makes it the one
        // completion inside a class body that works before the object has a name.
        let source = "struct Widget {\n  int size;\n  void grow() {\n    this->\n  }\n};\n";
        let found = completions_at(&[], "/p/a.cpp", source, "this->");

        let Known::Yes(completions) = found else {
            panic!("`this` is the enclosing `Widget`: {found:?}");
        };
        assert_eq!(completions.class, "Widget");
        assert_eq!(
            completions
                .members
                .members
                .iter()
                .map(|member| member.fact.name.as_str())
                .collect::<Vec<_>>(),
            ["grow", "size"]
        );
    }

    #[test]
    fn the_members_offered_include_the_ones_the_bases_declare() {
        // Completion is where the query-time base walk pays off: the whole inherited list is there, each entry
        // tagged with the class that declares it, without anything having been stored on `Derived`.
        let source = "struct Base {\n  int inherited_size;\n};\n\
                      struct Derived : public Base {\n  int own_size;\n};\n\
                      void f() {\n  Derived d;\n  d.\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "d.");

        let Known::Yes(completions) = found else {
            panic!("`d` is a `Derived`: {found:?}");
        };

        let listed: Vec<(&str, &str)> = completions
            .members
            .members
            .iter()
            .map(|member| (member.fact.name.as_str(), member.declared_in.as_str()))
            .collect();
        assert_eq!(
            listed,
            [("own_size", "Derived"), ("inherited_size", "Base")],
            "the class's own member first, then the inherited one, tagged with where it is declared"
        );
    }

    #[test]
    fn the_members_offered_cross_a_header() {
        let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n  w.\n}\n";
        let (index, tree) = analysed_while_typing(
            &[("/p/widget.h", "struct Widget {\n  int size;\n};\n")],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);

        let found = super::member_completions_at(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            after(source, "w."),
        );

        let Known::Yes(completions) = found else {
            panic!("the class is in the included header: {found:?}");
        };
        assert_eq!(completions.class, "Widget");
        assert_eq!(completions.members.members[0].file, Path::new("/p/widget.h"));
    }

    #[test]
    fn a_completion_after_a_call_offers_what_the_call_returns() {
        // `make().` — the keystroke this capability exists for, and the one the test below is the boundary of.
        // Before `returns` existed this answered `UnknownType`; now it is the same answer as `w.` for a `w` of the
        // same type, which is the whole point of recording a return type at all.
        let source = "struct Widget {\n  int size;\n  void grow();\n};\nWidget make();\n\
                      void f() {\n  make().\n}\n";
        let Known::Yes(completions) = completions_at(&[], "/p/a.cpp", source, "make().") else {
            panic!("`make()` returns a `Widget`");
        };

        assert_eq!(completions.class, "Widget");
        assert_eq!(
            completions
                .members
                .members
                .iter()
                .map(|member| member.fact.name.as_str())
                .collect::<Vec<_>>(),
            ["grow", "size"]
        );
    }

    #[test]
    fn a_completion_on_an_expression_with_no_type_offers_nothing_and_says_why() {
        // What is left of the boundary, and this is where a user meets it. Offering the members of some other
        // class would be worse than offering none: the list looks like an answer.
        let source = "struct Widget {\n  int size;\n};\nWidget* make();\n\
                      void f() {\n  (*make()).\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "(*make()).");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnknownType(_))),
            "a call's return type is not computed yet: {found:?}"
        );
    }

    #[test]
    fn a_completion_on_an_object_of_an_unknown_type_offers_nothing() {
        // The object resolves fine — `w`'s declaration *says* its type is `Nowhere` — and what fails is the type:
        // nothing here declares it. So the reason names the type rather than the object, which is what tells a
        // consumer whether to go looking for a missing declaration of `w` or of `Nowhere`.
        let source = "void f() {\n  Nowhere w;\n  w.\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "w.");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "`Nowhere` is a type nothing here declares: {found:?}"
        );
        let Known::Unknown(UnknownReason::NotDeclaredHere(name)) = found else {
            unreachable!("just checked")
        };
        assert_eq!(&*name, "Nowhere", "and the reason names the type, not the object");
    }

    #[test]
    fn a_cursor_that_is_not_on_a_member_access_offers_nothing() {
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "Widget w;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
            "a declaration is not a member access: {found:?}"
        );
    }

    #[test]
    fn a_cursor_on_the_object_asks_about_the_object() {
        // The offset decides, not the node: `w.size` with the cursor *on* `w` is a question about `w`, and
        // answering it with `Widget`'s members would insert a member name in the middle of the object's.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.size;\n}\n";
        let (index, tree) = analysed_while_typing(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);

        let found = super::member_completions_at(
            &index,
            &scopes,
            &root,
            Path::new("/p/a.cpp"),
            at(source, "w.size"),
        );

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
            "the cursor is on the object, before the operator: {found:?}"
        );
    }

    #[test]
    fn a_cursor_inside_a_name_filters_by_the_part_before_it() {
        // The range and the prefix answer two halves of one edit, and this is where they differ: the client
        // replaces all of `size`, while the filter matches `si` — the part the user has actually typed. Using the
        // whole spelling for both is what makes completion offer nothing in the middle of a word.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.size;\n}\n";
        let (index, tree) = analysed_while_typing(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);

        // Two characters into `size`.
        let cursor = at(source, "w.size") + 2 + 2;

        let found =
            super::member_completions_at(&index, &scopes, &root, Path::new("/p/a.cpp"), cursor);
        let Known::Yes(completions) = found else {
            panic!("the cursor is inside the member's name: {found:?}");
        };

        assert_eq!(completions.prefix, "si", "what has been typed so far");
        assert_eq!(
            &source[completions.member_range.start_offset..completions.member_range.end_offset()],
            "size",
            "and the whole name is what gets replaced"
        );
    }

    #[test]
    fn a_completed_member_access_still_answers_with_its_own_prefix() {
        // The state between two keystrokes: `w.size` is a finished expression, and a cursor at its end is still
        // asking for the members — with `size` as the prefix to replace. Nothing about the completion path is
        // special to the half-typed case.
        let source = "struct Widget {\n  int size;\n  void grow();\n};\n\
                      void f() {\n  Widget w;\n  w.size\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "w.size");

        let Known::Yes(completions) = found else {
            panic!("the cursor is at the end of `size`: {found:?}");
        };
        assert_eq!(completions.prefix, "size");
        assert_eq!(
            &source[completions.member_range.start_offset..completions.member_range.end_offset()],
            "size"
        );
    }

    #[test]
    fn a_jump_on_a_member_that_is_not_written_yet_has_nothing_to_jump_to() {
        // The completion query tolerates a nameless access; the *definition* query must not, and the difference is
        // the question rather than the shape. Reading `w.` as "the member named nothing" would answer
        // `NotDeclaredHere("Widget::")`, which says the class is missing a member that has not been typed.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.\n}\n";
        let (index, tree) = analysed_while_typing(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);

        let found = super::member_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/a.cpp"),
            after(source, "w."),
        );

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
            "there is no written name to resolve: {found:?}"
        );
    }

    // -------------------------------------------------------------------------------------------
    // Completion: the cursor-to-names query
    //
    // The sibling of the member query above, and the same kind of fixture: a *name being written* is not a
    // program, so these files do not parse and are not meant to. What each test asserts is one of the four
    // positions — after a `::`, after a class's `::`, after a global `::`, and with no qualifier at all — because
    // they reach the same query through different scopes.
    // -------------------------------------------------------------------------------------------

    /// The names offered at the cursor just past `needle`.
    fn names_at(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
        needle: &str,
    ) -> Known<super::NameCompletions> {
        let (index, tree) = analysed_while_typing(files, from, source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);

        super::name_completions_at(
            &index,
            &scopes,
            &root,
            Path::new(from),
            after(source, needle),
        )
    }

    /// The names in an answer, in the order the query put them.
    #[track_caller]
    fn offered(found: &Known<super::NameCompletions>) -> Vec<String> {
        match found {
            Known::Yes(completions) => completions
                .names
                .iter()
                .map(|name| name.fact.name.clone())
                .collect(),
            other => panic!("expected names: {other:?}"),
        }
    }

    #[test]
    fn a_qualified_scope_offers_the_names_written_in_it() {
        let source = "namespace ns {\n  struct Widget { };\n  int helper();\n}\nvoid f() {\n  ns::\n}\n";
        let found = names_at(&[], "/p/a.cpp", source, "ns::");

        assert_eq!(offered(&found), ["Widget", "helper"], "sorted within the scope");
        let Known::Yes(completions) = found else {
            unreachable!("asserted above")
        };
        assert_eq!(completions.scope, "ns", "the scope as a lookup key");
        assert_eq!(completions.prefix, "", "nothing of the name is typed yet");
        assert_eq!(
            completions.name_range.start_offset,
            after(source, "ns::"),
            "the empty range sits just past the `::`, where the name goes"
        );
        assert_eq!(completions.name_range.length, 0);
    }

    #[test]
    fn a_half_typed_name_keeps_the_prefix_and_replaces_the_whole_segment() {
        // The two halves of one edit, as in the member query: the range covers what the client replaces (`Wid`),
        // and the prefix is what it filters by — the part before the cursor.
        let source = "namespace ns {\n  struct Widget { };\n}\nvoid f() {\n  ns::Wid\n}\n";
        let Known::Yes(completions) = names_at(&[], "/p/a.cpp", source, "ns::Wid") else {
            panic!("the cursor is on the name being typed");
        };

        assert_eq!(completions.scope, "ns");
        assert_eq!(completions.prefix, "Wid");
        assert_eq!(
            completions.name_range.start_offset,
            at(source, "Wid"),
            "the range is the written segment, not the point"
        );
        assert_eq!(completions.name_range.length, 3);
    }

    #[test]
    fn a_class_scope_offers_its_members_and_the_ones_its_bases_declare() {
        // `Derived::` is a scope like `ns::`, and the difference between them is the language's: a class is
        // defined once, and the names it has include the ones it inherits.
        let source = "struct Base {\n  typedef int size_type;\n};\n\
                      struct Derived : Base {\n  typedef int value_type;\n};\n\
                      void f() {\n  Derived::\n}\n";
        let found = names_at(&[], "/p/a.cpp", source, "Derived::");

        assert_eq!(
            offered(&found),
            ["value_type", "size_type"],
            "the class's own names first, then the ones a base declares"
        );
        let Known::Yes(completions) = found else {
            unreachable!("asserted above")
        };
        assert_eq!(completions.scope, "Derived");
        assert_eq!(
            completions
                .names
                .iter()
                .map(|name| name.depth)
                .collect::<Vec<_>>(),
            [0, 1],
            "the base's name is one step further out, which is the order lookup walks in"
        );
    }

    #[test]
    fn a_global_scope_offers_only_what_is_declared_at_file_scope() {
        // `::` is a scope and not an empty qualifier: it asks about the global name space, so a namespace member
        // and a local are both *not* in the answer, while the file-scope declarations are — the enclosing function
        // and namespace included, because those are written there.
        let source = "int global;\nnamespace ns {\n  int inner;\n}\nvoid f() {\n  int local;\n  ::\n}\n";
        let found = names_at(&[], "/p/a.cpp", source, "::");

        assert_eq!(offered(&found), ["f", "global", "ns"]);
        let Known::Yes(completions) = found else {
            unreachable!("asserted above")
        };
        assert_eq!(completions.scope, "::", "the global name space, as spelled");

        let names = offered(&Known::Yes(completions.clone()));
        assert!(
            !names.contains(&"inner".to_string()) && !names.contains(&"local".to_string()),
            "a namespace member and a local are not in the global name space: {names:?}"
        );
    }

    #[test]
    fn a_bare_name_offers_the_scopes_the_cursor_is_inside_innermost_first() {
        // No qualifier: every name visible from here, and the order is C++'s — the body's own, then the class's,
        // then the namespace's, then the file's.
        let source = "int at_file_scope;\nnamespace ns {\n  int in_a_namespace;\n  struct C {\n    int member;\n    \
                      void g() {\n      int local;\n      loc\n    }\n  };\n}\n";
        let found = names_at(&[], "/p/a.cpp", source, "loc");

        assert_eq!(
            offered(&found),
            [
                // depth 0: the body the cursor is in
                "local",
                // depth 1: the class — its members, `g` among them
                "g",
                "member",
                // depth 2: the namespace
                "C",
                "in_a_namespace",
                // depth 3: file scope, which is also where every included file writes
                "at_file_scope",
                "ns"
            ]
        );

        let Known::Yes(completions) = found else {
            unreachable!("asserted above")
        };
        assert_eq!(completions.scope, "", "no qualifier was written");
        assert_eq!(completions.prefix, "loc");

        // The order is not alphabetical and is not meant to be: it is the order C++ searches in, so a consumer
        // that shows the list in this order shows the name the user would get first.
        let depths: Vec<usize> = completions.names.iter().map(|name| name.depth).collect();
        let mut sorted = depths.clone();
        sorted.sort_unstable();
        assert_eq!(depths, sorted, "nearer scopes come first");
        assert_eq!(depths[0], 0, "the local is in the scope the cursor is in");
    }

    #[test]
    fn another_files_local_is_not_offered_by_a_bare_name() {
        // The reason `local` is a field at all. The header's `__first` is a declaration in the index with no scope
        // to place it in, and the standard library's headers declare thousands of names like it — offering one
        // would fill the list with names the user cannot see, and the *right* answer for this cursor is the local
        // that really is here.
        let source = "#include \"widget.h\"\nvoid f() {\n  int mine;\n  min\n}\n";
        let found = names_at(
            &[(
                "/p/widget.h",
                "void g() {\n  int __first = 0;\n  int helper;\n}\nint global_name;\n",
            )],
            "/p/main.cpp",
            source,
            "min",
        );

        let names = offered(&found);
        assert!(
            names.contains(&"mine".to_string()),
            "the local of *this* file is offered, from its own scopes: {names:?}"
        );
        assert!(
            !names.contains(&"__first".to_string()) && !names.contains(&"helper".to_string()),
            "another file's locals are not names this file can write: {names:?}"
        );
        assert!(
            names.contains(&"global_name".to_string()),
            "…while a file-scope name in the same header is: {names:?}"
        );
    }

    #[test]
    fn a_namespace_is_merged_across_the_buffer_and_the_header() {
        // A namespace can be reopened by every file that mentions it, so the buffer's names are never the whole
        // answer — the difference from a class, which is defined once and listed from one place.
        let source = "#include \"ns.h\"\nnamespace ns {\n  int in_the_buffer;\n}\nvoid f() {\n  ns::\n}\n";
        let found = names_at(
            &[("/p/ns.h", "namespace ns {\n  int in_the_header;\n}\n")],
            "/p/main.cpp",
            source,
            "ns::",
        );

        assert_eq!(offered(&found), ["in_the_buffer", "in_the_header"]);
    }

    #[test]
    fn a_scope_nothing_declares_is_not_an_empty_list() {
        // "Declared and empty" and "no such scope" are different answers, and the second one is what stops a
        // consumer from reporting that the user is looking at a namespace with nothing in it.
        let source = "void f() {\n  nowhere::\n}\n";
        let found = names_at(&[], "/p/a.cpp", source, "nowhere::");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "nothing declares `nowhere`: {found:?}"
        );
    }

    #[test]
    fn a_cursor_that_is_not_in_a_name_offers_nothing() {
        let source = "struct Widget { int size; };\nvoid f() {\n  Widget w;\n}\n";
        let found = names_at(&[], "/p/a.cpp", source, "Widget w;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
            "the cursor is on a declaration's name, which is not a name being written: {found:?}"
        );
    }

    // -------------------------------------------------------------------------------------------
    // Macros
    //
    // Every test here is about **translation order**: which of the `#define`s and `#undef`s the
    // preprocessor would have gone past last. The fixtures are written so that the *set* of facts is the
    // same in several of them and only their order differs, because the set is what a naive
    // implementation gets right and the order is what it gets wrong.
    // -------------------------------------------------------------------------------------------

    /// The line a found fact is on, so a test can say "the second one" without arithmetic.
    fn line_of(source: &str, offset: usize) -> usize {
        source[..offset].matches('\n').count()
    }

    #[test]
    fn a_macro_defined_above_its_use_resolves_to_the_define() {
        let source = "#define MAX 1\nint x = MAX;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));

        let Known::Yes(found) = found else {
            panic!("the define above the use is the answer: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/a.cpp"));
        assert_eq!(found.fact.name, "MAX");
        assert!(found.fact.kind.is_definition());
        assert_eq!(
            &source[found.fact.range.start_offset..found.fact.range.end_offset()],
            "MAX",
            "and the range is the name a user would be sent to"
        );
    }

    #[test]
    fn a_macro_defined_below_its_use_is_not_in_force_yet() {
        // The file is read top to bottom, so a `#define` further down the page is not what the name above it
        // means. A table keyed by name alone answers this one wrong, and it is the reason the query is positional.
        let source = "int x = MAX;\n#define MAX 1\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "nothing above the use defines it: {found:?}"
        );
    }

    #[test]
    fn a_macro_in_an_included_header_resolves_into_that_header() {
        let source = "#include \"config.h\"\nint x = FEATURE;\n";
        let header = "#define FEATURE 1\n";
        let (index, tree) = analysed(&[("/p/config.h", header)], "/p/main.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));

        let Known::Yes(found) = found else {
            panic!("the header's define must be found: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/config.h"));
        assert_eq!(line_of(header, found.fact.range.start_offset), 0);
    }

    #[test]
    fn a_header_included_after_the_use_does_not_define_it_yet() {
        let source = "int x = FEATURE;\n#include \"config.h\"\n";
        let (index, tree) = analysed(&[("/p/config.h", "#define FEATURE 1\n")], "/p/main.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "the include is below the use: {found:?}"
        );
    }

    #[test]
    fn the_later_of_two_headers_that_define_it_wins() {
        // The set of facts is identical in this test and the next one; only the order differs. That is the pair a
        // name-keyed implementation cannot tell apart.
        let source = "#include \"one.h\"\n#include \"two.h\"\nint x = FLAG;\n";
        let one = "#define FLAG 1\n";
        let two = "#define FLAG 2\n";
        let (index, tree) = analysed(&[("/p/one.h", one), ("/p/two.h", two)], "/p/main.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FLAG;"));

        let Known::Yes(found) = found else {
            panic!("the last define gone past is the answer: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/two.h"));
    }

    #[test]
    fn swapping_the_two_includes_swaps_the_answer() {
        let source = "#include \"two.h\"\n#include \"one.h\"\nint x = FLAG;\n";
        let one = "#define FLAG 1\n";
        let two = "#define FLAG 2\n";
        let (index, tree) = analysed(&[("/p/one.h", one), ("/p/two.h", two)], "/p/main.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FLAG;"));

        let Known::Yes(found) = found else {
            panic!("order is the whole answer: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/one.h"));
    }

    #[test]
    fn a_local_define_after_an_include_wins_over_the_header() {
        let source = "#include \"config.h\"\n#define FEATURE 2\nint x = FEATURE;\n";
        let (index, tree) = analysed(
            &[("/p/config.h", "#define FEATURE 1\n")],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));
        let Known::Yes(found) = found else {
            panic!("the file's own later define is what is in force: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/main.cpp"));
    }

    #[test]
    fn a_local_define_before_an_include_loses_to_the_header() {
        // The same two facts as the test above with the lines the other way round, which is the whole point: this
        // is a positional query, so it is not enough to know that both files define the name.
        let source = "#define FEATURE 2\n#include \"config.h\"\nint x = FEATURE;\n";
        let (index, tree) = analysed(
            &[("/p/config.h", "#define FEATURE 1\n")],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));
        let Known::Yes(found) = found else {
            panic!("the header was pasted after the local define: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/config.h"));
    }

    #[test]
    fn a_nested_include_is_reached() {
        let source = "#include \"middle.h\"\nint x = DEEP;\n";
        let (index, tree) = analysed(
            &[
                ("/p/middle.h", "#include \"leaf.h\"\n"),
                ("/p/leaf.h", "#define DEEP 1\n"),
            ],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "DEEP;"));

        let Known::Yes(found) = found else {
            panic!("a define two includes down is still in force: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/leaf.h"));
    }

    #[test]
    fn an_undef_of_a_local_define_leaves_an_ordinary_identifier() {
        // The answer is not the `#define` it used to have: pointing there would send a user to a definition that is
        // not in force, which is a wrong answer rather than a missing one.
        let source = "#define MAX 1\n#undef MAX\nint x = MAX;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UndefinedHere(_))),
            "an `#undef` above the use settles it: {found:?}"
        );
    }

    #[test]
    fn an_undef_of_a_headers_define_also_counts() {
        // Across a file boundary, which is the case a single-file macro table cannot see at all: the `#undef` is
        // in the querying file and the `#define` is in the header.
        let source = "#include \"config.h\"\n#undef FEATURE\nint x = FEATURE;\n";
        let (index, tree) = analysed(
            &[("/p/config.h", "#define FEATURE 1\n")],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::UndefinedHere(_))),
            "the header's define is ended by the file's own `#undef`: {found:?}"
        );
    }

    #[test]
    fn a_define_after_an_undef_is_in_force_again() {
        let source = "#define MAX 1\n#undef MAX\n#define MAX 2\nint x = MAX;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
        let Known::Yes(found) = found else {
            panic!("the last fact is a define: {found:?}");
        };
        assert_eq!(line_of(source, found.fact.range.start_offset), 2);
    }

    #[test]
    fn a_define_inside_a_conditional_is_unknown_rather_than_guessed() {
        let source = "#if defined(USE_MAX)\n#define MAX 1\n#endif\nint x = MAX;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::ConditionalCompilation)),
            "whether that branch was taken is not knowable here: {found:?}"
        );
    }

    #[test]
    fn an_unconditional_define_beats_a_guarded_one_after_it() {
        // The rule the declaration query uses, applied to positions: the name certainly is a macro, so refusing to
        // answer would be refusing a question that has an answer. What the guarded define might do is stated in the
        // documentation rather than turned into an `Unknown`.
        let source = "#define MAX 1\n#if defined(OTHER)\n#define MAX 3\n#endif\nint x = MAX;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
        let Known::Yes(found) = found else {
            panic!("the unconditional define is what is certain: {found:?}");
        };
        assert_eq!(line_of(source, found.fact.range.start_offset), 0);
    }

    #[test]
    fn a_guarded_include_makes_the_answer_conditional() {
        let source = "#if defined(USE_CONFIG)\n#include \"config.h\"\n#endif\nint x = FEATURE;\n";
        let (index, tree) = analysed(
            &[("/p/config.h", "#define FEATURE 1\n")],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::ConditionalCompilation)),
            "the only path to the define goes through an `#if`: {found:?}"
        );
    }

    #[test]
    fn a_name_no_one_defines_is_not_declared_here() {
        let source = "int x = NOT_A_MACRO;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "NOT_A_MACRO;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "the index is a subset of the translation unit, so this is not a definite no: {found:?}"
        );
    }

    #[test]
    fn a_cursor_that_is_not_on_a_name_has_nothing_to_look_up() {
        let source = "#define MAX 1\nint x = MAX + 1;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "+ 1;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
            "the cursor is on an operator: {found:?}"
        );
    }

    #[test]
    fn a_cycle_of_includes_terminates() {
        // Two headers that include each other, both defining the name: the walk expands a file once per query, so
        // it ends instead of growing a chain for ever.
        let source = "#include \"a.h\"\nint x = SHARED;\n";
        let (index, tree) = analysed(
            &[
                ("/p/a.h", "#include \"b.h\"\n#define SHARED 1\n"),
                ("/p/b.h", "#include \"a.h\"\n"),
            ],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "SHARED;"));
        let Known::Yes(found) = found else {
            panic!("a cycle must not stop the define being found: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/a.h"));
    }

    #[test]
    fn an_undef_in_a_header_is_reached_like_a_define() {
        // The symmetric case, and the reason `#undef` is a fact rather than a note: the fact that settles the
        // question can be in either file.
        let source = "#include \"config.h\"\n#include \"cleanup.h\"\nint x = FEATURE;\n";
        let (index, tree) = analysed(
            &[
                ("/p/config.h", "#define FEATURE 1\n"),
                ("/p/cleanup.h", "#undef FEATURE\n"),
            ],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::UndefinedHere(_))),
            "the later header undefines it: {found:?}"
        );
    }
}

