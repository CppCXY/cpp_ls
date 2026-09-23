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
use crate::paths::FileId;

/// A declaration the file writes: enough to find it, name it, and say what kind of thing it is.
///
/// Deliberately not a syntax tree and not a type: the fields are the ones a *query* needs — where to jump, what to
/// call the symbol, what to filter by, and which branch it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclFact {
    /// The name as written, without qualification: `Widget` in `ns::Widget`.
    pub name: String,
    /// The qualifier as written, when the declaration spelled one: `ns` in `ns::Widget`. `None` for a plain name.
    pub qualifier: Option<String>,
    pub kind: DeclKind,
    /// The whole declaration, for a "go to definition" highlight.
    pub range: cpp_parser::SourceRange,
    /// Just the name, which is what a reference search matches. Separate from `range` for the reason
    /// [`crate::Binding`] documents: collapsing them renames whole declarations.
    pub name_range: cpp_parser::SourceRange,
    pub guard: FactGuard,
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
    pub function_like: bool,
    pub body: cpp_parser::MacroBody,
    /// Where the definition is, for "go to macro definition".
    pub range: cpp_parser::SourceRange,
    pub guard: FactGuard,
}

/// An `#include`, resolved or not.
///
/// The *target* is stored as the id the resolver produced, which is what the graph is built from; a header that did
/// not resolve keeps its spelling, because "this project includes something we cannot find" is a fact a consumer
/// wants to see rather than a gap to hide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeFact {
    pub form: IncludeForm,
    /// The spelling between the delimiters, as written.
    pub spelling: String,
    /// The resolved file, when the resolver found one.
    pub resolved: Option<FileId>,
    pub range: cpp_parser::SourceRange,
    pub guard: FactGuard,
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
    pub file: FileId,
    /// What this summary was built from — see [`SummaryKey`]. Stored so that a loaded entry can be *checked*
    /// against the file it claims to describe rather than trusted because it was found.
    pub key: SummaryKey,
    pub declarations: Vec<DeclFact>,
    pub macros: Vec<MacroFact>,
    pub includes: Vec<IncludeFact>,
    pub guards: SummaryGuards,
}

impl FileSummary {
    /// A summary with no facts, for `file`, built under `key`.
    pub fn empty(file: FileId, key: SummaryKey) -> Self {
        FileSummary {
            file,
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

/// Build the **declaration facts** of one file from its scope tree.
///
/// Declarations only: the macros and includes come from the directive reader, which is a separate walk with its own
/// vocabulary, and mixing the two here would make this function's contract "whatever the index happens to hold".
///
/// Two things this deliberately does *not* do:
///
/// * **resolve anything** — the name is stored as written (`identifier_text`), and a declaration whose name is not
///   a plain identifier (a destructor's `~S`, an `operator+`) is stored with the kind `Other` and no name, rather
///   than with a guess;
/// * **decide whether the declaration is visible** — that is a query over scopes and the include graph, and it needs
///   the environment the file was entered with.
///
/// The guard of every fact is `Unconditional` for now: region tracking needs the conditional walk that the include
/// facts come from, and claiming a fact is unconditional when it is not would be exactly the silent wrong answer
/// `docs/index-design.md` forbids — so it is recorded as a gap here rather than filled with a default.
pub fn build_declarations(scopes: &crate::ScopeTree) -> Vec<DeclFact> {
    let mut facts = Vec::new();

    for scope in scopes.scopes() {
        for binding in &scope.bindings {
            let name = binding
                .name
                .identifier_text()
                .map(str::to_string)
                .unwrap_or_default();

            facts.push(DeclFact {
                name,
                qualifier: None,
                kind: DeclKind::from_binding_kind(binding.kind),
                range: binding.range,
                name_range: binding.name_range,
                guard: FactGuard::Unconditional,
            });
        }
    }

    facts
}
