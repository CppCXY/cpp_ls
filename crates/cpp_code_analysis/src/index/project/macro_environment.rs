//! **The macro environment's own types** — the candidate list, the visible declarations, and the symbol a query
//! answers with.
//!
//! Moved here from `project/mod.rs` without a change.

use crate::Binding;
use crate::BindingKind;
use crate::DeclKind;
use crate::DeclModifiers;
use smol_str::SmolStr;

use super::*;

/// Every fact about one macro name that a file can reach — the answer to "what is this name here", before the
/// question of *where* is asked.
///
/// Built by [`ProjectIndex::macro_environment`]. The point of holding it is that the expensive half — walking the
/// include graph for the facts — does not depend on the offset, while the cheap half does.
#[derive(Debug, Clone, Default)]
pub struct MacroEnvironment {
    /// The name asked about, kept so that a reason can say *which* name could not be resolved — the walk is over
    /// one name and a caller reading the answer should not have to remember it.
    pub(crate) name: String,
    pub(crate) candidates: Vec<MacroCandidate>,
}

impl MacroEnvironment {
    /// **Which `#define` is in force** at `offset`, or why that cannot be said.
    ///
    /// This is the question "go to macro definition" asks, and it is the strict one. The rules are the
    /// preprocessor's:
    ///
    /// * **only facts pasted in at or before the cursor count.** The first element of a candidate's chain is where
    ///   it enters the file the cursor is in — the fact's own offset, or the `#include` that pulled its file in —
    ///   so one comparison covers both cases: a `#define` written below the use is not in force yet, and neither is
    ///   anything from an `#include` written below it. Everything *inside* an included file counts, because the
    ///   preprocessor pastes all of it at the `#include`.
    /// * **the last one wins**, by lexicographic comparison of those chains.
    /// * **an unconditional fact beats a conditional one** even when the conditional one is later: a fact behind an
    ///   `#if` may not be there at all, so one that is certainly there settles the name — and only when nothing
    ///   certain exists does the conditional one make the answer `Unknown(ConditionalCompilation)`.
    ///
    /// [`MacroFact::settles_the_name`] is deliberately **not** consulted here: it says the name is a macro either
    /// way, not *which* `#define` did it, and in the `#ifndef NAME` shape the definition may have come from an
    /// earlier header this file cannot see. [`MacroEnvironment::is_a_macro_at`] is the question it answers.
    pub fn at(&self, offset: usize) -> Known<&MacroCandidate> {
        let in_force: Vec<&MacroCandidate> = self.in_force(offset).collect();

        let best = last(
            in_force
                .iter()
                .copied()
                .filter(|candidate| !candidate.is_conditional()),
        )
        .or_else(|| last(in_force.iter().copied()));

        let Some(best) = best else {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(
                self.name.as_str(),
            )));
        };

        if best.is_conditional() {
            return Known::Unknown(UnknownReason::ConditionalCompilation);
        }

        self.as_answer(best)
    }

    /// **Is this name a macro** at `offset` — the weaker question a find-references asks.
    ///
    /// The difference from [`MacroEnvironment::at`] is one word: a fact inside an `#if` counts here when the
    /// conditional *cannot change whether the name is a macro* afterwards, which is what
    /// [`MacroFact::settles_the_name`] records (the `#ifndef NAME / #define NAME` idiom and a region whose every
    /// branch agrees). That is the whole reason this is a second method rather than a flag on the first: a use is a
    /// use even when which `#define` is in force depends on a macro nobody has.
    ///
    /// The `#undef` case is where the two genuinely part company, so it gets its own rule: a certain `#undef` says
    /// the name is not a macro here — **unless** something that might be in force *after* it is a definition.
    pub fn is_a_macro_at(&self, offset: usize) -> Known<&MacroCandidate> {
        let in_force: Vec<&MacroCandidate> = self.in_force(offset).collect();

        let Some(best) = last(
            in_force
                .iter()
                .copied()
                .filter(|candidate| candidate.is_certain_about_the_name()),
        ) else {
            // Nothing that reaches this offset settles it. Either there is nothing at all — the name is not a
            // macro here — or everything there is depends on a condition.
            return if in_force.is_empty() {
                Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(
                    self.name.as_str(),
                )))
            } else {
                Known::Unknown(UnknownReason::ConditionalCompilation)
            };
        };

        if best.is_definition() {
            return Known::Yes(best);
        }

        // The name is certainly *not* a macro here — a `#define` re-asserted inside a conditional is not a second
        // definition, and this fact already answered the question. But a `#undef` can be overturned by a
        // conditional definition that comes later in the stream, and then the honest answer is "maybe".
        let overturned = in_force
            .iter()
            .any(|candidate| candidate.position > best.position && candidate.is_definition());

        if overturned {
            return Known::Unknown(UnknownReason::ConditionalCompilation);
        }

        Known::Unknown(UnknownReason::UndefinedHere(Box::from(self.name.as_str())))
    }

    /// The in-force candidates, in no particular order: everything pasted in at or before `offset`.
    /// **Every definition in force at `offset`, for a caller that can use agreement.** The strict `at` refuses a name
    /// whose region cannot be decided, which is right for "go to definition" — one `#define` of two is a wrong jump.
    /// For a *value* the question is different: when every definition in force folds to the same number, that number
    /// is what the compilation has, whatever the preprocessor took.
    ///
    /// Measured on `EmmyLuaCodeStyle`: `const int FIRST_RESERVED = UCHAR_MAX + 1;` showed no value, and `UCHAR_MAX`'s
    /// definition sits inside a conditional in the toolchain's `limits.h` — `at` answered
    /// `Unknown(ConditionalCompilation)` and the fold had nothing to read.
    pub fn definitions_in_force(&self, offset: usize) -> Vec<&MacroCandidate> {
        self.in_force(offset)
            .filter(|candidate| candidate.is_definition())
            .collect()
    }

    pub(crate) fn in_force(&self, offset: usize) -> impl Iterator<Item = &MacroCandidate> {
        self.candidates
            .iter()
            .filter(move |candidate| candidate.position[0] <= offset)
    }

    /// The answer a settled candidate gives, whether or not its region was settled by the flag.
    pub(crate) fn as_answer<'a>(&self, best: &'a MacroCandidate) -> Known<&'a MacroCandidate> {
        if best.is_definition() {
            return Known::Yes(best);
        }

        Known::Unknown(UnknownReason::UndefinedHere(Box::from(self.name.as_str())))
    }

    /// Is there anything at all to say about the name in this file?
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

/// The last of a run of candidates in translation order.
pub(crate) fn last<'a>(
    candidates: impl Iterator<Item = &'a MacroCandidate>,
) -> Option<&'a MacroCandidate> {
    candidates.max_by(|one, other| one.position.cmp(&other.position))
}

/// One fact about a macro name, and where it sits in the translation unit's stream.
#[derive(Debug, Clone)]
pub struct MacroCandidate {
    /// The chain of offsets that pastes this fact in: the top-level `#include`, each nested one, and the fact's
    /// own offset. Compared lexicographically, which is what makes it a position.
    pub(crate) position: Vec<usize>,
    pub(crate) file: PathBuf,
    pub(crate) fact: MacroFact,
    /// Is one of the `#include`s that pull this fact's file in written inside a conditional?
    ///
    /// Kept apart from the fact's own [`MacroFact::guard`] because the two are answered by different things: a
    /// path through a conditional `#include` means the fact's *file* may not be part of the translation unit at
    /// all, which nothing about the fact's own text can repair.
    pub(crate) path_conditional: bool,
    /// What the environment makes of the fact's own region — see [`MacroCandidate::is_conditional`].
    pub(crate) region: FactReach,
}

impl MacroCandidate {
    /// The file the fact is written in.
    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn fact(&self) -> &MacroFact {
        &self.fact
    }

    /// Is the fact a `#define` (as opposed to an `#undef`)?
    pub fn is_definition(&self) -> bool {
        self.fact.kind.is_definition()
    }

    /// Is **which `#define` is in force** conditional — the question [`MacroEnvironment::at`] answers?
    ///
    /// Yes when the path in is conditional, or when the fact is written inside an `#if` that the environment
    /// cannot decide — including the `#ifndef NAME` whose body defines the name, where the name is certainly a
    /// macro but the *definition* it has may be somebody else's. A region the environment **decides** is not
    /// conditional in this sense: `#ifdef _WIN32` on a machine whose compiler defines `_WIN32` is a fact about
    /// which `#define` is in force, not a doubt about it.
    pub fn is_conditional(&self) -> bool {
        self.path_conditional || self.region == FactReach::Unknown
    }

    /// Is **whether the name is a macro here** settled — the question [`MacroEnvironment::is_a_macro_at`] answers?
    ///
    /// The path in still has to be unconditional: a fact in a header that may not have been included at all says
    /// nothing. The fact's own region is settled when the environment decides it *is* taken, and otherwise by
    /// [`MacroFact::settles_the_name`] — which is exactly the case the two questions differ in.
    pub fn is_certain_about_the_name(&self) -> bool {
        !self.path_conditional && (self.fact.settles_the_name || self.region == FactReach::Active)
    }
}

/// A declaration found in another file, with how the file that asked reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleDeclaration<'a> {
    pub file: PathBuf,
    pub fact: &'a DeclFact,
    pub visibility: IncludeVisibility,
}

/// One declaration a name resolved to, **owned** so that the walk that found it can hand it back: a
/// [`VisibleDeclaration`] borrows the index, and the two queries built on this want to sort, dedup and truncate the
/// list before they answer.
pub(crate) struct ProjectDeclaration {
    pub(crate) file: PathBuf,
    pub(crate) fact: DeclFact,
    pub(crate) start_offset: usize,
}

impl ProjectDeclaration {
    pub(crate) fn of(found: &VisibleDeclaration<'_>) -> ProjectDeclaration {
        ProjectDeclaration {
            file: found.file.clone(),
            fact: found.fact.clone(),
            start_offset: found.fact.name_range.start_offset,
        }
    }
}

/// **What the index says a name is**, as the two facts a colour is drawn from — the answer to
/// [`ProjectIndex::kind_of`].
///
/// The pair and not a [`DeclFact`], because a highlighter asks this once per distinct spelling in a file and a
/// `DeclFact` is five owned strings deep: cloning one per spelling to read two fields off it is most of what such a
/// request would cost. See [`ProjectIndex::kind_of`] for why agreement among the candidates is the rule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexedKind {
    pub kind: DeclKind,
    /// The qualified scope the declaration was written in, or `None` at file scope.
    ///
    /// Part of the answer rather than a detail: it is what tells a **member** from a free name (`std::string::size`
    /// from `::size`), and it is also why this type is ordered — two candidates are one answer only when both
    /// fields agree.
    pub scope: Option<Box<str>>,
    /// **Is this an enumerator?** — a `Variable` fact written inside an enumeration.
    ///
    /// The index has one kind for "a name that holds a value", and an enumerator is one: `DeclKind` cannot tell it
    /// from a variable without a field, and adding a variant is a summary-format change. What it *can* tell is that
    /// an enumerator records **no type** — `enum class Color { Red };` declares `Red` with the enumeration's type
    /// implied, while every variable records the spelling it was written with — and that it always has a scope.
    ///
    /// So this is read off the fact rather than guessed: a `Variable` in a scope with no written type is an
    /// enumerator. A highlighter draws it as `enumMember` (a theme colours it with the enum's own), and a reader
    /// hovering it sees the enumerator rather than a variable that claims no type.
    pub enumerator: bool,
    /// **Is this a variable template?** — `template <class T> constexpr bool v = …;`.
    ///
    /// `DeclFact::parameters` is the template head's parameter names, recorded by
    /// `declared_template_parameters_with` when the fact was built, so a variable whose list is non-empty was written
    /// with a `template <…>` in front of it and names a *family* of values rather than one. A highlighter draws that
    /// apart from a plain variable — see `TypeKind::VariableTemplate` in the server.
    pub template: bool,
    /// **Is this a constructor?** — a function named exactly like the class it is declared in, with no return type.
    ///
    /// The index has no constructor kind: a constructor is a `Function` fact, exactly as a member function is, and a
    /// summary records no "this is a ctor" flag. What it does record is enough to tell them apart — a constructor's
    /// **name is its class's** and it has **no written return type** — and those two are the language's own definition
    /// ([class.ctor]: a constructor has no name of its own, and no return type).
    ///
    /// Measured from an editor: `EditorconfigPattern::EditorconfigPattern() { … }` in a `.cpp` file — an
    /// out-of-class definition, so the answer comes from the index — was drawn as `function`, while the same name in
    /// the header was `type`. The binding path already knew (a `BindingKind::Constructor` maps to `NameKind::Type`);
    /// this is the other half of the same rule.
    pub constructor: bool,
}

impl IndexedKind {
    /// The answer for one declaration.
    pub(crate) fn of(found: &DeclFact) -> IndexedKind {
        IndexedKind {
            kind: found.kind,
            scope: found.scope.clone().map(|scope| Box::from(scope.as_str())),
            // **The fact's own flag**, not a derivation from what it happens to record: a variable with no written
            // type and a scope used to count as an enumerator, which is wrong at both ends — an anonymous enum's
            // member has *no* scope and so was not one, and an untyped scoped variable was one wrongly. See
            // [`DeclFact::enumerator`].
            enumerator: found.enumerator,
            // **The flag means a *variable* template**, which is what its name says and what its one reader uses it
            // for (`semantic.rs` draws a `Variable` with it as `variableTemplate`). A *class* template's head set it
            // too, and then a class template and its explicit specialization — `Pair<T, U>`, `Pair<T, T>`,
            // `Pair<bool, int>` and `template struct Pair<long, long>;` — became two different answers: `kind_of`
            // said `Ambiguous` and a use of the type got **no colour at all**, while the hover on the same byte
            // answered `struct Pair`. Measured on `test_cpp/src/templates.cpp:166`.
            template: found.kind == DeclKind::Variable && !found.parameters().is_empty(),
            constructor: found.kind == DeclKind::Function
                && found.returns.is_none()
                && found
                    .scope
                    .as_deref()
                    .is_some_and(|scope| scope.rsplit("::").next() == Some(found.name.as_str())),
        }
    }
}

/// The answer to a cross-file definition question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDefinition {
    pub file: PathBuf,
    /// The declaration, cloned out of the index so that an answer does not borrow the project for as long as a
    /// consumer wants to hold it — a language server hands the location to a client and moves on.
    pub fact: DeclFact,
}

/// **Every declaration a name refers to** — the plural answer to "where is this declared".
///
/// See [`ProjectIndex::definitions`] for what the list means, why one namespace is collapsed to one entry, and
/// what the order is. This type exists because the answer is genuinely plural: an overload set is several
/// declarations, and a client that can show a list should be given all of them rather than told "ambiguous".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDefinitions {
    /// The declarations to answer with, in the order [`ProjectIndex::definitions`] documents. One entry for the
    /// ordinary case, several for an overload set or a name declared in more than one place.
    ///
    /// Never empty: a name nothing declares is [`UnknownReason::NotDeclaredHere`] rather than an empty list, and a
    /// name whose only declarations are behind a conditional `#include` is
    /// [`UnknownReason::ConditionalCompilation`]. An empty list would be an answer that looks like "here they are"
    /// and says nothing.
    pub found: Vec<ProjectDefinition>,
    /// How many declarations of the same name the index holds that are reachable **only** through a conditional
    /// `#include`, and are therefore not in [`ProjectDefinitions::found`]: whether they are in scope depends on
    /// macros this layer does not have.
    ///
    /// Counted rather than dropped, because "one declaration" and "one declaration and two that might also be
    /// there" are different answers, and a consumer that shows the first without the second is claiming more than
    /// this layer knows.
    pub conditional: usize,
}

impl ProjectDefinitions {
    /// An answer of one declaration — the shape the single-file layer's own resolution produces.
    pub fn one(found: ProjectDefinition) -> Self {
        ProjectDefinitions {
            found: vec![found],
            conditional: 0,
        }
    }

    /// Does this name refer to exactly one declaration?
    pub fn is_one(&self) -> bool {
        self.found.len() == 1
    }
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
        self.members
            .iter()
            .filter(move |member| member.depth == depth)
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
    pub fn from_binding(path: &Path, binding: Binding, scope: Option<String>, local: bool) -> Self {
        ProjectDefinition {
            file: path.to_path_buf(),
            fact: DeclFact {
                enumerator: binding.kind == BindingKind::Enumerator,
                // The same bits the index fills — see `DeclModifiers`.
                modifiers: DeclModifiers::default(),
                // The effects belong to the summary the index writes, and this answer is a *place to jump to*: a
                // caller that wants `frees` asks the fact, which is the same rule `in_namespace` below follows.
                frees: 0,
                detail: None,
                name: binding.name.identifier_text().unwrap_or_default().into(),
                scope: scope.map(SmolStr::from),
                local,
                // `None` for the same reason `type_of` below is: this answer is a **place to jump to**, built
                // from a binding, and the namespaces around a body are a fact about the file's text that lives in
                // its summary. See [`DeclFact::in_namespace`].
                kind: DeclKind::from_binding_kind(binding.kind),
                // Unknown here, and harmlessly so: this answer is a **place to jump to**, never a candidate a member
                // query picks between definitions. See [`DeclFact::definition`].
                definition: false,
                // No type, no return type and no bases, because this answer is a *place to jump to* and the binding
                // it comes from carries none of them: they are facts about the file's text, and the file they
                // belong to has the summary that holds them. A consumer asking what a name *is* asks the index,
                // not this answer.
                type_of: None,
                returns: None,
                split_by_a_directive: false,
                // Same reason as `constraints` above: a clause is the declaration's text, and this is a place to jump to.
                // Nor a pattern: this fact is a place to jump to, and the class it names is already the one the
                // caller asked about. See [`DeclFact::pattern`].
                access: None,
                exported: false,
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

/// What a declaration query knows about the fact it is looking for, before it reads a single fact.
#[derive(Clone, Copy)]
pub(crate) enum Narrow<'q> {
    /// Nothing: every fact of every visible file is a candidate.
    Nothing,
    /// The fact's bare name is one of these.
    Named(&'q [&'q str]),
    /// The fact is written directly in this scope.
    Scoped(&'q str),
}

/// One workspace-symbol candidate, with everything its ordering needs.
pub(crate) struct Hit<'a> {
    pub(crate) rank: usize,
    /// Lowercased, because the order within a rank is alphabetical and case is not a difference a reader sees.
    pub(crate) name: String,
    /// The same, for the whole qualified name: two `Widget`s in different namespaces are ordered by where they are.
    pub(crate) qualified: String,
    pub(crate) file: &'a Path,
    pub(crate) posting: Posting,
    pub(crate) fact: &'a DeclFact,
}

/// Rank, then **name**, then qualified name, then **file** — answers that compare equal have to come back in one
/// order, and two files may declare the same name — then the position, so that even two overloads in one file do.
pub(crate) fn hit_order(one: &Hit<'_>, two: &Hit<'_>) -> std::cmp::Ordering {
    one.rank
        .cmp(&two.rank)
        .then_with(|| one.name.cmp(&two.name))
        .then_with(|| one.qualified.cmp(&two.qualified))
        .then_with(|| one.file.cmp(two.file))
        .then_with(|| one.posting.cmp(&two.posting))
}

pub(crate) fn sort_hits(hits: &mut [Hit<'_>]) {
    hits.sort_by(hit_order);
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
pub(crate) fn matches(fact: &DeclFact, name: &str) -> bool {
    if fact.local {
        return false;
    }

    // **A fact with no name declares no name.** `DeclFact::qualified_name` of a nameless fact *is* its scope, so
    // without this every nameless declaration answers for the class that encloses it — which is how a destructor
    // made `std::vector` ambiguous. Nothing is lost: a nameless fact is still reachable by position, which is
    // what it exists for (`fact_for` records the spelling for the rest).
    if fact.name.is_empty() {
        return false;
    }

    if let Some(global) = name.strip_prefix("::") {
        return fact.scope.is_none() && fact.name == global;
    }

    fact.qualified_name() == name || fact.name == name
}

/// The resolved include targets of a summary, as normalized path strings.
pub(crate) fn include_targets(summary: &FileSummary) -> Vec<String> {
    summary
        .includes
        .iter()
        .filter_map(|include| include.resolved.as_ref())
        .map(|path| normalize(path))
        .collect()
}

/// A path as the index keys on it.
pub(crate) fn normalize(path: &Path) -> String {
    // Case-insensitive on Windows, where two spellings of one path are one file. The rest of the crate reads the
    // same flag from the compiler configuration; here it is the platform, because the *index* has to agree with
    // the filesystem about identity and nothing else.
    normalize_path(path, cfg!(windows))
}

/// One declaration a **workspace symbol** search found — see [`ProjectIndex::symbols_matching`].
///
/// Owned rather than borrowed, unlike [`VisibleDeclaration`]: a search collects from several maps (the summaries
/// and the cooked readings) and sorts what it found, so there is nothing to borrow from by the time it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSymbol {
    pub file: PathBuf,
    pub fact: DeclFact,
}

/// How well a declaration matches a search query: lower is better, `None` is not a match.
///
/// # A bare word is about the **name**; a qualified query is about the qualified name's **tail**
///
/// That one rule is what keeps a search for `wid` from answering with every member of every matching class —
/// `ns::Widget::size` contains `wid` too, and two letters in a symbol box should not list a whole project's
/// members — while still letting `Widget::si` find that member, which is the query a user types when they *do* want
/// one.
///
/// ```text
/// `size`         exact, then prefix, then anywhere in the **name**      → finds `Widget::size` at any depth ✓
/// `Widget::si`   matched against the **last two** segments: the ones before the last must match exactly, the last
///                is a prefix → `ns::Widget::size` matches (its tail is `Widget::size`) ✓
/// `ns::widget`   …and `ns::Widget::size`'s tail is `Widget::size`, so `ns` does not match it ✗ — a class query
///                does not drag its members in. It matches `ns::Widget` itself ✓
/// ```
///
/// A leading `::` is dropped, so `::Widget` — how a reader spells "the global one" — finds what `Widget` finds. (The
/// index records a global declaration's qualified name as its bare name, and there is only one to find.)
pub(crate) fn rank_of(qualified: &str, name: &str, wanted: &str) -> Option<usize> {
    let wanted = wanted.trim_start_matches("::").to_lowercase();
    if wanted.is_empty() {
        return None;
    }

    let query: Vec<&str> = wanted.split("::").collect();

    if query.len() == 1 {
        let name = name.to_lowercase();

        if name == wanted {
            return Some(0);
        }
        if name.starts_with(&wanted) {
            return Some(1);
        }
        if name.contains(&wanted) {
            return Some(2);
        }

        return None;
    }

    let segments: Vec<String> = qualified
        .to_lowercase()
        .split("::")
        .map(str::to_string)
        .collect();

    // The **tail**: `Widget::si` has to be able to find `ns::Widget::size`, and it is the last segments it names.
    if segments.len() < query.len() {
        return None;
    }
    let tail = &segments[segments.len() - query.len()..];

    let (last_asked, asked) = query.split_last()?;
    let (last, rest) = tail.split_last()?;

    if rest.iter().map(String::as_str).ne(asked.iter().copied()) {
        return None;
    }

    if last == last_asked {
        return Some(0);
    }
    if last.starts_with(last_asked) {
        return Some(1);
    }

    None
}

/// **Are these two recordings the same declaration?**
///
/// The comparison is the model's own identity: every field a consumer can *read*. Two facts that agree on all of
/// them are one entity as far as any answer here is concerned — a hover would print the same type and the same
/// scope, a completion would show one entry, a jump would land where the name is declared.
///
/// # What is deliberately not compared
///
/// * `range` and `name_range` — *where* the declaration was written. Whether two recordings may be merged across
///   files is a separate question, and [`the_same_declaration_in_one_file`] answers it with "no".
/// * `clean` and `guard` — *how well* it was read and *which* `#if` it stands in. Two recordings of one declaration
///   legitimately disagree (one summary may have been built while that file had errors), and neither difference is
///   one a consumer could observe in an answer.
///
/// # What is compared, and why each
///
/// `name` and `scope` are the qualified name — the identity the index is keyed by. `local` separates a file-scope
/// name from a local one with the same spelling, which nothing outside its own body can name. `kind` separates a
/// class from a function of the same name. `type_of`, `returns`, `bases` and `parameters` are what a
/// **redeclaration** must agree about: a forward declaration and a definition differ in exactly these.
///
/// `parameter_list` is the sixth, and it was added late because the model did not have it: without it two overloads
/// of one function that share a return type — `string format(const string_view)`, `wstring format(const
/// wstring_view)`… and the four `std::format`s that *are* distinguishable only here — read as one recording of one
/// declaration. A fact that did not record it says `None` on both sides, which is the honest answer: "nothing here
/// tells them apart" is what `None == None` means, and it is why [`the_same_declaration_in_one_file`] still demands
/// the **presence** of evidence rather than its equality.
pub(crate) fn the_same_declaration(one: &DeclFact, other: &DeclFact) -> bool {
    one.name == other.name
        && one.scope == other.scope
        && one.local == other.local
        && one.kind == other.kind
        && one.type_of == other.type_of
        && one.returns == other.returns
        && one.bases() == other.bases()
        && one.parameters() == other.parameters()
        && one.parameter_list() == other.parameter_list()
}

/// Are these two candidates **one declaration recorded twice**, as far as this index can tell?
///
/// # The three conditions, and why each is there
///
/// 1. **The same file.** `int count;` in two headers is two answers, not one — the position this project pinned in
///    `two_declarations_of_one_name_are_a_list_for_a_consumer_that_shows_one`, and the language agrees: one name
///    written in two translation units is two objects or an error, and a reader asking where `count` is declared is
///    owed both places. Measured on one real file: of the candidate pairs that agree on everything readable,
///    **97 stand in the same file and 0 across files** — the duplicates are *inside* a header (MSVC's `<iostream>`
///    writes `cin` plain and again behind `_EXPORT_STD`), which is what this exists for.
/// 2. **A field that can tell two declarations apart must be _present_, not merely equal.** A variable's or an
///    alias's **type**; a function's **parameter list**. Equal-and-empty is not agreement: two overloads that share
///    a return type and whose parameters were never read would "agree" on everything, and an identity invented out
///    of silence collapses every overload set into one answer — which is what `std::getline`'s four overloads did
///    before the parameter list was recorded. A function whose parameters are unread is therefore **never** merged.
/// 3. **Everything else about them is equal** — see [`the_same_declaration`].
///
/// A class is deliberately not mergeable: this model records no member list, so two classes of one name in one file
/// are indistinguishable *and* genuinely different. What collapses a class's own redeclaration (`struct S;` then
/// `struct S { … };`) is not this rule — such a pair is one `DeclFact` here to begin with.
pub(crate) fn the_same_declaration_in_one_file(
    one: &ProjectDeclaration,
    other: &ProjectDeclaration,
) -> bool {
    if one.file != other.file || !the_same_declaration(&one.fact, &other.fact) {
        return false;
    }

    match one.fact.kind {
        // A variable, and an alias (which this model records as a `Type` with a spelling): the type tells two of
        // them apart, and an absent one is silence rather than agreement.
        DeclKind::Variable | DeclKind::Type => one.fact.type_of.is_some(),
        // A function: the parameter list, which is the field that separates overloads and the one this model reads
        // out of the declarator. `parameters` — the *template* parameter names — is not evidence about a function:
        // an ordinary function has none, so requiring it here would refuse to merge anything.
        DeclKind::Function => one.fact.parameter_list().is_some(),
        // A namespace has its own, wider rule at the call site; a macro and an anonymous construct have nothing here
        // that could tell two of them apart.
        _ => false,
    }
}
