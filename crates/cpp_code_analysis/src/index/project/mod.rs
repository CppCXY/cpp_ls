//! Many files at once: the summaries, the edges between them, and the queries that need both.
//!
//! [`crate::index`] turns one file into a [`FileSummary`]. This module is what holds a project's worth of them
//! and answers the questions that cross a file boundary — which is the whole reason an index exists, and the
//! layer calls `resolve`.
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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cpp_parser::CppSyntaxKind;

use super::names::{NameIndex, Posting};
use crate::Visibility;
use crate::file::paths::normalize_path;
use crate::include::graph::Marked;
use crate::preprocess::directive::IncludeForm;
use crate::sema::types::{Type, TypeBindings, parse_type_spelling};
use crate::summary::{DeclFact, DeclKind, FactGuard, FileSummary, MacroFact};
use crate::symbol::{Known, UnknownReason};

#[cfg(test)]
#[path = "../project_cache_tests.rs"]
mod cache_tests;

/// How many files a visibility walk will cross before giving up.
///
/// A backstop rather than a policy: real include graphs are shallow (the walker's own limit is
/// [`crate::MAX_INCLUDE_DEPTH`]), and a corpus that exceeds this is one whose summaries disagree with its
/// includes — which the visited set already handles. The number is here so that the *round* count of a
/// pathological graph cannot become an unbounded amount of work on a keystroke.
const MAX_VISIBILITY_DEPTH: usize = 128;

/// **Where an `#include` points** — the target a reader asks about by pointing at the header's name.
///
/// The fourth answer a "go to definition" can give, beside a declaration, an overload set and a macro, and the one
/// it was missing: nothing in the file *declares* `vector`, so every question the jump is built out of — the scope
/// walk, the index by name, the macro table — answers nothing at all for `#include <vector>`, and a reader who
/// ctrl-clicks it is told there is no definition to go to.
///
/// The name is the spelling between the delimiters (`vector`, `sys/types.h`), which is what the resolver searched
/// for and what a reader wrote; the delimiters are the *form*, which is why [`HeaderTarget::form`] is separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderTarget {
    /// The spelling between the delimiters, and — for [`IncludeForm::Macro`] — the macro's name.
    pub spelling: String,
    pub form: IncludeForm,
    /// The directive, as a range in the file **being edited**, which is what a client highlights.
    ///
    /// The whole `#include <vector>` rather than the name inside it, and that is the honest range: a jump to a
    /// header lands at the top of a file whose contents have nothing to do with this one, and highlighting the
    /// directive that asked for it is what tells the reader *why* they are looking at it.
    pub range: cpp_parser::SourceRange,
    /// The file it resolved to, as a path a client can open.
    pub resolved: PathBuf,
}

mod queries;

pub use queries::*;

/// **One file's cooked reading, as the index keeps it** — what a compiler sees written there, and what the parse of
/// that text complained about.
///
/// The two travel together because they are one reading of one rendering: a caller that handed over the declarations
/// and dropped the errors would make the index answer "nothing is wrong with this file" for a rendering the parser
/// had something to say about, and a caller that replaced the declarations without replacing the errors would
/// publish the *old* file's errors beside the new file's names.
/// **How many cooked readings are kept** — the map is sparse by design, so this only bounds the closure of the
/// open files, not what a query can answer: a miss falls back to the file`s own reading.
const MAX_COOKED_FILES: usize = 512;

#[derive(Debug, Clone, Default)]
pub struct CookedFile {
    /// The declarations the rendering read as, every range mapped back into the file.
    pub declarations: Vec<DeclFact>,
    /// **The macros the rendering was read with** — and this is the half that answers a question the file's own
    /// reading cannot.
    ///
    /// A rendering has no directives left in it: the preprocessor already chose which branches are compiled, so
    /// every `#define` the rendering contains is one that **is** in force, and every fact here carries
    /// `FactGuard::Unconditional`. The file's own reading keeps all of them, each tagged with the region it was
    /// written in, and every consumer then has to **decide those regions again** — which is a second implementation
    /// of the preprocessor's own decision, and it is the one that keeps being wrong.
    ///
    /// Measured on `vcruntime.h`, which writes `_STL_LANG` once per branch of `#ifdef __cplusplus`: the raw reading
    /// files all three, the last of them `0L`, and a lookup that takes "the last binding at or before this offset"
    /// answers `0L` for a file the compiler read as C++20. The rendering holds the taken one and nothing else, so a
    /// consumer that reads **these** has no region to judge.
    pub macros: Vec<MacroFact>,
    /// The rendering's parse errors, placed in the file — see [`crate::CookedDiagnostic`].
    pub diagnostics: Vec<crate::CookedDiagnostic>,
    /// Errors the parse of the rendering reported that **this file cannot show**, because the text they are about is
    /// not written here.
    ///
    /// Kept with the reading rather than with the caller, because it is what makes "no errors" honest: an empty
    /// [`CookedFile::diagnostics`] beside a non-zero count here means the rendering was not clean and the reader
    /// simply cannot be shown where.
    pub unplaced: usize,
}

impl CookedFile {
    /// A reading that found declarations and nothing to report.
    ///
    /// For a caller with facts and no parse of its own — a test that built a rendering by hand, or a future
    /// producer that reads the declarations back from a cache.
    pub fn declarations(declarations: Vec<DeclFact>) -> Self {
        CookedFile {
            declarations,
            macros: Vec::new(),
            diagnostics: Vec::new(),
            unplaced: 0,
        }
    }
}

impl From<crate::IndexedRendering> for CookedFile {
    /// What the index keeps of a rendering's reading: the declarations, **the macros it was read with**, the errors
    /// this file can show, and the count of the ones it cannot.
    ///
    /// The summary itself is still not kept, and the reason stands: a rendering's **directives and includes** are
    /// empty, because that text was spelled out by this crate rather than by the file, and keeping them would invite
    /// a reader to believe them. Macros are the exception and the point — a rendering's macros are the ones the
    /// preprocessor *chose*, with no region left to judge (see [`CookedFile::macros`]).
    fn from(indexed: crate::IndexedRendering) -> Self {
        CookedFile {
            declarations: indexed.summary.declarations,
            macros: indexed.summary.macros,
            diagnostics: indexed.diagnostics,
            unplaced: indexed.unplaced,
        }
    }
}

/// **What one file was read as**, beyond its own summary: the renderings of it, one per translation unit.
///
/// A file's summary is the file read **on its own** — its own text, its own closure, its own environment. A cooked
/// reading is the file read **inside a program**, and a header can be read by several programs: `a.cpp` that defines
/// `_GNU_SOURCE` before including it and `b.cpp` that does not read *different declarations* out of the same bytes.
/// So the readings are kept per program, and which one a lookup uses is decided by the **asking** file's program —
/// see [`ProjectIndex::the_reading_for`].
///
/// The id a [`Posting`] carries is **`0` for the file's own summary** (which lives in `ProjectIndex::summaries`, not
/// here), `1` for the reading it was cooked into on its own, and `2 + its position` for a program's reading — so a
/// reading keeps its id for as long as the file does and a re-read replaces it in place.
#[derive(Debug, Default)]
struct Readings {
    /// The file rendered in its own translation unit — what the per-file cook produces.
    on_its_own: Option<CookedFile>,
    /// The programs that read it, in the order they did.
    in_a_program: Vec<(u64, CookedFile)>,
    /// The last program to read it, which is the reading a caller that cannot name one gets.
    last: Option<u64>,
}

impl Readings {
    /// The id of a program's reading of this file, if it has one.
    fn id_for(&self, program: u64) -> Option<u32> {
        self.in_a_program
            .iter()
            .position(|(held, _)| *held == program)
            .map(|at| at as u32 + 2)
    }

    /// The reading filed under `id`: `1` is the file's own, `2..` are the programs'. `0` — the file's summary — is
    /// not a cooked reading and answers `None`.
    fn reading(&self, id: u32) -> Option<&CookedFile> {
        match id {
            0 => None,
            1 => self.on_its_own.as_ref(),
            id => self
                .in_a_program
                .get(id as usize - 2)
                .map(|(_, reading)| reading),
        }
    }

    /// The id the file's **own** cooked reading has, if it was ever cooked that way.
    const ON_ITS_OWN: u32 = 1;

    /// The reading a caller gets when it has no program to ask for: the last program's, else the file's own.
    ///
    /// **A reading with declarations wins over one without**, and the order is otherwise unchanged. Two readings of
    /// one file are two answers, and an empty one is the weaker: a file cooked on its own **before** its includes were
    /// in the index renders to nothing (`nlohmann/json.hpp` has 761 declarations from its program and an `on_its_own`
    /// entry with none), and a caller that took the empty one answered `NotDeclaredHere` for every member of the class
    /// — which is exactly the shape the report described, after the file had in fact been read.
    fn any(&self) -> Option<&CookedFile> {
        let from_the_last_program = self
            .last
            .and_then(|program| self.id_for(program))
            .and_then(|id| self.reading(id));
        let candidates = [
            from_the_last_program,
            self.on_its_own.as_ref(),
            self.in_a_program.first().map(|(_, reading)| reading),
        ];
        candidates
            .iter()
            .flatten()
            .copied()
            .find(|reading| !reading.declarations.is_empty())
            .or_else(|| candidates.iter().flatten().copied().next())
    }

    fn every(&self) -> impl Iterator<Item = (u32, &CookedFile)> {
        self.on_its_own
            .iter()
            .map(|reading| (Self::ON_ITS_OWN, reading))
            .chain(
                self.in_a_program
                    .iter()
                    .enumerate()
                    .map(|(at, (_, reading))| (at as u32 + 2, reading)),
            )
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
    /// The paths **by sequence number**, which is insertion order: a query over all of them is deterministic rather
    /// than `HashMap`-ordered. A definition jump that returned a different file on each run would be a bug that only
    /// shows up in a test that runs twice.
    ///
    /// A map and not a `Vec` so that forgetting a file is a removal rather than a scan of every path — an edit
    /// forgets its file, and on a project of a hundred thousand files a `retain` per keystroke is a cost with
    /// nothing in it that answers anything.
    order: BTreeMap<u32, String>,
    /// The sequence number each file was given the first time it appeared: the reverse of `order`, and the identity
    /// the name index's postings use. **Never reused** — a forgotten file's number stays retired, so a posting can
    /// never come to point at a different file than it was made for.
    sequence: HashMap<String, u32>,
    next_sequence: u32,
    /// **How many times the summaries have changed** — one bump per [`ProjectIndex::insert_at`] and
    /// [`ProjectIndex::forget`].
    ///
    /// Not a version of the answers: it is what makes the *check* cheap. A cached walk is valid if the readings it was
    /// made of are unchanged, and that is a fingerprint over its whole closure (measured: ~0.1 ms for 1063 files). A
    /// request asks the same closure question thousands of times, so the fingerprint is taken once per request: a walk
    /// built or validated at this number answers from the stamp alone until the index changes again.
    changed: u64,

    /// The inverted index — see [`crate::index::names`]. Derived from `summaries` and `cooked`, and updated in the
    /// same call that changes either; nothing else writes it.
    names: NameIndex,
    /// For each file, the files that include it. Derived from the summaries; see the module documentation.
    included_by: HashMap<String, BTreeSet<String>>,
    /// **Which file declares each module** — the second visibility edge, beside `included_by`.
    ///
    /// An `import m;` names a *module*, and the file behind it is the project's business rather than the
    /// preprocessor's: this is the map that turns the name into a file. Only **primary interface units** are here
    /// (see [`ProjectIndex::insert_at`]), because those are the units an importer sees.
    ///
    /// Derived from the summaries like `included_by`, and for the same reason: the visibility walk that needs it
    /// holds summaries and nothing else.
    module_interfaces: HashMap<String, String>,
    /// **Every file a module's interface unit was *rendered* from**, by module name — what an importer sees.
    ///
    /// A module is not a header, so the `#include` reconstruction in `walk_the_module_edges` answers a weaker question
    /// than the module does: it evaluates each guard against a state that is not the unit's, and a header the unit
    /// included behind `_HAS_CXX20` — MSVC's `xstring` behind `<string>` — is dropped. Measured on the reader's
    /// project, from `report.cppm` importing `std`:
    ///
    /// ```text
    /// 151 visible file(s), 4110 in `std`, string=[]        ← no std::string
    /// ```
    ///
    /// while the same question from `main.cpp` answered 173 files and `string=["Type"]` — only because that file also
    /// `#include`s a header that pulls the tree in textually. The unit's rendering already decided every guard, once,
    /// for the module ([`crate::RenderedUnit::walked`], 223 files for `std`), so that list is the answer; this field is
    /// where it is kept, filled by [`ProjectIndex::remember_the_unit_closure`].
    unit_closures: std::sync::Mutex<HashMap<String, Vec<String>>>,
    /// The macros a **compilation** starts with, which no summary can hold: the compiler's predefined names and the
    /// command line's `-D`s.
    ///
    /// Every condition stored in a summary is a question about these, and an index that has not been told them
    /// answers `Unknown` for every condition — which is what this type did before the field existed, and what it
    /// still does for a caller that builds an index by hand. See [`crate::index::environment`] for what the field
    /// is complete about and what it deliberately is not.
    macros: Marked,
    /// **What the cooked reading found**, by normalized path — the declarations a compiler sees, and the errors the
    /// parse of the rendering reported.
    ///
    /// A second reading beside the first, and only these two things of it, for reasons that are all about not
    /// lying: a rendering has no directives, so a cooked summary says `includes: []`, `macros: []`, `guards: []` — a
    /// reader that took those at face value would conclude the file includes nothing and defines nothing — and the
    /// two things a rendering knows that the file's own text does not are what it **declares** (the type a macro
    /// declared, `DECLARE_HANDLE(HWND)` declares `HWND__` and `HWND`; the *scope* a namespace-opening macro put it
    /// in, MSVC's `_STD_BEGIN` being `namespace std {`) and what the **parser said about it** — an error against
    /// text no branch, macro or conditional, keeps out of the compiler's sight.
    ///
    /// **Sparse on purpose**: cooking needs the translation unit's environment, so a caller has this for the files
    /// it actually read rather than for the whole project, and a file with no entry here is one nobody cooked.
    ///
    /// **Per program**, because the same bytes read in two translation units are two answers — see [`Readings`].
    cooked: HashMap<String, Readings>,
    /// **The cooked files in the order they were last used** — what [`ProjectIndex::insert_cooked`] evicts from.
    ///
    /// A queue rather than a counter, and behind a lock so that a **read** can move its file to the back: the policy
    /// is least-recently-*used*, not first-in-first-out, because the file a query just asked about is the one that
    /// must survive the wave that follows it. Measured on `EmmyLuaCodeStyle`: `nlohmann/json.hpp` was rendered and
    /// indexed with **761** declarations, then evicted by the standard-library headers cooked after it, and the hover
    /// that needed it answered `NotDeclaredHere` — the eviction made the answer depend on the order of a walk nobody
    /// asked for.
    cooked_order: std::sync::Mutex<std::collections::VecDeque<String>>,
    /// A visibility walk's answer, by the file it was asked about.
    ///
    /// **The whole-walk memo, beside the per-condition one**, and it exists for the reason the numbers say: the walk
    /// builds the closure once (18 ms over 145 files of the MSVC standard library, measured by
    /// `examples/completion_latency.rs`), and a completion asks the same question on every keystroke — a steady-state
    /// query measured **4–5 ms** with the per-condition memo in place and **84–90 ms** without one. The two memos
    /// share invalidation triggers: the walk memo is cleared on index changes, while condition answers for a known
    /// file are evicted only for that file and its dependents.
    ///
    /// Keyed by the root so that a server with several files open keeps at most one answer per file, and storing the
    /// walk's own `Seen` rather than a copy of the pair because that is what the walk produces.
    ///
    /// # Why an entry carries the files it was made of
    ///
    /// **Clearing the whole map on every insert is what made a keystroke pay for the walk.** A pull cooks (inserts)
    /// before it runs the checks, so every pull walked five closures from scratch: measured on `LuaSyntaxTree.cpp`,
    /// 13.0 / 3.6 / 4.9 / 2.8 / 9.2 ms = 33.5 ms of one 200 ms pull. But a walk's answer depends on exactly the
    /// files it read (their includes, their conditions' macros), so a re-read of a file invalidates precisely the
    /// entries whose closure contains it — and editing `LuaSyntaxTree.cpp` drops the walk *from* `LuaSyntaxTree.cpp`
    /// while the four walks from the headers it includes survive. A file the index has **never** seen still clears
    /// the whole map, because a new file can change how an `#include "x.h"` resolves for a closure that does not
    /// contain it. See [`ProjectIndex::insert_at`] and [`ProjectIndex::forget`].
    visibility: std::sync::Mutex<HashMap<String, std::sync::Arc<CachedWalk>>>,
    /// **Files a query found it could not answer from, and wants refined** — see
    /// [`ProjectIndex::ask_for_refinement`].
    ///
    /// A query cannot refine: refining is a walk of the file's closure and belongs to the pump, which is the one
    /// place that may spend it. What a query *can* say is which file would have answered — so it leaves the answer
    /// here and the session drains it (see `Session::advance`), the same shape `refined_on_request` already has for
    /// a file a request named.
    refinement_asks: std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
    /// **The walks being built right now**, by root — the single-flight that keeps one cold closure from being walked
    /// twice.
    ///
    /// A miss in `visibility` is computed outside the lock that guards it, which is right (a walk is milliseconds and
    /// a lock held across it is a lock every query waits on) and leaves the obvious hole: two callers that miss the
    /// same entry at the same moment each build it. The review in `docs/perf-review.md` observed exactly that — two
    /// cold walks of one closure, concurrently, each seconds long — because the pump's prewarm and a request from the
    /// client are two threads asking the same question about the same file.
    ///
    /// So a walk is **claimed** here before it starts and released when it finishes, and a caller that finds its
    /// root claimed waits for the claim to go rather than repeating the work. `walk_done` is what it waits on; the
    /// mutex is held for the duration of a `HashSet` operation and never across a walk.
    walking: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Signalled when a claimed walk is released — see [`ProjectIndex::walking`].
    walk_done: std::sync::Condvar,
    /// **How many visibility walks were built, and how many were answered from the memo** — the numbers that say
    /// whether the memo is doing its job, and the ones a regression asserts. Counted rather than logged because a
    /// test wants a number and a reader of a latency graph wants to know whether a request walked.
    ///
    /// `walks_built` counts *claims*, so with the single-flight in place a cold closure that two threads ask about
    /// counts **1** and one thread's answer is the other's.
    pub walks_built: std::sync::atomic::AtomicU64,
    /// Memo hits in [`ProjectIndex::visible_files_with_modules`] — validated by the walk's own fingerprint.
    pub walks_hit: std::sync::atomic::AtomicU64,
    /// **Calls that found their root already being walked** — the work the single-flight removed. A number that
    /// stays zero on a single-threaded run is the number that has to be non-zero before the claim is worth having.
    pub walks_met: std::sync::atomic::AtomicU64,
    /// What the **condition** on a guarded `#include` was last answered, by `(file, region)`.
    ///
    /// A memo, not a fact: it is cleared whenever a summary is inserted, because that is the only thing that can
    /// change an answer. It exists because the answer is expensive — evaluating one condition builds the file's
    /// whole closure state (`macros_at`) — and the same question is asked once per query and once per walk, over
    /// hundreds of edges. `std::sync::Mutex` rather than a `RefCell` so the index stays `Sync`: a language server
    /// holds one of these behind a lock, and a cache that cost that property would be a bad trade.
    ///
    /// # What clearing it whole costs a request, measured
    ///
    /// Before selective invalidation, every `insert` dropped the whole memo, so the next query re-evaluated every
    /// guarded include in its closure. Measured on the latency fixture, twelve completions fired from `didOpen`:
    ///
    /// ```text
    ///   the query's own work   #0 350 ms   #1 598 ms   #2 1270 ms   #3+ 2–5 ms
    ///   (all of it here)       at 821 guarded edges, against 3 ms once the inserts stop
    /// ```
    ///
    /// and the split says it is this memo and not the walk: `visible_declarations_upto`'s collection loop measures
    /// **0–2 ms** and the materialisation of its 548 candidates **1–2 ms** on the same queries, while
    /// `visible_files_with_modules` — which asks this memo for every guarded edge — measures 359–588 ms.
    ///
    /// An answer for `(file, region)` depends on the file's closure. Replacing a known summary therefore evicts that
    /// file and its dependents; a newly discovered path clears the whole memo because previous edges may reach it.
    ///
    /// The alternative — the one the `FactGuard::Region` arm's own comment names — is the incremental
    /// `macros_at`/`macro_environment` state, one walk per file with conditions answered as they are reached,
    /// which removes the per-edge cost instead of the invalidation.
    visibility_answers: std::sync::Mutex<HashMap<(String, u32), crate::Visibility>>,
    /// **The inference answers that outlive one request** — see [`ProjectIndex::a_remembered_type`].
    ///
    /// `TypeMemo` lives for one request, so a pull, a hover and the next pull ask the same questions of the same tree
    /// again. The key is the file, the expression's range and text, and its kind; the lifetime rule is the one the memos
    /// above have — **every insert and every forget clears it** — so a hit is always an answer about the index as it
    /// stands. Bounded, and cleared when it reaches the bound: an answer is cheap to recompute and a map that grows with
    /// a session is not.
    type_answers: std::sync::Mutex<HashMap<u64, Known<(Type, PathBuf)>>>,
    /// How many questions the memo answered and how many it had to compute — what says whether it is earning its map.
    type_hits: std::sync::atomic::AtomicU64,
    type_misses: std::sync::atomic::AtomicU64,
    /// **The member lists already built, by `(class, the file whose tree was used)`** — see
    /// [`ProjectIndex::cached_member_list`] for the measurement and for how an entry is invalidated.
    ///
    /// This is the one cache here that outlives a request on purpose: a class's members do not change when a reader
    /// types a character somewhere else, and every diagnostics pull asks for the same handful of classes.
    member_lists: std::sync::Mutex<HashMap<(String, PathBuf), CachedMemberList>>,
    /// **A declaration query's answer, by `(spelling, the file it was asked from)`** — see
    /// [`ProjectIndex::definitions`].
    ///
    /// The same repetition as [`ProjectIndex::member_lists`], one layer down: measured on `LuaSource.cpp`, one
    /// diagnostics pull asked **95 of its 99 slow questions about `std`**, and each cost 0.6–1.3 ms. Within a pull
    /// the name set is tiny (`std`, `std::vector`, a handful of members) and the index does not change while the
    /// checks run, so the answer cannot go stale inside that window.
    ///
    /// Cleared wherever [`ProjectIndex::visibility`] is — an insert, a cook, or a forget — because a fact arriving
    /// is exactly what can change it.
    /// **One verdict per guarded declaration** — see `certain_declarations`. A guard costs a macro walk, and the
    /// same fact is asked again on every pull that names the declaration: measured on the real project, the
    /// uncached filter took a diagnostic pull from **28 ms to 71 ms** (`lsp_status`).
    guard_answers: std::sync::Mutex<HashMap<(PathBuf, usize, u32), crate::Visibility>>,
    /// **The macro environment of one `(name, file)`** — [`ProjectIndex::macro_environment`]'s walk, remembered.
    ///
    /// The walk is the expensive half of every macro question and it does not depend on the offset, but it was done
    /// again for every ask. Measured on neovim (`arglist.c`, 853 files in the closure): **1.19 s per walk**, three
    /// identical walks in a row, and the cost landed on the user as `end`: a hover on a member whose name is a macro
    /// (`buf->b_fnum`) took **3.2 s**, and took 3.2 s *again* on the next hover. `semanticTokens` paid it too — one
    /// walk for the whole file's name set, which is what [`ProjectIndex::macro_names_in_force`] is for.
    ///
    /// Invalidated exactly like [`ProjectIndex::visibility`]: an entry is dropped when a file it read is re-read (the
    /// contributors are recorded with it), and the whole map is cleared when a file the index has never seen arrives,
    /// because that can change how an `#include` resolves for a closure that does not contain it.
    macro_environments: std::sync::Mutex<HashMap<(String, String), CachedMacroWalk>>,
    /// **Every macro name in force in one file** — [`ProjectIndex::macro_names_in_force`]'s walk, remembered.
    ///
    /// Same measurement, same invalidation: the colouring pass asks about every identifier in the file and this is
    /// the one question that needs the whole closure, so it is asked once per file rather than once per request.
    macro_names: std::sync::Mutex<HashMap<String, CachedMacroNames>>,
    definition_answers: std::sync::Mutex<HashMap<(String, String), Known<ProjectDefinitions>>>,
    /// **Every path the index has ever filed**, in normalized form — what tells a *re-read* from a *new file*.
    ///
    /// [`ProjectIndex::known_paths`] outlives [`ProjectIndex::forget`] on purpose: an edit is a forget followed by a
    /// re-read, so at the moment the re-read inserts, the summaries map has no entry for the file — and testing that
    /// map made every keystroke look like a brand-new file, which cleared the whole walk memo that the contributor
    /// rule exists to keep. It is a set of `String` and not of ids because `sequence` goes with the summary (see
    /// `forget`), and because a path that was known once must stay known after it is forgotten.
    known_paths: std::collections::HashSet<String>,
    /// **What the visibility walks read from each file**, as one number — see
    /// [`ProjectIndex::the_walks_fingerprint`].
    ///
    /// Kept per file rather than recomputed from the summaries when a walk is validated: validating a walk reads one
    /// entry per contributor, and a closure here is over a thousand files, so hashing their includes and macro facts
    /// on every query would cost more than the walk it is validating.
    walk_inputs: std::collections::HashMap<String, u64>,
}

/// **What evaluating guarded `#include` conditions has cost**, in asks and nanoseconds — see the `FactGuard::Region`
/// arm and the `[visibility]` probe. Only written when `CPPLS_TRACE_TIME` is on, so a normal run does not touch it.
pub(crate) static CONDITION_ASKS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static CONDITION_NANOS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// One entry of [`ProjectIndex::member_lists`]: the members, and **what the answer was made of**.
///
/// The contributors are the files whose facts the list holds — every member's own file. An entry is dropped when one
/// of them changes, which is the narrowest invalidation that is sound without recording which *bases* were walked:
/// a base's members are in the list and carry the base's file.
///
/// # A list with an `unlisted` base, and the generation it was built at
///
/// A gap — "this base is not declared anywhere I can see" — is a statement about the **files that have been read**, not
/// about the class, so such a list used to be remembered at all: every file that mentioned the class rebuilt it.
/// Measured on a real project: `std::vector<LuaSyntaxNode>` 1.7 ms from one file and **20 ms** from the next, twelve
/// lists built per pull and **zero hits**. Remembering it together with the **number of summaries at the time** is
/// sound instead: a file being read is exactly what can fill the gap, so an entry built at an older count is treated as
/// absent. A pull reads nothing new, so the count does not move and the entry is reused.
#[derive(Debug)]
struct CachedMemberList {
    contributors: Vec<PathBuf>,
    list: MemberList,
    read_at: usize,
}

/// How many member lists [`ProjectIndex::member_lists`] may hold before it is emptied.
///
/// A list is a few hundred [`ProjectMember`]s, each with a `DeclFact`, so an entry is tens to a hundred kilobytes and
/// this is a few tens of megabytes at worst. Cleared whole rather than evicted one at a time: the sets that matter
/// are "the classes this file mentions", which are asked together.
const MAX_CACHED_MEMBER_LISTS: usize = 96;

/// One entry of [`ProjectIndex::macro_environments`]: the answer, and **the files it was made of**.
///
/// The contributor rule is [`ProjectIndex::visibility`]'s, for the same reason: a walk's answer is a function of the
/// files it read, so a re-read of one of them invalidates precisely this entry and nothing else.
#[derive(Debug)]
struct CachedMacroWalk {
    contributors: Vec<String>,
    environment: MacroEnvironment,
}

/// One entry of [`ProjectIndex::macro_names`] — the same shape, for the whole-name-set walk.
#[derive(Debug)]
struct CachedMacroNames {
    contributors: Vec<String>,
    names: std::collections::HashSet<String>,
}

/// How many macro walks each memo may hold before it is emptied.
///
/// An environment is one name's candidates (a handful of facts) and a name set is tens of thousands of short strings,
/// so a few hundred entries is tens of megabytes at worst. Cleared whole when full, like the other memos: the sets
/// that matter are "the names this file asks about", which are asked together.
const MAX_CACHED_MACRO_WALKS: usize = 256;

/// How many declaration answers [`ProjectIndex::definition_answers`] may hold before it is emptied.
///
/// A pull asks for a few dozen spellings and the whole point of the memo is that the repeats hit; the bound is
/// there so that a long-running session cannot grow it without limit, and emptying it costs one pull's lookups.
const MAX_DEFINITION_ANSWERS: usize = 4096;

/// How many guard verdicts [`ProjectIndex::a_guard_is_taken`] may hold before it is emptied.
///
/// One per guarded declaration any lookup has asked about — a few thousand on a standard-library closure, each a
/// `(path, offset, region)` and an enum.
const MAX_GUARD_ANSWERS: usize = 16_384;

mod members;

pub use members::*;

mod completions;

pub use completions::*;

mod type_of;

pub(crate) use type_of::*;

mod deductions;

pub(crate) use deductions::*;

mod calls;

pub(crate) use calls::*;

mod aliases;

pub use aliases::*;

mod member_facts;

/// **[member_candidates] — one member name's whole overload set.**
mod member_candidates;

pub(crate) use member_candidates::member_candidates;
pub(crate) use member_facts::*;

mod bases;

pub(crate) use bases::*;

mod macro_walk;

pub(crate) use macro_walk::*;

mod macro_environment;

pub use macro_environment::*;

#[cfg(test)]
mod tests;

/// The name index against the scan it replaced: the same questions, asked of the same index, answered both ways.
///
/// The inverted index is derived data, and derived data has one failure that no ordinary test finds — it drifts
/// from what it was derived from after some particular sequence of edits. So this builds a corpus that has every
/// kind of collision (one name in many files, one name in many scopes, a name only the cooked reading declares),
/// mutates it the way a session does (forget, re-insert edited, cook, forget the cooking), and after **every**
/// step asserts that each name query equals the brute-force answer over `summaries()`.
#[cfg(test)]
mod name_index_tests;

mod substituted;

pub use substituted::*;
