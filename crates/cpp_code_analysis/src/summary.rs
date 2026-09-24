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
    /// Where the fact is, for "go to macro definition".
    pub range: cpp_parser::SourceRange,
    pub guard: FactGuard,
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
/// The conditions themselves are [`crate::Guard`] values — macro expressions, which is what makes "is this code
/// even being compiled" answerable as `Active`/`Inactive`/`Unknown` without the index knowing the machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SummaryGuards {
    pub regions: Vec<cpp_parser::SourceRange>,
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
