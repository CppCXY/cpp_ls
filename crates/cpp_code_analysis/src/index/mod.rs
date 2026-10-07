//! Building one file's [`FileSummary`] — the join nothing else in this crate performs.
//!
//! # Why this needs a module of its own
//!
//! The three passes a summary is made of are each somebody else's job and none of them knows about the others:
//!
//! ```text
//! cpp_parser                 the tree
//! preprocess::preprocess     the directives, the macro table, the conditions
//! sema::build_scopes         the scopes and their bindings
//! declarations::build_facts  the declarations *and* which `#if` each was written in   ← needs all three
//! ```
//!
//! [`crate::build_facts`] needs the scopes *and* the preprocessing, because a declaration has to say which
//! conditional region it is in — and neither of those layers can produce it alone. That join is what this module
//! is, and writing it anywhere else would mean one of the three layers reaching into another.
//!
//! # What is deliberately not here
//!
//! No caching, no filesystem walks, no invalidation. This builds a summary from text it is given; deciding
//! *whether* to build one, and what to do with the result, is [`crate::cache`]'s and the consumer's business.
//! Keeping the decision out is what makes this testable on a string with no project around it.
//!
//! # Why the resolver is optional
//!
//! An include's target is a fact about the file, but *where it resolved to* is a fact about the project and its
//! include paths. A caller with no configuration — a test, or an editor looking at a file outside any project —
//! still gets every other fact, and the includes come out unresolved with their spelling intact. That is the
//! honest answer rather than a missing one: the file said `#include "widget.h"` and nothing here knows where
//! that is.

use std::path::{Path, PathBuf};

use cpp_parser::{CppParser, CppSyntaxTree, ParserConfig};

pub mod environment;
mod names;
pub mod project;
pub mod references;
pub mod store;
pub mod watch;
pub mod worklist;

pub use environment::{MacrosHere, visibility_at};

pub use project::{
    CookedFile, HeaderTarget, IncludeVisibility, MemberCompletions, MemberList, NameCompletions, NameProvenance,
    OfferedName, ProjectDefinition, ProjectDefinitions, ProjectIndex, ProjectMacro, ProjectMember,
    ProjectSymbol, UnlistedBase, VisibleDeclaration, definition_across_files,
    definitions_across_files, macro_across_files, member_across_files, member_completions_at,
    header_at, member_definitions_across_files, members_of, name_completions_at,
};
pub use references::{
    FileReferences, MacroReferences, Reference, ReferenceBudget, ReferenceKind, Rename,
    SymbolReferences, SymbolToFind, macro_references, symbol_references,
};
pub use store::{
    IncludeBudget, IncludeIndex, NotIndexed, NotIndexedReason, StoreStats, SummaryStore,
    UnresolvedEdge,
};
pub use watch::{ChangeBatch, EventKind, FileEvent, PathPattern, Response, WatchFilter};
pub use worklist::{Priority, Step, StepOutcome, Worklist, outcome_of};

use crate::cache::{SummaryKey, content_hash};
use crate::include::config::CompilerConfig;
use crate::file::paths::{FileProvider, PathInterner};
use crate::include::IncludeResolver;
use crate::preprocess::directive::{Directive, SpannedDirective};
use crate::sema::declarations::{assign_guards, build_facts, mark_settling_macro_facts};
use crate::sema::scopes::build_scopes;
use crate::stages::{Stage, StageTimer};
use crate::summary::{FactGuard, FileSummary, IncludeFact, MacroFact};
use crate::summary_codec::DecodeError;

/// Everything needed to turn a file's text into a summary, minus the text.
///
/// The resolver is a [`FileProvider`] and a [`CompilerConfig`] rather than an [`IncludeResolver`] because the
/// resolver borrows both, and a caller indexing a project wants to keep the configuration and the provider around
/// across calls.
pub struct FileIndexer<'a, F: FileProvider> {
    files: &'a F,
    config: &'a CompilerConfig,
    /// What the file's **includes** say about the macros it invokes — see [`crate::sema::scopes::MacroBodies`].
    ///
    /// Optional because most callers have no include closure: a buffer on its own, a test, a probe over a list of
    /// files. `None` is "nobody says", which produces exactly the summary this type produced before the field
    /// existed — the reading a file's own tokens support and nothing more.
    ///
    /// **Two consumers, one value**, and that is why the type is the parser's `MacroEnvironment` rather than a
    /// trait object: the *parse* reads it through `ParserConfig::with_macros_from_includes` (which is what lets a
    /// rule take `_STD addressof(*p)` as one qualified name when `_STD` is `::std::`), and the *scope walk*
    /// reads it through [`crate::sema::scopes::MacroBodies`] (which is what opens `std` from `_STD_BEGIN`'s
    /// `namespace std {`). A second value for either would be a second answer to the same question.
    bodies: Option<&'a dyn cpp_parser::MacroBodies>,
    /// The **same evidence**, for the reader that wants the parser's questions rather than the scope walk's one:
    /// `ParserConfig::with_macros_from_includes` takes a [`cpp_parser::MacroFacts`], and a `dyn MacroBodies` is not
    /// one. Both point at the single value the caller handed to [`FileIndexer::with_macro_bodies`], so the parse and
    /// the scope walk cannot be given two different environments — which is the whole reason this type takes a body
    /// reader at all.
    ///
    /// A **unit's timeline** ([`crate::MacroView`]) is such a value: it answers the parser's questions positionally
    /// out of one walk, where the alternative was materialising a map per file.
    macro_facts: Option<&'a dyn cpp_parser::MacroFacts>,
    /// Macro facts whose directive sits at one of these offsets are left out of the summary — the unit walk's own
    /// decision about which `#define`s a compiler would have read. See [`FileIndexer::without_these_macros`].
    dead_macros: Vec<usize>,
    /// **What the compilation defines**, which no file in the closure knows: `__cplusplus` and the rest of the
    /// compiler's own names. See [`FileIndexer::with_seed`].
    seed: Option<&'a crate::Marked>,
    /// The includes a scan already resolved — see [`FileIndexer::scan_includes`]. Resolving an include is a
    /// filesystem search, so a caller that has done it once passes the answers along instead of paying twice.
    scanned: Option<&'a ScannedIncludes>,
}

/// The `#include`s of one file, found and resolved **before** its parse — see [`FileIndexer::scan_includes`].
#[derive(Debug, Default)]
pub struct ScannedIncludes {
    /// By the offset the directive starts at. The directive itself is kept too, so that an answer is only reused
    /// for the same include: a scan that read a line differently from the parser would find its answer refused, not
    /// wrongly used.
    found: std::collections::HashMap<usize, (crate::preprocess::directive::IncludeForm, Box<str>, bool, Option<PathBuf>)>,
    order: Vec<usize>,
}

impl ScannedIncludes {
    /// The resolved targets, in the order the file writes them.
    pub fn targets(&self) -> Vec<PathBuf> {
        self.order
            .iter()
            .filter_map(|at| self.found.get(at)?.3.clone())
            .collect()
    }

    fn resolved(&self, at: usize, include: &crate::preprocess::directive::Include) -> Option<Option<PathBuf>> {
        let (form, target, is_next, resolved) = self.found.get(&at)?;
        (*form == include.form && **target == *include.target && *is_next == include.is_next)
            .then(|| resolved.clone())
    }
}

impl<'a, F: FileProvider> FileIndexer<'a, F> {
    pub fn new(files: &'a F, config: &'a CompilerConfig) -> Self {
        FileIndexer {
            files,
            config,
            bodies: None,
            macro_facts: None,
            dead_macros: Vec::new(),
            seed: None,
            scanned: None,
        }
    }

    /// Index with includes a scan already resolved, so that the parse does not search for them a second time.
    pub fn with_scanned_includes(mut self, scanned: &'a ScannedIncludes) -> Self {
        self.scanned = Some(scanned);
        self
    }

    /// The same, for a caller that may or may not have run the scan — `None` leaves the sweep to find the
    /// `#include`s itself, which is the same answer one search later.
    ///
    /// One method rather than a `match` at each call site, because "the scan is optional" is a property of this
    /// builder and not of the callers: [`crate::SummaryStore::prepare`] has no wave to feed and skips it, and
    /// [`crate::SummaryStore::prepare_closure`] needs it. See that pair for the measurement.
    pub fn with_scanned_includes_opt(mut self, scanned: Option<&'a ScannedIncludes>) -> Self {
        self.scanned = scanned;
        self
    }

    /// Index with what the file's includes say about the macros it invokes.
    ///
    /// The one piece of evidence a summary can contain that is **not** in the file it describes: `_STD_BEGIN`'s
    /// `namespace std {` is in `yvals_core.h`, and every declaration in MSVC's `<vector>` is scoped by it. See
    /// [`crate::summary::MacroScopeReading`], which is where the reading and the body behind it are kept.
    ///
    /// Generic over the reader rather than taking a `dyn` one, because the two traits it has to satisfy are
    /// unrelated as *trait objects* (`MacroFacts` implies `MacroBodies` through a blanket impl, which the compiler
    /// cannot see through `dyn`): the call site passes a concrete reader — a materialised `MacroEnvironment`, or a
    /// [`crate::MacroView`] into a unit's timeline — and both coercions happen here.
    pub fn with_macro_bodies<T: cpp_parser::MacroFacts>(mut self, bodies: &'a T) -> Self {
        self.bodies = Some(bodies);
        self.macro_facts = Some(bodies);
        self
    }

    /// The same evidence, for a caller that **already holds it as two trait objects**.
    ///
    /// [`FileIndexer::with_macro_bodies`] is generic because a `dyn MacroFacts` does not itself satisfy
    /// `MacroFacts` — the blanket impl that makes a facts reader a bodies reader cannot be seen through the trait
    /// object. A caller that has already erased the type (`SummaryStore::prepare_with_the_environment`, which is
    /// handed one from a session) says so here instead of naming a concrete type it does not have.
    ///
    /// Both arguments are the **same value** in every call in this crate — the field documentation above says why
    /// there are two of them at all — so a caller passes one thing twice.
    pub fn with_macro_body_readers(
        mut self,
        bodies: &'a dyn cpp_parser::MacroBodies,
        facts: &'a dyn cpp_parser::MacroFacts,
    ) -> Self {
        self.bodies = Some(bodies);
        self.macro_facts = Some(facts);
        self
    }

    /// The same, for a caller that has the two readers as one `Option` — which is how
    /// [`crate::SummaryStore::prepare_with_the_environment`] carries them, since a caller preparing a whole wave
    /// may or may not have an environment for each file of it.
    pub fn with_a_macro_environment(
        self,
        environment: Option<(&'a dyn cpp_parser::MacroBodies, &'a dyn cpp_parser::MacroFacts)>,
    ) -> Self {
        match environment {
            Some((bodies, facts)) => self.with_macro_body_readers(bodies, facts),
            None => self,
        }
    }

    /// **What the compilation itself defines** — `-D`s, `-std=`, and the compiler's own several hundred names.
    ///
    /// Here for one question, and it is the question this type could not previously answer: *is `__cplusplus`
    /// defined?* A file's closure knows what its includes define, and the answer for a name the **compiler**
    /// predefines is nowhere in that closure. Measured on `vcruntime.h`, whose `#ifdef __cplusplus` decides
    /// `_STL_LANG`: the closure answered "does not know it", so the branch came out false, so `_STL_LANG` was `0L`
    /// while `__cplusplus` and `_MSVC_LANG` were both `202400L`.
    ///
    /// The same value the store was seeded with (`SummaryStore::with_macros`), so that there is one answer to what
    /// the compilation defines rather than two.
    pub fn with_seed(mut self, seed: &'a crate::Marked) -> Self {
        self.seed = Some(seed);
        self
    }

    /// **`#define`s the unit's walk decided are not compiled**, by directive offset.
    ///
    /// The decision belongs to the walk, which evaluates each guarded fact against the unit's own state — the same
    /// evaluation the cook uses, and the only one that knows what the compilation defines. This type cannot make it:
    /// it has a file and a `Marked`, and the question (`#ifdef __cplusplus`) is about the *unit*.
    ///
    /// What it does with the answer is **drop those facts**, so that a consumer reading the summary does not have to
    /// decide the region a second time — which it would do with a different environment, and answer differently.
    /// Measured on `vcruntime.h`, which writes `_STL_LANG` once per branch of `#ifdef __cplusplus`: the walk takes
    /// the `_MSVC_LANG` branch and skips the `#else`'s `0L`, while a lookup over the raw facts answers `0L` for a
    /// file the compiler reads as C++20.
    pub fn without_these_macros(mut self, dead: &[usize]) -> Self {
        self.dead_macros = dead.to_vec();
        self
    }

    /// Build the summary of the file at `path`, whose text is `source`.
    ///
    /// `key` is the caller's, because only the caller knows the compilation context — the configuration and the
    /// directory the file sits in. It is stored in the summary rather than recomputed, so that a loaded entry can
    /// be checked against the file it claims to describe.
    ///
    /// # The one part of the key that is *not* the caller's
    ///
    /// `key.content_hash` is **replaced** by the hash of `source`. `source` is the same text that was just parsed,
    /// and no caller can describe it more accurately than hashing it — while a caller that got it wrong would
    /// store a summary under a name that does not describe its own text, which is a wrong answer rather than a
    /// cache miss. The authority is here because the text is here.
    ///
    /// # See also
    ///
    /// [`FileIndexer::includes_of`], which answers the one thing about a file that *other* files' reading waits for —
    /// what it includes — without building any of the rest.
    pub fn index(&self, path: &Path, source: &str, key: SummaryKey) -> FileSummary {
        // Parsed **for the configuration's target**: which compiler's reserved spellings mean what is part of the
        // compilation, not of the text — `__int128` is a type to g++ and a name to cl.exe. See
        // [`CompilerConfig::dialect`], and note that the same value is part of `context_hash`, so a summary
        // written for one target is never read as if it were written for the other.
        //
        // …and **with the include closure's macro bodies when the caller has them**: a rule that takes
        // `_STD addressof(*p)` for one qualified name needs to know that `_STD` is `::std::`, and that body is in
        // a header. Without them the parse is the shape-only reading, which is what a buffer on its own
        // gets and all it can get.
        // …and **no macro environment**, deliberately. The parse is of a file's own text and the grammar makes no
        // reading turn on "is this name a macro": the text it is meant to read has been preprocessed, and the
        // version that passed a positional environment here was answering a *stream* offset against a *file*
        // timeline — see `MacroView::applies_here`, which records the trap. The environment is still built and
        // still used, one layer down, by the scope walk (`sema::scopes::MacroBodies`).
        let config = ParserConfig::default().with_dialect(self.config.dialect());

        let tree = {
            let _parse = StageTimer::new(Stage::Parse);
            CppParser::parse(source, config)
        };
        let summary = {
            let _sweep = StageTimer::new(Stage::Sweep);
            self.index_tree(path, source, &tree, key)
        };
        // **Destroying the tree is work**, and it is timed because it is not free and not obvious: a file's tree is
        // tens of thousands of green nodes held by `Arc`s, and dropping it walks every one of them. On the 138-file
        // project this was the missing third of the cold index after the shape walk was fixed — the stages summed to
        // 6.0 s of a 9.5 s run, and the difference was here.
        {
            let _drop = StageTimer::new(Stage::Drop);
            drop(tree);
        }
        summary
    }

    /// **The files `source` includes**, resolved — without parsing it.
    ///
    /// The include graph is what decides how much of a project can be read at once, and it is only known by reading:
    /// a file's includes are a product of its text. But they are not a product of its *tree*. A `#include` is a
    /// line, the lexer finds every line that begins with a `#`, and the directive scan reads the ones that are
    /// includes — a pass that runs at the speed of the lexer (a hundred and fifty megabytes a second) where the parse
    /// that follows it is an order of magnitude slower. So a caller that wants to start reading a header's includes
    /// **before it has finished parsing the header** can: this is the answer `summary.includes` will hold, obtained
    /// early.
    ///
    /// It is the same answer, not an approximation of it — the same directives, the same resolver, the same search
    /// path and the same directory — which is what lets a caller act on it: every file named here is a file
    /// `index` will record as an include of this one. Order is the text's, and a target the search did not find is
    /// simply not in the list (the summary records it as unresolved).
    pub fn includes_of(&self, path: &Path, source: &str) -> Vec<PathBuf> {
        self.scan_includes(path, source).targets()
    }

    /// [`FileIndexer::includes_of`], keeping the resolutions so that [`FileIndexer::with_scanned_includes`] can hand
    /// them to the parse that follows — a search of the include path is paid for once, not once per pass.
    pub fn scan_includes(&self, path: &Path, source: &str) -> ScannedIncludes {
        let (mut tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
        fold_quoted_header_names(source, &mut tokens);

        let mut interner = PathInterner::new(cfg!(windows));
        let resolver = IncludeResolver::new(self.files, self.config);
        let directory = path.parent().unwrap_or(Path::new("."));

        let mut scanned = ScannedIncludes::default();
        for spanned in crate::preprocess::directive::scan_directives(source, &tokens) {
            let Directive::Include(include) = &spanned.directive else {
                continue;
            };
            let Some(fact) = include_fact(&spanned.directive, spanned.range, directory, &resolver, &mut interner, None) else {
                continue;
            };

            let at = spanned.range.start_offset;
            scanned
                .found
                .insert(at, (include.form, include.target.clone(), include.is_next, fact.resolved));
            scanned.order.push(at);
        }
        scanned
    }

    /// **Index a file through its cooked stream** — the reading a compiler would parse, with every range turned
    /// back into a position in the file.
    ///
    /// The tree is built from `rendered.text` (a rendering, whose offsets are not file offsets), so this indexes
    /// the rendering and then answers for every range through
    /// [`FileSummary::map_into_the_file`](crate::FileSummary::map_into_the_file). What comes out is a summary of
    /// the same *shape* as one from the file's own text, describing the program a compiler sees instead of the
    /// text the file says: `DECLARE_HANDLE(HWND)` declares `HWND` here and declares nothing there, and a name
    /// inside a branch nobody takes is declared there and not here.
    ///
    /// Two readings rather than one replacement, and that is the architecture's split rather than a limitation:
    /// the raw summary answers "what is written in this file" (every branch, every macro body — what a reader
    /// editing the file sees) and this answers "what is compiled" (what the editor's *other* features must agree
    /// with). See `examples/cooked_index.rs` for the measurement of how far apart they are.
    ///
    /// The rendering is the caller's because the caller cooked the stream: what to define before it, and which
    /// file's environment to use, are compilation decisions this type has no way to make.
    ///
    /// # The errors come back too, and placed
    ///
    /// A rendering is the text a compiler parses, so what the parser says *about it* is the honest answer to "does
    /// this file compile" — and it is a strictly better answer than the file's own text gives, because a shape the
    /// file writes as a macro call stops being a guess once the macro is expanded, and a declaration in a branch
    /// nobody takes is not there at all. So the errors are not discarded here: each one is asked of the same map
    /// every fact's range goes through ([`crate::RenderedCooked::reported_span`], whose answer is always a place in
    /// this file — the invocation when a macro produced the text), and a rendering error that **cannot** be placed
    /// here is counted rather than reported against a text the reader cannot see.
    pub fn index_rendering(
        &self,
        path: &Path,
        rendered: &crate::preprocess::cooked::RenderedCooked,
        key: SummaryKey,
    ) -> crate::IndexedRendering {
        // **The stream is already cooked**, so there is nothing for a macro environment to add and a great deal for
        // it to get wrong: its answers are positional in the original file's coordinates while the parser reads the
        // rendering's. See the same note in [`FileIndexer::index`].
        let config = ParserConfig::default().with_dialect(self.config.dialect());

        let tree = {
            let _parse = StageTimer::new(Stage::RenderParse);
            CppParser::parse(&rendered.text, config)
        };
        let mut summary = {
            let _sweep = StageTimer::new(Stage::RenderSweep);
            self.index_tree(path, &rendered.text, &tree, key)
        };
        let report = {
            let _map = StageTimer::new(Stage::Map);
            summary.map_into_the_file(rendered)
        };

        // The tree's errors, each asked of the map. `reported_span` answers for a node's whole range — from the first
        // token it covers to the last — and `reported_at` for the span an offset falls in, which is the fallback for
        // an error with no token of its own (the parser can report a range that covers nothing).
        let mut diagnostics = Vec::new();
        let mut unplaced = 0usize;
        for error in tree.get_errors() {
            let range = cpp_parser::source_range(error.range);
            let placed = rendered
                .reported_span(range)
                .or_else(|| rendered.reported_at(range.start_offset));
            match placed {
                Some(range) => diagnostics.push(crate::CookedDiagnostic {
                    range,
                    message: error.message.clone(),
                }),
                None => unplaced += 1,
            }
        }

        crate::IndexedRendering {
            summary,
            mapped: report,
            diagnostics,
            unplaced,
        }
    }

    /// **Parse a unit's rendering once, and file the facts under the files they stand in.**
    ///
    /// This is the reading the plan calls *the unit reading*, and it is one step further than
    /// [`FileIndexer::index_rendering`]: that one indexes a **single file's** rendering and maps the ranges back
    /// into that file, while this indexes the **whole program's** rendering — the stream
    /// [`crate::TranslationUnit::cook_the_unit`] stitches in include order — and splits what it finds by the file
    /// each declaration was written in.
    ///
    /// # What it buys
    ///
    /// The index holds "what the compiler saw" for **every file in the program**, including headers nobody has
    /// opened. Without it a header's cooked facts exist only if some request happened to name the file
    /// ([`crate::Session::want_cooked_reading`]), so a cross-file answer about `std::string`'s members depends on
    /// whether a reader had looked at `<xstring>` — a difference no answer should have.
    ///
    /// # The mapping, and what is dropped
    ///
    /// A rendering's offsets are not file offsets, so every range goes back through
    /// [`crate::RenderedUnit::written_span`], which answers with the place a reader can act on: a token the file
    /// wrote keeps its own range, and one that came out of a macro body answers with the **outermost call site**.
    /// A fact whose **name** and whose **range** come out in different files is dropped and counted — half in one
    /// file and half in another is not "a bit less", it is a fact about nothing (the same rule
    /// [`crate::FileSummary::map_into_the_file`] applies per file).
    ///
    /// # Fences, and what changed about them
    ///
    /// One parse of the whole program lets one file's mistake become every later file's: a scope the parser opens
    /// in one file and does not close there stays open, and everything spliced after it is *inside*. The measured
    /// case is `CodeAnalysis/sourceannotations.h`, whose `REPEATABLE [attribute] struct X { … };` the parser reads
    /// as an expression with a lambda body and never closes — so the rest of the program, `<string>` included,
    /// became the body of a lambda, or (before this) sat in `vc_attributes::` for want of a `}`.
    ///
    /// So each parse is checked for a brace paired with a brace of a **different** file (a scope that opens and
    /// closes in one file — even around an `#include` — is not one), and the check is *repaired* rather than
    /// *punished*:
    ///
    /// * the pairing is **neutralised in place** ([`crate::RenderedUnit::neutralized`]): the two brace tokens are
    ///   replaced by markers that pair with nothing, and every other byte of every file stays where it was. The
    ///   result is the same length as the original, so the original span table still answers for every range and a
    ///   fact found in the repaired text is filed under the file that wrote it — which is what makes one parse
    ///   enough;
    /// * the file whose brace had to be given up is parsed **on its own as well** ([`crate::RenderedUnit::only`]),
    ///   so what it declares is filed with whatever mistakes its own parse makes — and those mistakes end at the
    ///   end of the file;
    /// * the repair is **counted** ([`crate::IndexedUnit::repaired`]) and named ([`crate::IndexedUnit::quarantined`]),
    ///   so the cost is visible. It is not a reason to file nothing.
    ///
    /// # What this replaces, and the measurement that asked for it
    ///
    /// The version before this one **removed the leaking file's tokens from the program** and refused to file the
    /// reading at all if a crossing survived three rounds. Both halves of that are the failure mode the plan's §4
    /// names: *"no layer may give up an answer it already has because it is unsure about something else."* A file
    /// that leaked one brace lost every declaration in it, and a program that still crossed after the removal lost
    /// **all of them** — which is how `std::format` had no members while the file that declares them was in the
    /// program, and why `members_of(std::string)` could answer `NotDeclaredHere` about a header that was read.
    ///
    /// Supplying the missing `}` in the leaking file's place was tried before that and does not hold — the parser
    /// is as likely to spend a supplied closer on something inside the leak. That is an argument about *adding* a
    /// token; this is the other direction, and it is sound for a reason the other one is not: a brace that the
    /// parser paired across two files is a brace whose pairing is already wrong, so removing **that** pairing takes
    /// nothing away that was right.
    ///
    /// # The other gate, and why this one had to be repaired first
    ///
    /// There were **two** all-or-nothing gates, and this function is the second. The first was one layer down, in
    /// the cook ([`crate::RenderedUnit::unbalanced`]): a file whose own text does not balance its braces had its
    /// tokens **left out of the stream** before any parse happened, so the imbalance never reached here to be
    /// repaired. That gate is now gone as well — the text goes in and is named — and the consequence is worth being
    /// explicit about, because it makes this function **load-bearing rather than an improvement**:
    ///
    /// ```text
    /// before:  the unbalanced file's tokens were dropped, so the stream balanced by construction
    /// now:     the stream carries the imbalance, and only the repair below keeps the file after it
    ///          out of the leaked scope
    /// ```
    ///
    /// So a change that made `brace_crossings` miss an unclosed-scope crossing would be a regression that no
    /// longer hides behind the cook. `tests/translation_unit.rs`'s
    /// `a_file_that_does_not_balance_still_contributes_its_tokens` pins the cook's half (the stream is left
    /// unbalanced on purpose) and the session's
    /// `a_file_that_does_not_balance_its_braces_is_named_and_still_read` pins this half (the file after it is still
    /// scoped by its own namespace, *and* the unbalanced file kept its declarations).
    pub fn index_unit_rendering(
        &self,
        root: &Path,
        stream: &crate::RenderedUnit,
        key: SummaryKey,
    ) -> crate::IndexedUnit {
        // Built afresh for each parse: a configuration is consumed by the parser it is given to.
        //
        // **No macro environment** — see the note in [`FileIndexer::index`], and note that this is the one parse
        // where it would have been most tempting and most wrong: `stream.text` is a *concatenation of every file's
        // rendering*, so an offset into it is a coordinate no file in the unit has.
        let config = || ParserConfig::default().with_dialect(self.config.dialect());

        // **One parse, of the stream the caller cooked.**
        //
        // There used to be a fence here: the stream was checked for a brace the parse paired across two files, the
        // pair was neutralised, and the files involved were read on their own as well. It is gone, and the reason is
        // the plan's §3.0 — *"真正做预处理之后,解析器看不到 `MacroCall`"*.
        //
        // A crossing needs the parser to pair a brace in one file with a brace in another, and the parser can only do
        // that if the text it is given has braces whose file it cannot tell. Unparsed macro invocations were where
        // they came from: `_STD_BEGIN … _STD_END`, `extern "C" { … }` inside a body, the whole family of
        // open-a-scope-in-a-header macros. **This stream has none of them** — [`crate::RenderedUnit`] is the cooked
        // text, so every macro is already replaced by what it expands to, and `{`/`}` in it are the braces the
        // program has. The processor no longer sees a macro call, so it no longer pairs a brace across a file for a
        // reason that is not in the program.
        //
        // # What is kept, and what it costs to keep
        //
        // [`crate::RenderedUnit::unbalanced`] is still recorded: a file whose *own* text does not balance its braces
        // is a fact about the input, and naming it is the plan's rule 3. What is gone is the **repair**, and with it
        // the two counters the repair needed (`repaired`, `uncured`) and the per-file second parse. A file is read
        // once, by the one parse that reads the program.
        //
        // # If a crossing is seen again
        //
        // It would mean a brace pair really is being made across two files by a stream that has no macro invocations
        // in it — a genuine parser defect, and one that should be fixed **in the parser** rather than absorbed here.
        // The place to look first is a file whose text does not balance: `RenderedUnit::unbalanced` names them, and
        // that list is the honest starting point. Absorbing it a second time would be exactly the "越走越远的
        // workaround" this milestone exists to delete.
        let tree = {
            let _parse = StageTimer::new(Stage::RenderParse);
            // **And where one file's text ends and the next begins**, which is the parser's half of "a scope may not
            // cross a file boundary". `stream` has known it all along — [`crate::RenderedUnit::file_boundaries`] is
            // the first token of each file after the first — and nothing passed it on until now, which is why a
            // header that opens a `namespace` and never closes it held every file spliced after it. The note above
            // is the same rule from the other side: the repair this replaces was deleted on purpose, and the parser
            // is where it belongs.
            CppParser::parse(
                &stream.text,
                config().with_file_boundaries(stream.file_boundaries()),
            )
        };

        let summary = {
            let _sweep = StageTimer::new(Stage::RenderSweep);
            self.index_tree(root, &stream.text, &tree, key)
        };

        let mut files: Vec<(std::path::PathBuf, crate::CookedFile)> = stream
            .files
            .iter()
            .map(|path| (path.clone(), crate::CookedFile::default()))
            .collect();
        let mut unplaced = 0usize;
        let errors = tree.get_errors().len();

        file_what_was_found(
            summary.declarations,
            &tree,
            stream,
            &mut files,
            &mut unplaced,
        );

        crate::IndexedUnit {
            files,
            unplaced,
            tokens: stream.len(),
            files_with_tokens: stream.files_with_tokens(),
            missing: stream.missing,
            unbalanced: stream.unbalanced.clone(),
            braces: stream.braces,
            errors,
        }
    }

    /// [`FileIndexer::index`] for a caller that already has the tree.
    ///
    /// Parsing twice is the most expensive thing this layer can be asked to do, and an editor usually has the tree
    /// already — it is what it is displaying.
    pub fn index_tree(
        &self,
        path: &Path,
        source: &str,
        tree: &CppSyntaxTree,
        key: SummaryKey,
    ) -> FileSummary {
        // See `index`: the content part of the key is the text, always. The caller supplies the part that
        // describes the *compilation* — the configuration and the directory the file sits in.
        //
        // Not timed here: the caller that asked for the build has timed this same hash already
        // (`SummaryStore::get` does it to look the entry up), and timing it twice would count it twice.
        let key = SummaryKey::new(content_hash(source), key.context_hash);

        let root = tree.get_red_root();
        let preprocessing = {
            let _scan = StageTimer::new(Stage::Scan);
            // **With what the compilation already knows**, so a `#define` in a branch nobody compiles is not
            // recorded: `vcruntime.h` writes `_STL_LANG` once per branch of `#ifdef __cplusplus`, the last of them
            // `0L`, and a table that keeps all three answers `0L` for a file compiled as C++20. See
            // [`preprocess_with`], which is where the rule and its cost are stated.
            match self.seed {
                Some(seed) => {
                    let started = crate::preprocess::preprocess_with_a_seed(source, tree.get_tokens(), seed);
                    started
                }
                None => {
                    crate::preprocess::preprocess(source, tree.get_tokens())
                }
            }
        };
        // The same evidence object for both readers — see the field's note. The cast is the point: the scope walk
        // asks the trait, the parse asked the concrete type, and there is one value behind both.
        let evidence: &dyn cpp_parser::MacroBodies = match self.bodies {
            Some(bodies) => bodies,
            None => &crate::sema::scopes::NoMacroBodies,
        };
        let scopes = {
            let _scopes = StageTimer::new(Stage::Scopes);
            build_scopes(&root, evidence)
        };
        // The diagnostics, as ranges, for the one field a fact takes from them rather than from the tree — see
        // [`DeclFact::clean`]. Collected once for the whole file: the parser reports a handful per file, and
        // asking per declaration would be a scan of the list per fact.
        let errors: Vec<cpp_parser::SourceRange> = tree
            .get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range))
            .collect();

        let (mut declarations, mut guards) = {
            let _facts = StageTimer::new(Stage::Facts);
            build_facts(&scopes, &preprocessing, &root, &errors)
        };

        let mut interner = PathInterner::new(cfg!(windows));
        let resolver = IncludeResolver::new(self.files, self.config);
        let directory = path.parent().unwrap_or(Path::new("."));

        // The includes are their own stage because **resolving one is a filesystem search**, per file, per pass —
        // the one step here that is not proportional to the text but to the search path. Measured on the 138-file
        // project, this is where most of the sweep goes.
        let (mut macros, mut includes) = {
            let _includes = StageTimer::new(Stage::Includes);

            let macros: Vec<MacroFact> = preprocessing
                .directives
                .iter()
                .filter_map(macro_fact)
                .collect();

            // The interner is local: resolving an include mints an id as a side effect, and the id is discarded
            // because a summary stores the *path*. A `FileId` is an index into a run's interner and means nothing to
            // whoever reads the summary back.
            let includes: Vec<IncludeFact> = preprocessing
                .directives
                .iter()
                .filter_map(|spanned| {
                    include_fact(
                        &spanned.directive,
                        spanned.range,
                        directory,
                        &resolver,
                        &mut interner,
                        self.scanned,
                    )
                })
                .collect();

            (macros, includes)
        };

        // Every kind of fact carries a guard, and they are assigned together rather than per kind: the sweep is
        // over *offsets*, and running it once per fact list would rebuild the region list each time. An include's
        // guard is the one that earns its keep — it is what a cross-file lookup reads to decide whether a
        // declaration reached through that include is unconditionally in scope — but a `#define` inside an `#if`
        // is the same question about a macro.
        macros.sort_by_key(|fact| fact.range.start_offset);
        includes.sort_by_key(|fact| fact.range.start_offset);

        let mut guarded: Vec<(&mut FactGuard, usize)> = macros
            .iter_mut()
            .map(|fact| (&mut fact.guard, fact.range.start_offset))
            .chain(
                includes
                    .iter_mut()
                    .map(|fact| (&mut fact.guard, fact.range.start_offset)),
            )
            .collect();
        guarded.sort_by_key(|(_, at)| *at);

        let own_guard = {
            let _guards = StageTimer::new(Stage::Guards);

            assign_guards(&mut guarded, &preprocessing, &mut guards);

            // The file's **own include guard is not a condition**, and this is the step that makes the standard
            // library queryable at all: a header puts its whole body inside `#ifndef _GLIBCXX_STRING`, so without this
            // rule every `#include` written inside a header is "conditional" and every declaration reached through one
            // is `ConditionalCompilation` — measured on the closure of `<string>`, that is *every* cross-file answer
            // there is. See `deguard_the_files_own_guard` for why calling it unconditional is the honest reading.
            let own_guard = own_guard_region(&preprocessing, &root);

            // Stored, not just used here: a *walk* evaluating a condition needs the same rule, and it has only the
            // summary. See `SummaryGuards::own_guard` — a file's own guard is not a condition on anything.
            guards.own_guard = own_guard.map(|region| region as u32);

            // **And whether the file asks to be entered once by pragma**, which is the same scan `detect_guard`
            // already did — kept because the renderer needs to emit the line and has no directives to look at.
            guards.visit_once = preprocessing.directives.iter().any(|spanned| {
                matches!(
                    &spanned.directive,
                    crate::directive::Directive::Pragma { tokens }
                        if tokens.first().is_some_and(|token| token.text() == "once")
                )
            });

            if let Some(region) = own_guard {
                let mut all: Vec<&mut FactGuard> = declarations
                    .iter_mut()
                    .map(|fact| &mut fact.guard)
                    .chain(macros.iter_mut().map(|fact| &mut fact.guard))
                    .chain(includes.iter_mut().map(|fact| &mut fact.guard))
                    .collect();
                deguard_the_files_own_guard(&mut all, region);
            }

            own_guard
        };

        // What the guards above cannot say on their own: whether a `#define` inside an `#if` still *settles* the
        // name whichever branch is taken — the `#ifndef NAME / #define NAME` idiom, which is how the system headers
        // define most of the macros a project uses. It runs after the de-guard step because a fact already
        // `Unconditional` has nothing to settle, and it is told which region the own guard is because the rule it
        // applies nests inside conditionals and must agree with that step about what a file guard means.
        {
            let _settling = StageTimer::new(Stage::Settling);
            mark_settling_macro_facts(&preprocessing, own_guard, &mut macros);
        }

        // **Header units are resolved in the same stage as includes**, because they are the same filesystem search:
        // `import <vector>;` names a header, and the answer to "which vector" has to be the compiler's answer. See
        // [`modules_of`].
        let modules = {
            let _includes = StageTimer::new(Stage::Includes);
            modules_of(&root, directory, &resolver, &mut interner)
        };

        FileSummary {
            path: path.to_path_buf(),
            key,
            declarations,
            macros,
            includes,
            guards,
            // Where a scope came out of a macro's replacement list rather than out of this file's braces. Taken
            // from the walk rather than recomputed: the walk is the only thing that asked, and asking again here
            // would be a second reader of the same evidence, free to disagree with the scopes it is describing.
            macro_readings: scopes.macro_readings,
            modules,
        }
    }
}

/// **What a file declares about modules**, read from its tree.
///
/// The same reading [`crate::ModuleScanner`] makes ([`crate::ModuleInfo::from_tree`]), reduced to the three things
/// the *visibility* walk needs and stored in the summary so that it can be read without a tree — see
/// [`crate::ModuleReading`].
///
/// All three resolved-file lists are here for the same reason: **the walk holds summaries and can search nothing**,
/// so a file the walk has to reach has to have been found already. What is *not* here is the edge's direction or its
/// `export`: an `export import` re-exports and a plain `import` does not, and this model records neither — the
/// direction of that omission is stated on [`crate::ModuleReading`].
fn modules_of<F: FileProvider>(
    root: &cpp_parser::CppSyntaxNode,
    including: &Path,
    resolver: &IncludeResolver<'_, F>,
    interner: &mut PathInterner,
) -> crate::ModuleReading {
    let info = crate::ModuleInfo::from_tree(root);

    // **A header unit is a header**, and `import <vector>;` finds it by the same search `#include <vector>` uses —
    // so the same resolver is asked, and a second implementation of "which `vector` did you mean" does not exist to
    // disagree with the compiler. The *resolved path* is what is recorded rather than the spelling: the visibility
    // walk holds summaries and cannot search anything.
    //
    // `None` for a header that cannot be found (no include paths configured, a header outside them) — the walk
    // then knows nothing about that import, which is the honest state and not an empty header unit.
    let header_units: Vec<std::path::PathBuf> = info
        .imports
        .iter()
        .filter_map(|declaration| match &declaration.target {
            crate::ImportTarget::HeaderUnit { name, is_angle } => {
                let include = crate::directive::Include {
                    target: name.to_string().into(),
                    form: if *is_angle {
                        crate::directive::IncludeForm::Angle
                    } else {
                        crate::directive::IncludeForm::Quote
                    },
                    is_next: false,
                };

                resolver
                    .resolve(&include, including, None, interner)
                    .resolved()
                    .map(|resolved| resolved.path.clone())
            }
            _ => None,
        })
        .collect();

    // **A partition of this file's own module**, which is not a module and is not found by an include search:
    // `export import :area;` in `shapes.cppm` means "the `area` partition of `shapes`", and the file that declares
    // it is found by the module naming convention — the same resolution `scan_imports` uses, through the same
    // [`crate::ModuleScanner`], so that "which file is `shapes:area`" has one answer in this crate.
    //
    // It has to be resolved here rather than left to the visibility walk for the reason the walk's own note gives:
    // the walk holds summaries and can search nothing. And it has to be *recorded* because a partition's names are
    // part of the module's interface when the import is an `export import` — measured on the partition fixture,
    // where `rectangle` and `square` answered "nothing declares it" from a file that says `import shapes;` while
    // `perimeter`, declared directly in the interface unit, resolved.
    //
    // A partition whose file cannot be found is **absent**, like an unresolved header unit: that is the honest
    // state — nothing is known about it — rather than an empty partition.
    let partitions: Vec<std::path::PathBuf> = match info.module_name.as_deref() {
        Some(module) if info.has_an_imported_partition() => {
            let mut scanner = crate::ModuleScanner::new(resolver.files(), resolver.config());
            let mut found = Vec::new();

            for declaration in &info.imports {
                let Some(name) = declaration.target.partition_name() else {
                    continue;
                };

                if let crate::ImportOutcome::Resolved(unit) =
                    scanner.resolve_partition(Some(module), name, including, interner)
                    && let Some(entry) = scanner.units().iter().find(|entry| entry.file == unit)
                {
                    found.push(entry.path.clone());
                }
            }

            found
        }
        _ => Vec::new(),
    };

    crate::ModuleReading {
        module: info.module_name,
        partition: info.partition_name,
        // **`is_interface` is `ModuleUnit::is_interface`, not `== InterfaceUnit`.** A partition has two unit kinds
        // and only the interface one is what an `import :part;` reaches — so a reader that asked for the primary
        // interface variant answered `false` for every partition interface unit there is, which is how this was
        // written first. `ModuleUnit` already states the rule (and `exports_to_importers` states the *different*
        // question a module name asks); this is the caller that has to use it.
        is_interface: info.unit.is_some_and(crate::ModuleUnit::is_interface),
        imports: info
            .imports
            .iter()
            .filter_map(|declaration| declaration.target.module_name().map(Box::from))
            .collect(),
        header_units,
        partitions,
    }
}

/// A summary of a file with **no project around it**: every include unresolved, every macro recorded.
///
/// The entry point for a caller that has text and nothing else — a test, or an editor on a file outside any
/// project. Equivalent to a [`FileIndexer`] whose resolver finds nothing, and named separately so that the
/// ordinary case does not have to construct a configuration it is not going to use.
pub fn summarize(path: &Path, source: &str, key: SummaryKey) -> FileSummary {
    /// A provider that has no files, so every include comes out unresolved.
    struct NoFiles;

    impl FileProvider for NoFiles {
        fn read(&self, _path: &Path) -> Option<String> {
            None
        }

        fn exists(&self, _path: &Path) -> bool {
            false
        }
    }

    FileIndexer::new(&NoFiles, &CompilerConfig::default()).index(path, source, key)
}

/// The [`MacroFact`] for a `#define` or an `#undef`, or `None` for every other directive.
///
/// Both are facts about the same name's history, and a query needs both to be answerable: a name `#undef`ed above
/// the cursor is not a macro, and a table that only remembers definitions would point at a `#define` that is no
/// longer in force. See [`crate::MacroKind`].
///
/// `spanned` rather than the bare directive because an `#undef`'s *name* position is not tracked by the directive
/// reader — it keeps the name and no range — so the fact carries the directive's range, which is the line to show
/// a user asking where a macro stops being one.
fn macro_fact(spanned: &SpannedDirective) -> Option<MacroFact> {
    match &spanned.directive {
        Directive::Define(define) => {
            let definition = define.macro_def.as_ref()?;

            Some(MacroFact {
                name: definition.name.to_string(),
                kind: crate::summary::MacroKind::Definition,
                function_like: definition.is_function_like(),
                body: macro_body_shape(definition),
                value: macro_value(definition),
                alias: macro_alias(definition),
                // The *name's* range, not the directive's: "go to macro definition" is a jump to the name a user
                // can see, and a rename edits it. The guard sweep reads this fact's offset too, and the name is
                // inside the same conditional region as the directive that wrote it — a `#define` cannot span an
                // `#endif`.
                range: definition.name_range,
                guard: crate::summary::FactGuard::Unconditional,
                // **Where the body is**, derived from the body's own tokens rather than searched for in the
                // directive's text: `MacroDef::body.tokens` each carry their range, so the span from the first
                // to the last *is* the replacement list. `None` for an empty body (`#define FOO`), which is the
                // ordinary feature flag and has nothing to expand.
                body_range: body_range_of(definition),
                // Filled in by `mark_settling_macro_facts`, which is the only place that knows where the
                // conditionals are; see `MacroFact::settles_the_name`.
                settles_the_name: false,
            })
        }
        Directive::Undef { name: Some(name) } => Some(MacroFact {
            name: name.to_string(),
            kind: crate::summary::MacroKind::Undefinition,
            function_like: false,
            // Nothing to say about a body, and saying `Unknown` is the honest way to say it.
            body: cpp_parser::MacroBody::Unknown,
            // An `#undef` has no value: the name stops being a macro, which is the whole of what it says.
            value: None,
            alias: None,
            range: spanned.range,
            body_range: None,
            guard: crate::summary::FactGuard::Unconditional,
            settles_the_name: false,
        }),
        _ => None,
    }
}

/// The value a condition could read out of a definition, or `None`.
///
/// One integer literal in the body, and nothing else — which is exactly what
/// [`crate::condition::macro_value`] accepts, so the two agree by construction rather than by convention. A
/// function-like macro is not a value at all (`#if F(x)` is not how a macro is asked about), and neither is a body
/// of two tokens, a body that is a name, or an empty body: a condition asking about any of them is `Unknown`, and
/// storing a spelling that no evaluator can use would only make the cache bigger.
fn macro_value(definition: &crate::preprocess::macros::MacroDef) -> Option<Box<str>> {
    if definition.is_function_like() {
        return None;
    }

    let mut significant = definition.body.significant();
    let only = significant.next()?;

    if significant.next().is_some() {
        return None;
    }

    (only.kind == cpp_parser::CppTokenKind::IntegerLiteral).then(|| only.text.clone())
}

/// **The one name a macro's body *is*, when the body is exactly one identifier** — `#define A B`.
///
/// Computed here, where the replacement list is in hand, because a `MacroSummary` cannot answer it later: what it
/// stores of a body is a [`cpp_parser::MacroBody`] (a *shape*, not tokens) and a range into the file the `#define`
/// was written in. The name is the whole of what following the alias needs, and it is one token to read now.
///
/// Why it matters: a condition read against an index answers `#if _STL_LANG > 201402L` only if it learns that
/// `_STL_LANG` is `__cplusplus`, which is itself a macro with a value. `vcruntime.h` writes exactly that, and with
/// the alias unreadable `_HAS_CXX17` never became defined — so every `#if _HAS_CXX17` below it, including the one
/// around `<atomic>` in `<memory>`, was undecidable.
fn macro_alias(definition: &crate::preprocess::macros::MacroDef) -> Option<Box<str>> {
    // `#define A(x) x` used without arguments is a name, not `x`: only an object-like macro aliases another.
    if definition.is_function_like() {
        return None;
    }

    let mut significant = definition.body.significant();
    let only = significant.next()?;

    if significant.next().is_some() {
        return None;
    }

    (only.kind == cpp_parser::CppTokenKind::Identifier).then(|| Box::from(only.text()))
}

/// The region a file's own include guard opens, when it has one.
///
/// `Some(0)` or `None`, and the zero is not a coincidence: [`crate::detect_guard`] only recognises a guard that is
/// the file's **first** conditional, at depth 0, with no declaration before it — so when there is a guard, the
/// first region in the list is the one it opens. `#pragma once` is a guard with no region at all, which is why
/// the answer is an index rather than a boolean.
fn own_guard_region(
    preprocessing: &crate::FilePreprocessing,
    root: &cpp_parser::CppSyntaxNode,
) -> Option<usize> {
    match crate::guards::detect_guard(&preprocessing.directives, root) {
        // The macro form: region 0 **is** the file's guard.
        crate::guards::Guard::Macro(_) => Some(0),
        // `#pragma once` alone opens no region, so there is nothing to de-guard. But it is usually written
        // **above** the `#ifndef` guard — every MSVC header starts `#pragma once` then `#ifndef _STRING_` —
        // and `detect_guard` reports the pragma first, which lost the macro guard entirely. The cost was
        // measured and it is the whole library: without `own_guard`, the walk evaluated `#ifndef _STRING_` at
        // `<string>`'s own `#include` (where `_STRING_` is defined by the line above), answered `Inactive`, and
        // **dropped every include of the file** — so `_STL_COMPILER_PREPROCESSOR` was never defined, region 1
        // (`#if _STL_COMPILER_PREPROCESSOR`) never held, and every query in MSVC's STL answered
        // `ConditionalCompilation`.
        crate::guards::Guard::PragmaOnce => {
            crate::guards::has_a_macro_guard(&preprocessing.directives, root).then_some(0)
        }
        crate::guards::Guard::None => None,
    }
}

/// Treat everything inside a file's own include guard as **unconditional**.
///
/// # Why a guard is not a condition
///
/// A `#if` makes a fact conditional because whether the branch was taken depends on macros this analysis does not
/// have — the fact might be invisible, so a consumer is told `Unknown` rather than `Yes`. A file's **own** guard is
/// the one `#if` where that reasoning does not hold: the condition is `#ifndef _GLIBCXX_STRING`, and *entering the
/// file at all* is what defines `_GLIBCXX_STRING`. Every inclusion of a guarded header that does anything takes
/// the branch; the visits that do not are the ones where the header has already been read, and the declarations
/// are visible from those too.
///
/// So a declaration inside the guard is visible to **any** file that includes the header, which is exactly what
/// `Unconditional` means here, and what it does not mean is "there was no `#if` in the text" — the region is
/// still in [`crate::SummaryGuards::regions`], and the guard macro is still a fact of the file.
///
/// # What it costs, measured
///
/// Without it, the closure of `<string>` answers **every** cross-file query with `ConditionalCompilation`: the
/// standard library's headers guard their bodies, so every `#include` inside one is inside a region.
/// `examples/std_query.rs` is the probe that shows it — 0 of 7 ordinary queries resolved before this rule.
fn deguard_the_files_own_guard(facts: &mut [&mut FactGuard], region: usize) {
    for fact in facts {
        if **fact == FactGuard::Region(region as u32) {
            **fact = FactGuard::Unconditional;
        }
    }
}

/// The shape of a macro's body, in the vocabulary the parser's rules ask about.
///
/// This classifier is the join between the two ends of a macro's life. On one side
/// [`crate::preprocess::macros::MacroBody`] holds the body's **tokens**, because expansion needs them; on the
/// other [`cpp_parser::MacroBody`] is the *shape* the grammar acts on — `Statement` for a macro whose invocation
/// needs no `;`, `Specifier` for one that stands where a declaration specifier goes. Nothing else converts
/// between them, so every macro-shaped reading in the parser ends up guessing; this is the evidence that replaces
/// the guess.
///
/// # How the shape is decided
///
/// By the first and last significant tokens, which is what a reader uses too:
///
/// ```text
/// __declspec(dllexport)      a specifier — call-like, and it is not a statement
/// do { … } while (false)     a statement that brings its own `;`-free block
/// if (x) { … }               a statement
/// ((a) > (b) ? (a) : (b))    an expression
/// int                        a type
/// { … }                      a block, always followed by `{`
/// ```
///
/// The order of the tests is the order the shapes exclude each other: a body that opens with a control keyword or
/// `do` is a statement whatever else is in it, and only a body that is *entirely* a parenthesised expression is an
/// expression — `(a) + (b)` is a sum, and calling a sum an expression is right, while calling `((a) > (b) ? …)` a
/// statement would be wrong.
///
/// `MacroBody::Unknown` is the answer when nothing matches, and it is not a failure: it already rules out the
/// *call* reading, which is most of what a caller gets from knowing a name is a macro.
/// The range of a macro's **replacement list** inside its `#define` — see [`crate::summary::MacroFact::body_range`].
///
/// Derived from the body's own tokens, never searched for: each token carries the range it was lexed at, so the
/// span from the first to the last *is* the body, whatever spelling the directive used around it.
fn body_range_of(definition: &crate::preprocess::macros::MacroDef) -> Option<cpp_parser::SourceRange> {
    let first = definition.body.tokens.first()?;
    let last = definition.body.tokens.last()?;
    let start = first.range.start_offset;
    let end = last.range.start_offset + last.range.length;
    Some(cpp_parser::SourceRange::new(start, end.saturating_sub(start)))
}
fn macro_body_shape(definition: &crate::preprocess::macros::MacroDef) -> cpp_parser::MacroBody {
    use cpp_parser::MacroBody;
    use cpp_parser::CppTokenKind;

    let tokens: Vec<CppTokenKind> = definition
        .body
        .significant()
        .map(|token| token.kind)
        .collect();

    let Some(first) = tokens.first().copied() else {
        // `#define FOO` — a macro that expands to nothing. Used for feature flags, so it is ordinary rather than
        // malformed; with no body there is no shape to report.
        return MacroBody::Unknown;
    };

    // A body that is only a brace is the gtest shape: the invocation is always followed by the block it wrote.
    if first == CppTokenKind::LeftBrace && tokens.last().copied() == Some(CppTokenKind::RightBrace) {
        return MacroBody::Block;
    }

    if matches!(
        first,
        CppTokenKind::IfKeyword
            | CppTokenKind::ForKeyword
            | CppTokenKind::WhileKeyword
            | CppTokenKind::SwitchKeyword
            | CppTokenKind::DoKeyword
            | CppTokenKind::ReturnKeyword
            | CppTokenKind::ThrowKeyword
            | CppTokenKind::TryKeyword
            | CppTokenKind::BreakKeyword
            | CppTokenKind::ContinueKeyword
            | CppTokenKind::GotoKeyword
    ) {
        return MacroBody::Statement;
    }

    // A specifier: an attribute, an export marker, a calling convention. Two spellings reach here and they need
    // separate tests, because one is C++ and the other is not:
    //
    // * the specifier *keywords* — `static`, `extern`, `inline` — which no other construct can begin with;
    // * the compiler's own attribute spelling — `__declspec(dllexport)`, `__attribute__((…))` — which is an
    //   identifier followed by a parenthesised group, and therefore shaped exactly like `MAX(a, b)`.
    if is_specifier_like(tokens[0]) || is_compiler_specifier_macro(&tokens, first_spelling(definition)) {
        return MacroBody::Specifier;
    }

    // A type: the body is one or two type keywords and nothing else — `#define MY_INT int`. A second *word* rules
    // it out, because `int x` is a declaration and not a type.
    if tokens.iter().all(|kind| is_type_word(*kind)) {
        return MacroBody::Type;
    }

    // An expression: everything in the body is an operand, an operator, or a bracket, and the whole body is
    // parenthesised. The parenthesis test is what separates `((a) > (b) ? (a) : (b))` from `(a) + (b)`, which is
    // also an expression — and both answers are `Expression`, so the test is about *confidence*: a body that is
    // one parenthesised group is an expression whatever is inside it.
    if is_one_parenthesised_group(&tokens) {
        return MacroBody::Expression;
    }

    MacroBody::Unknown
}

/// Is this token only ever a declaration specifier?
fn is_specifier_like(kind: cpp_parser::CppTokenKind) -> bool {
    use cpp_parser::CppTokenKind as K;

    matches!(
        kind,
        K::ExternKeyword
            | K::StaticKeyword
            | K::InlineKeyword
            | K::ConstexprKeyword
            | K::VirtualKeyword
            | K::ExplicitKeyword
            | K::MutableKeyword
            | K::ThreadLocalKeyword
            | K::TypedefKeyword
            | K::FriendKeyword
    )
}

/// Is this token a word that can name a type on its own?
fn is_type_word(kind: cpp_parser::CppTokenKind) -> bool {
    use cpp_parser::CppTokenKind as K;

    matches!(
        kind,
        K::VoidKeyword
            | K::BoolLiteral
            | K::CharKeyword
            | K::ShortKeyword
            | K::IntKeyword
            | K::LongKeyword
            | K::FloatKeyword
            | K::DoubleKeyword
            | K::SignedKeyword
            | K::UnsignedKeyword
    )
}

/// Is this macro body a compiler-specific **specifier** spelling?
///
/// `__declspec(dllexport)` and `__attribute__((visibility("default")))` are what an export macro is written as on
/// the two mainstream compilers, and both are *call-shaped*: an identifier followed by a parenthesised group. They
/// are not C++ at all, so no rule of the grammar could recognise them — but a macro whose body is one of them
/// stands exactly where a declaration specifier does, and that is the fact the parser needs from this classifier.
///
/// Recognised **by name** rather than by shape alone, because the shape is shared with every other function-like
/// macro: `MAX(a, b)` is also an identifier and a parenthesised group, and calling that a specifier would make
/// `MAX(1, 2)` a declaration. The names are the compilers', so the list does not grow with user code.
fn is_compiler_specifier_macro(tokens: &[cpp_parser::CppTokenKind], spelling: &str) -> bool {
    use cpp_parser::CppTokenKind as K;

    if tokens.first().copied() != Some(K::Identifier)
        || tokens.get(1).copied() != Some(K::LeftParen)
        || tokens.last().copied() != Some(K::RightParen)
    {
        return false;
    }

    matches!(
        spelling,
        "__declspec" | "__attribute__" | "__attribute" | "__cdecl" | "__stdcall" | "__fastcall"
    )
}

/// The spelling of a macro body's first significant token, for the tests that have to read it by name.
fn first_spelling(definition: &crate::preprocess::macros::MacroDef) -> &str {
    definition
        .body
        .significant()
        .next()
        .map(|token| token.text.as_ref())
        .unwrap_or("")
}

/// Is the whole body wrapped in one pair of parentheses?
fn is_one_parenthesised_group(tokens: &[cpp_parser::CppTokenKind]) -> bool {
    use cpp_parser::CppTokenKind as K;

    if tokens.first().copied() != Some(K::LeftParen) || tokens.last().copied() != Some(K::RightParen) {
        return false;
    }

    // The group has to *close* at the last token, not earlier: `(a) + (b)` also starts with `(` and ends with
    // `)`, and it is not one group.
    let mut depth = 0isize;
    for (index, kind) in tokens.iter().enumerate() {
        match kind {
            K::LeftParen => depth += 1,
            K::RightParen => {
                depth -= 1;
                if depth == 0 {
                    return index == tokens.len() - 1;
                }
            }
            _ => {}
        }
    }

    false
}

/// File the declarations and errors one parse found under the files they were written in.
///
/// Every range goes back through [`crate::RenderedUnit::written_span`], which answers with the file the tokens
/// **stand in** and a range in *that* file.
///
/// # Two things are counted as unplaced, and the second one used to be silent
///
/// A range that maps nowhere, and a range whose answer does not land in the file the answer names — the check is
/// [`crate::RenderedUnit::span_lands_in`], and it is the one thing that would have caught
/// `basic_string`'s 93 971-byte class body being filed under a 1.2 KB header. Both halves of a fact are checked
/// (`range` and `name_range`) and they must agree about the file: half in one file and half in another is not "a
/// bit less", it is a fact about nothing (the same rule [`crate::FileSummary::map_into_the_file`] applies per file).
///
/// An error that cannot be placed in any file is counted rather than reported against a text the reader cannot see.
///
/// # Why there is no `skip` set any more
///
/// There used to be one: a file whose brace the fence had given up was filed from a **second parse of its own text**
/// and not from the program's, because the program's had read a text that was no longer the file's. With the fence
/// gone there is one parse of one stream, and every fact from it belongs to the file the fact was written in.
fn file_what_was_found(
    declarations: Vec<crate::DeclFact>,
    tree: &CppSyntaxTree,
    stream: &crate::RenderedUnit,
    files: &mut [(std::path::PathBuf, crate::CookedFile)],
    unplaced: &mut usize,
) {
    let _files = StageTimer::new(Stage::UnitFiles);
    for mut fact in declarations {
        let Some((file, range)) = stream.written_span(fact.range) else {
            *unplaced += 1;
            continue;
        };
        let Some((name_file, name_range)) = stream.written_span(fact.name_range) else {
            *unplaced += 1;
            continue;
        };
        if name_file != file {
            *unplaced += 1;
            continue;
        }
        if !stream.span_lands_in(file, range) || !stream.span_lands_in(name_file, name_range) {
            *unplaced += 1;
            continue;
        }
        fact.range = range;
        fact.name_range = name_range;
        if let Some((_, cooked)) = files.get_mut(file as usize) {
            cooked.declarations.push(fact);
        }
    }

    for error in tree.get_errors() {
        let range = cpp_parser::source_range(error.range);
        match stream.written_span(range) {
            Some((file, range)) => {
                if let Some((_, cooked)) = files.get_mut(file as usize) {
                    cooked.diagnostics.push(crate::CookedDiagnostic {
                        range,
                        message: error.message.clone(),
                    });
                }
            }
            None => *unplaced += 1,
        }
    }
}


/// Give `#include "local.h"` the token the parser gives it: a header name.
///
/// The parser folds the name after `#include` into one `HeaderName` token while it parses, because only the parser
/// knows a header name is expected there; the lexer alone leaves a quoted one as a string literal, and the directive
/// reader — which reads `HeaderName` — would call the line an unreadable directive. A scan that runs before the parse
/// has to do the same relabelling itself, by the same rule the parser uses (no escapes: a header name has none). The
/// angle form needs nothing: the directive reader already reassembles `<a/b.h>` from the tokens between the brackets.
fn fold_quoted_header_names(source: &str, tokens: &mut [cpp_parser::CppTokenData]) {
    use cpp_parser::CppTokenKind;

    let next_significant = |tokens: &[cpp_parser::CppTokenData], from: usize| {
        (from..tokens.len()).find(|&at| !cpp_parser::is_trivia(tokens[at].kind))
    };

    for at in 0..tokens.len() {
        if tokens[at].kind != CppTokenKind::Hash {
            continue;
        }
        let Some(name) = next_significant(tokens, at + 1) else {
            continue;
        };
        let word = &source[tokens[name].range.start_offset..tokens[name].range.end_offset()];
        if word != "include" && word != "include_next" {
            continue;
        }
        let Some(target) = next_significant(tokens, name + 1) else {
            continue;
        };
        if tokens[target].kind != CppTokenKind::StringLiteral {
            continue;
        }
        let text = &source[tokens[target].range.start_offset..tokens[target].range.end_offset()];
        if !text.contains('\\') {
            tokens[target].kind = CppTokenKind::HeaderName;
        }
    }
}

/// The [`IncludeFact`] for an `#include`, or `None` for every other directive.
///
/// A resolution failure is not an error here: an include that was not found is stored with its spelling and no
/// path, which is what a consumer needs to report "cannot find `widget.h`" rather than to hide it.
fn include_fact<F: FileProvider>(
    directive: &Directive,
    range: cpp_parser::SourceRange,
    including: &Path,
    resolver: &IncludeResolver<'_, F>,
    interner: &mut PathInterner,
    scanned: Option<&ScannedIncludes>,
) -> Option<IncludeFact> {
    let Directive::Include(include) = directive else {
        return None;
    };

    // A search of the include path is a filesystem search: a scan that already made it is believed, but only for
    // the very same directive.
    let resolved: Option<PathBuf> = match scanned.and_then(|scanned| scanned.resolved(range.start_offset, include)) {
        Some(resolved) => resolved,
        None => resolver
            .resolve(include, including, None, interner)
            .resolved()
            .map(|resolved| resolved.path.clone()),
    };

    Some(IncludeFact {
        form: include.form,
        spelling: include.target.to_string(),
        resolved,
        is_next: include.is_next,
        range,
        guard: crate::summary::FactGuard::Unconditional,
    })
}

/// Put a summary on disk, under the path its key names.
///
/// The write is a `.tmp` file followed by a rename, so a crash mid-write leaves the old summary rather than half
/// of a new one — and a half-written summary is worse than none, because a *wrong* entry answers queries that a
/// missing one would have sent to a rebuild.
///
/// `cache_directory` is the directory the summaries live in (`<root>/.cppls` unless a project renamed it); the key
/// names the file inside it, and nothing else about the project's layout is involved.
pub fn write_summary(summary: &FileSummary, cache_directory: &Path) -> std::io::Result<PathBuf> {
    let path = summary.key.path_under(cache_directory);
    let bytes = crate::summary_codec::encode(summary);

    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }

    // Unique per write: two files with the same text share a key, and workers writing them at once must not share a
    // temporary — one would rename the other's half-written bytes into place.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temporary = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&temporary, &bytes)?;
    std::fs::rename(&temporary, &path)?;

    Ok(path)
}

/// Read a summary from disk, or say why it could not be used.
///
/// Every failure is the caller's cue to rebuild: a missing file, a corrupt one, and one written by a different
/// format version are the same decision, and the distinctions are kept because only the last two are worth
/// logging.
pub fn read_summary(path: &Path) -> Result<FileSummary, SummaryReadError> {
    let bytes = std::fs::read(path).map_err(SummaryReadError::Io)?;
    crate::summary_codec::decode(&bytes).map_err(SummaryReadError::Decode)
}

/// Why a stored summary could not be read.
#[derive(Debug)]
pub enum SummaryReadError {
    /// The file could not be read at all.
    Io(std::io::Error),
    /// The bytes are not a summary this build can use.
    Decode(DecodeError),
}

impl std::fmt::Display for SummaryReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SummaryReadError::Io(error) => write!(f, "cannot read the summary: {error}"),
            SummaryReadError::Decode(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SummaryReadError {}

#[cfg(test)]
mod tests {
    use super::{FileIndexer, include_fact, macro_body_shape, summarize, write_summary};
    use crate::cache::SummaryKey;
    use crate::include::config::CompilerConfig;
    use crate::file::paths::{FileProvider, MemoryFiles, PathInterner};
    use crate::include::IncludeResolver;
    use crate::preprocess::directive::{Directive, IncludeForm};
    use crate::preprocess::macros::MacroTable;
    use crate::summary::{DeclKind, FactGuard, MacroKind};
    use cpp_parser::{CppParser, MacroBody, ParserConfig};
    use std::path::{Path, PathBuf};

    fn key() -> SummaryKey {
        SummaryKey::new(0, 0)
    }

    fn summary(source: &str) -> crate::summary::FileSummary {
        summarize(Path::new("/p/widget.cpp"), source, key())
    }

    #[test]
    fn a_summary_is_read_for_the_compiler_it_was_configured_with() {
        // The dialect has to reach the **parser**, not just the key: `int __int128;` declares a variable called
        // `__int128` when the compiler spells `__int128` as an ordinary name (MSVC), and declares **nothing** when
        // it is a type keyword (GNU) — a name cannot be a declarator if it is a type. A configuration that stopped
        // at the cache key would hash two targets apart and then read both the same way, which is the worst of
        // both.
        //
        // The spelling this test used before was `unsigned __int128 x;`, and it stopped distinguishing the two
        // dialects when the specifier sequence learned to let a name join a type that is already there
        //: `__int128 x` is a type and a declarator under *both* dialects now, which
        // is the better reading of both. What the dialect decides is whether the token **can** be a declarator, and
        // that is what this spelling asks.
        let files = MemoryFiles::new();
        let names = |dialect: cpp_parser::Dialect| {
            let config = CompilerConfig::default().with_dialect(dialect);
            let summary = FileIndexer::new(&files, &config).index(
                Path::new("/p/a.cpp"),
                "int __int128;\n",
                key(),
            );
            summary
                .declarations
                .iter()
                .map(|fact| fact.name.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            names(cpp_parser::Dialect::Gnu),
            Vec::<String>::new(),
            "under GNU `__int128` is a type, so `int __int128;` has no declarator"
        );
        assert_eq!(
            names(cpp_parser::Dialect::Msvc),
            vec!["__int128".to_string()],
            "and under MSVC the same text declares a variable called `__int128`"
        );
    }

    /// Every region's branches, as `kind condition body-start..body-end`, for a short assertion.
    ///
    /// The regions of a summary used to be spans and nothing else, and a span of a *condition* is not a
    /// question any layer could answer. This is the shape that replaced it: what each branch asks, and which
    /// text it guards.
    fn region_shapes(source: &str) -> Vec<Vec<String>> {
        let summary = summary(source);

        summary
            .guards
            .conditionals
            .iter()
            .map(|conditional| {
                conditional
                    .branches
                    .iter()
                    .map(|branch| {
                        format!(
                            "{:?} {} {}..{}",
                            branch.kind,
                            branch.condition.as_deref().unwrap_or("-"),
                            branch.body.start_offset,
                            branch.body.end_offset()
                        )
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_summary_stores_what_each_conditional_asks() {
        // The three spellings a condition comes in, and the one that has none. `#ifdef`/`#ifndef` store the
        // *name*, `#if` stores the expression's text — read back with the lexer the evaluator will use — and
        // `#else` stores nothing, which is not the same as storing an empty expression.
        let shapes = region_shapes(
            "#if defined(_WIN32)\n#include <a.h>\n#elif __cplusplus >= 201703L\n#include <b.h>\n#else\n#include <c.h>\n#endif\n",
        );

        assert_eq!(shapes.len(), 1, "one conditional, three branches");
        let branches = &shapes[0];
        assert_eq!(branches.len(), 3);
        assert!(
            // **The source's spelling, not a space between every token.** `defined(_WIN32)` is what the file
            // writes and what a configuration carries; the space-joined form was an artefact of how the text used
            // to be rebuilt, and rebuilding it faithfully is what keeps `__cplusplus >= 201703L` from becoming
            // `__cplusplus > = 201703L` when the `>`-family arrives in pieces. See `conditional_branch`.
            branches[0].starts_with("If defined(_WIN32)"),
            "the expression's tokens, spelled as the source wrote them: {}",
            branches[0]
        );
        assert!(
            branches[1].starts_with("Elif __cplusplus >= 201703L"),
            "a version test is stored as the text a value can be compared against: {}",
            branches[1]
        );
        assert!(
            branches[2].starts_with("Else -"),
            "`#else` asks nothing: {}",
            branches[2]
        );

        // And the bodies are where the reader would put them: each branch's text is between its own directive
        // and the *start of the next one*, which is where the `#endif` is — the directive itself is not body.
        let source = "#ifdef A\n#include <a.h>\n#endif\n";
        let shapes = region_shapes(source);
        assert_eq!(
            shapes[0][0],
            format!(
                "Ifdef A {}..{}",
                source.find("#ifdef").expect("the directive") + "#ifdef A\n".len(),
                source.find("#endif").expect("the #endif")
            ),
            "the `#ifdef` branch guards everything between its directive and the `#endif`"
        );
    }

    #[test]
    fn a_regions_parent_is_the_conditional_it_is_written_inside() {
        // A nested region's condition lies inside its parent's body, and so does the text of a sibling branch —
        // which is why the nesting is stored rather than derived from the spans.
        let nested = summary("#ifdef A\n#ifdef B\nint x;\n#endif\n#endif\n");
        let parents: Vec<Option<u32>> = nested
            .guards
            .conditionals
            .iter()
            .map(|conditional| conditional.parent)
            .collect();
        assert_eq!(parents, [None, Some(0)]);

        // …and an `#elif` does not open a second region: the region *is* the conditional, and its branches are
        // the chain written inside it.
        let chained =
            summary("#ifdef A\nint a;\n#elif defined(B)\nint b;\n#else\nint c;\n#endif\n");
        assert_eq!(chained.guards.conditionals.len(), 1);
        assert_eq!(chained.guards.conditionals[0].branches.len(), 3);
        assert!(chained.guards.conditionals[0].exhaustive());
    }

    #[test]
    fn the_code_a_position_is_in_is_found_by_walking_outwards() {
        // `conditions_at` is what a fact's guard index cannot say on its own: a fact records the *innermost*
        // region, and whether the code is compiled is a question about every region around it.
        let source = "#ifdef A\n#ifdef B\nint x;\n#endif\n#endif\n";
        let summary = summary(source);
        let x = source.find("int x;").expect("the declaration");

        let chain = summary.guards.conditions_at(x);
        assert_eq!(
            chain.iter().map(|at| at.region).collect::<Vec<_>>(),
            [1, 0],
            "innermost first"
        );
        assert!(
            chain
                .iter()
                .all(|at| at.condition_at < x),
            "and each condition is evaluated at its *own* offset, which is above the code: {chain:?}"
        );

        // Code outside every conditional is in no region at all — the common case, and the cheap one.
        let source = "int x;\n#ifdef A\nint y;\n#endif\n";
        assert!(summary
            .guards
            .conditions_at(source.find("int x;").expect("the declaration"))
            .is_empty());
    }

    #[test]
    fn a_position_is_looked_up_in_the_branch_it_sits_in() {
        // The branch in force is what decides whether the code is compiled, and it is a question about the
        // position — `#else` is not "the whole region", it is one branch of it.
        let source = "#ifdef A\nint taken;\n#else\nint not_taken;\n#endif\n";
        let summary = summary(source);
        let region = summary.guards.conditionals.first().expect("one region");

        let first = source.find("int taken").expect("the first branch");
        let second = source.find("int not_taken").expect("the second branch");
        assert_eq!(region.branch_at(first), Some(0));
        assert_eq!(region.branch_at(second), Some(1));
    }

    #[test]
    fn the_chain_a_guard_names_is_the_chain_the_position_is_in() {
        // Two ways to the same answer: from the guard a fact carries (each region names its parent, so this is a
        // walk outwards), and from the position (a search over every region's span). They must agree — the guard
        // is the innermost region the sweep found, and the spans are that same sweep's arithmetic. Where they do
        // not, the file's directives do not balance and the structure is two readings of one broken file; the
        // guard is the one the rest of the index is consistent with.
        for source in [
            "#ifdef A\nint x;\n#endif\n",
            "#ifdef A\n#ifdef B\nint x;\n#endif\n#endif\n",
            "#ifdef A\nint a;\n#elif defined(B)\nint b;\n#else\nint c;\n#endif\n",
            "#ifndef GUARD\n#define GUARD\n#ifdef A\nint x;\n#endif\n#endif\n",
            "int early;\n#if defined(A) && defined(B)\n#ifdef C\nint x;\n#else\nint y;\n#endif\n#endif\n",
        ] {
            let summary = summary(source);

            for (region, conditional) in summary.guards.conditionals.iter().enumerate() {
                for branch in &conditional.branches {
                    // Anywhere inside the branch's body is a position whose guard names this region — unless a
                    // *nested* region is there, which is what the chain has to account for either way.
                    if branch.body.length == 0 {
                        continue;
                    }

                    let at = branch.body.start_offset;
                    assert_eq!(
                        summary.guards.conditions_at(at),
                        summary.guards.conditions_of(region as u32),
                        "{source:?} at {at} (region {region})"
                    );
                }
            }
        }
    }

    #[test]
    fn a_definition_carries_a_value_exactly_when_a_condition_could_read_one() {
        // `#if NAME` expands the name and reads the result as a number, and the evaluator accepts **one integer
        // literal** — so the fact stores a value in exactly that case and nothing else. A name defined to a name,
        // to an expression, to nothing, or as a function-like macro is `Unknown` to a condition, and a spelling no
        // evaluator can use would only make the cache bigger.
        let value_of = |source: &str, name: &str| {
            summary(source)
                .macros
                .iter()
                .find(|fact| fact.name == name)
                .and_then(|fact| fact.value.clone())
        };

        assert_eq!(value_of("#define ABI 1\n", "ABI").as_deref(), Some("1"));
        assert_eq!(
            value_of("#define WIDE 0x10UL\n", "WIDE").as_deref(),
            Some("0x10UL"),
            "the spelling is kept as written: the evaluator is what reads it"
        );
        assert_eq!(value_of("#define NAME other\n", "NAME"), None);
        assert_eq!(value_of("#define EXPR 1 + 2\n", "EXPR"), None);
        assert_eq!(value_of("#define EMPTY\n", "EMPTY"), None);
        assert_eq!(value_of("#define CALL(x) x\n", "CALL"), None);
        assert_eq!(
            value_of("#undef GONE\n", "GONE"),
            None,
            "an `#undef` says the name stops being a macro, which is all it says"
        );
    }

    /// The facts about `name`, with their guards and the settling flag, for a short assertion.
    fn macro_facts(source: &str, name: &str) -> Vec<(crate::summary::MacroKind, FactGuard, bool)> {
        summary(source)
            .macros
            .iter()
            .filter(|fact| fact.name == name)
            .map(|fact| (fact.kind, fact.guard, fact.settles_the_name))
            .collect()
    }

    #[test]
    fn a_define_inside_ifndef_settles_the_name_whatever_the_branch() {
        // The idiom the whole flag exists for, and the shape the system headers define most macros in:
        // `#ifndef NAME / #define NAME`. Taken → defined here; not taken → it was defined already. Either way the
        // name is a macro, so a *use* after this block is a use and not a "maybe".
        //
        // A declaration before the conditional is what keeps it from being the **file's own guard** — which is a
        // separate rule that would make the fact `Unconditional` instead (see the test after this one).
        assert_eq!(
            macro_facts("int early;\n#ifndef NAME\n#define NAME 1\n#endif\n", "NAME"),
            [(MacroKind::Definition, FactGuard::Region(0), true)]
        );
    }

    #[test]
    fn the_files_own_guard_is_not_a_condition_and_neither_is_a_define_inside_it() {
        // `#ifndef WIDGET_H / #define WIDGET_H` wrapping the file is a guard, so its facts are unconditional
        // already — and a `#ifndef NAME` nested inside one inherits that: reaching the file at all is what the
        // guard means, which is the same rule the de-guard step applies to declarations and includes.
        assert_eq!(
            macro_facts(
                "#ifndef WIDGET_H\n#define WIDGET_H\n#ifndef NAME\n#define NAME 1\n#endif\n#endif\n",
                "NAME"
            ),
            [(MacroKind::Definition, FactGuard::Region(1), true)]
        );
        assert_eq!(
            macro_facts("#ifndef WIDGET_H\n#define WIDGET_H\n#endif\n", "WIDGET_H"),
            [(MacroKind::Definition, FactGuard::Unconditional, false)]
        );
    }

    #[test]
    fn a_region_whose_every_branch_agrees_settles_the_name() {
        // The general case behind the idiom: whichever branch ran, it did the same thing to the name. The two
        // definitions may differ in what they define it *as* — that is the question the flag deliberately does not
        // answer, and `macro_definition` therefore still ignores it.
        assert_eq!(
            macro_facts(
                "int early;\n#if defined(A)\n#define NAME 1\n#else\n#define NAME 2\n#endif\n",
                "NAME"
            ),
            [
                (MacroKind::Definition, FactGuard::Region(0), true),
                (MacroKind::Definition, FactGuard::Region(0), true)
            ]
        );

        // `#ifdef NAME / #undef NAME` is the mirror: after it the name is certainly not a macro.
        assert_eq!(
            macro_facts("int early;\n#ifdef NAME\n#undef NAME\n#endif\n", "NAME"),
            [(MacroKind::Undefinition, FactGuard::Region(0), true)]
        );
    }

    #[test]
    fn a_region_that_does_not_settle_the_name_marks_nothing() {
        // The four shapes that must stay false, because a rule that over-claims is worse than no rule at all: a
        // single branch that may not be taken; branches that disagree; a branch whose last word on the name is the
        // opposite of what it started with; and a `#define` in a branch that the condition does not name.
        for source in [
            // No `#else`: "nothing ran" is a possibility, and then the name may not be a macro.
            "int early;\n#if defined(A)\n#define NAME 1\n#endif\n",
            // Branches that disagree about the name.
            "int early;\n#if defined(A)\n#define NAME 1\n#else\n#undef NAME\n#endif\n",
            // The branch's *last* word is `#undef`, so the branch leaves the name undefined.
            "int early;\n#ifndef NAME\n#define NAME 1\n#undef NAME\n#endif\n",
            // The condition is not about this name, so nothing about it is settled.
            "int early;\n#ifndef OTHER\n#define NAME 1\n#endif\n",
            // A name defined in only one of two exhaustive branches: the other one leaves it as it found it.
            "int early;\n#if defined(A)\n#define NAME 1\n#else\n#define OTHER 2\n#endif\n",
        ] {
            let facts = macro_facts(source, "NAME");
            assert!(
                facts.iter().all(|(_, _, settles)| !settles),
                "{source:?} must settle nothing, got {facts:?}"
            );
        }
    }

    #[test]
    fn a_settling_region_inside_a_plain_conditional_settles_nothing() {
        // The chain rule, and the case it exists for: if `A` is false the inner `#ifndef NAME` never runs, so
        // nothing about the name is certain — the outer region does not settle it either.
        assert_eq!(
            macro_facts(
                "int early;\n#if defined(A)\n#ifndef NAME\n#define NAME 1\n#endif\n#endif\n",
                "NAME"
            ),
            [(MacroKind::Definition, FactGuard::Region(1), false)]
        );
    }

    #[test]
    fn a_settling_region_reached_through_an_if_defined_settles_the_name() {
        // The shape the system headers actually use: `#if !defined(NAME)` written out instead of `#ifndef NAME`,
        // and wrapped in parentheses for good measure. Recognised as the same claim, because it *is* the same
        // claim — the condition is about the very name the body writes.
        for condition in ["!defined(NAME)", "!defined NAME", "(!defined(NAME))"] {
            let source = format!("int early;\n#if {condition}\n#define NAME 1\n#endif\n");
            assert_eq!(
                macro_facts(&source, "NAME"),
                [(MacroKind::Definition, FactGuard::Region(0), true)],
                "{condition} should settle the name"
            );
        }
    }

    #[test]
    fn a_file_whose_conditionals_do_not_balance_settles_nothing() {
        // The safety net, and the reason it exists: the rule reads a *nesting*, and the nesting comes from the
        // directives the parser found. A file with syntax errors can lose one — measured, `winnt.h` loses eight
        // `#endif`s and 417 errors is what it costs — and a parent chain that is wrong in the shorter direction
        // would claim a name is settled when an enclosing `#if` says it may not be. So an unbalanced file gets no
        // claims at all, in either direction.
        for source in [
            // A region never closed: the `#ifndef NAME` may be inside something that is not taken.
            "int early;\n#if defined(A)\n#ifndef NAME\n#define NAME 1\n#endif\n",
            // An `#endif` that closes nothing: an opener is missing, and the depth after it is wrong.
            "int early;\n#endif\n#ifndef NAME\n#define NAME 1\n#endif\n",
        ] {
            let facts = macro_facts(source, "NAME");
            assert!(
                facts.iter().all(|(_, _, settles)| !settles),
                "{source:?} must settle nothing, got {facts:?}"
            );
        }
    }

    #[test]
    fn a_file_with_declarations_and_macros_summarizes_both() {        let summary = summary(
            "#define MY_API\n#define MAX(a, b) ((a) > (b) ? (a) : (b))\nstruct Widget { int size; };\n",
        );

        assert_eq!(summary.declarations.len(), 2, "the class and its member");
        assert_eq!(summary.macros.len(), 2, "both macros");

        let api = summary
            .macros
            .iter()
            .find(|fact| fact.name == "MY_API")
            .expect("the object-like macro is a fact");
        assert!(!api.function_like);
        assert_eq!(api.body, MacroBody::Unknown, "an empty body has no shape");

        let max = summary
            .macros
            .iter()
            .find(|fact| fact.name == "MAX")
            .expect("the function-like macro is a fact");
        assert!(max.function_like);
        assert_eq!(
            max.body,
            MacroBody::Expression,
            "the body is one parenthesised group"
        );
    }

    #[test]
    fn a_declaration_inside_the_files_own_guard_is_unconditional() {
        // The rule that makes a header's contents visible at all: `#ifndef H` … `#endif` around the whole file is
        // not a condition anybody has to decide, because *including the file* is what defines `H`. Measured on the
        // closure of `<string>`, without this rule every cross-file answer about the standard library is
        // `ConditionalCompilation` — the headers guard their bodies, so every `#include` inside one is "guarded".
        let summary =
            summary("#ifndef H\n#define H\nstruct Widget { int size; };\n#include \"other.h\"\n#endif\n");

        let widget = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "Widget")
            .expect("the guarded declaration is a fact");
        assert_eq!(
            widget.guard,
            FactGuard::Unconditional,
            "the file's own guard is entered by including the file at all"
        );

        let include = summary
            .includes
            .iter()
            .find(|fact| fact.spelling == "other.h")
            .expect("the include is a fact");
        assert_eq!(
            include.guard,
            FactGuard::Unconditional,
            "and so is an `#include` written inside it"
        );

        assert_eq!(
            summary.guards.regions.len(),
            1,
            "the region is still recorded: the `#if` is a fact about the text either way"
        );
    }

    #[test]
    fn a_conditional_that_is_not_the_files_guard_still_guards() {
        // What the rule must not swallow: `#if defined(A)` is a real condition, and a fact inside it is still
        // `Unknown` to a consumer that cannot evaluate it.
        let summary = summary("#if defined(A)\nint inside;\n#endif\n");

        let fact = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "inside")
            .expect("the declaration is a fact");
        assert_eq!(fact.guard, FactGuard::Region(0));
    }

    #[test]
    fn a_header_that_declares_something_before_its_guard_is_not_guarded_by_it() {
        // `detect_guard` refuses a guard that does not wrap the whole file, and the rule follows it rather than
        // making up its own answer: the `#ifndef` here is an ordinary conditional and stays one.
        let summary = summary("int early;\n#ifndef H\n#define H\nint inside;\n#endif\n");

        let early = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "early")
            .expect("the declaration before the guard is a fact");
        assert_eq!(
            early.guard,
            FactGuard::Unconditional,
            "it is outside every region, which is a different answer from the guard's"
        );

        let inside = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "inside")
            .expect("the declaration inside is a fact");
        assert_eq!(
            inside.guard,
            FactGuard::Region(0),
            "a guard that does not wrap the file does not de-guard what it does contain"
        );
    }

    #[test]
    fn a_declaration_in_a_conditional_keeps_its_region_through_the_builder() {
        let summary = summary("#if defined(A)\nint inside;\n#endif\n");

        assert_eq!(summary.guards.regions.len(), 1);
        let fact = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "inside")
            .expect("the guarded declaration is a fact");
        assert_eq!(fact.guard, FactGuard::Region(0));
    }

    #[test]
    fn an_include_that_cannot_be_resolved_keeps_its_spelling() {
        let summary = summary("#include <vector>\n#include \"local.h\"\n");

        assert_eq!(summary.includes.len(), 2);

        let vector = &summary.includes[0];
        assert_eq!(vector.form, IncludeForm::Angle);
        assert_eq!(vector.spelling, "vector");
        assert_eq!(
            vector.resolved, None,
            "with nothing to search, the include is unresolved rather than dropped"
        );

        assert_eq!(summary.includes[1].form, IncludeForm::Quote);
        assert_eq!(summary.includes[1].spelling, "local.h");
    }

    #[test]
    fn an_include_resolves_against_the_provider() {
        let files = MemoryFiles::new()
            .with_file("/p/local.h", "int x;\n")
            .with_file("/usr/include/vector", "// the real one\n");

        // An *angle* include searches the configured paths and not the including file's directory, so the
        // configuration is what makes it resolvable — a distinction worth a test, because a resolver that
        // searched the local directory for `<vector>` would find a project's own file of that name and shadow
        // the standard header.
        let mut config = CompilerConfig::default();
        config.include_paths = vec![crate::include::config::IncludePath::system("/usr/include")];

        let indexer = FileIndexer::new(&files, &config);
        let summary = indexer.index(
            Path::new("/p/widget.cpp"),
            "#include <vector>\n#include \"local.h\"\n",
            key(),
        );

        let vector = summary
            .includes
            .iter()
            .find(|fact| fact.spelling == "vector")
            .expect("the angle include is a fact");
        assert_eq!(
            vector.resolved.as_deref(),
            Some(Path::new("/usr/include/vector")),
            "an angle include searched the configured path"
        );

        let local = summary
            .includes
            .iter()
            .find(|fact| fact.spelling == "local.h")
            .expect("the quoted include is a fact");
        assert_eq!(
            local.resolved.as_deref(),
            Some(Path::new("/p/local.h")),
            "a quoted include looks beside the including file first"
        );
    }

    #[test]
    fn every_summary_survives_being_written_and_read_back() {
        let directory = std::env::temp_dir().join("cppls-summary-round-trip");
        let _ = std::fs::remove_dir_all(&directory);

        let summary = summary(
            "#define MY_API\n#if defined(A)\nstruct Widget { int size; };\n#endif\n#include <vector>\n",
        );
        let written = write_summary(&summary, &directory).expect("the write must succeed");

        assert!(
            written.starts_with(&directory),
            "the summary lands under the project root: {written:?}"
        );

        let read = super::read_summary(&written).expect("what was written must be readable");
        assert_eq!(read, summary);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_macro_body_classifier_reads_each_shape() {
        let cases = [
            ("#define A __declspec(dllexport)", MacroBody::Specifier),
            ("#define A extern \"C\"", MacroBody::Specifier),
            ("#define A do { } while (false)", MacroBody::Statement),
            ("#define A if (x) { }", MacroBody::Statement),
            ("#define A return", MacroBody::Statement),
            ("#define A ((a) > (b) ? (a) : (b))", MacroBody::Expression),
            ("#define A (a)", MacroBody::Expression),
            ("#define A int", MacroBody::Type),
            ("#define A unsigned long", MacroBody::Type),
            ("#define A { }", MacroBody::Block),
            ("#define A", MacroBody::Unknown),
            ("#define A something_else + 1", MacroBody::Unknown),
        ];

        for (source, expected) in cases {
            let tree = CppParser::parse(source, ParserConfig::default());
            let preprocessing = crate::preprocess::preprocess(source, tree.get_tokens());

            let Directive::Define(define) = &preprocessing.directives[0].directive else {
                panic!("{source:?} must be a define");
            };
            let definition = define.macro_def.as_ref().expect("a readable definition");

            assert_eq!(
                macro_body_shape(definition),
                expected,
                "{source:?} should be {expected:?}"
            );
        }
    }

    #[test]
    fn a_body_that_is_not_one_parenthesised_group_is_not_an_expression() {
        // `(a) + (b)` is a sum: it starts with `(` and ends with `)` and the group closes before the end. Calling
        // it an expression would be right in this case and wrong for a statement that happens to be written that
        // way, so the answer is `Unknown` — which still rules out the call reading.
        let tree = CppParser::parse("#define A (a) + (b)", ParserConfig::default());
        let preprocessing = crate::preprocess::preprocess("#define A (a) + (b)", tree.get_tokens());

        let Directive::Define(define) = &preprocessing.directives[0].directive else {
            panic!("must be a define");
        };
        let definition = define.macro_def.as_ref().expect("a readable definition");

        assert_eq!(macro_body_shape(definition), MacroBody::Unknown);
    }

    #[test]
    fn an_unresolved_include_does_not_stop_the_other_facts() {
        let tree = CppParser::parse("#include <nonexistent_xyz>\nstruct W { int a; };\n", ParserConfig::default());
        let preprocessing = crate::preprocess::preprocess("#include <nonexistent_xyz>\nstruct W { int a; };\n", tree.get_tokens());

        let files = MemoryFiles::new();
        let config = CompilerConfig::default();
        let resolver = IncludeResolver::new(&files, &config);
        let mut interner = PathInterner::new(cfg!(windows));

        let facts: Vec<_> = preprocessing
            .directives
            .iter()
            .filter_map(|spanned| {
                include_fact(
                    &spanned.directive,
                    spanned.range,
                    Path::new("/p"),
                    &resolver,
                    &mut interner,
                    None,
                )
            })
            .collect();

        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].spelling, "nonexistent_xyz");
        assert_eq!(facts[0].resolved, None);
    }

    #[test]
    fn a_macro_fact_points_at_the_name_not_the_directive() {
        let summary = summary("  #define   MY_API   int\n");

        let fact = summary
            .macros
            .iter()
            .find(|fact| fact.name == "MY_API")
            .expect("the macro is a fact");

        // The name is what a rename edits and what a "go to definition" puts the cursor on.
        let text = "  #define   MY_API   int\n";
        assert_eq!(
            &text[fact.range.start_offset..fact.range.end_offset()],
            "MY_API"
        );
    }

    #[test]
    fn a_summary_carries_no_resolved_conclusions() {
        // The first invariant, asserted rather than assumed: a summary holds names as
        // written and nothing that would have to be recomputed when a header changes.
        let summary = summary("namespace ns { struct Widget { int member; }; }\n");

        let widget = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "Widget")
            .expect("the class is a fact");

        assert_eq!(widget.kind, DeclKind::Type);
        assert_eq!(
            widget.scope.as_deref(),
            Some("ns"),
            "the scope is the spelling the file used, not a resolved identity"
        );
        assert_eq!(widget.qualified_name(), "ns::Widget");
    }

    /// A provider with one file, for the resolution test above.
    struct OneFile(PathBuf, String);

    impl FileProvider for OneFile {
        fn read(&self, path: &Path) -> Option<String> {
            (path == self.0).then(|| self.1.clone())
        }

        fn exists(&self, path: &Path) -> bool {
            path == self.0
        }
    }

    #[test]
    fn a_provider_that_has_one_file_still_answers_for_it() {
        let files = OneFile(PathBuf::from("/p/only.h"), "int x;\n".to_string());
        let table = MacroTable::new();
        let _ = table;

        assert_eq!(files.read(Path::new("/p/only.h")).as_deref(), Some("int x;\n"));
        assert_eq!(files.read(Path::new("/p/other.h")), None);
    }
}

