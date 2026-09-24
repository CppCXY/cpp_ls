//! What the index remembers about **one file** — the unit the whole cache is built from.
//!
//! This module is the shape; the builder that fills it from a tree lands next, and the encoder after that. What is
//! fixed here is the vocabulary and the rules that make a summary *indexable*:
//!
//! # Facts, not conclusions
//!
//! Everything below is something the file **says** — a declaration, a `#define`, an `#include`, a module. Nothing
//! is a resolved answer: no "this name refers to that declaration", no "this type is `int`", no expansion result.
//! `docs/index-design.md` fixes that as an invariant, and the reason is invalidation: a `#define` changes the
//! meaning of every name below it in every file that includes it, so a stored conclusion would have to be
//! recomputed project-wide, while a stored *fact* goes stale exactly when its own file changes.
//!
//! # Every fact says which branch it is on
//!
//! A declaration inside `#if defined(_WIN32)` exists on some machines and not others, and an editor must be able
//! to say which — so every fact carries a [`FactGuard`]: the region it was written in, as an index into the file's
//! guard list. The list itself is per-file data (see [`SummaryGuards`]), which is what keeps a fact small and lets
//! two facts from the same region share one answer.
//!
//! # Names are stored as written
//!
//! A [`DeclFact`] carries the spelling in the source (`Widget`, `ns::Widget`) and how it was qualified, never a
//! resolved identity. Resolving is a query over this data plus the include graph, and it needs the *environment* the
//! file was entered with — which is exactly what the summary's key records.

use crate::cache::SummaryKey;
use crate::guard::{Branch, Region, Visibility};

/// A declaration the file writes: enough to find it, name it, and say what kind of thing it is.
///
/// Deliberately not a syntax tree and not a type: the fields are the ones a *query* needs — where to jump, what to
/// call the symbol, what to filter by, and which branch it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclFact {
    /// The name as written, without qualification: `Widget` in `ns::Widget`.
    pub name: String,
    /// The **qualified** name of the scope the declaration was written in, without the name itself: `ns::C` for a
    /// member of that class.
    ///
    /// The one field that makes a flat list of declarations indexable. A name alone does not identify a
    /// declaration — two files may each declare `Widget`, and one file may declare `f` at file scope and again as
    /// a member — so a lookup needs the scope, and the qualified spelling is the scope's identity that survives
    /// being written to disk. A [`ScopeId`] would not: it is only meaningful inside the tree it came from.
    ///
    /// `None` for a declaration at file scope, which is what distinguishes `f` from `ns::C::f`. It is also `None`
    /// for a declaration inside a construct that has no qualified name — a local in a function body, a member of an
    /// anonymous class — because those genuinely have none, and an empty string would be a name that matches
    /// nothing while looking like an answer.
    ///
    /// [`ScopeId`]: crate::ScopeId
    pub scope: Option<String>,
    /// Was this declaration written inside a **function body, a block or a lambda**?
    ///
    /// The other half of [`DeclFact::scope`], and the reason it is a field rather than something a consumer can
    /// work out: a fact whose scope is `None` is either a declaration at *file* scope — `void helper();`,
    /// `extern int errno;`, which every file that includes this one can name — or a **local**, which nothing
    /// outside its own body can name at all. The two are indistinguishable from the rest of the fact, and the
    /// difference decides answers: a name lookup that answered with another file's local would be offering a name
    /// the user cannot see, and `std::vector`'s headers alone declare thousands of them (`__first`, `__n`, `_Tp`),
    /// so the wrong answer is not a corner case — it is most of what a completion would show.
    ///
    /// `true` for a declaration whose scope chain reaches a function body, a block or a lambda, however deeply it
    /// is nested in there — a local class, a local `typedef`, a variable inside a loop inside a member function.
    /// `false` at file scope, in a namespace, and in a class body, including a member function's *declaration* —
    /// the body is what makes a declaration local, not the entity it belongs to.
    ///
    /// # What a consumer is expected to do with it
    ///
    /// The index is a per-file list of declarations, so it cannot place a local in the function it belongs to;
    /// [`crate::ProjectIndex::definition`] therefore **skips** these facts when it looks a name up across files,
    /// because the scope tree of the file being edited is the only thing that can resolve one. A consumer with the
    /// file's own scopes in hand (a completion in the open buffer) lists locals from there and never needs this
    /// field; a consumer listing *another* file's declarations uses it to leave them out.
    pub local: bool,
    pub kind: DeclKind,
    /// The type a variable-like declaration was written with, as the file spells it.
    ///
    /// The first field in a fact that is not simply "what the file says about this name" but "what the file says
    /// about the *type* of it", and it exists for one query: `widget.size` is answered by finding `Widget` and
    /// then `size` inside it, and nothing else in a fact says how to get from `widget` to `Widget`.
    ///
    /// Three limits, all deliberate:
    ///
    /// * **As written, not resolved.** `Widget`, `ns::Widget` and `std::vector<int>` are stored as spelled, so a
    ///   consumer resolves them with the qualified-name machinery that already exists — and gets `Unknown` where
    ///   that machinery cannot go, rather than a guess.
    /// * **Only for variables, fields and parameters.** A class or a function declares no type in this sense: a
    ///   class *is* one and a function *returns* one, and those spellings live in a different part of the syntax.
    ///   `None` is the honest answer for them.
    /// * **Declaration specifiers are stripped.** `static const Widget` records `Widget`, because a specifier is
    ///   not part of the type's name and a lookup by name is what this is for. `unsigned long` survives, because
    ///   there the words *are* the type.
    pub type_of: Option<String>,
    /// The type a **function** returns, as the file spells it.
    ///
    /// The other half of [`DeclFact::type_of`], and separate from it for the reason that field's documentation
    /// gives: a class *is* a type and a function *returns* one, so `Widget make();` has no `type_of` — `make` is
    /// not a `Widget` and has no members — while its `returns` is what the *call* `make()` has. Collapsing the two
    /// would make `make.size` resolve as if the function were the object.
    ///
    /// It exists for one query: `make().size` is answered by finding what `make` returns and then `size` inside
    /// that, and nothing else in a fact says how to get from a call to a type.
    ///
    /// Same limits as `type_of`: **as written**, so a consumer resolves it with the qualified-name machinery and
    /// gets `Unknown` where that cannot go; declaration specifiers stripped (`static inline Widget make()` records
    /// `Widget`); `None` for everything that is not a function.
    ///
    /// A **trailing return type wins**, and it is the reason this cannot be read out of the specifier sequence
    /// alone: `auto make() -> Widget` spells the type after the parameter list, so the specifiers say `auto`.
    /// `None` for a return type the file does not state — `auto make() { … }` is deduced, and `auto` is not a
    /// class anything can be looked up in.
    pub returns: Option<String>,
    /// For a class-like declaration, the base classes it was written with, in declaration order.
    ///
    /// Spelled as written — `B`, `ns::C`, `Base<int>` — for the same reason [`DeclFact::type_of`] is: a consumer
    /// resolves them with the machinery that already exists, and `Unknown` where it cannot go beats a guess.
    ///
    /// Empty for a class with no bases **and** for everything that is not a class, which is one answer because it
    /// is the same answer to the question a consumer is asking: a member that is not here is not inherited from
    /// anywhere this declaration knows about.
    ///
    /// Access and `virtual` are not recorded. They decide whether a member is *reachable* and how the class is
    /// laid out, and this is a fact about the text rather than a semantic property — a lookup that used them would
    /// be the first thing here to need real semantics, and it would need the whole of them.
    ///
    /// # What is *not* in a derived class's fact, and why
    ///
    /// The members `B` happens to have are **not** copied onto `D`. The list is walked at query time
    /// (`index::project::members_of`), and the reason is the one thing a per-file key cannot catch: whether an
    /// added member of `B` reaches `D` depends on `D`'s base list, and *which* `B` the spelling refers to depends
    /// on macros and includes `D` never mentions. A stored copy would therefore go stale while `D`'s own text and
    /// key stayed identical, so nothing would invalidate it. `bases` is a spelling; the chain is a query.
    pub bases: Vec<String>,
    /// The whole declaration, for a "go to definition" highlight.
    pub range: cpp_parser::SourceRange,
    /// Just the name, which is what a reference search matches. Separate from `range` for the reason
    /// [`crate::Binding`] documents: collapsing them renames whole declarations.
    pub name_range: cpp_parser::SourceRange,
    /// Was this declaration read **without a diagnostic touching it**?
    ///
    /// `false` when a parse error fell inside the declaration this fact was written in. The tokens are all there
    /// and the node is well formed — the parser is total — but the reading around it was *recovered from*, so
    /// what this declaration says is not to be trusted. This is the record `docs/index-design.md`'s third
    /// invariant asks for: the parser is tolerant, so the layers above have to be able to see how much was
    /// recovered.
    ///
    /// # Why per fact, and not per file
    ///
    /// Measured on the closure of `<vector>` (279 files from one compiler): **5388 of 8499** declarations come
    /// from files that do not parse cleanly, and a per-file rule would throw all of them away — including the 157
    /// `std::` types that are indexed correctly today (`std::allocator`, `std::pair`, `std::tuple`). Of those
    /// 5388, only **214 — 4% —** are marked unclean here, and the 3111 declarations of the clean files are all
    /// clean: **8285 of 8499** declarations survive the question. The question is therefore about a declaration
    /// rather than about a file, and by a factor of twenty-five.
    ///
    /// # The range the question is asked about
    ///
    /// Not the fact's own range, which is the declarator: a diagnostic in the **type** is outside the declarator
    /// and inside the declaration, and the type is what a consumer reads off `type_of`. What is asked about is
    /// the innermost declaration node containing this fact's name. Measured, that costs 108 declarations over the
    /// declarator rule (106 against 214, both against 5388 for the file), and the 108 are exactly the ones worth
    /// having: by node kind, 89 are `Declaration` and 19 are `TemplateDecl` — the type part and the template
    /// header. The looser rule, "any declaration whose range contains the name", marks **2907** — one error in a
    /// class body would condemn every member of it — which is why the innermost one is the rule.
    ///
    /// # What it does not mean
    ///
    /// `true` is **not** a promise that the declaration is right. A recovery can land badly *before* a
    /// declaration and change where it belongs: the closing `}` of a block eaten by an error turns everything
    /// after it into locals, and those declarations' own tokens are untouched, so they are clean while their
    /// *scope* is wrong. A diagnostic that falls inside no declaration at all — an unexpected token at file
    /// scope, between two declarations — marks neither of them, and neither does one that lands in a declaration
    /// the walk produced no fact for, which is what a failed declarator usually does: the fact is then missing
    /// rather than unclean. The field claims exactly one thing, and it is the one thing the parser can answer:
    /// whether a diagnostic fell inside the declaration this fact was written in.
    pub clean: bool,
    pub guard: FactGuard,
}

impl DeclFact {
    /// The declaration's full name, qualified by the scope it was written in.
    ///
    /// What a symbol search shows and what a lookup keys on. Joining is done here rather than stored, so the two
    /// halves cannot disagree — and a caller that wants the segments separately still has them.
    pub fn qualified_name(&self) -> String {
        match &self.scope {
            Some(scope) if !self.name.is_empty() => format!("{scope}::{}", self.name),
            Some(scope) => scope.clone(),
            None => self.name.clone(),
        }
    }

    /// The segments of [`DeclFact::qualified_name`], outermost first.
    ///
    /// For a consumer that needs to walk a qualification rather than print it — resolving `ns::C::f` one segment
    /// at a time is the ordinary case, and splitting a joined string at every step would be work done per query.
    pub fn qualified_segments(&self) -> Vec<&str> {
        let mut segments: Vec<&str> = Vec::new();

        if let Some(scope) = &self.scope {
            segments.extend(scope.split("::"));
        }
        if !self.name.is_empty() {
            segments.push(&self.name);
        }

        segments
    }
}

/// What kind of declaration a [`DeclFact`] is.
///
/// A **smaller** vocabulary than [`crate::BindingKind`] on purpose: this is what a summary stores, and a stored
/// value is read by consumers that were written before it. `Other` is not a failure — it is "a declaration is here,
/// and its kind is not one the index distinguishes", which still answers "jump to the definition".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    Type,
    Function,
    Variable,
    Namespace,
    MacroLike,
    Other,
}

/// A `#define`, as the index needs it.
///
/// The body is **not** stored — only its shape, which is what the parser's rules ask for (`MacroBody`) and what the
/// analysis layer uses to decide whether an expansion is worth attempting. A caller that needs the tokens re-reads
/// the file: they are in the tree, and copying them into every cache entry would make the cache larger than the
/// project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroFact {
    pub name: String,
    /// Whether this is the name becoming a macro or ceasing to be one.
    pub kind: MacroKind,
    pub function_like: bool,
    pub body: cpp_parser::MacroBody,
    /// The macro's value, when its body is **one integer literal**: `#define _GLIBCXX_USE_CXX11_ABI 1`.
    ///
    /// The one piece of a body a condition can read. `#if NAME` on a macro expands it and re-reads the result, and
    /// `condition::macro_value` only accepts a single integer literal — so a body of two tokens, a body
    /// that is a name, and no body at all are the same answer to a condition, and storing any of them would be a
    /// longer way of saying `Unknown`. This is what makes `#if __cplusplus >= 201703L && _GLIBCXX_USE_CXX11_ABI`
    /// decidable once the file that defines the second name has been walked (see `docs/roadmap.md` §3.5c).
    ///
    /// Stored as the literal's **text**, so that reading it back needs no lexer: whoever reads it knows what it is
    /// — the same reason the fact stores a name's range rather than a way to find it.
    pub value: Option<Box<str>>,
    /// Where the fact is, for "go to macro definition".
    pub range: cpp_parser::SourceRange,
    pub guard: FactGuard,
    /// Does this fact settle the name's macro state **whatever branch of its `#if` is taken**?
    ///
    /// False for a fact outside every conditional (there is nothing to settle), and false for the ordinary
    /// conditional fact, whose existence depends on a macro nobody has. True for the two shapes where the
    /// conditional *cannot* change the answer:
    ///
    /// ```text
    /// #ifndef NAME            the condition IS "NAME is not defined yet", and the body defines it:
    /// #define NAME 1          taken → defined here; not taken → it was already defined. Either way: a macro.
    /// #endif
    ///
    /// #if A                   every branch ends in the same kind of fact about the same name, and there
    /// #define NAME 1          is an `#else` — so one of them ran, and whichever it was, the name is a macro.
    /// #else
    /// #define NAME 2
    /// #endif
    /// ```
    ///
    /// `#ifdef NAME` whose body ends in `#undef NAME` is the mirror, and says the name is **not** a macro after
    /// the block. A single-branch `#if A` never settles anything, because the branch may not be taken at all — and
    /// a region nested inside an unsettled one settles nothing either, which is why this is computed for the whole
    /// chain of enclosing conditionals rather than for the innermost alone.
    ///
    /// # What it does *not* say
    ///
    /// **Not which `#define` is in force.** In the second shape the branches differ in what they define the name
    /// *as*, and in the first the name may have been defined by an earlier header that this file cannot see. So
    /// `macro_definition` — the question "where is this macro defined" — ignores this field, and the reference
    /// query, which asks the weaker "is this name a macro *here*", is what reads it. Two questions, two answers.
    ///
    /// # Why this is a fact and not a conclusion
    ///
    /// The invariant every field here is judged by is "would a change to another file make it stale?" — a resolved
    /// type would, a `FileId` would, an instantiation would. This one cannot: it is computed from **this file's own
    /// directives**, and no header can change how a file writes its `#if`s. That is also why it is stored rather
    /// than recomputed per query: the answer needs the branch structure, and a summary keeps only the regions.
    pub settles_the_name: bool,
}

/// Which of the two things a macro name's history is made of.
///
/// `#undef` is stored because a query that answers "where is this macro defined" has to be able to answer "it is
/// not a macro here" instead: a name `#undef`ed above the cursor is an ordinary identifier, and pointing at the
/// `#define` it used to have would be a wrong answer rather than a missing one. Both are *facts about the text* —
/// they say what the file does, not what any compilation concludes — so the same ordering rule settles them
/// together: whichever comes last in translation order wins, and it can be either kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroKind {
    /// `#define NAME ...`
    Definition,
    /// `#undef NAME`
    Undefinition,
}

impl MacroKind {
    /// Is this fact the name becoming a macro?
    pub fn is_definition(self) -> bool {
        matches!(self, MacroKind::Definition)
    }
}

/// An `#include`, resolved or not.
///
/// The *target* is stored as the **path** the resolver found, for the same reason [`FileSummary::path`] is: an
/// id is an index into a run's interner and means nothing once that run is over, while the path is what a
/// consumer opens and what a reverse-include map is built from. A header that did not resolve keeps its
/// spelling, because "this project includes something we cannot find" is a fact a consumer wants to see rather
/// than a gap to hide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeFact {
    pub form: IncludeForm,
    /// The spelling between the delimiters, as written.
    pub spelling: String,
    /// Where the include resolved to, when the resolver found it.
    pub resolved: Option<std::path::PathBuf>,
    /// Whether the directive was `#include_next`.
    ///
    /// Stored, although no *query* has ever wanted it, because the answer has to be **re-derivable**: a summary
    /// that says where an include resolved can only be trusted while re-running the search gives the same answer,
    /// and a search that skipped candidates differently is a different search. See [`IncludeFact::as_include`].
    pub is_next: bool,
    pub range: cpp_parser::SourceRange,
    pub guard: FactGuard,
}

impl IncludeFact {
    /// The directive this fact was made from, as the resolver takes one.
    ///
    /// The round trip back from a fact to a directive is what makes a stored `resolved` **checkable**: `#include`
    /// resolution depends on which paths *exist*, and that is a fact about the filesystem which no key computed
    /// from the text can name — so a cached summary is a candidate that has to be re-verified against the
    /// filesystem before it is used. That check is only faithful if every field the search reads is here, which is
    /// why [`is_next`](Self::is_next) is stored and not dropped.
    pub fn as_include(&self) -> crate::preprocess::directive::Include {
        crate::preprocess::directive::Include {
            form: self.form,
            target: Box::from(self.spelling.as_str()),
            is_next: self.is_next,
        }
    }
}

/// `#include "local.h"` against `#include <system.h>`.
///
/// **Re-used from the preprocessor rather than re-declared here**: the two are searched for differently, the
/// directive reader is what knows that, and a second enum with the same two variants is how the two layers would
/// come to disagree about a spelling.
pub use crate::preprocess::directive::IncludeForm;

/// Which conditional region a fact was written in.
///
/// An index into the file's [`SummaryGuards`] rather than an inline condition: many facts share one region, and a
/// region is a small tree while a fact is meant to be tiny. [`FactGuard::Unconditional`] is the common case — code
/// outside every `#if` — and it is a variant rather than `0` so that a fact can never be misread as guarded by
/// forgetting to fill a field in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactGuard {
    Unconditional,
    Region(u32),
}

/// The conditional regions a file's facts refer to, in the order they were opened.
///
/// A region is stored as its **question**, never as its answer. Whether a region is entered depends on the
/// macros in force, and those are a property of the compilation rather than of the file: the `-D`s, the `-std=`
/// that fixes `__cplusplus`, and the five hundred names a compiler predefines. A summary is keyed without any of
/// that (see `cache.rs`), so the answer cannot live here — the same reasoning that keeps a resolved type out of a
/// declaration fact. A query that has the environment evaluates the stored question; one that does not answers
/// `Unknown`, which is what every consumer of this type did before the questions were stored at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SummaryGuards {
    /// One entry per region: the span of its conditions, which is its identity in [`FactGuard::Region`].
    pub regions: Vec<cpp_parser::SourceRange>,
    /// One entry per region, in the same order: the branches written for it, and what encloses it.
    pub conditionals: Vec<ConditionalRegion>,
    /// The region the file's **own include guard** opens, when it has one.
    ///
    /// Not a condition, and that is the whole point: `#ifndef _GLIBCXX_STRING` at the top of `string` is the file
    /// saying "read me once", not a feature test — entering the file at all is what the guard means, so a fact
    /// inside it is as visible as one written outside every `#if`. The index already treats the facts whose guard
    /// is *exactly* this region that way ([`crate::FileSummary`], `deguard_the_files_own_guard`); storing the
    /// index is what lets a walk treat the *nested* ones that way too, and a nested fact is the common case —
    /// `#ifndef GUARD / #define GUARD` followed by a file full of `#if __cplusplus` blocks.
    ///
    /// Without it those blocks read as "the guard is not taken", because by the time they are evaluated the guard
    /// has *defined* its own name — a file whose contents are `#ifndef X / #define X / … #endif` would answer
    /// "inactive" to everything inside it, which is exactly backwards.
    pub own_guard: Option<u32>,
}

/// One conditional region: the chain of branches a single `#if` opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionalRegion {
    /// The branches in source order; the first is the `#if`/`#ifdef`/`#ifndef` that opened the region.
    pub branches: Vec<GuardBranch>,
    /// The conditional this one is written inside, by region index.
    ///
    /// Stored rather than derived from the spans, because a region's span is its **condition**: a nested
    /// region's condition lies inside its parent's body, and so does the text of a sibling `#elif`. Which
    /// conditional encloses which is a fact about the nesting, and arithmetic on two ranges would be a second
    /// way of computing it — free to disagree with the walk that knew.
    ///
    /// Regions are numbered in opening order, so a parent always has a smaller index than its children.
    pub parent: Option<u32>,
}

/// One branch of a conditional: what it asks, and the body it guards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardBranch {
    /// Which directive wrote it: `#if`, `#ifdef`, `#ifndef`, `#elif` or `#else`.
    pub kind: crate::DirectiveKind,
    /// The condition as **text to evaluate**: the expression's tokens for `#if`/`#elif` (`__cplusplus >=
    /// 201703L`), the name for `#ifdef`/`#ifndef` (`_WIN32`), and `None` for `#else`, which has no condition and
    /// holds when nothing before it did.
    ///
    /// Text rather than tokens because a token list is four fields and a range per token in every cache entry,
    /// and because reading it back is one call to the lexer that read it the first time. Text rather than a
    /// *value* because there is nothing to evaluate it with here — see [`SummaryGuards`].
    pub condition: Option<Box<str>>,
    /// The body: from after this branch's directive to the next branch's directive, or to the `#endif`.
    ///
    /// Zero-length for an empty branch, which is what `#if A\n#else\n...` writes and what a consumer has to
    /// read as "no code here" rather than as "a body I could not find".
    pub body: cpp_parser::SourceRange,
    /// Where the directive is, so that a consumer explaining "this is not compiled" can point at the condition
    /// that decided it.
    pub range: cpp_parser::SourceRange,
}

/// One conditional that contains a position, and where its own condition is.
///
/// The two are different offsets and both are needed: which branch the position falls in is asked at the
/// position, while *what the condition means* is decided where the condition was written — a `#define NAME`
/// inside a region's own body changes the answer to `#ifndef NAME` if it is read at the wrong end of the region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConditionAt {
    /// Which entry of [`SummaryGuards::regions`] this is.
    pub region: u32,
    /// The offset of the region's own condition, which is where it must be evaluated.
    pub condition_at: usize,
}

impl SummaryGuards {
    /// The conditionals containing `offset`, **innermost first**.
    ///
    /// Empty for code outside every `#if`, which is the common case and the cheap one. This is the general
    /// answer, for a caller that has a position and no guard; a caller that *has* the guard — which is every
    /// caller in this crate, because a fact records one — should use [`SummaryGuards::conditions_of`] instead:
    /// it walks the nesting outwards from the region the guard names, where this has to look for it.
    pub fn conditions_at(&self, offset: usize) -> Vec<ConditionAt> {
        let mut found: Vec<ConditionAt> = Vec::new();

        // Innermost first: the smallest region that contains the position, then its parent, and so on. A region
        // contains the position when the position is inside one of its branch *bodies* — the region's own span
        // starts at its condition, and a position before the first branch's body is not in the region at all.
        let mut current = (0..self.conditionals.len())
            .filter(|index| self.conditionals[*index].contains(offset))
            .min_by_key(|index| self.span_of(*index as u32).map_or(usize::MAX, |span| span.length));

        while let Some(index) = current {
            let Some(span) = self.span_of(index as u32) else {
                break;
            };

            if Some(index as u32) != self.own_guard {
                found.push(ConditionAt {
                    region: index as u32,
                    condition_at: span.start_offset,
                });
            }

            current = self.conditionals[index].parent.map(|parent| parent as usize);
        }

        found
    }

    /// The conditionals around the region a fact's guard names, **innermost first**.
    ///
    /// The same answer as [`SummaryGuards::conditions_at`] for the region that guard was assigned from — and the
    /// walk is the *point*: the nesting is already recorded (each region names its parent), so this costs one
    /// step per level of nesting instead of a search over every conditional in the file. A file with two hundred
    /// conditionals would otherwise pay for all of them on every include it has, in every query that walks it.
    ///
    /// The file's own guard is left out: it is not a condition — see [`SummaryGuards::own_guard`].
    pub fn conditions_of(&self, region: u32) -> Vec<ConditionAt> {
        let mut found = Vec::new();
        let mut current = Some(region);

        while let Some(index) = current {
            let Some(span) = self.span_of(index) else {
                break;
            };

            if Some(index) != self.own_guard {
                found.push(ConditionAt {
                    region: index,
                    condition_at: span.start_offset,
                });
            }

            current = self
                .conditionals
                .get(index as usize)
                .and_then(|conditional| conditional.parent);
        }

        found
    }

    /// The region a fact's guard names, as the preprocessor's own value, with the branch **in force at
    /// `offset`** as its `active_branch`.
    ///
    /// `None` when the index names no region, which a summary whose guards were written by another producer can
    /// have. The caller evaluates the returned region against the macros in force where the *condition* was
    /// written — [`SummaryGuards::conditions_at`] says where that is.
    pub fn region_at(&self, region: u32, offset: usize) -> Option<Region> {
        let conditional = self.conditionals.get(region as usize)?;

        Some(Region {
            branches: conditional.branches.iter().map(GuardBranch::as_branch).collect(),
            active_branch: conditional.branch_at(offset)?,
        })
    }

    /// The span of a region's condition, when the index names one.
    pub fn span_of(&self, region: u32) -> Option<cpp_parser::SourceRange> {
        self.regions.get(region as usize).copied()
    }

    /// Was the code the guard names compiled, given the macros in force at each condition?
    ///
    /// `macros_at` is asked for a table **per condition**, and takes that condition's own offset — not
    /// `offset`. That is not a detail: a region's condition is decided where it is written, and the tokens that
    /// matter are the ones in force there. `#ifndef NAME` whose own body then writes `#define NAME` is the shape
    /// that makes the difference, and reading the condition at the *fact's* offset would decide it the other way
    /// round — for every include guard and every `#ifndef X / #define X` block in the corpus.
    ///
    /// The rule over the chain of enclosing regions is [`crate::Guard::visibility`]'s: one region known not to be
    /// taken makes the code `Inactive` whatever the others say, one that cannot be decided makes it `Unknown`, and
    /// only when every one of them is taken is it `Active`. A table that cannot speak about a name answers
    /// [`Lookup::Unanswered`](crate::Lookup), which the evaluator turns into `Unknown` — so a condition this
    /// index has no evidence about costs an answer, never a wrong one.
    pub fn visibility_of<M: crate::MacroValues>(
        &self,
        guard: FactGuard,
        offset: usize,
        macros_at: impl Fn(usize) -> M,
    ) -> Visibility {
        // The fast path, and it is the common one: code outside every conditional is compiled without anything
        // being evaluated, and asking would cost a walk per include of every file a query touches.
        let FactGuard::Region(region) = guard else {
            return Visibility::Active;
        };

        let mut unknown = false;

        for at in self.conditions_of(region) {
            let Some(region) = self.region_at(at.region, offset) else {
                // A guard naming a region this summary does not describe — a summary written before the regions
                // carried their conditions, or one whose bytes were produced by something else. Nothing can be
                // said about it, and `Unknown` is what every query said about every region before this existed.
                unknown = true;
                continue;
            };

            match region.visibility(&macros_at(at.condition_at)) {
                Some(true) => {}
                Some(false) => return Visibility::Inactive,
                None => unknown = true,
            }
        }

        if unknown {
            Visibility::Unknown
        } else {
            Visibility::Active
        }
    }
}

impl ConditionalRegion {
    /// Which branch's body holds `offset`.
    ///
    /// The last branch that starts at or before the position when no body contains it: the position can only be
    /// in the directives between two bodies (a fact's range starts after its directive, so this is the
    /// degenerate case of a zero-length body), and "the branch that was open there" is the reading
    /// [`crate::GuardStack`] itself takes — it makes the branch in force the last one it observed.
    ///
    /// `None` only for a region with no branches at all, which no walk produces and a foreign summary can.
    pub fn branch_at(&self, offset: usize) -> Option<usize> {
        if let Some(index) = self.branches.iter().position(|branch| {
            branch.body.start_offset <= offset && offset < branch.body.end_offset()
        }) {
            return Some(index);
        }

        self.branches
            .iter()
            .rposition(|branch| branch.body.start_offset <= offset)
            .or(if self.branches.is_empty() { None } else { Some(0) })
    }

    /// Is there an `#else`? Then one of the branches is taken whatever the conditions say.
    pub fn exhaustive(&self) -> bool {
        self.branches
            .iter()
            .any(|branch| branch.kind == crate::DirectiveKind::Else)
    }

    /// Does this region's body hold `offset`?
    fn contains(&self, offset: usize) -> bool {
        self.branches.iter().any(|branch| {
            branch.body.start_offset <= offset && offset < branch.body.end_offset()
        })
    }
}

impl GuardBranch {
    /// This branch as the guard layer reads it: the same condition, with its text read back into tokens.
    ///
    /// `#ifdef NAME` becomes a branch whose *name* is set and whose tokens are empty, which is how
    /// [`crate::Branch::holds`] reads it — so the two layers cannot disagree about what `#ifdef` means, and
    /// neither can they about `#else` or about an expression.
    pub fn as_branch(&self) -> Branch {
        match self.kind {
            crate::DirectiveKind::Ifdef | crate::DirectiveKind::Ifndef => Branch {
                kind: self.kind,
                tokens: Vec::new(),
                name: self.condition.clone(),
                range: self.range,
            },
            _ => Branch {
                kind: self.kind,
                tokens: self.tokens(),
                name: None,
                range: self.range,
            },
        }
    }

    /// The stored condition, read back into tokens whose ranges point into the file they came from.
    ///
    /// The lexer rather than a split on whitespace, for the reason the reference query gives: this has to be the
    /// same reader that produced the tokens the condition was written as, and a second reader would disagree
    /// about `'` digit separators, about a suffix, and about a `//` comment ending a condition early.
    ///
    /// Ranges are shifted by the directive's own position, so a consumer that follows a token to the file lands
    /// in the right place. A text that does not lex at all is not an error: the evaluator reads what it can, and
    /// a condition that cannot be read is `Unknown` rather than wrong — the same answer the guard layer gives for
    /// a condition it cannot parse.
    fn tokens(&self) -> Vec<crate::Token> {
        let Some(text) = self.condition.as_deref() else {
            return Vec::new();
        };

        let mut errors = Vec::new();
        let mut lexer = cpp_parser::CppLexer::new(text, cpp_parser::LexerConfig::default(), &mut errors);

        let base = self.range.start_offset;
        lexer
            .tokenize()
            .into_iter()
            .filter(|token| !cpp_parser::is_trivia(token.kind))
            .map(|token| {
                crate::Token::new(
                    token.kind,
                    text.get(token.range.start_offset..token.range.end_offset())
                        .unwrap_or_default(),
                    cpp_parser::SourceRange::new(base + token.range.start_offset, token.range.length),
                )
            })
            .collect()
    }
}

/// Everything the index remembers about one file.
///
/// [`FileSummary::is_empty`] is not a curiosity: an empty summary is what a file that failed to parse, or a header
/// with nothing but comments, produces — and a consumer that treats "no facts" as "no declarations exist" is making
/// a claim the index never made. See `Known` in [`crate::symbol`] for the vocabulary that keeps those apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSummary {
    /// Where the file is, as the path it was read from.
    ///
    /// A path rather than a [`FileId`], and the reason is what survives a restart: a `FileId` is an index into a
    /// [`PathInterner`] that is built fresh every run, so an id written to disk means something different — or
    /// nothing at all — when it is read back. The path is also what a consumer needs anyway: a "go to
    /// definition" in another file opens it by name.
    ///
    /// [`FileId`]: crate::FileId
    /// [`PathInterner`]: crate::PathInterner
    pub path: std::path::PathBuf,
    /// What this summary was built from — see [`SummaryKey`]. Stored so that a loaded entry can be *checked*
    /// against the file it claims to describe rather than trusted because it was found.
    pub key: SummaryKey,
    pub declarations: Vec<DeclFact>,
    pub macros: Vec<MacroFact>,
    pub includes: Vec<IncludeFact>,
    pub guards: SummaryGuards,
}

impl FileSummary {
    /// A summary with no facts, for the file at `path`, built under `key`.
    pub fn empty(path: impl Into<std::path::PathBuf>, key: SummaryKey) -> Self {
        FileSummary {
            path: path.into(),
            key,
            declarations: Vec::new(),
            macros: Vec::new(),
            includes: Vec::new(),
            guards: SummaryGuards::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty() && self.macros.is_empty() && self.includes.is_empty()
    }

    /// Put a fact's guard into the file's region list, returning the index to store in the fact.
    ///
    /// The single entry point for guarding a fact, so that the "many facts, one region" sharing cannot be forgotten
    /// at one call site out of five — the same reason the parser's rules share their predicates.
    pub fn intern_region(&mut self, region: Option<cpp_parser::SourceRange>) -> FactGuard {
        let Some(range) = region else {
            return FactGuard::Unconditional;
        };
        match self.guards.regions.iter().position(|known| *known == range) {
            Some(index) => FactGuard::Region(index as u32),
            None => {
                self.guards.regions.push(range);
                FactGuard::Region((self.guards.regions.len() - 1) as u32)
            }
        }
    }
}

impl DeclKind {
    /// The index's vocabulary for a binding, which is deliberately **coarser** than the analysis layer's.
    ///
    /// `parser_symbols` maps the same source vocabulary onto `cpp_parser::SymbolKind`, and that is not a duplicate
    /// of this: the parser is asked "which reading should I take" (five answers, in its own enum), while the index
    /// stores "what kind of thing is written here" and has to keep that answer readable by consumers written before
    /// the finer kinds existed. Two questions, two vocabularies — the same reason `IncludeForm` is re-used rather
    /// than re-declared.
    pub fn from_binding_kind(kind: crate::BindingKind) -> DeclKind {
        use crate::BindingKind as B;
        match kind {
            B::Class | B::Enum | B::Alias | B::Typedef | B::TemplateParameter => DeclKind::Type,
            B::Function
            | B::Constructor
            | B::Destructor
            | B::ConversionFunction
            | B::OperatorFunction
            | B::LiteralOperator => DeclKind::Function,
            B::Variable | B::Enumerator => DeclKind::Variable,
            B::Namespace => DeclKind::Namespace,
            // A label, a `using`, or something the walker could not classify: the declaration is real and worth a
            // "go to definition", so it is stored as `Other` rather than dropped.
            _ => DeclKind::Other,
        }
    }
}

// Declaration facts are built by [`crate::declarations::build_facts`], in the `sema` layer rather than here, and
// the reason is worth knowing before looking for it: a fact has to say which `#if` it was written in, which means
// the walk needs the *preprocessor* state as well as the scopes — and this module is the shape of what gets
// stored, not a place that knows about directives. There was a `build_declarations` here that took only a scope
// tree and filled every guard with `Unconditional`; it was deleted rather than kept, because a function whose
// contract is "the guards are wrong" is one a caller reaches for by accident.
