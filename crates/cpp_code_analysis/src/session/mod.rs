//! The driver: one open project, with the four layers joined and a queue that notifications re-seed.
//!
//! Everything below this module is a *capability*: [`discover`](crate::discover) can find a toolchain,
//! [`SummaryStore`] can cache a file's facts, [`Worklist`](crate::Worklist) knows the order that answers a user's
//! question first, and the queries in [`crate::index`] answer about a name, a member or a cursor. Nothing joined
//! them, which is why calls "opening a project" the shortest piece of product work left:
//!
//! ```text
//! open a project     discover the toolchain, read compile_commands.json, list the sources
//! say what changed   didOpen / didChange / didClose / didSave, and the client's own file events
//! index some of it   one file per call, open files and what they include before the rest
//! answer a question  parse the buffer the cursor is in, then ask the index about it
//! ```
//!
//! # Who owns the providers
//!
//! The session does. A [`Session`] reads through a [`SessionFiles`] — the editor's open buffers in front of the
//! filesystem — and keeps it, together with the [`SummaryStore`] that reads through the same chain:
//!
//! ```text
//! let documents = OpenDocuments::new();
//! let files = SessionFiles::new(documents.clone(), DiskFiles);
//! let mut session = Session::open(root, files, WatchFilter::new(root));
//!
//! documents.open(path, buffer_text);   // ← the handle is a second owner, not a borrow
//! ```
//!
//! The clone on the second line is the whole of the arrangement, and it is not a trick: every provider in this
//! crate is a **handle** rather than data — `OpenDocuments` is a map behind a lock that an `Arc` shares, `DiskFiles`
//! is a unit struct — so the session's copy and the caller's copy read the same buffers and duplicate nothing. An
//! edit reaches the analysis while the store reads through that chain, and no borrow is involved on either side:
//! what the two owners share has interior mutability and both read it through `&self`.
//!
//! The version before this one *borrowed* the provider for the session's whole life, which forced every caller to
//! declare a provider that outlived the session. A test can do that; a language server cannot, because it builds
//! its session once when the workspace opens and keeps it until the editor exits — the borrow would have had to be
//! leaked, or the session boxed behind a self-referential struct, to fit in a function that returns.
//!
//! # What it deliberately is not
//!
//! * **No LSP types.** Positions here are byte offsets, and the answers are this crate's own. A server maps
//!   `Position` onto an offset (`cpp_parser::LineIndex::get_offset` is that mapping) and the answers onto protocol
//!   responses; keeping the protocol out means this layer is testable without a client.
//! * **No OS watcher, no threads, no clock.** The client is the event source — LSP's `didOpen`/`didChange`/
//!   `didSave`/`didClose`/`workspace/didChangeWatchedFiles` are notifications, so `notify` is not needed and a
//!   debounce clock is the client's. [`Session::advance`] does a bounded amount of work per call and returns, so a
//!   caller decides whether that is an idle tick, a budget, or a loop.
//! * **No conclusion about a file that has not been read.** See below.
//!
//! # Lazy indexing makes "not here" mean two things, and this is where that is handled
//!
//! A query about a name that nothing has indexed answers
//! [`UnknownReason::NotDeclaredHere`](crate::UnknownReason) — the same answer a
//! fully indexed project gives for a name that genuinely is not there. That is deliberate and it is the honest
//! answer ([`ProjectIndex::definition`] explains why the index never claims "nowhere"), and it is *different* from
//! what a user needs: a consumer that reports absence must not report it while the file's includes are still being
//! read. [`Session::pending`] is how it tells the two apart, and the rule for the layer above is:
//!
//! ```text
//! pending() > 0   →  say nothing (and do not report an unresolved name as an error)
//! pending() == 0  →  NotDeclaredHere is now a finding about the project
//! ```
//!
//! # One configuration for the project
//!
//! [`Session::open`] reads `compile_commands.json` if there is one, and the flags it finds are **the project's**:
//! the first entry's, applied to every file. Real projects compile different targets with different `-D`s, so this
//! is an approximation, and the place it would be fixed is [`SummaryStore`], which holds one configuration for
//! every file it caches — the key already records the whole compilation context, so per-file configurations are
//! representable and simply not implemented. The store is what carries the single configuration today.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::cache::SummaryKey;
use crate::file::coordinates::{FileOffset, ReadingOffset};
use crate::file::paths::{DiskFiles, FileProvider, OverlayFiles, normalize_path};
use crate::file::vfs::Vfs;
use crate::file::view::{FileView, TokensOf};
use crate::include::config::{CompileCommands, CompilerConfig, project_config_from_flags};
use crate::include::toolchain::{self, Environment, Toolchain};
use crate::index::project::{
    MemberCompletions, MemberList, NameCompletions, ProjectDefinition, ProjectDefinitions,
    ProjectIndex, ProjectMacro, substituted_members,
};
use crate::index::references::{MacroReferences, ReferenceBudget, macro_references};
use crate::index::store::{StoreStats, SummaryStore};
use crate::index::watch::{ChangeBatch, FileEvent, Response, WatchFilter};
use crate::index::worklist::StepOutcome;
use crate::index::worklist::{Priority, Step, outcome_of};
use crate::index::{
    FileIndexer, definition_across_files, definitions_across_files, macro_across_files,
    member_completions_at, members_of,
};
use crate::inlay::ParameterHint;
use crate::macros::MacroTable;
use crate::project::{ConfigReport, ProjectDiscovery};
use crate::sema::modules::{ImportOutcome, ModuleScanner};
use crate::summary::{FileSummary, OutlineSymbol, UnitReading};
use crate::symbol::{Known, UnknownReason};
use crate::{PathInterner, cache};

/// How many files one drain **cooks**.
///
/// The same shape as the indexing slice, and for the same reason: cooking a file is a unit walk, a parse of the
/// rendering and an index of it, so a slice keeps the writer lock short enough that the server answers between two
/// of them. The rest stays in the list and the next drain takes the next few — see `Session::pending`, which is
/// what tells a caller there is work left.
// **How many files one cooking slice takes.** The drain itself is the pump's ([`Session::take_cooking_materials`]
// is what it calls), and this is the count that call passes — kept here because the number is a property of cooking
// rather than of a language server: one file is a render and a parse of it, measured at 9–269 ms on MSVC's headers.
#[allow(dead_code)]
/// The sharing cache's file name inside the cache directory — see [`Session::save_the_sharing`].
const SHARING_FILE: &str = "sharing.bin";
const COOK_SLICE: usize = 4;

/// **Where one `diagnostics` call spent its time**, on `CPPLS_TRACE_DIAGNOSTICS=1`.
///
/// The call has four parts and they have four different fixes: the **cooked lookup** (a hash), the **view** (the
/// file's own tree — a parse), the **module notes**, and the **checks**. Measured on MSVC's `<vector>`: the whole
/// call is **3.5 s on every request**, and the tree alone is 21 ms — so the split is not what the code around it
/// suggests, which is the whole reason this exists. Nested timings are printed as they end, one line each.
pub struct DiagTrace(Option<(&'static str, std::time::Instant)>);

impl DiagTrace {
    pub fn new(what: &'static str) -> DiagTrace {
        DiagTrace(crate::probes::diagnostics().then(|| (what, std::time::Instant::now())))
    }
}

impl Drop for DiagTrace {
    fn drop(&mut self) {
        if let Some((what, started)) = self.0 {
            eprintln!(
                "      [diagnostics] {what:<28} {:>7} ms",
                started.elapsed().as_millis()
            );
        }
    }
}

/// **How many files one drain re-reads**, once the closure is in hand.
///
/// The second of the two things a drain does that are proportional to the project, and the one that was measured
/// holding the writer for **10.9 s**: a file is re-read here when its reading may have changed now that its includes
/// are in the index, and after a cold start that is every file in the closure. Eight is small on purpose — the work
/// per file is a unit lookup and a re-index, and the queue the rest waits in is drained by the next call.
///
/// **Four is measured, not chosen.** The whole point of the number is the writer budget, so it was set by watching
/// that budget move: at **8** a slice held the writer for 1105 ms and at **1** for 150 ms, which is linear in the
/// count and says the re-read is the whole of what this constant controls. Four is the middle — the second pass
/// costs four times what one file does and the queue still drains at sixteen files a second.
///
/// What it does **not** control is a second cost in the same closure: even at 1, one slice in this project held the
/// writer for **1003 ms**. That is `units_for_the_pass` — the unit walk, which is one indivisible step because a
/// translation unit is built for a closure rather than for a file. It is the next thing to cut, and the budget
/// warning is what will say when it has been.
const RE_READ_SLICE: usize = 16;

/// **How much summary memory a session keeps before it forgets the biggest ones** — P2's byte budget.
///
/// The density it has to be a budget *of*, measured on the standard library (`tests/modules_probe.rs`, 401 files):
/// **52 MB of summaries and 25 MB of cooked readings** — 130 KB and 112 KB per file, because the STL's headers are the
/// heavy end of the distribution. At that density 8 000 such files hold a megabyte; a hundred-thousand-file project
/// holds **13 GB**, which is the case this exists for, and a 400-file closure holds 52 MB, which is not worth evicting
/// anything for. So the budget is set where it only fires on a project nobody could hold: **1 GiB of summaries**.
///
/// Forgetting is safe by construction: a summary that goes is re-read on demand ([`Session::needs_catching_up`]), which
/// is the same path a cache miss takes. What is *not* free is forgetting the wrong file — see
/// [`Session::enforce_the_summary_budget`] for the three kinds it never touches.
const SUMMARY_BUDGET_BYTES: usize = 1 << 30;

/// **The most items a project may ask a completion for** — the ceiling `[completion] max_items` is clamped to.
///
/// Twenty thousand items is a megabyte of JSON per keystroke and more rows than any client renders; the default is
/// [`crate::completion::ITEM_BUDGET`]. A budget of zero is refused for a different reason: it answers "there is nothing
/// here", which is a lie with no upside.
const MAX_COMPLETION_ITEMS: usize = 20_000;

/// **How many files a slice needs before its second pass shares one timeline instead of walking one per candidate.**
///
/// The second pass re-reads a slice's files and every file that defines a macro one of them mentions. A candidate
/// with no timeline falls to `a_timeline_of_its_own`, which walks that file's whole closure — and a slice can reach
/// far more candidates than it has files: measured on the modules fixture, **6 files reached 44 candidates and the
/// re-read cost 46.8 s**, which was the whole `import std;` read.
///
/// The shared timeline is not free of consequence: a reading built through it has the *slice root's* environment, not
/// the candidate's own, so it cannot be stored as that file's refinement. Four is where a fixture stops being a
/// fixture: below it, one walk per candidate is the cheaper side of that trade and the refinement stays cacheable.
///
/// # The per-candidate walk was tried again, and it is not the way
///
/// The argument for it is that its reading *is* cacheable — the environment is the candidate's own closure — so a
/// first start pays the walks and later ones serve them. Measured, with this constant forced to `usize::MAX` so no
/// slice ever shares, on the same fixture and the same cache:
///
/// ```text
/// shared (WALKS_WORTH_SHARING = 4):   cold 2.5 s     warm 1.1 s
/// own walks (no sharing at all):      cold 38.6 s    warm 66–104 s in three runs
/// ```
///
/// So the half of the argument about caching does not hold: the walks are paid again on every start, and a warm start
/// that walks is *forty to a hundred times* slower than one that shares. Whatever makes those readings unservable (the
/// closure key moving, or `commit_the_re_read` never storing them), the shared timeline is the cheaper default and
/// this constant is the lever that keeps it.
const WALKS_WORTH_SHARING: usize = 4;
/// **How many macro environments one drain builds.**
///
/// One, where [`COOK_SLICE`] is four, and the difference is **measured**: a cook is a unit walk over one file's
/// closure with the results indexed, while building a macro environment takes a copy of every file's text in the
/// closure — 103.7 ms for a 151-file closure against a view's 163.2 µs. A slice of four of those would be most of a
/// second in one drain, which is the thing the slice exists to prevent.
///
/// The queue is what makes a slice this small acceptable: a query that asked for an environment still answers from
/// the plain reading, so nothing is waiting on this — it is a reading being improved, not a request being served.
const MACRO_SLICE: usize = 1;

/// **How long a session waits after an edit before it reads a whole program.**
///
/// The one indivisible piece of work in this session: a unit read is a walk, a cook of every file in include order,
/// a parse of the whole program and a sweep of every declaration in it. Its preparing half takes the *read* lock, and
/// a keystroke takes the write one, so a reader typing during it waits for the whole program to be parsed.
///
/// So it is deferred by an idle period, and the number is a person's typing rhythm rather than a benchmark: at 1 s, a
/// reader who is mid-sentence defers it indefinitely (each keystroke pushes it out), and a reader who has stopped
/// gets the whole closure queryable a second later. The *bound* on what a keystroke can then wait for is
/// [`THE_LARGEST_PROGRAM_READ_AT_ONCE`], not this.
const UNTIL_A_UNIT_MAY_BE_READ: std::time::Duration = std::time::Duration::from_secs(1);

/// **How often a root's whole-program read is attempted before the session gives up on it.**
///
/// Each attempt is a *gather* — the walk, the texts — and a read declines when the closure cannot be walked (a
/// dependency the session has no text for). The first decline used to be final: the root was marked read, and measured
/// on a real project three of its own roots were never read because one attempt declined while the index was still
/// filling. Three attempts cover that without letting a genuinely unbuildable root be chosen on every wake — and a
/// fourth would not learn anything the third did not.
const PROGRAM_READ_ATTEMPTS: u8 = 3;

/// **The largest program this session reads in one piece, in bytes of the source text it is made of.**
///
/// A unit read is linear in its program: **5.09 s for 7.07 MB of source** (159 files, 737 525 tokens; measured in
/// `tests/unit_read_probe.rs` on a real project), i.e. 0.72 s per megabyte. Since the *reading* was split so that only
/// its 44 ms walk holds a lock (`Session::gather_a_unit_read` → [`UnitReadMaterials::read_the_program`] →
/// `Session::commit_a_unit_reading`), what this bounds is **background CPU and memory**, not a reader's keystroke:
/// the middle phase needs no lock, so a keystroke during it is answered beside it.
///
/// Eight megabytes is ~5.8 s of one core, which is the largest that stays in the same order as the index itself — and
/// it is deliberately **above** the program this was measured on (7.07 MB, MSVC's STL from one source), so a real
/// translation unit of a real project is covered. A closure larger than this is either pathological or a mistake
/// (a generated file included everywhere), and it keeps the per-file reading, which never blocks and never runs away.
const THE_LARGEST_PROGRAM_READ_AT_ONCE: usize = 8 * 1024 * 1024;

/// How many files [`Session::want_the_closure_cooked`] reaches for **one level below the direct includes**.
///
/// A payload bound, like [`COOK_SLICE`]: the rule above it is "a header's own headers are where the standard
/// library keeps what a reader asks for", and the cost of applying it without a bound is the 11.7 s the whole
/// closure took. Sixty-four covers the layer under every MSVC header that is a thin wrapper over another
/// (`<string>` → `<xstring>`, `<vector>` → `<xmemory>`, `<map>` → `<xtree>`); a translation unit that includes
/// the whole library wants [`Session::want_everything_cooked`], which is the caller's decision and not a default.
/// **How many files the second level beyond a file`s includes may cook** — see [`Session::want_the_closure_cooked`].
///
/// The first level (`COOK_ONE_LEVEL_FURTHER`, 64) reaches the headers a file includes; this one reaches the headers
/// *those* include, which is where a class a header uses is declared: MSVC`s `<string>` is a thin layer over
/// `<xstring>`, and `basic_string` is in `xstring`. Measured: cooking `xstring` took **2.17 s**, and without it
/// `std::string` answered `NotDeclaredHere` in a file that includes the header that includes the header that
/// declares it.
///
/// Smaller than the first level because it is further from the file being edited and is reached only when the first
/// level does not already answer.
const COOK_TWO_LEVELS_FURTHER: usize = 24;

const COOK_ONE_LEVEL_FURTHER: usize = 64;

/// How many files a project scan will list before it stops.
///
/// A bound on a directory walk whose size is a fact about a machine rather than about the project: a checkout with
/// a generated tree in it can be arbitrarily large, and a session that spent a minute listing it would delay the
/// first answer for a reason the user cannot see. Ten thousand translation units and headers is far past any
/// project that has been measured here, and the number is reported rather than silent — [`Session::project_files`]
/// is what a caller reads to see what the scan found.
const MAX_PROJECT_FILES: usize = 10_000;

/// The extensions a project scan treats as sources.
///
/// # Why a list here, when the event filter deliberately has none
///
/// `WatchFilter` refuses an extension allow-list for events, because `#include <vector>` has no extension and a
/// filter keyed on names would drop exactly the files an index most needs. A **scan** is a different question: it
/// is looking for the files a project *owns*, and it has no include to follow — so the alternative to a list is
/// guessing that every file in the tree is a source. Both halves of that distinction are the point:
///
/// ```text
/// an include   the file was NAMED — open it whatever it is called
/// a scan       the file was FOUND  — take it when its name says it is a source
/// ```
///
/// A header with no extension is still indexed: it arrives through an include, which is how a compiler finds it
/// too.
const SOURCE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cxx", "c++", "h", "hh", "hpp", "hxx", "h++", "ipp", "inl", "tpp", "tcc",
    "cppm", "ixx",
];

/// The files a session reads through: the editor's buffers in front of a fallback provider.
pub type SessionFiles<F = DiskFiles> = OverlayFiles<OpenDocuments, F>;

/// The documents an editor has open, as a provider.
///
/// # Why this exists next to [`crate::OverlayFiles`]
///
/// `OverlayFiles` is the *shape* — one provider in front of another — and it is generic over both halves. This is
/// the front half an editor needs: a set of buffers that changes while the analysis reads through it, keyed by
/// normalized path, and readable through `&self` because the handle is shared rather than borrowed — the session
/// holds one, and every caller that kept this handle holds another.
///
/// # What an open document means
///
/// The text in here is **the text**, and the file on disk is not consulted for it. That is the whole difference
/// between an analysis that works while a user types and one that works after they save: a buffer's includes
/// resolve against what has been typed, a buffer that has never been saved has no file at all, and a save is a
/// notification that the two now agree. An unsaved buffer is indexed as itself — see [`Session::advance`] — and its
/// summary is filed under the hash of *its* text, so the cache entry is correct the moment that text reaches a
/// disk and merely unused until it does.
///
/// # Why a lock rather than `&mut self`
///
/// Because two owners write here and they are not the same owner. A session writes through
/// [`Session::did_open`], which holds `&mut Session` and is the editor's ordinary notification path; a caller that
/// kept this handle writes without the session at all. An editor that had to borrow its session mutably to record
/// one keystroke would serialise every request behind that keystroke. A read-mostly lock is what lets an edit land
/// *between* two queries instead — this handle is written while a session reads through it. The lock is never held
/// across one.
#[derive(Debug, Clone, Default)]
pub struct OpenDocuments {
    buffers: Arc<RwLock<HashMap<String, String>>>,
}

impl OpenDocuments {
    pub fn new() -> Self {
        OpenDocuments::default()
    }

    /// A document is now open, with this text — LSP's `didOpen`, or any later text for the same path.
    ///
    /// One method for both because the analysis has one question about it: which text is this path's. The protocol
    /// distinguishes the two events; nothing here does.
    ///
    /// `&self` rather than `&mut self`, because the buffer map is behind a lock: a caller holding the handle can
    /// write a buffer while a session reads through it, which is the whole reason the handle exists.
    pub fn open(&self, path: impl AsRef<Path>, text: impl Into<String>) {
        self.buffers
            .write()
            .expect("the buffer map is not poisoned")
            .insert(normalize_path(path.as_ref(), cfg!(windows)), text.into());
    }

    /// The document is closed: the path is the filesystem's again.
    ///
    /// Returns whether there was a buffer to drop. The text is **not** kept: after a close, the disk is the answer,
    /// and holding the last buffer would answer with an edit that may never have been saved.
    pub fn close(&self, path: impl AsRef<Path>) -> bool {
        self.buffers
            .write()
            .expect("the buffer map is not poisoned")
            .remove(&normalize_path(path.as_ref(), cfg!(windows)))
            .is_some()
    }

    /// The buffer for a path, if it is open.
    pub fn text(&self, path: impl AsRef<Path>) -> Option<String> {
        self.buffers
            .read()
            .expect("the buffer map is not poisoned")
            .get(&normalize_path(path.as_ref(), cfg!(windows)))
            .cloned()
    }

    pub fn is_open(&self, path: impl AsRef<Path>) -> bool {
        self.buffers
            .read()
            .expect("the buffer map is not poisoned")
            .contains_key(&normalize_path(path.as_ref(), cfg!(windows)))
    }

    /// Every open document, in an unspecified order.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.buffers
            .read()
            .expect("the buffer map is not poisoned")
            .keys()
            .map(PathBuf::from)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.buffers
            .read()
            .expect("the buffer map is not poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl FileProvider for OpenDocuments {
    fn read(&self, path: &Path) -> Option<String> {
        self.text(path)
    }

    fn exists(&self, path: &Path) -> bool {
        self.is_open(path)
    }

    fn is_open_buffer(&self, path: &Path) -> bool {
        self.is_open(path)
    }
}

/// **What a session's translation units cost it** — the unit-side counterpart of [`crate::StoreStats`].
///
/// The three numbers are the three paths [`Session::translation_unit_of`] can take, and they answer the only
/// question worth asking about this cache: **is a keystroke paying for a walk?**
///
/// ```text
///   from_memory   this session already held the unit            — no read, no decode, no walk
///   from_disk     decoded from the cache, closure check included — a read and a hash per file
///   walked        the whole closure read and a timeline built    — and written for the next run
/// ```
///
/// Before the root of a unit was keyed on its preamble it moved on every keystroke, and no other
/// number in this crate would have shown it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnitStats {
    pub from_memory: usize,
    pub from_disk: usize,
    pub walked: usize,
}

/// Which of [`Session::translation_unit_of`]'s three paths answered — the argument to `Session::note_a_unit`.
#[derive(Debug, Clone, Copy)]
enum Which {
    FromMemory,
    FromDisk,
    Walked,
}

/// One open project: the configuration, the facts read so far, and the order the rest will be read in.
///
/// The provider chain is owned rather than borrowed, which is what lets a long-lived caller — a language server —
/// hold a session in a field. See the module documentation: the caller keeps a handle to the same chain by cloning
/// it before the session takes it, and a clone of a provider is a second handle on the same files.
/// **A file's configuration and its language, worked out once** — the value [`Session::config_for_file`] memoizes.
///
/// The language is kept beside the configuration because deriving it is not free and it was being derived per query:
/// [`CompilerConfig::language_for`] looks the file up in the compilation database *again* and re-parses `-std=` out of
/// the command line, for an answer that cannot change while the session lives.
#[derive(Debug)]
struct PerFileConfig {
    config: std::sync::Arc<CompilerConfig>,
    language: cpp_parser::features::LanguageConfig,
    cuda: bool,
}

/// **What a cached view was parsed from**, and the whole reason a stale one cannot be served.
///
/// A `FileView` is a function of exactly these seven things, and each of them moves for a reason:
///
/// ```text
/// path       one view per file, whatever spelling the caller used
/// text       the content hash of the text parsed — a rendering's, or the file's own for a buffer view
/// spans      how many rendering spans went with it: part of the identity of the offset mapping
/// id         the VFS's identity for the file, which a delete-and-recreate moves
/// cuda       the parse's vendor switch
/// language   the file's own language level (`-std=`), which changes what a name may mean
/// open       whether the text is a buffer rather than the file on disk
/// ```
///
/// The **text** is the load-bearing one, and it is why this needs no invalidation hooks: a rendering whose text
/// changed for any reason — a header edited, a macro moved, the closure grown — is a different key rather than a
/// stale hit. The rendering cache can rely on `retain(dependent)` for the same job because it is keyed by the
/// *dependent's* text; this one is keyed by what was actually parsed, which is strictly narrower.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ViewKey {
    path: PathBuf,
    /// **Which rendering this is**, by object — see [`RenderingId`]. `None` for a buffer view, which has none.
    ///
    /// By object rather than by the hash of its text, and the difference is the cost of a hit on a large file: hashing
    /// a 1 MB rendering is about a millisecond and would be paid on every hit, while comparing an `Arc` pointer is one
    /// word.
    rendering: Option<RenderingId>,
    /// The file's own text, by hash — what a **buffer view** was parsed from.
    text: u64,
    id: crate::file::paths::FileId,
    cuda: bool,
    language: Option<cpp_parser::features::LanguageConfig>,
    open: bool,
    /// **The macro environment a buffer view was parsed with**, when there was one. `None` is the empty environment,
    /// which is one value however often it is made.
    ///
    /// Held rather than identified by a raw address: an id that outlived its allocation could name a *different*
    /// environment that happened to be allocated where the old one was. The environment is an `Arc`, so holding it is
    /// one reference count — and the identity is `Arc::ptr_eq`, which is exactly "the same environment object".
    environment: Option<EnvironmentId>,
}

/// The identity of a rendering — **by object, held**, see [`ViewKey::rendering`].
#[derive(Debug, Clone)]
struct RenderingId(std::sync::Arc<crate::preprocess::cooked::RenderedCooked>);

impl PartialEq for RenderingId {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for RenderingId {}

impl std::hash::Hash for RenderingId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::sync::Arc::as_ptr(&self.0).hash(state);
    }
}

/// The identity of a macro environment — **by object, held**, see [`ViewKey::environment`].
#[derive(Debug, Clone)]
struct EnvironmentId(std::sync::Arc<cpp_parser::MacroEnvironment>);

impl PartialEq for EnvironmentId {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for EnvironmentId {}

impl std::hash::Hash for EnvironmentId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::sync::Arc::as_ptr(&self.0).hash(state);
    }
}

/// What a session is holding — see [`Session::cache_sizes`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheSizes {
    /// Cached renderings, by `(path, content hash)`. **Unbounded** — see [`Session::cache_sizes`].
    pub renderings: usize,
    /// The text of those renderings, in bytes.
    pub rendering_bytes: usize,
    /// Translation units held (bounded by `MAX_UNITS`).
    pub units: usize,
    /// Summaries in the index.
    pub summaries: usize,
    /// Declaration facts across them.
    pub facts: usize,
    /// Macro facts across them.
    pub macros: usize,
    /// **Cooked readings held**: files, and the declarations in them.
    pub cooked_files: usize,
    pub cooked_declarations: usize,
    /// **Parse trees held for reuse**: how many views, and their estimated tree bytes — see [`Session::view`].
    pub views: usize,
    pub view_bytes: usize,
}

/// **What one parsed view costs**, in the estimate the view cache's budget is stated in.
///
/// A rowan tree weighs 8–24× the text it describes, measured on two corpora (`docs/parser-review.md` §2): one
/// allocation per token with the token's bytes copied into it, one per node with its children. 8 is the low end and a
/// source file is the case that hits it, so `text × 8` plus a word per token is the estimate — deliberately an
/// under-estimate, because over-stating it would evict views that are cheap to keep and the budget is what matters,
/// not the last megabyte.
///
/// **Why not the rendering weight**: that is `text + spans × 16`, which is what the *rendering* cache is bounded by.
/// The two caches hold different things and a view is the larger one.
fn a_view_weighs(view: &FileView) -> usize {
    view.source.len().saturating_mul(8) + view.tree.get_tokens().len().saturating_mul(16)
}

pub struct Session<F: FileProvider = DiskFiles> {
    root: PathBuf,
    config: CompilerConfig,
    /// What `discover` found, or `None` when no compiler answered. Kept for a human reading a log line, and for
    /// 's measurements.
    toolchain: Option<Toolchain>,
    /// **What this analysis thinks the project is**: the configuration file, the compile database and where it was
    /// found, and every problem. Kept whole rather than reduced to the parts the analysis uses,
    /// because the first question anybody asks about a wrong answer is "what did it think this project was" — and
    /// the sections a language server reads ([`crate::DiagnosticsSection`], [`crate::HoverSection`]) live in the
    /// same struct, since one file is parsed once, in one place.
    discovery: ProjectDiscovery,
    /// The providers. Owned; the store holds a second handle to the same chain.
    files: SessionFiles<F>,
    /// The buffer half of `files`, as a handle this session can write through.
    documents: OpenDocuments,
    /// **The files this session is holding** — their text and their line indexes, kept in step with each other.
    ///
    /// The text source for everything a *cursor* query needs, and the reason a view costs a pointer rather than a
    /// scan: see [`crate::file::vfs`]. It reads through the same provider chain the store does, so a buffer and a
    /// file are the same question to both.
    vfs: Vfs<SessionFiles<F>>,
    store: SummaryStore<SessionFiles<F>>,
    /// Which paths the client's events and the project scan are about. Kept because both questions — "is this
    /// event interesting" and "is this file part of the project" — have one answer, and the cache directory is the
    /// case that makes that concrete: it is not project source and it is not an event anybody wants.
    filter: WatchFilter,

    /// **Whether the walk cache's cross-walk contribution sharing is used.** Off unless the A/B probe
    /// (`CPPLS_WALK_SHARE`) asks for it: the sharing replays a file's contribution at a position the compiler would
    /// not, and that drops include edges — measured on `import std;`, `<limits>` lost `<cwchar>` and `<xstddef>`.
    share_contributions: bool,

    /// What the project scan found, so that a configuration change can re-seed from it.
    project: Vec<PathBuf>,
    queue: Work,
    /// The files this session has **parsed** since the last body pass — the candidates for
    /// [`SummaryStore::re_read_where_a_body_decides`], which may only run once the closure is in hand.
    ///
    /// Accumulated across `advance` calls rather than taken from one: a closure is read in chunks (64 files at a
    /// time), and a file parsed in the first chunk is exactly as much a candidate as one parsed in the last. Taking
    /// only the chunk that happened to drain the queue is what left MSVC's `<xstring>` and `<vector>` — both read in
    /// the first chunk, both scoped by a macro body defined in a later one — reading at file scope for ever.
    parsed_since_the_last_pass: Vec<PathBuf>,
    /// **The files somebody has asked to have refined** — phase 0 of `docs/indexing-design.md`.
    ///
    /// The second pass used to drain the whole backlog at startup: on `EmmyLuaCodeStyle` that was 1745 files and
    /// **59–120 s**, for answers about files the reader had not opened. A start now refines nothing; `catch_up` marks
    /// the file a request or an open is about, and the pump refines only what it finds here. The backlog stays for the
    /// files nobody asked about — they are refined the first time somebody does.
    ///
    /// **Behind a lock of its own**, because the thing that asks is a *request*: it holds the session for reading, and
    /// taking the write lock to record a `HashSet` insert is the wait this whole phase exists to remove.
    refined_on_request: std::sync::Mutex<std::collections::HashSet<PathBuf>>,
    /// **The files whose reading in the index is a refined one** — what [`Session::is_refined`] answers, and what
    /// phase 1 needs to know when an answer is still the shape-only reading.
    refined_readings: std::sync::Mutex<std::collections::HashSet<PathBuf>>,
    /// **The open files whose cooked reading is missing or stale** — what [`Session::cook`] is called for when the
    /// queue drains.
    ///
    /// A queue rather than "all open files, every time", for two reasons that are both about cost: a drain happens
    /// after every keystroke, and re-cooking a file whose text did not change is a unit walk and a parse for an
    /// answer the index already holds. A file joins this when a buffer changes ([`Session::did_change`] and
    /// friends) and when it is opened, because those are the two moments its reading can be out of date — a file
    /// whose summary came from the **disk cache** was never parsed in this run, so nothing else would notice that it
    /// has no cooked reading at all.
    cooking: Cooking,
    /// **Every `#define` this session has read back**, by the file and offset it was written at — see
    /// [`crate::MacroDefinitions`].
    ///
    /// One per session rather than one per cook, because the expensive part is reading a definition out of a file's
    /// text and the same definition is in force for hundreds of files: project-wide cooking turned a per-cook cache
    /// into the difference between 4.5 s and 12.7 s for 255 files (measured; the census's own note records the same
    /// effect at 40 s). Invalidated **per path** when a file changes, since the keys are offsets in that file.
    /// **The macro definition cache, behind its own lock** — see [`Session::held_definitions`].
    definitions: std::sync::Mutex<crate::MacroDefinitions>,
    /// **The project's translation units that have been read as programs** — see [Session::read_the_unit].
    ///
    /// Cleared wherever [Session::units] is: a unit read is a reading of the same timeline, so anything that
    /// moves inside it makes this list a claim about a reading that no longer describes its closure.
    units_read: std::collections::HashSet<String>,
    /// **Translation units the session has been told to read as programs** that are not open documents — the module
    /// interface units, in practice.
    ///
    /// `import std;` names a module, not a file, so nothing in the project includes `bits/std.cc`: it arrives through
    /// [`Session::queue_the_modules_a_file_imports`], and from then on it is one of the project's units. A compiler
    /// reads it as a translation unit — the whole closure, rendered in *its* environment — which is the only reading
    /// in which the library's own scopes are right: read raw, `shared_ptr_base.h`'s `_Sp_counted_ptr_inplace` never
    /// closes (its class head is written three times under `#if`/`#else`) and every class after it is filed as its
    /// nested class.
    ///
    /// [`Session::the_next_unit_to_read`] considers these after the open documents, so the pump reads a module once,
    /// at the same budget as any other unit read, and never before a reader's own file.
    programs_wanted: std::collections::HashSet<String>,
    /// **How often a root's program was attempted and declined** — see [`Session::commit_a_program`]. A root that
    /// cannot be walked yet is retried, but not for ever: three attempts and it keeps the reading the per-file path
    /// gave it.
    units_attempted: std::collections::HashMap<String, u8>,
    /// **The findings one file's last pull produced, and the state they were produced from.**
    ///
    /// A pull builds a `SemanticModel` (it borrows the tree and holds a non-`Send` cache of header views, so it cannot
    /// outlive the call) and runs every rule over it — measured **143.4 ms** on `EmmyLuaCodeStyle`, 97 % of it type
    /// inference. The findings themselves are plain data and a function of everything the pull reads, so the *answers*
    /// are what is remembered; the key names the state ([`Session::the_state_of_a_pull`]).
    checks_memo: std::sync::Mutex<Option<(u64, Vec<crate::sema::check::Finding>)>>,
    /// **When this session last took an edit** — the clock the one indivisible piece of work in it is deferred by.
    ///
    /// A unit read ([`Session::prepare_a_unit_reading`]) is a walk, a render and a **parse of a whole program** in one
    /// piece: 5 s measured on a 159-file closure. It is `&self` work and it runs beside queries, but a *keystroke*
    /// takes the write lock and would wait for it — so nothing in a session may start one while a reader is typing.
    /// [`Session::a_unit_may_be_read_now`] is that rule, and this is the instant it is measured from.
    last_edit: std::time::Instant,
    /// **The last program this session indexed, and what it was** — one entry, keyed by the stream's own bytes.
    ///
    /// A unit read is a walk, a render, a **parse of the whole program** and a sweep of every declaration in it, and
    /// the reading is a pure function of the stream: the same bytes give the same facts. Sources that include the
    /// same headers therefore produce the **same** stream — measured on twenty sources each including `<future>`, all
    /// twenty render to 617 607 tokens, byte for byte — and re-parsing it nineteen more times is work nobody asked
    /// for.
    ///
    /// # What the measurement was
    ///
    /// Twenty sources, one shared header set, on this machine:
    ///
    /// ```text
    ///                  ms        share of the run
    /// [indexing]     131 322       71.9%     ← the render's parse and sweep, per unit
    ///   render-parse  59 190       32.4%
    ///   render-sweep  72 133       39.5%
    /// [units]         47 963       26.2%
    ///   unit-render   45 489       24.9%     ← the render itself, also per unit
    /// ```
    ///
    /// 179 seconds for twenty units is 300-odd for forty, which is the number this exists to remove. The cache is
    /// one entry rather than a map because the sources of one project include the same headers **in runs**: a build
    /// walks them in directory order, and the entry that helps is the one just computed.
    ///
    /// Keyed by [`crate::cache::content_hash`] of the stream so nothing large is kept twice, and dropped wherever the
    /// index or the configuration moves, because a program's reading is only valid for the configuration that read it.
    ///
    /// # Why it is behind a lock
    ///
    /// Because the read that fills it is the expensive half of a unit read — a render, a parse and a sweep — and that
    /// half has to be payable **outside the write lock** ([`Session::prepare_a_unit_reading`]) if a whole program is
    /// ever to be read as one. Same reason, and same shape, as [`Session::units`].
    unit_index: std::sync::Mutex<Option<(u64, crate::IndexedUnit)>>,
    /// **The last unit this session rendered, and what its inputs were** — one entry, keyed by the *content* of
    /// every file the walk read.
    ///
    /// A render is a walk, a cook of every file in include order, and a stitched stream of megabytes; the plan's
    /// §5.1 M4 exists because the second one costs exactly as much as the first. What a reader does is ask for the
    /// same unit again and again — every keystroke that lands on a name, every drain — so the second answer is the
    /// one worth having cheap.
    ///
    /// # The key is the inputs, not the root path
    ///
    /// Keyed on the root alone it would answer with a stream built from text the files no longer have: a file the
    /// reader has open and has **not saved** comes from the overlay, and an edit changes the program without changing
    /// its name. So the key is a hash of `(path, content hash)` for every file the walk reached, which is exactly
    /// what the render is a function of. **One entry**, because the interesting repetition is the same unit asked
    /// for twice in a row, and a map would trade a render for a memory of every stream the session ever built.
    ///
    /// Behind a lock for the same reason [`Session::unit_index`] is.
    rendered_unit: std::sync::Mutex<Option<(u64, crate::RenderedUnit)>>,
    /// **How many items one completion may answer with** — `[completion] max_items` in `.cppls.toml`, clamped.
    ///
    /// A **payload** bound rather than a UI one: the client renders ten rows and receives all of them, and an
    /// untruncated `std::` would be 12 867 items and megabytes of JSON per keystroke. What makes the bound harmless is
    /// that a cut list reports itself (`isIncomplete: true`), so the client asks again on the next keystroke and the
    /// prefix filter — applied in the index before anything is cloned — narrows the answer exactly. Measured on the
    /// reader's project: `std::` 1000 items in 53 ms (truncated), `std::s` 167 in 7 ms, `std::st` 55, `std::str` 38,
    /// `std::string` 6 — exact from one letter on.

    /// **Which key a root's rendering was filed under** — `(root, context) → units_key`, for this session.
    ///
    /// The walk exists to produce that key and costs **70.7 ms** on the standard library's 256 files. A rendering
    /// records the walk's own file list (`RenderedUnit::walked`), so the key can be reproduced from the rendering
    /// alone — and validated against those files' texts, so an edit between two asks is a miss rather than a stale
    /// answer.
    unit_roots: std::sync::Mutex<std::collections::HashMap<(PathBuf, u64), u64>>,
    /// **What each edited file's directives said the last time it changed**, by the path the queue keys on.
    ///
    /// The comparison an edit is judged by: an edit that leaves a file's directives where and what they were cannot
    /// have changed the environment any file reads it in, or the timeline of any unit it is part of — so it drops
    /// neither. See [`crate::preprocess::directive::DirectiveSignature`].
    directive_signatures:
        std::collections::HashMap<String, crate::preprocess::directive::DirectiveSignature>,
    /// **Files already read and parsed, waiting for the queue to reach them** — see [`Session::advance`].
    ///
    /// Bounded by what one `advance` asked for, and emptied by anything that could make a prepared answer describe a
    /// text the file no longer has: an edit, a close, a filesystem event. It is a read-ahead, never a source of
    /// truth: an entry that is not here is simply read when its turn comes.
    prefetched: HashMap<String, crate::index::store::Prepared>,
    /// **The translation unit each cooked file was read in**, kept across cooks.
    ///
    /// The one-walk timeline ([`crate::TranslationUnit`]) is what makes a *file's* environment a position in a walk
    /// rather than a walk of its own — but [`Session::cook`] was building one **per cooked file**, so cooking the
    /// 138 files of one real project's closure walked that closure 138 times. The compiler's answer to the same
    /// repetition is a preamble, and the crate already has it on disk ([`crate::TranslationUnitCache`], keyed on the
    /// content of every file the walk entered); this is the in-session half, so that a second cook of the same
    /// translation unit does not even decode the entry again.
    ///
    /// Cleared whole by any change: a unit is a reading of its closure, and the shortest correct invalidation is
    /// "nothing in it may have moved".
    ///
    /// **Bounded** ([`MAX_UNITS`]): a unit holds a whole closure's timeline, and a project with a thousand sources
    /// would otherwise keep one per source it was ever asked about.
    /// **The units table, behind its own lock rather than behind `&mut self`.** See [`UnitTable`].
    ///
    /// It was a plain field, which made [`Session::translation_unit_of`] take `&mut self` — and a unit build is the
    /// single most expensive thing a drain does: measured from the server's write-budget warning, **1003 ms**, on
    /// the first slice after a cold start. `&mut self` is what put it *inside* the writer, where every request in
    /// flight waits for it. A lock of its own lets the unit walk happen under the **read** lock instead, beside
    /// whatever a person is typing, which is the whole point.
    units: std::sync::Mutex<UnitTable>,
    /// **A timeline already walked in this run, by what makes it the same timeline.**
    ///
    /// A `TranslationUnit` is a function of its closure's **content** and the compilation context — which is exactly
    /// what [`crate::tu_cache`]'s on-disk key says, and why a warm run reports `reused 1494 / rebuilt 235`: the disk
    /// already shares what is shareable *between* runs. What it cannot do is share **within** one, because nothing is
    /// on it yet.
    ///
    /// Measured on `EmmyLuaCodeStyle`: 2110 roots, **786 distinct closures**, and **977 walks (46%) that a shared
    /// result would remove**. The largest single group was 638 roots with the same (empty) closure. Those repeats are
    /// walked one after another, each re-evaluating every guard of every file it reaches — 68 s of a 186 s index in
    /// `walk-macro-guard` alone.
    ///
    /// The key is the context plus a hash of the closure's files **by content**, so two roots whose closures hold the
    /// same text share one timeline whatever the paths are, and a root whose closure differs by one byte of one
    /// header is walked on its own. See [`Session::closure_identity`].
    ///
    /// A `Mutex` of its own rather than the units table's: the two are read at different points of the same call, and
    /// holding one across the other is how a re-entrant lock becomes a deadlock.
    ///
    /// **Bounded in bytes** ([`MAX_WALKS_BY_CONTENT_BYTES`]), not in entries: a timeline measures 0.1–6.2 MB, so a
    /// count would be a budget that says nothing about what it holds. Least recently used goes first; a unit larger
    /// than the whole budget is still kept, because the walk that just produced it is the one a query is about to
    /// use and rebuilding it is the most expensive thing this session can do.
    walked_by_content: std::sync::Mutex<
        cache::cache_lru::Cache<(u64, u64), std::sync::Arc<crate::TranslationUnit>>,
    >,
    /// **The headers the include search path can name**, read once — what `#include` is completed from.
    ///
    /// Built with the session rather than on the first completion, and the reason is the budget rather than the
    /// cost: this walks the `-I` directories with a depth bound and a file count, so the work is bounded and
    /// happens once, while doing it lazily would put a filesystem walk on the first keystroke after a `#include`.
    ///
    /// Behind a lock because the **project's** half of it grows: [`crate::Session::add_project_files`] re-derives
    /// that half when a build system hands over more translation units, and a completion may be asked for before,
    /// during or after. The search-path half never changes after this field is built.
    headers: crate::SharedHeaders,
    /// **The macros a file's closure defines, remembered by the content they were read from.**
    ///
    /// [`Session::view_with_macros`] walks the file's whole closure and takes a copy of every file's text to build
    /// one of these, and that is **measured at 106.7 ms** beside 167.8 µs for a view without it — 635×, on a
    /// closure of 151 files. Which is affordable once and not per query, so it is kept.
    ///
    /// The key carries the **content hash** rather than the path: a buffer being typed in has a new text on every
    /// keystroke and the same path, and an environment built from the old text would be a wrong answer rather than
    /// a stale one. Two files with the same content share an entry, which is right — the environment is a reading
    /// of that text and its closure.
    ///
    /// Behind a lock because the queries that want it hold a read on the session, and an environment is only ever
    /// added (§[`Session::view_with_macros`] takes `&self`).
    macro_environments: std::sync::Mutex<
        cache::cache_lru::Cache<(PathBuf, u64), std::sync::Arc<cpp_parser::MacroEnvironment>>,
    >,
    /// **Files whose macros something has asked for and not yet got.**
    ///
    /// The other half of the arrangement [`Session::macro_environments`] describes: a query cannot pay 103.7 ms,
    /// so it asks for the environment and reads the plain view, and the **work loop** builds it — the same shape
    /// [`Session::want_cooked_reading`] and `Cooking` have, for the same reason. Behind a lock because the asking
    /// happens in a query (`&self`) and the building happens in the loop (`&mut self`).
    macro_work: std::sync::Mutex<Cooking>,
    /// **The run's parsed macro definitions** — see `ParsedDefinitions`: a body parses once per run, not once per
    /// file that sees it. Keyed by the definition's own text, so it needs no invalidation: the same text is the same
    /// definition, and a file that changed contributes a different one.
    parsed_definitions: std::sync::Mutex<crate::ParsedDefinitions>,
    /// **A file's `CompilerConfig`, worked out once** — see [`Session::config_for_file`].
    ///
    /// [`CompilerConfig::for_file`] clones the whole configuration, applies the project's `remove_args`/`extra_args`
    /// and looks the file up in the compilation database, and it was being called by every `view` — **after** the
    /// parse became a cache hit, it was the whole cost of one (measured: 0.28 ms per hit on an 11 KB file, against
    /// 0.3 ms for the parse it was hiding behind).
    ///
    /// **No invalidation, and the reason is structural**: a session's configuration is built once by `Session::open`
    /// and nothing reassigns it. A configuration that changes is a *new session* (the server replaces the analysis
    /// state), which is the same rule the rest of this type follows.
    config_for_file: std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<PerFileConfig>>>,
    /// **The renderings built for `view`**, remembered against the content they were read from.
    ///
    /// The same arrangement as [`Session::macro_environments`] and for the same reason: a rendering is a unit walk
    /// and a preprocess, a query cannot pay it, and the file it is asked about does not change between two
    /// keystrokes. Keyed by the **content hash** rather than the path, because a buffer being typed in has a new
    /// text on every keystroke and a rendering of the old text would be a wrong answer rather than a stale one.
    /// **Surfaces already scanned**, shared by every effect walk — see [`crate::effect::Surfaces`].
    surfaces: std::sync::Mutex<crate::effect::Surfaces>,
    /// **Effects already computed**, shared by every walk — the other half of what makes a walk hash lookups.
    effects: std::sync::Mutex<HashMap<u64, crate::effect::Effect>>,
    renderings: std::sync::Mutex<
        cache::cache_lru::Cache<
            (PathBuf, u64),
            std::sync::Arc<crate::preprocess::cooked::RenderedCooked>,
        >,
    >,
    /// **Views already built**, by what they were parsed from — the answer to "a pull is a parse per request".
    ///
    /// [`Session::view`] parses a file's **rendering**, which for a header-heavy file is its whole closure; measured
    /// on a 0.74 MB unit at the parser's 6 MB/s, that is **120 ms per call**, and a hover, a pull, an inlay hint and
    /// a semantic-token request each call it. The tree is the expensive half and nothing was remembering it: the
    /// rendering cache holds the rendered *text*, and the parse was made again every time.
    ///
    /// # Why this key cannot serve a stale view
    ///
    /// A view is a function of: the text parsed (the rendering's), the rendering's spans (the offsets between the
    /// rendering and the file), the parse's own configuration (`cuda`, the language level), whether the buffer is
    /// open, and the file's identity. All seven are in [`ViewKey`], and the text is keyed by its **content hash** —
    /// the same rule the rendering cache uses, and stricter here, because a rendering whose text changed for any
    /// reason (a header edited, a macro moved) is a different key rather than a stale hit. A config change moves the
    /// language, an open/close moves `open`, a re-read of the file moves the text. Nothing else can change what a
    /// parse of that input means.
    ///
    /// `FileView` is cheap to hand out because its tree and its scopes are behind `Arc`s: a hit is a handful of
    /// reference-count bumps, which is what makes a cache worth having at all (a clone that copied the token stream
    /// would cost twelve bytes per token — megabytes for the units this exists for).
    ///
    /// The value is a [`crate::file::view::ParsedView`] and not a [`FileView`]: a view holds rowan's red root, which is
    /// neither `Send` nor `Sync`, and a session is shared across the server's threads. The root is one allocation,
    /// made per hit — see `FileView::from_parsed`.
    views: std::sync::Mutex<
        cache::cache_lru::Cache<ViewKey, std::sync::Arc<crate::file::view::ParsedView>>,
    >,
    /// **The summary of a rendering**, by `(path, content hash of the rendered text)`.
    ///
    /// A rendering is what the preprocessor resolved, so a declaration a directive splits has one answer there —
    /// `reference` for libstdc++'s `emplace_back` — where the file's own text has two. Reading it needs the *summary*
    /// of the rendered text, which is a parse: measured at **20.2 ms, and 19.1 ms again** on the second ask
    /// (`tests/rendering_real.rs`), because the rendering is cached by the cook path and its summary was not.
    rendering_summaries: std::sync::Mutex<
        cache::cache_lru::Cache<(PathBuf, u64), std::sync::Arc<crate::FileSummary>>,
    >,
    ///
    /// `cooking_materials` answers `None` for a file whose unit is provisional, whose summary is not in the index, or
    /// whose render declines — and the caller that wanted it (`want_the_closure_cooked`, once per drain) wants it
    /// again. Measured on a real session, that is a loop: the server's own log repeated
    ///
    /// ```text
    /// index slice: prepared 0, committed 0, write lock held 0 ms, 4 to cook, 1 left
    /// index slice: prepared 0, committed 0, write lock held 0 ms, 4 to cook, 2 left
    /// index slice: prepared 0, committed 0, write lock held 0 ms, 2 to cook, 0 left
    /// ```
    ///
    /// every second for as long as the window stayed open, at 28 % of a core and 916 MB.
    ///
    /// A refusal is not a conclusion about the file, only about *this* moment: [`Session::want_cooked_reading`] — a
    /// query that names the file — clears it and queues again, so nothing is lost but the retry.
    cooking_refused: std::sync::Mutex<std::collections::HashSet<String>>,
    /// **What this session's units have cost it** — see [`UnitStats`], which says what the three numbers mean.
    ///
    /// Atomics rather than one `Mutex<UnitStats>`: `translation_unit_of` takes `&self` and runs on the pump's read
    /// side, where the pump and a request can be inside it at once. Nothing here decides an answer, so a relaxed
    /// order is all a counter needs.
    unit_from_memory: std::sync::atomic::AtomicUsize,
    unit_from_disk: std::sync::atomic::AtomicUsize,
    unit_walked: std::sync::atomic::AtomicUsize,
    /// **How many times each root's timeline has been *walked*** — the diagnostic that answers "who keeps asking".
    ///
    /// `UnitStats::walked` is the total, and a total cannot tell one root walked two hundred times from two hundred
    /// roots walked once. The difference is the whole of the next optimisation, and this is the measurement that
    /// decides which one it is — see [`Session::walks_by_root`].
    walks_by_root: std::sync::Mutex<std::collections::HashMap<String, u64>>,
}

/// **One `import` a file writes**, as a link or a jump can use it — see [`Session::module_imports_of`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleImport {
    /// The module's name as written: `math`, `std.compat`.
    pub name: String,
    /// The **name's** own span in the file — what a document link covers, not the whole declaration.
    pub range: cpp_parser::SourceRange,
    /// The interface unit the name reaches, when the analysis can point at one.
    pub resolved: Option<PathBuf>,
}

/// **The macros the C and C++ *standards* define** — every compiler defines these, whatever the project's build system
/// says, and their values follow from the language the file is read as.
///
/// `ext
/// __STDC__            1                     both families
/// __STDC_HOSTED__     1                     both families (a freestanding implementation says 0 and this cannot know)
/// __STDC_VERSION__    201710L / 202311L     C99 and later, by the file's standard
/// __cplusplus          201703L / 202302L    every C++ standard
/// `
///
/// `None` for a name that is not one of them, and for `__STDC_VERSION__` in C89 — where it is deliberately undefined,
/// which is exactly the question `#ifdef __STDC_VERSION__` asks.
/// The **last directive that settles `name`** at or before `offset`, as its own text.
///
/// ```text
/// #define MAX(a, b) ((a) > (b) ? (a) : (b))      → the whole line, `\`-continuations joined
/// #define COUNT  10                            → `#define COUNT  10`
/// #undef  COUNT                               → `#undef COUNT` (the name is settled, and by an un-definition)
/// ```
///
/// A text scan rather than a parse: it is asked about the file the reader is in, at the moment they hover, and a
/// directive is the one construct whose meaning is entirely in its first line. `None` when no directive mentions the
/// name, or the last one *after* `offset` — a `#define` written below the point asked about does not apply there.
fn the_directive_defining(name: &str, text: &str, offset: usize) -> Option<String> {
    let mut settled: Option<String> = None;
    let mut at = 0usize;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        if start > offset {
            break;
        }
        let trimmed = line.trim_start();
        let rest = if let Some(rest) = trimmed.strip_prefix("#define") {
            rest.trim_start()
        } else if let Some(rest) = trimmed.strip_prefix("#undef") {
            rest.trim_start()
        } else {
            continue;
        };
        // The name is the first identifier of what follows, and the directive is `#define NAME` or `#define NAME(…)`.
        let end = rest
            .find(|character: char| !(character.is_alphanumeric() || character == '_'))
            .unwrap_or(rest.len());
        if &rest[..end] != name {
            continue;
        }
        // A definition only counts if it is written *before* the point asked about — or is the one the cursor is on.
        if start > offset {
            continue;
        }
        let mut directive = trimmed.trim_end().to_string();
        // `\`-continuations: a body written on the next line is part of the same directive.
        while directive.ends_with('\\') {
            let next_start = at;
            let Some(next) = text[next_start..].split_inclusive('\n').next() else {
                break;
            };
            at += next.len();
            directive.pop();
            directive.push_str(next.trim_end());
            if next.trim_end().ends_with('\\') {
                continue;
            }
            break;
        }
        settled = Some(directive.trim_end().to_string());
    }
    settled
}
fn the_standards_own_macro(
    name: &str,
    language: &cpp_parser::features::LanguageConfig,
) -> Option<String> {
    use cpp_parser::features::{Family, Standard};
    let family = language.family();
    let c_standard = language
        .standards()
        .filter(|standard| standard.family() == Family::C)
        .max();
    let cpp_standard = language
        .standards()
        .filter(|standard| standard.family() == Family::Cpp)
        .max();
    match name {
        "__STDC__" if family != Some(Family::Cpp) => Some("1".to_string()),
        "__STDC_HOSTED__" if family != Some(Family::Cpp) => Some("1".to_string()),
        "__STDC_VERSION__" if family != Some(Family::Cpp) => match c_standard? {
            Standard::C89 => None,
            Standard::C99 => Some("199901L".to_string()),
            Standard::C11 => Some("201112L".to_string()),
            Standard::C17 => Some("201710L".to_string()),
            Standard::C23 => Some("202311L".to_string()),
            // A C++ standard in the C slot cannot happen (max per family), and answering nothing is right.
            _ => None,
        },
        "__cplusplus" if family != Some(Family::C) => match cpp_standard? {
            Standard::Cpp11 => Some("201103L".to_string()),
            Standard::Cpp14 => Some("201402L".to_string()),
            Standard::Cpp17 => Some("201703L".to_string()),
            Standard::Cpp20 => Some("202002L".to_string()),
            Standard::Cpp23 => Some("202302L".to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// **The standard's macros whose value depends on where they are written** — `__LINE__`, `__FILE__`, `__DATE__`,
/// `__TIME__`, and C++11's `__COUNTER__`.
///
/// They are macros in every translation unit, exactly as `__cplusplus` is, but neither table this session has can say
/// so: `-dM` does not print them (measured — `g++ -dM -E -x c++ NUL` answers `#define __cplusplus 201703L` and none of
/// these), and [`the_standards_own_macro`] answers *values*, which these have none of that a reading could fold:
/// `__LINE__` is the line it stands on. What a caller needs for a colour is the one bit — "this name is a macro" — so
/// this is its own list rather than a fake value in the other one.
fn a_macro_whose_value_depends_on_the_use(name: &str) -> bool {
    matches!(
        name,
        "__LINE__" | "__FILE__" | "__DATE__" | "__TIME__" | "__COUNTER__"
    )
}
impl Session<DiskFiles> {
    /// Open a project the way a language server does: ask the machine what it compiles with.
    ///
    /// The four steps, in the order their results depend on each other:
    ///
    /// ```text
    /// 0. .cppls.toml, if the project has one  — exclusions, source extensions, and how it says it is built
    /// 1. compile_commands.json                — the flags and the file list
    /// 2. discover(files, …)                   — which compiler, and where its own headers are
    /// 3. the scan                             — the sources the project owns
    /// ```
    ///
    /// **The compiler is run**, once, with `-E -v -x c++ -` to print its search list. That is a process and
    /// therefore tens of milliseconds, paid once at open; a project with no compiler on the machine is not an
    /// error, and [`Session::toolchain`] answers `None` — the analysis then resolves the project's own headers and
    /// reports `<vector>` as unresolved, which is the honest answer rather than a silently empty index.
    ///
    /// The file list is the **queue's seed**, not a list to consult later: opening a project is the caller saying
    /// "index this", so [`Session::pending`] counts the project from the first call and [`Session::advance`] has
    /// something to do before anything is opened.
    pub fn open(
        root: impl Into<PathBuf>,
        files: SessionFiles<DiskFiles>,
        filter: WatchFilter,
    ) -> Session<DiskFiles> {
        Session::open_with_config_file(root.into(), files, filter, None)
    }

    /// [`Session::open`] for a caller that knows where the configuration file is (`--config`).
    ///
    /// A named file that is not there is reported rather than ignored — an instruction is not a convention — and
    /// everything else proceeds as if no configuration had been given.
    pub fn open_with_config_file(
        root: PathBuf,
        files: SessionFiles<DiskFiles>,
        filter: WatchFilter,
        config_file: Option<&Path>,
    ) -> Session<DiskFiles> {
        // **What the project is** — its own file and its build — worked out before anything is
        // asked of a compiler, because the first half decides *which* compiler gets asked (`named_compilers`) and
        // what flags it is asked about.
        let discovery = crate::project::discover(&files, &root, config_file);
        let project_config = discovery.config.clone();

        // **The project's own language wins.** `locale = "zh-CN"` in `.cppls.toml` is a statement about this
        // repository, and the editor's setting (`initialize.locale`) is a statement about the editor — so the file is
        // applied here, after the client's has already been applied. Applying it in the *analysis* rather than in the
        // server is what keeps the two in one order: whoever opens a project gets that project's messages, including
        // the messages produced while it is being indexed.
        if let Some(locale) = project_config.config.locale.as_deref() {
            cpp_parser::set_locale(locale);
        }

        let filter = apply_project_config(filter, &project_config);

        let for_file = discovery
            .database
            .as_ref()
            .and_then(|database| database.commands.commands.first())
            .map(|command| command.file.clone())
            .unwrap_or_else(|| root.clone());

        // **Which flags won, and then what the project does to them.** `[compile].args` is the compilation when a
        // person wrote it, and the database's first entry when the project's build says so. Either way the result
        // goes through one pass — `remove_args` drops,
        // `extra_args` appends — so that the three keys have one implementation and one order, whichever list they
        // start from.
        // **The project-wide answer, which a database can only approximate.** Its first entry is *a* compilation
        // rather than the project's — with one entry per translation unit there is no such thing — and this is the
        // one place that has to live with that: the defines here are what the **cook** seeds every file's macro
        // environment with, and a per-file define cannot reach that yet. The include paths, which are what decide
        // *which* header a file resolves, are per file where they are used — see `CompilerConfig::for_file`, which
        // the include resolver asks.
        let base = project_config_from_flags(
            &discovery.flags(),
            discovery.working_directory(),
            &project_config.config.compile,
        );

        // **The project root, so that an include nothing could place can be looked for in the tree.**
        //
        // The one search that is about where files *are* rather than where a compiler looked, and the reason it is
        // needed at all is that a Visual Studio solution cannot export a compile database and a checkout nobody has
        // configured has none. `[compile].fuzzy_includes` decides whether it runs; the root is recorded either way,
        // because a flag that is off must not make the rest of the configuration differ.
        let base = CompilerConfig {
            root: Some(root.to_path_buf()),
            // **On by default where it is the only thing that can work.** The flag is tri-state: `true` and `false`
            // are a project saying so, and *absent* means "as the situation decides" — on when the project has no
            // compile database and off when it has one. A database states where the compiler looked, so a guess can
            // add nothing there; without one, the alternative is a closure of fourteen files instead of the program,
            // which is what a Visual Studio solution and an unconfigured checkout both produce.
            fuzzy_includes: project_config
                .config
                .compile
                .fuzzy_includes
                .unwrap_or(discovery.database.is_none()),
            targets: crate::project::targets_for(&project_config.config.compile),
            // **The libraries a target may depend on** — `[[library]]`, resolved at the moment the file's config is
            // applied (see `CompilerConfig::for_file`), because that is where a file and its target meet.
            libraries: project_config.config.libraries.clone(),
            database: discovery
                .database
                .as_ref()
                .map(|database| std::sync::Arc::new(database.commands.clone())),
            remove_args: project_config.config.compile.remove_args.clone(),
            extra_args: project_config.config.compile.extra_args.clone(),
            ..base
        };

        // **The standard the project states in its own file**, which is the person's word and so wins: the key
        // exists because `-std=` is the one flag nearly every project sets and nearly every target restates.
        let base = match (&project_config.config.compile.standard, &base.standard) {
            (Some(stated), _) => base.with_standard(stated.clone()),
            (None, _) => base,
        };

        // **The standard the project's own build states**, read out of the compilation database's command line by
        // `discovery.standard()`, applied only when nothing above it stated one: what the build ran is closer to the
        // truth than a compiler's default, and a project's own `args` are closer still.
        let base = match (base.standard.clone(), discovery.standard()) {
            (None, Some(standard)) => base.with_standard(standard),
            _ => base,
        };

        // The compilers the *project* named, in their order (`discovery.named_compilers`): a project that says
        // `clang++` gets clang's headers, and a project whose build names a compiler gets that one — before
        // `CXX`, before the platform's own, and before anything on `PATH`.
        let environment = Environment::current();
        let layout = crate::include::msvc::WindowsLayout::current();
        let named = discovery.named_compilers();

        // **The compiler's answers are cached under the project's own cache**, beside the summaries they are the
        // environment of: a warm open replays them instead of running a compiler (`Session::open` measured 141 ms,
        // almost all of it the invocations). See [`toolchain::CachedCommands`].
        let toolchain_cache = project_config
            .config
            .index
            .cache_dir
            .clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join(".cppls"))
            .join("toolchain");
        let commands = toolchain::CachedCommands::new(toolchain_cache);
        let toolchain = toolchain::discover_with(
            &files,
            &commands,
            &named,
            // **What the project says this file is built as**: the database's entry for it, or — when the database
            // states nothing — the standard the configuration already resolved (`[compile] args = ["-std=c++17"]`,
            // the build's `-std=`). The compiler has to be asked about *that* standard: its
            // `__cplusplus` is what every `#if` in every header is a question about, and a table printed for some
            // other language version is a table that silently says `<format>` is empty.
            toolchain::BuildStatement {
                commands: discovery
                    .database
                    .as_ref()
                    .map(|database| &database.commands),
                standard: base.standard.as_deref(),
            },
            &for_file,
            &environment,
            &layout,
        );

        let config = match &toolchain {
            Some(found) => found.config(&base),
            None => base,
        };

        // **And what the project says about CUDA**, which is the one layer discovery cannot work out on a machine
        // where `nvcc` is not installed: the toolkit's location, the host compiler's name, extra includes. Applied
        // after the toolchain so that `enable` can override a discovery that found no `nvcc` — see
        // `crate::project::cuda::CudaSection`.
        let config = match &project_config.config.compile.cuda {
            Some(cuda) => cuda.applied_to(config, &root),
            None => config,
        };

        // **And what it says about Unreal Engine** — where the engine is installed, so that `#include "CoreMinimal.h"`
        // and `#include "GameFramework/Actor.h"` resolve the way a build resolves them, plus the macros an engine build
        // defines and one `<MODULE>_API` per module the project's `*.uproject` names. Applied after the toolchain and
        // the database, so that a define the build itself stated wins. See `crate::project::ue::UnrealSection`.
        let config = match &project_config.config.unreal {
            Some(unreal) => unreal.applied_to(config, &root),
            None => config,
        };

        // **Which compiler this is**, asked of the compiler rather than guessed from the flags: GCC and Clang
        // predefine `__GNUC__`, MSVC predefines `_MSC_VER`, and they arrive in the table the same call that gave
        // the search paths (see `toolchain::discover`). It matters to the *parser*, which has to know what
        // `__int128` means to the compiler reading the file — see `CompilerConfig::dialect`.
        //
        // No toolchain, no answer: the configuration keeps its default, and a caller that knows the target
        // (`with_config`) sets it itself.
        let config = match toolchain.as_ref().and_then(|found| found.dialect) {
            Some(dialect) => config.with_dialect(dialect),
            None => config,
        };

        let session = Session::assemble(root, files, filter, config, toolchain, discovery);
        // (the file-cache sweep is gone with the file cache: `cache.redb` collects its own generations —
        // `SummaryStore::collect_the_cache`.)
        session
    }
}

/// **Everything a cook needs, gathered under the write lock and then used without it.**
///
/// Owned rather than borrowed on purpose: the caller has to let go of the session between
/// `Session::cooking_materials` and `Session::render_a_cooked`, so nothing here may borrow it. The unit is an
/// `Arc` the cache already holds, and the seed is a table built from the index's own macros.
/// **A file, rendered and parsed, waiting to be filed** — and the hash of the text it was made from.
///
/// The hash is the whole reason this is a struct rather than the pair it used to be. A cook is **two halves under
/// two locks** ([`Session::render_a_cooked`] and [`Session::commit_a_cooked`]) and the file can be edited between
/// them — at which point the reading describes text nobody has any more. Committing it files a stale reading *under
/// the new text*, and nothing re-cooks a file that has a cooked reading: measured, a pull right after an edit
/// answered `reading Cooked, 0 error(s)` for a file whose `;` had just been deleted. `text_hash` is what lets the
/// commit say "this is not the text any more" and drop it.
pub struct RenderedCook {
    /// `cache::content_hash` of the text the rendering was made from.
    pub text_hash: u64,
    pub indexed: crate::IndexedRendering,
    pub reading: CookedReading,
}

pub struct CookingMaterials {
    /// The index's own spelling of the path, which is what every other layer keys on.
    pub path: PathBuf,
    /// The summary key the rendered reading is filed under.
    pub key: SummaryKey,
    /// **The timeline whose view answers positionally**, when one covers the file — `FileMacros` over its
    /// definitions, which is what the reading has always used where a unit exists.
    pub unit: Option<std::sync::Arc<crate::TranslationUnit>>,
    /// **The composed starting state**, for a file no timeline covers: `Over(configuration, seed)`.
    ///
    /// Two fields rather than one because they are not equivalent: a `MacroView` is a **position** in a walk,
    /// so it answers `#define`s that arrive mid-file and directives that split a declaration; a materialised
    /// `Configuration` is the state a file *starts* with. The walk stays preferred wherever it exists.
    pub configuration: Option<crate::preprocess::cooked::Configuration>,
    /// The file's own text.
    pub text: String,
    /// **The hash of that text**, which is what the commit compares the file's text against — see
    /// [RenderedCook].
    pub text_hash: u64,
    /// What the compilation defines, before the file's own `#define`s.
    pub seed: MacroTable,
    /// The names the file's raw reading already declares, so the render's addition can be counted.
    pub already_declared: std::collections::HashSet<String>,
}

/// **What a wave prepared, and the units the pass that follows it will need.**
///
/// Both halves are the same kind of thing: work that is a pure function of the files and the index, done under the
/// read lock so that the write lock only has to *insert*. The units belong here for a measured reason — a unit walk
/// held the writer for **1003 ms** on the first slice after a cold start, which was the largest single thing a drain
/// did — and they can be built in the preparing half because `Session::units_for_the_pass` takes `&self`: the two
/// caches a walk needs (the units table and the macro definitions) have locks of their own rather than needing
/// `&mut self`.
#[derive(Default)]
pub struct PreparedWave {
    /// The summaries `Session::commit_a_wave` is about to put into the index.
    pub prepared: Vec<(PathBuf, crate::index::store::Prepared)>,
    /// The timelines the second pass reads its environments out of, for the files this wave will parse.
    pub units: Vec<std::sync::Arc<crate::TranslationUnit>>,
    /// **Refined readings the cache already holds for the slice this wave will drain.**
    ///
    /// The second pass refines a file against its own closure, and its answer is a function of that closure — so a
    /// stored one is served instead of being rebuilt, and the files it covers need no timeline at all. This is what
    /// makes a second start cheap: the pass costs a lookup rather than a parse per file.
    pub refinements: Vec<crate::FileSummary>,
}

/// **What a drained slice still has to do, decided but not paid for.**
///
/// The write lock's last remaining expenses — other than the inserts themselves — were the second pass
/// ([`SummaryStore::prepare_the_re_read`], 150 ms per file measured) and one macro environment (65–190 ms). Neither
/// changes the session when it is *worked out*; both were inside the writer only because the code that decided to do
/// them was. So the decision is made here, cheaply, under the write lock, and the work is handed to
/// [`Session::prepare_the_drain`], which takes `&self`.
///
/// A plan that is prepared and never committed is dropped, exactly like a [`PreparedWave`] that is never committed:
/// the files it named are re-read when something asks about them again.
#[derive(Default)]
pub struct Drain {
    /// The files the second pass will re-read, claimed from the backlog by this plan — see
    /// [`Session::parsed_since_the_last_pass`].
    files: Vec<PathBuf>,
    /// The timelines that pass reads its environments out of, built for exactly this slice by
    /// [`Session::prepare_a_wave`].
    units: Vec<std::sync::Arc<crate::TranslationUnit>>,
    /// The macro environments a query asked for and has not got — up to `MACRO_SLICE` of them.
    environments: Vec<PathBuf>,
}

impl Drain {
    /// Is there nothing to do? A caller that would take a lock only for this can skip it.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.environments.is_empty()
    }
}

/// **What [`Session::prepare_the_drain`] made** — the re-reads built and the environments rendered.
///
/// **A whole program is not in here**, and that is the point of [`UnitReadMaterials`]: it costs 4.9 s, and a drain is
/// planned under the writer. The pump takes it as its own three phases, with every lock dropped for the middle one.
#[derive(Default)]
pub struct PreparedDrain {
    /// The second pass, planned: `None` when the slice's backlog was empty.
    re_read: Option<crate::index::store::ReRead>,
    /// The rendered environments, keyed the way [`Session::known_rendering_of`] looks them up: the file, and the
    /// hash of the text it was rendered from.
    rendered: Vec<(
        (PathBuf, u64),
        std::sync::Arc<crate::preprocess::cooked::RenderedCooked>,
    )>,
}

/// **A unit read whose expensive half is done** — see [`Session::prepare_a_unit_reading`].
pub struct PreparedUnitReading {
    root: PathBuf,
    /// The hash of the rendering these facts were read from — what the memo and the database file them under.
    program: u64,
    indexed: crate::IndexedUnit,
    /// **Every file the unit was rendered from** — the module's closure, which is what an importer of it sees.
    ///
    /// Carried out of the read rather than looked up afterwards because the rendering is consumed there: a **cold**
    /// read cooks it inside `read_the_program`, so the only layer that ever holds it is the caller of this function —
    /// and a warm read has it earlier, at gather time. Recording it in both places is what keeps the answer from
    /// depending on whether the cache was warm (measured: a cold run still answered 34 completions where a warm one
    /// answered 38).
    walked: Vec<PathBuf>,
}

/// **What a program read needs, gathered under the session lock and read without it.**
///
/// The three parts of a unit read, and the measurement that decides where each one is paid
/// (`tests/unit_read_probe.rs`, a 159-file / 7.07 MB program, 5.09 s in total):
///
/// ```text
/// gather   the walk, the closure's texts, two cache reads        44 ms      needs &Session — this type comes from it
/// read     the cook (1.25 s), the parse and the sweep (3.66 s)   ~4.9 s     needs none of it — `read_the_program`
/// commit   one insert_cooked per file, and queue.mark_worked     a few ms   needs &mut Session
/// ```
///
/// So the caller may drop **every** lock between [`Session::gather_a_unit_read`] and
/// [`Session::commit_a_unit_reading`], which is the difference between a whole-program reading that a keystroke waits
/// five seconds for and one it does not wait for at all. The session's own schedule
/// ([`Session::a_unit_may_be_read_now`]) only allowed programs under a megabyte while this split did not exist; the
/// budget is about a pathological closure now, not about a person's typing speed.
pub struct UnitReadMaterials<F: FileProvider> {
    root: PathBuf,
    /// The compilation context, which is part of the key a summary is filed under.
    key: SummaryKey,
    /// The key the rendering is filed under: the content hash of every file the walk entered, folded.
    units_key: u64,
    /// The walked timeline — **absent when a stored rendering made the walk unnecessary**.
    ///
    /// The key a rendering is filed under folds the walk's file list, and a rendering now records that list
    /// (`RenderedUnit::walked`), so a session that has already walked a program can find the rendering again without
    /// re-walking it. What is missing then is the timeline, and it is only needed to **cook** — a served rendering is
    /// already cooked.
    unit: Option<std::sync::Arc<crate::TranslationUnit>>,
    /// The text of every file the walk entered, by path — what the cook reads.
    sources: HashMap<PathBuf, String>,
    /// What the compilation itself defines ([`crate::Marked`]), which no file in the closure knows.
    macros: crate::Marked,
    config: CompilerConfig,
    files: F,
    /// The store's own database, for the two rows a program read reads and writes.
    database: Option<std::sync::Arc<cache::cache_db::SummaryCache>>,
    /// The rendering's hash, when the rendering was already in hand.
    program: Option<u64>,
    /// The rendering, when this session or the database already had it.
    rendered: Option<crate::RenderedUnit>,
    /// The facts, when they were already read.
    known: Option<crate::IndexedUnit>,
}

impl<F: FileProvider> UnitReadMaterials<F> {
    /// The root this program is read for, by the index's spelling.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// **The key the rendering is filed under** — what [`Session::prepare_a_unit_reading`] remembers per root.
    pub fn units_key(&self) -> u64 {
        self.units_key
    }

    /// **Read the program** — the cook, the parse and the sweep, and no session at all.
    ///
    /// The order is the one the measurements put in: facts if they are known, then the rendering (a cook, or the one
    /// gathered), then the facts again by the rendering's own hash, then the one parse. Both rows are written back,
    /// so the next session decodes instead of computing — 63 ms against 3.66 s on the measured program.
    pub fn read_the_program(self) -> Option<PreparedUnitReading> {
        if let Some(indexed) = self.known {
            return Some(PreparedUnitReading {
                root: self.root,
                program: self.program?,
                indexed,
                walked: self
                    .rendered
                    .as_ref()
                    .map(|r| r.walked.clone())
                    .unwrap_or_default(),
            });
        }

        let stream = match self.rendered {
            Some(rendered) => rendered,
            None => {
                // **A cook needs the walk**, and the fast path (`Session::unit_roots`) is the one that has no timeline.
                // Materials without a unit are only ever built *with* a rendering, so this is unreachable rather than
                // an answer, and saying so is better than cooking a program from texts nobody walked.
                let unit = self.unit.as_ref()?;
                let definitions = unit.definitions();
                let seed = MacroTable::from_marked(&self.macros);
                // **The include search, so `__has_include` is answerable.** Without it the operator answers
                // `Unknown`, which is honest but decides nothing, and `#if __has_include(<span>)` is a shape real
                // headers are full of. See [`Session::render_the_unit`].
                let search = crate::preprocess::cooked::Search::new(&self.files, &self.config);
                let cooking = std::time::Instant::now();
                #[cfg(feature = "perf-probe")]
                crate::perf::mark("before the cook");
                let mut stream = unit.cook_the_unit(
                    &self.sources,
                    &definitions,
                    Some(&seed),
                    true,
                    Some(&search),
                );
                #[cfg(feature = "perf-probe")]
                crate::perf::mark("cook");
                if crate::probes::units() {
                    eprintln!(
                        "[unit] cook  {:>6.1} ms  ({} file(s), {} bytes)",
                        cooking.elapsed().as_secs_f64() * 1000.0,
                        stream.files.len(),
                        stream.text.len()
                    );
                }
                // **And the walk's own file list, which is what its key folds.** `files` names only the files that
                // contributed tokens; a header of nothing but `#define`s writes none, and a rendering that did not
                // record it could never reproduce the key it is filed under — see `RenderedUnit::walked`.
                // **Deduplicated**, because `files()` yields one entry per *inclusion* and the key folds one entry per
                // path (`the_units_inputs` collects into a map). A file included twice is one input.
                stream.walked = unit
                    .files()
                    .map(|path| path.to_path_buf())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                // **Kept under the key, provisionally or not — the key is what makes that safe.** It folds every
                // `(path, content)` the walk entered, so a rendering of a closure that was still filling has a key
                // the finished closure cannot repeat: the row is never hit again, it is merely wasted.
                if let Some(database) = &self.database {
                    let _ = database.put_unit(self.units_key, &stream);
                }
                stream
            }
        };

        let program = crate::cache::content_hash(&stream.text);
        if let Some(database) = &self.database
            && let Ok(Some(known)) = database.indexed_unit(program)
        {
            return Some(PreparedUnitReading {
                root: self.root,
                program,
                indexed: known,
                walked: stream.walked.clone(),
            });
        }

        let indexer = FileIndexer::new(&self.files, &self.config);
        let indexed = indexer.index_unit_rendering(&self.root, &stream, self.key);
        if let Some(database) = &self.database {
            let _ = database.put_indexed_unit(program, &indexed);
        }

        Some(PreparedUnitReading {
            root: self.root,
            program,
            indexed,
            walked: stream.walked.clone(),
        })
    }
}

/// **How the effect chain reads files**: the session`s own provider, and the include graph the index resolved.
struct SessionReads<'a, F: FileProvider> {
    files: &'a F,
    index: &'a crate::ProjectIndex,
}

impl<F: FileProvider> crate::effect::Reads for SessionReads<'_, F> {
    fn read(&self, path: &std::path::Path) -> Option<String> {
        self.files.read(path)
    }

    fn resolve(&self, from: &std::path::Path, spelling: &str) -> Option<PathBuf> {
        // **The resolution the index already made.** A summary records where each `#include` resolved to, so the
        // chain does not re-run the search: a second search would be a different answer to the same question, which
        // is the reason `IncludeFact::resolved` is stored at all.
        let wanted = spelling.trim_matches(|c| c == '"' || c == '<' || c == '>');
        let summary = self.index.summary(from)?;
        summary
            .includes
            .iter()
            .find(|include| include.spelling.trim() == wanted)
            .and_then(|include| include.resolved.clone())
    }
}

impl<F: FileProvider + Clone> Session<F> {
    /// A session with the configuration the caller already has: no compile database is read and no compiler is run.
    ///
    /// The path for a caller that knows how the project is built — a test, a build-system integration — and the
    /// only one that works over a provider that is not the disk.
    ///
    /// The project's **own** file is still read, because the two answer different questions and only one of them
    /// was given away here: `config` is what the compiler was told (the caller's, and it wins over `[compile]`),
    /// while `.cppls.toml` also says which files are the project's source and where its cache goes
    /// (`workspace.exclude`, `workspace.source_extensions`, `index.cache_dir`). A session that ignored those
    /// would analyse a different project from the one [`Session::open`] analyses.
    pub fn with_config(
        root: impl Into<PathBuf>,
        files: SessionFiles<F>,
        filter: WatchFilter,
        config: CompilerConfig,
    ) -> Session<F> {
        let root = root.into();
        // The project's own file is read here too, and by the same code: a session the caller configured still
        // analyses *this* project (`workspace.exclude`, `index.cache_dir`), and the build-system half is skipped
        // because the caller has said what the compilation is.
        let config_report = crate::project::load_config(&files, &root, None);
        let filter = apply_project_config(filter, &config_report);

        let discovery = ProjectDiscovery {
            root: root.clone(),
            config: config_report,
            database: None,
            problems: Vec::new(),
        };

        Session::assemble(root, files, filter, config, None, discovery)
    }

    fn assemble(
        root: PathBuf,
        files: SessionFiles<F>,
        filter: WatchFilter,
        config: CompilerConfig,
        toolchain: Option<Toolchain>,
        discovery: ProjectDiscovery,
    ) -> Session<F> {
        let project_config = discovery.config.clone();
        let database = discovery
            .database
            .as_ref()
            .map(|database| &database.commands);
        let documents = files.overlay.clone();
        // **A CUDA project's scan takes `.cu` and `.cuh`**, and a project that is not CUDA does not: the engine's own
        // list is what a C++ project is made of, and a `.cu` in one is either a vendored file or an accident. With
        // `[compile.cuda]` (or an `nvcc` toolchain) it is the project's own source, and asking every CUDA project to
        // write `workspace.source_extensions = ["cu", "cuh"]` in its configuration file would be asking it to restate
        // what the toolchain already said.
        let mut extra_extensions = project_config.config.workspace.source_extensions.clone();
        if config.cuda() {
            extra_extensions.extend(["cu".to_string(), "cuh".to_string()]);
        }
        // **A library's headers and module interfaces join the project's file list**, and its `.cpp` files never do —
        // a dependency is used through what it exports, and reading its implementation would multiply the index by
        // every dependency's sources for an answer nobody reading *this* project can act on. See `LibrarySection`.
        let from_libraries = library_files_to_read(&project_config.config.libraries, &root);
        let mut project = project_files(
            &files,
            &filter,
            &root,
            database,
            &extra_extensions,
            project_config
                .config
                .index
                .max_files
                .unwrap_or(MAX_PROJECT_FILES),
        );
        for file in from_libraries {
            if !project.iter().any(|held| held == &file) {
                project.push(file);
            }
        }

        // **What makes the macro environment complete: the compiler answered.**
        //
        // The flag is one claim — *a name none of the witnesses mentions is not defined* — and the witnesses are
        // the compiler's predefined names, the command line's `-D`s, and the file's own `#define`s. The second is
        // what `discovery` reads (`.cppls.toml`, `compile_commands.json`) and what
        // `toolchain::discover_with` asks the compiler with. The **first** is the compiler's own answer and exists
        // nowhere else: `_MSC_VER`, `__cplusplus` and five hundred more are built in, not written down. So the
        // claim is made whenever those macros came back, and the project's build description is a *source of
        // flags* rather than the licence for the claim.
        //
        // What it is not: a promise that every flag of the project's build is known. A build description nobody
        // can read — a `Makefile` with `-DFOO`, which no discovery in this crate parses — is a hole. The ordinary
        // such project has `-I` flags in the same file, and those are *self-reporting*: `Marked::mark_incomplete`
        // takes the claim back the moment an `#include` fails to resolve, and nothing is decided after that.
        //
        // **Measured on the workspace that asked for this** (`C:\work\one_file_project`: one `main.cpp`,
        // 2678 bytes, no build system of any kind). With the claim tied to a compile database — which is what this
        // line said before — MSVC's whole standard library was unreachable: `declarations_in("std")` was **13**,
        // every one of them from a C header that writes `namespace std` literally; `definition("std::string")`,
        // `std::cin`, `std::cout`, `std::basic_string` were all `NotDeclaredHere`; 46 of the file's identifiers had
        // no answer and 26 more were `ConditionalCompilation`; and the cooked reading of `<string>` rendered to
        // **nothing at all** (0 declarations, 0 mapped ranges). The cause is one name: `yvals_core.h` defines
        // `_STL_COMPILER_PREPROCESSOR` under `#if defined(RC_INVOKED) || defined(Q_MOC_RUN) || defined(__midl)`,
        // and MSVC's headers put *everything* inside `#if _STL_COMPILER_PREPROCESSOR` — so with `RC_INVOKED`
        // `Unknown`, nothing in the library is in force. With the claim made on the compiler's answer: **2600**
        // declarations in `std`, the three names above resolved, `<xstring>`'s 129 facts became **1631**, the
        // cooked reading of `<string>` 1000 declarations over 2000 ranges, and the identifiers with no answer fell
        // to **9**: three macro names (`stdout`, `stderr` ×2), one keyword (`static_cast`), one local, one member
        // reached through an object whose type is still unknown (`std::cin.read`), and `std::string::npos` ×3. The
        // cost is the cold read of that closure: 3.5 s → 16.5 s (warm 8.5 s), which is the price of reading four
        // megabytes of headers instead of whitespace.
        let configured = the_compilation_is_known(toolchain.as_ref());
        let mut store = SummaryStore::with_provider(root.clone(), config.clone(), files.clone())
            .with_macros(crate::index::environment::compilation_environment(
                &config,
                toolchain.as_ref(),
                configured,
            ));

        if let Some(cache_dir) = &project_config.config.index.cache_dir {
            store = store.with_cache_directory(cache_dir);
        }

        // The search path, read once. Before the struct literal rather than inside it because it borrows `config`,
        // which the literal moves.
        let headers = crate::completion::header_index(&config, &root, project.clone());

        let mut session = Session {
            root: root.clone(),
            config,
            toolchain,
            discovery,
            vfs: Vfs::new(files.clone()),
            files,
            documents,
            store,
            filter,
            share_contributions: crate::probes::share(),
            project: project.clone(),
            queue: Work::default(),
            parsed_since_the_last_pass: Vec::new(),
            refined_on_request: std::sync::Mutex::new(std::collections::HashSet::new()),
            refined_readings: std::sync::Mutex::new(std::collections::HashSet::new()),
            cooking: Cooking::default(),
            definitions: std::sync::Mutex::new(crate::MacroDefinitions::default()),
            units: std::sync::Mutex::new(UnitTable::default()),
            walked_by_content: std::sync::Mutex::new(cache::cache_lru::Cache::new(
                MAX_WALKS_BY_CONTENT_ENTRIES,
                MAX_WALKS_BY_CONTENT_BYTES,
            )),
            units_read: std::collections::HashSet::new(),
            programs_wanted: std::collections::HashSet::new(),
            units_attempted: std::collections::HashMap::new(),
            checks_memo: std::sync::Mutex::new(None),
            unit_index: std::sync::Mutex::new(None),
            rendered_unit: std::sync::Mutex::new(None),
            unit_roots: std::sync::Mutex::new(HashMap::new()),

            last_edit: std::time::Instant::now(),
            directive_signatures: std::collections::HashMap::new(),
            prefetched: HashMap::new(),
            headers,
            macro_work: std::sync::Mutex::new(Cooking::default()),
            parsed_definitions: std::sync::Mutex::new(crate::ParsedDefinitions::new()),
            config_for_file: std::sync::Mutex::new(HashMap::new()),
            macro_environments: std::sync::Mutex::new(cache::cache_lru::Cache::macro_environments()),
            surfaces: std::sync::Mutex::new(crate::effect::Surfaces::new()),
            effects: std::sync::Mutex::new(HashMap::new()),
            renderings: std::sync::Mutex::new(cache::cache_lru::Cache::renderings()),
            views: std::sync::Mutex::new(cache::cache_lru::Cache::views()),
            rendering_summaries: std::sync::Mutex::new(cache::cache_lru::Cache::new(
                RENDERING_SUMMARY_ENTRIES,
                RENDERING_SUMMARY_BYTES,
            )),
            cooking_refused: std::sync::Mutex::new(std::collections::HashSet::new()),
            unit_from_memory: std::sync::atomic::AtomicUsize::new(0),
            unit_from_disk: std::sync::atomic::AtomicUsize::new(0),
            unit_walked: std::sync::atomic::AtomicUsize::new(0),
            walks_by_root: std::sync::Mutex::new(std::collections::HashMap::new()),
        };

        // The scan is the queue's **seed**, not a list to consult later: opening a project is the caller saying
        // "index this", and the order it is indexed in is the whole of what this type is for. Doing it here rather
        // than leaving it to the caller is what keeps `pending()` an honest answer from the first moment.
        for path in session.project.clone() {
            session.queue.add(path, Priority::Rest, 0);
        }

        session
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    // The configuration every summary in this session is keyed against.

    /// **The configuration one file is analysed with** — its own command from the compilation database, plus its
    /// target's include directories and defines.
    ///
    /// [`Session::config`] cannot answer this and should not pretend to: a database has one entry per translation
    /// unit and a project with several targets has no single compilation, so the project-wide answer is *a*
    /// compilation rather than the project's. This is the question the include resolver asks, and the one a caller
    /// debugging "why did this file resolve that header" wants.
    pub fn config_for(&self, file: &std::path::Path) -> CompilerConfig {
        self.config().for_file(file)
    }

    /// **A file's configuration and its language, worked out once per session** — see [`Session::config_for_file`]
    /// for the field and the measurement.
    ///
    /// The same answers [`Session::config_for`] and [`CompilerConfig::language_for`] give. Both are memoized together
    /// because both were on the same path and each was the whole cost of a *cached* view: `for_file` clones the
    /// configuration and looks the file up in the database, and `language_for` looks it up **again** and re-parses
    /// `-std=` out of the command line — measured together at **132 µs per call**, which was 90 % of what a view-cache
    /// hit cost until this existed.
    ///
    /// Bounded by count, and the bound clears the whole memo: a reader meets a few dozen files, and evicting one entry
    /// at a time would buy nothing for the bookkeeping it costs.
    fn config_for_file(&self, file: &std::path::Path) -> std::sync::Arc<PerFileConfig> {
        if let Ok(known) = self.config_for_file.lock()
            && let Some(held) = known.get(file)
        {
            return std::sync::Arc::clone(held);
        }
        let config = std::sync::Arc::new(self.config().for_file(file));
        let worked_out = std::sync::Arc::new(PerFileConfig {
            language: config.language_for(file),
            cuda: config.cuda(),
            config,
        });
        if let Ok(mut known) = self.config_for_file.lock() {
            if known.len() >= CONFIG_FOR_FILE_ENTRIES {
                known.clear();
            }
            known.insert(file.to_path_buf(), std::sync::Arc::clone(&worked_out));
        }
        worked_out
    }

    pub fn config(&self) -> &CompilerConfig {
        &self.config
    }

    /// What the project's own configuration file said, and everything wrong with it.
    ///
    /// A report with no path is a project without a `.cppls.toml`, which is the ordinary case and not a problem.
    /// The sections a language server reads live here too — see [`Session::discovery`].
    pub fn project_config(&self) -> &ConfigReport {
        &self.discovery.config
    }

    /// **What this analysis thinks the project is** — its configuration file, its build, and
    /// everything that could not be worked out.
    ///
    /// The first question anybody asks about a wrong answer, and the reason the discovery is kept rather than
    /// consumed: "it read the wrong compilation" is diagnosable from here and from nowhere else. It is what
    /// `cpp_code_analysis`'s `discover` example prints, and what a server should log at startup.
    pub fn discovery(&self) -> &ProjectDiscovery {
        &self.discovery
    }

    pub fn toolchain(&self) -> Option<&Toolchain> {
        self.toolchain.as_ref()
    }

    pub fn compile_database(&self) -> Option<&CompileCommands> {
        self.discovery
            .database
            .as_ref()
            .map(|database| &database.commands)
    }

    pub fn documents(&self) -> &OpenDocuments {
        &self.documents
    }

    /// Every summary loaded so far — what the session currently knows, as opposed to what it will.
    pub fn index(&self) -> &ProjectIndex {
        self.store.index()
    }

    /// **What the file directs, without parsing it** — the reading a timeline walk needs, available *before* a file
    /// is indexed. See [`crate::index::FileIndexer::directive_summary`], and
    /// [`SummaryStore::directive_summary`](crate::SummaryStore::directive_summary) for why the store remembers it.
    pub fn directive_summary(&self, path: &Path) -> Option<crate::FileSummary> {
        self.store
            .directive_summary(path)
            .map(|summary| (*summary).clone())
    }

    /// **Parse one file now, from its own text** — see [`SummaryStore::parsed_summary`]: the raw reading, whatever
    /// the index holds (after a cook it holds the *rendering's* reading).
    pub fn parsed_summary(&self, path: &Path) -> Option<crate::FileSummary> {
        self.store.parsed_summary(path)
    }

    /// **What the session is holding** — the instrument for a report that says the server grew to gigabytes.
    ///
    /// Each field is a thing a cache *adds* to the process, and the numbers are the ones a report has to name before
    /// anything can be called a leak. Measured shape of the question this answers: a reader edited
    /// `EmmyLuaCodeStyle`'s `LuaSource.cpp` for a few minutes and the server sat at **3.3 GB** and 26% of a core.
    ///
    /// `rendering_bytes` is the text of every cached rendering, which is the cache with **no cap** — its key is
    /// `(path, content hash)`, so a keystroke adds an entry and nothing ever removes one. `summaries` and `facts`
    /// are the index, which is bounded by the project and is the part that is supposed to be large.
    /// **The summary database, when `CPPLS_CACHE_DB` turned it on** — rows and bytes.
    ///
    pub fn summary_database_sizes(&self) -> Option<cache::cache_db::DatabaseSizes> {
        self.store.summary_database_sizes()
    }

    /// **Collect and compact the summary database** — the idle step, never a query.
    pub fn collect_the_cache(&mut self) -> Option<cache::cache_db::Collected> {
        self.store.collect_the_cache()
    }

    /// Commit whatever summaries the database is holding — the end of a slice.
    pub fn flush_summaries(&self) {
        self.store.flush_summaries();
    }

    pub fn cache_sizes(&self) -> CacheSizes {
        let (renderings, rendering_bytes) = self
            .renderings
            .lock()
            .map(|known| {
                let bytes = known
                    .values()
                    .map(|rendering| rendering.text.len())
                    .sum::<usize>();
                (known.len(), bytes)
            })
            .unwrap_or((0, 0));

        let summaries = self.store.index().summaries().count();
        let facts = self
            .store
            .index()
            .summaries()
            .map(|summary| summary.declarations.len())
            .sum();
        let macros = self
            .store
            .index()
            .summaries()
            .map(|summary| summary.macros.len())
            .sum();

        let (cooked_files, cooked_declarations) = self.store.index().cooked_size();
        let (views, view_bytes) = self
            .views
            .lock()
            .map(|known| (known.len(), known.weight()))
            .unwrap_or((0, 0));
        CacheSizes {
            renderings,
            rendering_bytes,
            units: self.held_units().held.len(),
            summaries,
            facts,
            macros,
            cooked_files,
            cooked_declarations,
            views,
            view_bytes,
        }
    }

    /// **A view already parsed from exactly this input**, or `None`.
    ///
    /// A hit is a handful of `Arc` bumps and one red root — see [`Session::view`] for what the key is made of and why
    /// none of it can serve a stale parse, and [`crate::file::view::ParsedView`] for why the cached half has no root.
    fn a_view_already_parsed(&self, key: &ViewKey) -> Option<FileView> {
        let mut known = self.views.lock().ok()?;
        let parsed = known.get(key)?;
        Some(FileView::from_parsed((**parsed).clone()))
    }

    /// Remember `view` under `key`, weighed by what it actually costs — see [`a_view_weighs`].
    fn remember_a_view(&self, key: ViewKey, view: &FileView) {
        let weight = a_view_weighs(view);
        if let Ok(mut known) = self.views.lock() {
            let parsed = view.clone().into_parsed();
            known.insert(key, std::sync::Arc::new(parsed), weight);
        }
    }

    /// **What every table the session owns is holding, in bytes** — see [`crate::MemoryReport`].
    ///
    /// [`Session::cache_sizes`] answers "how many entries", which is the wrong unit for the
    /// question a five-gigabyte report asks: an entry is a `#define` or a rendering, and the two
    /// differ by six orders of magnitude. **How many walk contributions this session's store
    /// holds** — a diagnostic, and the number the sharing test asserts. The table itself is the
    /// store's; see [`crate::index::walk_cache::WalkCache`].
    /// **Turn the walk cache's cross-walk sharing on or off**, overriding the `CPPLS_WALK_SHARE` probe.
    ///
    /// An A/B and a test hook, not a setting: the sharing is off by default because a contribution replayed at another
    /// walk's position drops include edges — and the test that has to exercise the machinery itself asks for it here.
    pub fn set_sharing_contributions(&mut self, on: bool) {
        self.share_contributions = on;
    }

    pub fn walk_contributions_held(&self) -> usize {
        self.store.contributions().held()
    }

    /// **Keep the index's summaries under a byte budget** — P2's byte budget, applied after a drain.
    ///
    /// The largest summaries go first, because that is what a cap is for: the standard library's headers are 130 KB
    /// each and they are the same files on every machine, while a project's own sources are small. What it never
    /// touches:
    ///
    /// * **a file the reader has open** — a `didChange` would find its summary gone and the answer would be "not read
    ///   yet" for the file somebody is typing in;
    /// * **a file refined in this session** — its reading is what the refinement bought, and forgetting the summary
    ///   under it would keep the facts and lose the text they were mapped against;
    /// * **a file still in the queue** — the queue is about to read it, and a summary dropped now is one built twice.
    ///
    /// # And the fourth rule is why this rarely fires
    ///
    /// A file in `parsed_since_the_last_pass` is protected too, because the second pass needs its summary to re-read it.
    /// Under the demand-driven contract that backlog is **never empty** — a batch no longer drains it (it asks for
    /// nothing), and only a request does — so on a project of any size every indexed file is protected by one rule or
    /// another and the budget cannot fire. That is a real tension between P2's two halves, and the honest resolution is
    /// a policy for *which queued files may be forgotten* (the ones the pass would skip anyway: no bodied macro
    /// mentioned, no guarded declaration) rather than a looser filter here.
    ///
    /// A forgotten summary is re-read on demand ([`Session::needs_catching_up`]), which is the path a cache miss already
    /// takes, so the only cost is the re-read.
    pub fn enforce_the_summary_budget(&mut self, budget: usize) -> usize {
        let (_, bytes, _, _) = self.store.index().accounting();
        if bytes <= budget {
            return 0;
        }
        // The candidates: everything with a summary, with its size, largest first.
        let mut candidates: Vec<(PathBuf, usize)> = self
            .store
            .index()
            .summaries()
            .map(|summary| {
                (
                    summary.path.clone(),
                    crate::FileSummary::heap_bytes(summary),
                )
            })
            .collect();
        candidates.sort_unstable_by_key(|candidate| std::cmp::Reverse(candidate.1));

        let open: std::collections::HashSet<PathBuf> = self.documents.paths().into_iter().collect();
        let refined: std::collections::HashSet<PathBuf> = self
            .refined_readings
            .lock()
            .map(|refined| refined.iter().cloned().collect())
            .unwrap_or_default();

        let mut over = bytes.saturating_sub(budget);
        let mut forgotten = 0usize;
        for (path, size) in candidates {
            if over == 0 {
                break;
            }
            if open.contains(&path)
                || refined.contains(&path)
                || self.queue.is_queued(&path)
                || self
                    .parsed_since_the_last_pass
                    .iter()
                    .any(|queued| queued == &path)
            {
                continue;
            }
            if self.store.index_mut().forget(&path) {
                over = over.saturating_sub(size);
                forgotten += 1;
            }
        }
        forgotten
    }

    pub fn memory_report(&self) -> crate::MemoryReport {
        let (vfs_files, vfs_text_bytes, vfs_line_index_bytes) = self.vfs.accounting();
        let (summaries, summary_bytes, cooked_files, cooked_bytes) =
            self.store.index().accounting();
        let (definitions, definition_bytes) = self
            .definitions
            .lock()
            .map(|definitions| definitions.accounting())
            .unwrap_or((0, 0));
        // The table keeps its own byte total, so the report does not walk every unit's events on every ask.
        let (units, unit_bytes) = self.held_units().accounting();
        let (renderings, rendering_bytes) = self
            .renderings
            .lock()
            .map(|known| {
                let bytes = known
                    .values()
                    .map(|rendering| rendering.text.len())
                    .sum::<usize>();
                (known.len(), bytes)
            })
            .unwrap_or((0, 0));
        let (dead_macro_files, dead_macro_offsets) = self.dead_macro_decisions();
        let (views, view_bytes) = self
            .views
            .lock()
            .map(|known| (known.len(), known.weight()))
            .unwrap_or((0, 0));
        let (walk_cache_entries, walk_cache_bytes) = self.store.contributions().accounting();
        let (walks_by_content_entries, walks_by_content_bytes) = self
            .walked_by_content
            .lock()
            .map(|held| (held.len(), held.weight()))
            .unwrap_or((0, 0));
        // **Which timelines are one allocation counted twice.** The unit table and the sharing cache hold the same
        // `Arc` whenever a timeline was both held for a root and remembered for its content — the common case — so
        // adding their byte counts would report the same megabytes twice. Measured rather than assumed: the pointers
        // are compared, and the walk table's half of every overlap is what the totals subtract.
        let shared_timeline_bytes = {
            let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
            for unit in self.held_units().timelines() {
                seen.insert(std::sync::Arc::as_ptr(unit) as *const () as usize);
            }
            let mut shared = 0usize;
            if let Ok(held) = self.walked_by_content.lock() {
                for unit in held.values() {
                    if !seen.insert(std::sync::Arc::as_ptr(unit) as *const () as usize) {
                        shared += unit.heap_bytes();
                    }
                }
            }
            shared
        };
        let tables = self.store.index().table_accounting();

        crate::MemoryReport {
            vfs_files,
            vfs_text_bytes,
            vfs_line_index_bytes,
            summaries,
            summary_bytes,
            cooked_files,
            cooked_bytes,
            definitions,
            definition_bytes,
            units,
            unit_bytes,
            renderings,
            rendering_bytes,
            views,
            view_bytes,
            dead_macro_decisions: dead_macro_files,
            dead_macro_bytes: dead_macro_offsets * std::mem::size_of::<usize>(),
            directive_signatures: self.directive_signatures.len(),
            include_searches: self.store.include_search_count().unwrap_or(0),
            units_read: self.units_read.len(),
            walk_cache_entries,
            walk_cache_bytes,
            walks_by_content_entries,
            walks_by_content_bytes,
            shared_timeline_bytes,
            index_names_bytes: tables.names.bytes,
            index_names_entries: tables.names.name_entries,
            index_scope_entries: tables.names.scope_entries,
            index_name_postings: tables.names.name_postings,
            index_scope_postings: tables.names.scope_postings,
            index_edges: tables.edges,
            index_edge_bytes: tables.edge_bytes,
            index_member_lists: tables.member_list_entries,
            index_member_list_bytes: tables.member_list_bytes,
            index_memo_entries: tables.guard_entries
                + tables.visibility_entries
                + tables.condition_entries,
            index_memo_bytes: tables.guard_bytes + tables.visibility_bytes + tables.condition_bytes,
        }
    }
    /// **What a name is at one position in one file** — see [`crate::MacroAt`] for why this exists and what the
    /// offset is for.
    ///
    /// Reads the unit the way [`Session::read_the_unit`] does, minus the indexing: this is a question *about* a file,
    /// and asking it must not change the answer to anything else.
    ///
    /// # Which unit a file belongs to
    ///
    /// A file in the project is its own unit. One that is not — a system header, a file the caller named by hand — is
    /// a member of some *other* file's unit, and which one is answered by the project's source list: the first source
    /// whose walk reaches it. `None` when nothing in the project reaches it, which is the honest "this session
    /// cannot speak about that file" rather than a `false` that would read as "not defined".
    pub fn macro_at(&mut self, file: &Path, name: &str, offset: usize) -> Option<crate::MacroAt> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if self.project.iter().any(|root| root == file) {
            candidates.push(file.to_path_buf());
        }
        candidates.extend(self.project.iter().filter(|root| *root != file).cloned());

        for root in candidates {
            let Some(unit) = self.translation_unit_of(&root) else {
                continue;
            };
            if let Some(answer) = unit.macro_at(file, name, offset) {
                return Some(answer);
            }
        }

        None
    }

    pub fn stats(&self) -> StoreStats {
        self.store.stats()
    }

    /// **How many guarded `#define`s the second pass has decided about** — `(files, offsets)`.
    ///
    /// The one output of the second pass that a caller cannot get from an answer, and the one that decides whether
    /// the pass's unit walks are buying anything: a walk that records nothing and rebuilds nothing is a walk that can
    /// be skipped. See [`crate::SummaryStore::dead_macro_decisions`].
    pub fn dead_macro_decisions(&self) -> (usize, usize) {
        self.store.dead_macro_decisions()
    }

    /// **Where this session writes the summaries it can reuse**, so that a caller can say what a *second* run costs.
    ///
    /// Exposed because "the second time is fast" is a claim about a directory, and a caller measuring the first time
    /// has to be able to say which directory it means: a benchmark that reports a warm number as a cold one is how a
    /// five-second stall ships as a feature. Deleting what is here is a safe thing for a caller to do — every entry
    /// is derived from a file's text and is rebuilt on demand — and it is the only way to measure the cold path on a
    /// machine that has run before.
    pub fn cache_directory(&self) -> &Path {
        self.store.cache_directory()
    }

    /// The sources the project scan listed, which is the seed of the rest of the work list.
    pub fn project_files(&self) -> &[PathBuf] {
        &self.project
    }

    /// Add files the caller knows about to the project list — a build system's translation units, a test's
    /// fixtures, a directory listing somebody else did better.
    ///
    /// They join the **rest** half of the queue and the list a configuration change re-seeds from, exactly as the
    /// scan's files do. Returns how many were queued, which is not always how many were passed: a file already in
    /// the list is one file.
    ///
    /// This is also how a session over an in-memory project gets a project list at all: the scan reads a
    /// directory, and a set of buffers has none. See [`Session::with_config`].
    pub fn add_project_files(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> usize {
        // A set rather than a scan of `self.project` per path: a build system hands over its whole translation-unit
        // list at once, and "is this path already here" asked once per path is a quadratic walk of ten thousand
        // paths — which is the one size of project this layer exists to survive.
        let mut held: HashSet<String> = self.project.iter().map(|path| queue_key(path)).collect();
        let mut added = 0;

        for path in paths {
            if held.insert(queue_key(&path)) {
                self.project.push(path.clone());
                added += 1;
            }

            self.queue.add(path, Priority::Rest, 0);
        }

        // **The header list sees the new files.** A header of this project is offered in a `#include` completion,
        // and "which files are the project" is exactly the list that just grew — so the half of the header index
        // that is derived from it is derived again. The search-path half is *not* re-read: that is a filesystem
        // walk, and nothing that happened here can have changed it.
        if added > 0
            && let Ok(mut headers) = self.headers.write()
        {
            *headers = headers.clone().with_project(self.project.clone());
        }

        added
    }

    // ---------------------------------------------------------------------------------------------
    // What changed
    // ---------------------------------------------------------------------------------------------

    /// The client opened a document, or said what its buffer now contains.
    ///
    /// The buffer becomes the text for that path, the summary built from the *old* text is dropped, and the file
    /// goes to the front of the open half of the queue. Dropping the summary is the honest half of this: the
    /// analysis could re-read the buffer here, and until it does, a query about a name in this file must answer
    /// "not read yet" rather than answer from text the user has already changed.
    ///
    /// The files it **includes** are marked for cooking one drain later, not here, and the reason is arithmetic: at
    /// this moment the index has nothing to follow — the file's own summary is the one that was just dropped — so
    /// its closure is not known yet. See `advance`, where the closure of every open file is marked once the index
    /// queue is empty.
    pub fn did_open(&mut self, path: impl AsRef<Path>, text: &str) {
        self.buffer_changed(path.as_ref(), text);
    }

    /// The client changed an open document's buffer.
    ///
    /// The same work as [`Session::did_open`] — one method exists per protocol event because a server matches on
    /// them, not because the analysis does anything different — and the reason it is not a *cheaper* path is worth
    /// stating: a change can add a declaration, remove one, or add an `#include` that changes what the file sees,
    /// so there is nothing to update in place. The summary is dropped and the file is re-read; the work is one
    /// parse, and the parse is what the cache cannot answer because the text has never been seen.
    pub fn did_change(&mut self, path: impl AsRef<Path>, text: &str) {
        self.buffer_changed(path.as_ref(), text);
    }

    /// The client saved a document — with the text, when it sent one.
    ///
    /// A save changes nothing about what the analysis reads: the buffer was already the text, and the disk now
    /// agrees. `None` is therefore no work at all, which is why the text is an `Option` rather than the analysis
    /// re-reading the file to compare it. A client that sends the text gets the same treatment as a change, because
    /// a save can carry an edit a change event did not.
    pub fn did_save(&mut self, path: impl AsRef<Path>, text: Option<&str>) {
        if let Some(text) = text {
            self.buffer_changed(path.as_ref(), text);
        }
    }

    /// The client closed a document: the file is the filesystem's again.
    ///
    /// The summary is dropped, because the one in the index describes the buffer and the disk may never have seen
    /// it — a language server that kept answering from a closed, unsaved buffer would report declarations that do
    /// not exist anywhere. The file goes to the **rest** half of the queue: the user has stopped looking at it, and
    /// everything else they are looking at now comes first.
    pub fn did_close(&mut self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        self.documents.close(path);
        // The text the analysis was holding was the *buffer's*, and the disk may never have seen it. Dropping the
        // entry is what makes the next question read the file again — a closed buffer is not a text this session
        // knows any more, and a view built on it would answer about an edit nobody saved.
        self.vfs.close(path);
        self.store.forget(path);
        self.prefetched.clear();
        self.directive_signatures.remove(&queue_key(path));
        self.units
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.units_read.clear();
        self.queue.again(path.to_path_buf(), Priority::Rest, 0);
    }

    /// The client reported filesystem changes — its own watcher's events, not ours.
    ///
    /// The entry point for the "no OS watcher" decision: a client that watches (or an
    /// editor that simply knows, because it is what wrote the file) sends events, and this turns them into work.
    /// The batch is built here from the same [`WatchFilter`] the session was opened with, so the cache directory,
    /// `.git` and any ignored build tree are filtered once, in one place.
    ///
    /// What comes back is [`SummaryStore::respond`]'s answer — what was forgotten, what has to be re-read, and
    /// whether the configuration changed — and the session applies it to its queue before returning it, so a caller
    /// that ignores the return value still gets the work done. A configuration change re-seeds from the project
    /// scan and the open buffers, in that order's reverse: what the user is looking at is read first.
    pub fn changed(&mut self, events: impl IntoIterator<Item = FileEvent>) -> Response {
        let mut batch = ChangeBatch::new(self.filter.clone());
        batch.extend(events);
        self.respond(&batch)
    }

    /// [`Session::changed`] for a caller that did its own coalescing.
    pub fn respond(&mut self, batch: &ChangeBatch) -> Response {
        // Anything read ahead was read before this batch said the world moved.
        self.prefetched.clear();

        // **What the batch's files said, before the store takes their summaries away**: a watched change is the same
        // event as an edit as far as a dependent's reading is concerned, and the reverse walk needs the edges and the
        // macro facts that are still there.
        for (path, _) in batch.changed() {
            // What the file says **now**: the buffer if one is open, the disk otherwise, and nothing when it is gone.
            let text = self.files.read(path);
            let moved = self.directives_moved(path, text.as_deref());

            if moved.environment {
                self.invalidate_dependents(path);
            }
            // …and the definitions this session read **out of** that file: their keys are offsets in it, so an edit
            // that moved them would leave entries a later fact could reach and never wrote.
            self.held_definitions().forget(path);
            // …and the **translation units**: a unit is a reading of its whole closure, so a file whose *directives*
            // moved anywhere in it makes every unit built over it a reading of a timeline that is no longer there.
            // Cleared whole, for the reason `Session::translation_unit_of` gives: the honest short answer to "did
            // something inside it move" is "assume so", and the disk entry checks file by file when it is asked
            // again. A change that left every directive where it was leaves the timeline as it was.
            if moved.layout {
                self.units
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clear();
                self.units_read.clear();
            }
        }

        let response = self.store.respond(batch);

        if response.everything {
            // The configuration is part of every summary's key, so every stored summary is now a miss: the work is
            // the whole project again, open documents first — and `respond` has already been through the paths in
            // the batch, so this is not a repeat of them but the list to rebuild from.
            //
            // `requeue` rather than `add`: these files have been worked, and `add` exists to refuse exactly that.
            let project = self.project.clone();
            for path in project {
                self.queue.requeue(path, Priority::Rest, 0);
            }
            for path in self.documents.paths() {
                self.queue.again(path, Priority::Open, 0);
            }
        } else {
            for path in &response.reindex {
                // A file the user has open is the file they are waiting for; `respond` does not know about
                // buffers, and this is the one thing the session adds to its answer.
                let priority = if self.documents.is_open(path) {
                    Priority::Open
                } else {
                    Priority::Rest
                };
                self.queue.again(path.clone(), priority, 0);
            }
        }

        response
    }

    /// The two events that replace a path's text, which differ in the protocol and not here.
    ///
    /// The VFS is told **in the same breath** as the overlay, and that order matters: the schema is that a file's
    /// text and its line index are never out of step, so the entry takes the new text (and builds its index) before
    /// anything can ask a question about the file — and the summary is dropped with it, because the old one
    /// describes text that no longer exists.
    fn buffer_changed(&mut self, path: &Path, text: &str) {
        // **The clock the one indivisible piece of work is deferred by.** Recorded first, so that nothing this edit
        // starts can be mistaken for the idle period that follows it — see [`Session::a_unit_may_be_read_now`] and
        // [`UNTIL_A_UNIT_MAY_BE_READ`].
        self.last_edit = std::time::Instant::now();
        // **Judged before the old text is replaced**: the first edit of a file has no signature on record, and the
        // text the VFS is holding is what it said until this moment.
        if !self.directive_signatures.contains_key(&queue_key(path))
            && let Some(held) = self.vfs.held(path)
        {
            let before = crate::preprocess::directive::directive_signature(&held.text);
            self.directive_signatures.insert(queue_key(path), before);
        }
        let moved = self.directives_moved(path, Some(text));
        self.prefetched.clear();

        self.documents.open(path, text);
        self.vfs.insert(path, text, true);

        // **Who read what this file used to say**, asked while it still says it: `forget` below takes the summary
        // with it, and the answer is "every file that includes this one, at any depth" — see
        // [`Session::invalidate_dependents`]. Only when the file's directives changed: a file that says the same
        // things to its includers says them to the same readings.
        if moved.environment {
            self.invalidate_dependents(path);
        }

        // Drops the summary **and the cooked reading**: what this file was read *as* is no longer what it is, and a
        // declaration whose range points into the text the user just replaced is a wrong answer rather than a
        // missing one. The reading is rebuilt when the queue drains — see [`Session::cooking`].
        self.store.forget(path);
        self.held_definitions().forget(path);
        // **The translation unit of *this* file, and only this file's.** A unit is a timeline over a closure, so an
        // edit that moves a directive has changed the one rooted here; it has not changed the one rooted at a file
        // that merely *includes* this one, whose timeline is still the same sequence of directives at the same
        // offsets. Clearing the whole table was the first version of this and it threw away up to sixteen units to
        // invalidate one — including, on the next keystroke, the file's own.
        //
        // The disk entry needs no such care: it is keyed per file and checked against the closure's hashes, so it
        // refuses itself the moment anything it was built from moves (`crate::tu_cache`).
        if moved.layout {
            self.held_units().remove(&queue_key(path));
            // **And the program this file is in, for the same reason and no other.** A body edit changes the file's own
            // text, but not the program's *shape*: the closure's directives, includes and conditions are where they
            // were, so the facts a program read filed for every **other** file of the closure are still true — and the
            // edited file's own facts are rebuilt by the per-file cook (`Session::want_the_closure_cooked`). Re-reading
            // the program after every pause in typing would be **5.24 s of background CPU for an answer that did not
            // change** (measured: 180 files, 806 008 tokens, `tests/keystroke_probe.rs`).
            //
            // A *directive* edit is the other case: every offset the timeline recorded moved, and the rendering of the
            // whole program changes with it, so this file's program is read again at the next quiet moment.
            self.units_read.remove(&queue_key(path));
        }
        // **The one place a refusal is cleared**, and it is cleared because the reason for it — the text — has
        // changed. See [`Session::want_cooked_reading`], which used to clear it on every request and turned a cook
        // that cannot succeed into a loop.
        if let Ok(mut refused) = self.cooking_refused.lock() {
            refused.remove(&queue_key(path));
        }
        self.cooking.want(path);

        self.queue.again(path.to_path_buf(), Priority::Open, 0);
    }

    /// **Did this text move any directive of the file?** — the two comparisons an edit is judged by, and the record
    /// the next one is judged against.
    ///
    /// `None` for the text is a file that is gone, and it moved everything. A file with no signature on record has
    /// never been compared, so it is assumed to have moved: the conservative answer, which is one re-read.
    fn directives_moved(&mut self, path: &Path, text: Option<&str>) -> DirectivesMoved {
        let key = queue_key(path);

        let Some(text) = text else {
            self.directive_signatures.remove(&key);
            return DirectivesMoved {
                environment: true,
                layout: true,
            };
        };

        let now = crate::preprocess::directive::directive_signature(text);
        match self.directive_signatures.insert(key, now) {
            Some(before) => DirectivesMoved {
                environment: before.environment != now.environment,
                layout: before.layout != now.layout,
            },
            None => DirectivesMoved {
                environment: true,
                layout: true,
            },
        }
    }

    /// **Drop the cooked readings a change to `path` invalidates, and want them built again.**
    ///
    /// A file's cooked reading is a reading of its *environment* as well as of its text: `#define BEGIN_NS namespace
    /// ns {` is a fact about `ns.h`, and every file that includes `ns.h` — at any depth — read as whatever that
    /// macro said when its own reading was built. So a change here invalidates the reading of **every file that can
    /// see it** ([`ProjectIndex::dependents_of`]).
    ///
    /// # Why it is no longer "the open files that include this one"
    ///
    /// Because that was the set of files that *had* a reading. The session now cooks the whole project
    /// ([`Session::want_the_project_cooked`]), so a header's dependents hold readings too — and a reading left
    /// behind after its environment changed is a **wrong** answer, not a missing one. Dropping rather than marking
    /// is the same rule as before: between the change and the next drain, "not found" is honest, while a declaration
    /// whose range describes text that is no longer there is not.
    ///
    /// # When it is called
    ///
    /// Only for a file whose **directives** changed ([`Session::directives_moved`]). The environment a dependent reads
    /// in is what its includes' directives brought into force: a `#define` (added, removed, or with a new body), an
    /// `#undef`, an `#include`, a condition. An edit that changes none of those — a function body, a comment, a
    /// declaration — says the same thing to every includer, so their readings stay. The gate used to be "the file
    /// defined macros *before* the edit", which is the wrong question twice over: it read the text the user had
    /// already replaced (so the edit that *added* the first `#define` to a header was never noticed), and it ignored
    /// everything but `#define` (an `#include` added to a macro-free header changes what every includer sees).
    ///
    /// Conservative in the safe direction: the cost of being wrong is one re-cook, and the cost of being wrong the
    /// other way is a stale answer.
    fn invalidate_dependents(&mut self, path: &Path) {
        let dependents = self.store.index().dependents_of(path);
        for dependent in dependents {
            // **And the timeline it was read out of.** A cooked reading is not the only thing a dependent holds
            // that was made out of this file: it also has a **translation unit**, and the in-memory one is checked
            // against nothing at all. The disk entry is keyed on the content of its closure, so this edit refuses
            // it on its own — which is why the defect hid here: the cache looked invalidation-proof, and the table
            // in front of it was not.
            //
            // Measured: with the readings dropped and the timelines left behind, `ns.h`'s macro changed from
            // `namespace one` to `namespace two`, the file was re-cooked **out of the timeline that still said
            // `one`**, and `two::Widget` came back `NotDeclaredHere` — with every step of the invalidation
            // working. The re-cook is what needs the new timeline, so this is where it has to go.
            self.held_units().remove(&queue_key(&dependent));

            // **And the summary itself, which is the part this loop did not used to own.** A dependent's summary is
            // now a reading of *this* file's replacement lists — `Widget` is in `std` because `open.h` says
            // `#define _STD_BEGIN namespace std {`, and the parser read that out of the rendering — so a directive
            // that moved makes every dependent's summary a statement about a body that no longer exists.
            //
            // Dropping it is the *same* answer the cooked reading gets, for the same reason: between the edit and
            // the next drain, "not found" is honest while `std::Widget` pointing at a body that now opens `studio`
            // is not. Nothing is lost for ever — the file is queued, read, and the re-read pass renders it again
            // from the new timeline, which is exactly the pipeline that built it.
            //
            // Measured: without this, `std::Widget` kept answering after `_STD_BEGIN` became `namespace studio {`,
            // with every other step of the invalidation working — and the assertion that used to stand for this
            // (`macro_readings` non-empty) was a note nothing read, so the capability had never been wired at all.
            let was_indexed = self.store.forget(&dependent);
            if was_indexed {
                self.queue.again(dependent.clone(), Priority::Rest, 0);
            }

            // **The named rendering too.** `renderings` is keyed by the *dependent's* own text, which did not
            // change — so the entry left behind is a rendering of a world whose macros have moved, and the next
            // ask would find it instead of rendering again.
            if let Ok(mut known) = self.renderings.lock() {
                known.retain(|(held, _)| held != &dependent);
            }
        }
    }

    /// Mark **the files a reader is looking at** for cooking, skipping the ones that already have a reading.
    ///
    /// # What this used to cover, and the measurement that changed it
    ///
    /// Cooking used to cover the open file's **whole closure** and then **every indexed file**. Measured on one real
    /// project — a single `main.cpp` including `<cstdio>`, `<iostream>`, `<optional>` and `<string>`, whose closure
    /// is **138 files**:
    ///
    /// ```text
    /// index (summaries, cached)            cold 24.2 s | warm  4.2 s
    /// cooking every file of the closure    cold 11.7 s | warm 11.9 s   ← the cache cannot help: a reading is not stored
    /// ```
    ///
    /// and the reading that half buys is **one declaration in a thousand**: `cook(<string>)` reports 1000
    /// declarations, of which **1** is one the raw reading does not already have. The rest — the scopes, the
    /// members, the aliases, `std::string` itself — comes from the raw reading once the closure's **macro bodies**
    /// are in hand ([`crate::FileIndexer::with_macro_bodies`]).
    ///
    /// # What is looked at, and why the direct includes are part of it
    ///
    /// ```text
    /// the open files                    what a reader is reading
    /// the files their text names        `#include "api.h"` is a line the reader wrote; a declaration a macro
    ///                                   writes in `api.h` is **`api.h`'s** declaration, and a question asked from
    ///                                   `main.cpp` has to be answered with a jump into `api.h`
    /// ```
    ///
    /// and nothing else. A file one level further down has no reading until something looks at it —
    /// [`Session::want_cooked_reading`] is that "something" for everything the editor did not open, and it is one
    /// file at a time rather than a subtree.
    ///
    /// # Why a file that already has a reading is not wanted again
    ///
    /// Because this runs at **every** idle drain, so "want it" has to mean "if it is not already read": a queue
    /// that is refilled with files that have readings never empties, and a session whose backlog never empties
    /// never reports itself idle — which is a hang, not a slowdown. (Measured the hard way: the pump spun for over
    /// a minute in `a_file_nobody_looked_at_is_not_cooked_and_one_a_request_names_is`, and every request behind it
    /// waited.)
    fn want_the_closure_cooked(&mut self, root: &Path) {
        let already_read =
            |session: &Self, path: &Path| session.store.index().cooked_declarations(path).is_some();

        // **A file the index cannot describe is not a file to want.** `Session::cooking_materials` answers `None`
        // for it — the summary lookup is the first thing a cook needs — so wanting it is a queue entry that can never
        // be served: the slice claims it, gathers no materials, commits nothing, and the call that wanted it (this
        // one, per drain) wants it again. Measured on `import std;` in a real project, that is a **livelock**: the
        // server log showed `index slice: prepared 0, committed 0, write lock held 0 ms, 2 to cook, 0 left` once a
        // second for as long as the window stayed open, and every diagnostic pull — which waits for
        // `pending_work() == 0` — took **1.4 s** to be answered by an index that had nothing left to do.
        //
        // The two files in that measurement were the standard module's source and its companion, wanted by a file
        // that says `import std;`: they are reachable through the *module* graph, not through `#include`, so no read
        // ever filed a summary for them under the path this walk knows.
        let can_be_cooked =
            |session: &Self, path: &Path| session.store.index().summary(path).is_some();

        let direct = self.direct_includes_of(root);
        // **And a file a cook has already refused is not wanted again.** The guard above covers the file the index
        // cannot describe; a file whose *materials* cannot be gathered — a provisional unit, a render that declines —
        // passes it, is claimed, builds nothing, and is wanted again by the next drain. Measured on a real session:
        // `4 to cook, 1 left` / `2 left` / `0 left` every second, forever, at 28 % of a core.
        let refused = |session: &Self, path: &Path| {
            session
                .cooking_refused
                .lock()
                .map(|refused| refused.contains(&queue_key(path)))
                .unwrap_or(false)
        };

        for included in &direct {
            if !already_read(self, included)
                && can_be_cooked(self, included)
                && !refused(self, included)
            {
                if crate::probes::cooking() {
                    eprintln!(
                        "want_the_closure_cooked({}): queued {} (not read, cookable, not refused)",
                        root.display(),
                        included.display()
                    );
                }
                self.cooking.want(included);
            }
        }

        // **One level further, because a macro that opens a namespace lives one file down.**
        //
        // MSVC's `<string>` is a thin layer: it declares `getline` and the numeric conversions, and everything a
        // reader actually asks for — `basic_string`, and the `string` alias itself — is written in `<xstring>`,
        // which `<string>` includes. A **raw** reading of `<xstring>` cannot place any of it: its text says
        // `_STD_BEGIN`, not `namespace std {`, so every declaration in it is filed at **file scope** —
        // `::basic_string`, not `std::basic_string` — and the macro's own body is only substituted by a cook.
        //
        // Measured on a file whose whole text is `#include <string>`:
        //
        // ```text
        // std::  → 23 items  ["getline", "stod", "stof", "stoi", "stol", "stold"]
        //           every one of them `<string>`'s own, and `std::string` is not there
        // #include <xstring> instead → 25 items, `basic_string` among them
        // ```
        //
        // So the level that was missing was exactly the one between the two, and this is it. It is bounded
        // because the unbounded version is the 11.7 s above: a translation unit that includes all of the standard
        // library has hundreds of files at this depth, and cooking is a render and an index per file.
        let mut budget = COOK_ONE_LEVEL_FURTHER;
        let mut nested_files: Vec<PathBuf> = Vec::new();
        for included in &direct {
            for nested in self.direct_includes_of(included) {
                if budget == 0 {
                    break;
                }
                if already_read(self, &nested) || refused(self, &nested) {
                    continue;
                }
                if !nested_files.contains(&nested) {
                    nested_files.push(nested.clone());
                }
                self.cooking.want(&nested);
                budget -= 1;
            }
        }

        // **And one level beyond that**, which is where a class a header *uses* is declared.
        //
        // The level above reaches the headers a file includes; the class those headers talk about is often one more
        // `#include` down. MSVC's `<string>` is the ordinary case: it is a thin layer over `<xstring>`, and
        // `basic_string` — and therefore `std::string` — is declared in `xstring`. Measured on EmmyLuaCodeStyle's
        // `LuaParser/src/File/LuaSource.cpp` (whose `LuaSource.h` includes `<string>`):
        //
        // ```text
        //                    std::string before   after cooking `xstring` by hand
        //   members_of        NotDeclaredHere      Yes(222 members)
        // ```
        //
        // — so `std::string` was unknown in a file that includes the header that includes the header that declares
        // it, and the same reading left `std::basic_string` and `std::size_t` unknown. The cook of `xstring` costs
        // **2.17 s** and happens once per session.
        //
        // Bounded like the level above, and for the same reason: this is a queue of work for the pump, and a
        // translation unit that includes the standard library has hundreds of files at every depth.
        let mut budget = COOK_TWO_LEVELS_FURTHER;
        for nested in &nested_files {
            for deeper in self.direct_includes_of(nested) {
                if budget == 0 {
                    break;
                }
                if already_read(self, &deeper) || refused(self, &deeper) {
                    continue;
                }
                self.cooking.want(&deeper);
                budget -= 1;
            }
        }

        if !already_read(self, root) {
            self.cooking.want(root);
        }
    }

    /// The files `path` includes **directly**, as the index resolved them.
    fn direct_includes_of(&self, path: &Path) -> Vec<PathBuf> {
        self.store
            .index()
            .summary(path)
            .map(|summary| {
                summary
                    .includes
                    .iter()
                    .filter_map(|include| include.resolved.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// **Mark one file for cooking** — "something is about to look at this".
    ///
    /// The entry point for a caller that knows which file a question is about: the shell calls it for every file a
    /// request names, so the answer *after* this one is the compiler's reading and this one is the file's own. It
    /// is deliberately not a "cook it now": the caller is a query, the cooking is the pump's work, and a keystroke
    /// must not wait for a unit walk.
    /// **The diagnostics of a whole project**, which until now no consumer could ask for.
    ///
    /// `diagnostics` answers for a file whose **cooking** the index holds, and a reading is cooked because something asked
    /// for one — so a batch consumer had to know three things the server's own pump knew and nothing published: that cooking
    /// is a queue of its own, that `advance` does not drive it, and that its slices must be rendered *and committed*. Measured
    /// on a 1216-file project: cooking all of it took 56 s, and a consumer that drove only `advance` cooked 46 files and saw
    /// nothing.
    ///
    /// This is that loop as one method: ask for every rendering, drain the index queue, drain the cooking queue in slices
    /// (rendering each and committing it), then answer with every file the index has.
    ///
    /// ```text
    /// 160 s   index       the caller does this, or `index_everything`
    ///  56 s   cook        this method
    ///   …     diagnose    the checks, per file
    /// ```
    pub fn diagnose_everything(
        &mut self,
        up_to: usize,
    ) -> Vec<(std::path::PathBuf, FileDiagnostics)> {
        let paths: Vec<std::path::PathBuf> = self
            .store
            .index()
            .summaries()
            .map(|summary| summary.path.clone())
            .take(up_to.max(1))
            .collect();

        self.want_everything_cooked();

        // The index queue first: a reading cannot be cooked for a file whose summary is not there.
        let mut waves = 0usize;
        while self.pending() > 0 {
            if self.advance(64).is_empty() {
                break;
            }
            waves += 1;
            if waves > 100_000 {
                break;
            }
        }

        // **Then the cooking queue**, in the pump's own three steps: claim a slice, build its materials, render **and
        // commit** them. Committing is the step that makes the reading visible to `diagnostics` — a rendering that is
        // returned and dropped is a rendering nobody can ask about.
        let mut cooked = 0usize;
        loop {
            let claimed = self.claim_cooking(64);
            if claimed.is_empty() {
                break;
            }
            let materials = self.materials_for(claimed);
            let rendered = self.render_them(materials);
            cooked += self.commit_them(rendered).len();
            if cooked > up_to.max(1) * 2 + 1024 {
                break;
            }
        }

        paths
            .into_iter()
            .filter_map(|path| self.diagnostics(&path).map(|found| (path, found)))
            .collect()
    }
    pub fn want_cooked_reading(&mut self, path: &Path) {
        // **A refusal is not cleared by asking again, and that is the fix for a live loop.** It used to be: every
        // diagnostics pull called this, cleared the refusal of the file the pull was about, and re-queued it — so a
        // file whose cook can never succeed was queued, claimed, refused, cleared and queued again, for as long as
        // the window stayed open, at one core. Measured from a real session's log: an `index slice` line every
        // ~250 ms, minutes after the index had drained. A refusal is cleared where the *reason* could have changed —
        // an edit ([`Session::buffer_changed`]) — not by a request that changes nothing.
        let refused = self
            .cooking_refused
            .lock()
            .map(|refused| refused.contains(&queue_key(path)))
            .unwrap_or(false);
        if refused {
            return;
        }
        // **And read it, if nothing has.** A reading is built out of a summary ([`Session::cook`] answers `None`
        // without one), and a file a request names may be one no pump has reached — a header outside every
        // translation unit, a file the workspace scan did not list. Wanting it cooked and not reading it is a
        // promise the session never keeps: the next request gets the same raw reading and the `isIncomplete` flag
        // goes on telling the client to come back for an answer that is not coming.
        if self.store.index().summary(path).is_none() {
            self.queue.add(path.to_path_buf(), Priority::Open, 0);
        }
        // **"Want" means "if it is not already read", and that guard was missing here.** The same rule
        // [`Session::want_the_closure_cooked`] has had all along, and the reason it has it: a queue refilled with
        // files that have readings never empties. Every diagnostics pull calls this for the file it is about, so
        // without the guard **every pull cooked the open file again** — measured on a real project with
        // `CPPLS_TRACE_COOKING`, ten pulls in 2.8 s: `cook slice: asked 1, rendered 1` ten times, each a render and a
        // parse of a 4 000-line file, and the same again for one of its includes.
        //
        // The reading is dropped by [`Session::buffer_changed`] the moment the text or an included file changes, so a
        // file that *needs* a new reading never reaches this line with a stale one.
        //
        // **"Has a reading" is not "has what a view needs".** A reading the store **served** carries the facts and no
        // rendering — `take_the_stored_refinements` inserts the record and marks the file refined, and a rendering is
        // the caller's half. A file in that state answered this line's `return` and never got a rendering, so every
        // `view` of it was `None` — which is why the wave used to queue **all** served files for a full cook, eagerly,
        // and why a warm start of the modules fixture cost 20–506 s doing renders and parses for facts the store had
        // already answered. The question this line has to ask is therefore the *other* one: is there a rendering for
        // this text? If not, the cook is still owed, and it is owed when someone asks — this call — rather than for
        // every file a wave served.
        if self.store.index().cooked_declarations(path).is_some() {
            let rendered = self
                .vfs
                .held(path)
                .is_some_and(|file| self.known_rendering_of(&file.path, &file.text).is_some());
            if rendered {
                return;
            }
        }
        self.cooking.want(path);
    }

    /// **Would [`Session::want_cooked_reading`] change anything for this file?** — the question a caller asks *before*
    /// taking the write lock.
    ///
    /// # Why this exists, and what it is worth
    ///
    /// A request's wait is the time until the lock it needs is free, and the pump holds the session for a hundred
    /// milliseconds at a time and sometimes for more than a second — it is parsing a project's include closure, and
    /// parsing has to read the index. So a request that takes the **write** lock for work it does not have to do
    /// waits for all of it. Measured over the wire on the latency fixture: a completion's three write-lock
    /// acquisitions cost **8, 9, 0 ms** of work and **9, 8, 499, 427, 137, 293, 179, 1147, 119, 306 ms** of *wait* —
    /// every millisecond of it the queue, none of it the work.
    ///
    /// And the work is usually nothing at all: the file a request is about is normally one the editor has open, so
    /// the VFS is holding its text (`didOpen`) and its cooked reading is either built or already asked for.
    ///
    /// # Why it is not a second opinion about anything
    ///
    /// It asks the two questions the mutation itself asks, of the same state: the VFS's own `held`
    /// table, the index's own cooked-reading slot, and the cooking queue's own membership set
    /// ([`Cooking::wants`], which reads the set [`Cooking::want`] inserts into).
    ///
    /// Conservative in the safe direction: `false` means "take the lock and do the work", which is always correct.
    /// `true` means the two calls above would each do nothing, so skipping them changes no answer — the query that
    /// follows reads what is already there.
    pub fn is_ready_for_a_request_about(&self, path: &Path) -> bool {
        self.vfs.held(path).is_some()
            && (self.store.index().cooked_declarations(path).is_some() || self.cooking.wants(path))
    }

    /// **Mark every indexed file for cooking** — the project-wide sweep, kept behind the caller's choice rather
    /// than done on every drain.
    ///
    /// It is what a client that publishes diagnostics for **every file in the workspace** needs (the workspace
    /// pass, and a pull request about a file it is showing): a file with no cooked reading is answered from its own
    /// text, where a declaration a macro writes is a guess and a branch nobody takes is still there. It is also
    /// what made starting a one-file project cost **11.7 s** (the measurement in
    /// [`Session::want_the_closure_cooked`]), which is why the session no longer does it by default: what is looked
    /// at is cooked, and a file nobody looks at is answered from its own text and says so
    /// ([`crate::DiagnosticReading`]).
    ///
    /// A caller that wants the whole project read as a compiler reads it says so — that is what this is for.
    pub fn want_everything_cooked(&mut self) {
        let wanted: Vec<PathBuf> = self
            .store
            .index()
            .summaries()
            .filter(|summary| {
                self.store
                    .index()
                    .cooked_declarations(&summary.path)
                    .is_none()
            })
            .map(|summary| summary.path.clone())
            .collect();

        for path in wanted {
            self.cooking.want(&path);
        }
    }

    // ---------------------------------------------------------------------------------------------
    // The work
    // ---------------------------------------------------------------------------------------------

    /// Index up to `steps` files, and say what each one did.
    ///
    /// One file per step, in the order [`Worklist`](crate::Worklist)'s documentation fixes — what the user is
    /// looking at, then what those files include, then the rest of the project — because that order is the
    /// difference between a first answer after one file and after ten thousand. The session's list is not a
    /// [`Worklist`](crate::Worklist) for two reasons: it outlives any one batch (a notification re-seeds it), and a
    /// `Worklist` borrows the store mutably, so a session holding one could not answer a query between two steps.
    /// The *order* is the same one, and the outcome of a step is read by the same rule ([`outcome_of`]).
    ///
    /// A caller decides how much: one step per idle tick is a server that stays responsive, `advance(64)` is a
    /// server that catches up after a notification, and the loop in [`Session::index_everything`] is what a test or
    /// a batch caller wants.
    ///
    /// # The parallel half, and why it changes no answer
    ///
    /// Reading a file into the index is a parse, and a parse is a pure function of the file: the summary is made of
    /// the text, the configuration and the filesystem, and never of what the index already holds
    /// ([`SummaryStore::prepare`]). So the files at the head of the queue are **prepared together**, on the machine's
    /// cores, before the loop starts — and then the loop is what it always was: pop the next file, put its summary in
    /// the index, queue what it includes. The order the index sees files in, the order includes are discovered in and
    /// what a step reports are all the sequential ones; only the waiting is shared out. A prepared file the loop does
    /// not reach in this call (a file it discovers outranks it) is simply dropped and read again when its turn comes.
    pub fn advance(&mut self, steps: usize) -> Vec<Step> {
        let mut done = Vec::new();
        // **What one batch costs**, under the unit trace. The modules fixture's module read is nine of these and
        // 48–59 s, and no number outside could say which of the nine — or which half of a batch, the wave's closure
        // preparation or the per-file steps — was the seconds.
        let started = std::time::Instant::now();

        for taken in 0..steps {
            let Some((path, priority, depth)) = self.queue.pop() else {
                break;
            };

            // **A file nobody prepared is the front of a wave**: the file just popped, the files queued behind it,
            // and — this is the part that makes the wave as wide as the project — everything *they* include, found by
            // scanning `#include` lines before any of them is parsed ([`SummaryStore::prepare_closure`]). The first
            // file of a closure is read alone; the moment it has been lexed, every core is reading what it names.
            //
            // Bounded by what this call was asked to do, twice over: at most two calls' worth of files are read
            // ahead, and the rest wait for the next call — a slice that read a whole closure would hold the writer
            // for the whole of a cold start.
            let key = queue_key(&path);
            let left = steps - taken;
            // **A file that is going to be read through a timeline is left out of the wave.** The wave parses on the
            // read path, where a summary is a pure function of the text — which is exactly what cannot place a scope
            // a macro in **another** file opens. For the one file this session knows better about, the wave would
            // build the wrong reading *and write it to the disk cache*, and then `index_one`'s own attempt would be
            // served that entry as a hit: measured, dropping the wave's answer afterwards turned the step into
            // `Reused` and left the environment unused, which is worse than either.
            //
            // So it is not a root of this wave, and its includes are prepared by the iteration that reads it.
            // **This is what makes §5.2's steps 1–3 reachable at all**: they live at `index_one`'s `None` arm, and
            // the pump's path always reached it with `Some`, so the mechanism was correct, tested, and dead.
            //
            // The question is [`Session::will_read_it_as_the_program`], which is the timeline in hand **or one that
            // can be walked now** — the file a reader is typing in, whose unit was evicted from the bounded table or
            // has never been built. It is asked here, before the wave, because after it the raw reading would already
            // have been written down.
            //
            // **And only when a wave is about to be built.** `left == 1` is the single-file step — the one a pull and
            // a `didOpen` take — and it never consults `prefetched` for a gate: with nothing prepared, `index_one`
            // decides for itself, so asking here would be the same question twice and the second answer is the one
            // that matters.
            let wave_would_parse_it_as_text = left > 1
                && !self.will_read_it_as_the_program(&path)
                && !self.prefetched.contains_key(&key);
            if wave_would_parse_it_as_text {
                let mut wave = vec![path.clone()];
                wave.extend(
                    self.queue
                        .peek(left - 1)
                        .into_iter()
                        .filter(|queued| !self.prefetched.contains_key(&queue_key(queued))),
                );

                let room = (steps * 4).saturating_sub(self.prefetched.len());
                let prepared_from = std::time::Instant::now();
                let made = {
                    let queue = &self.queue;
                    let prefetched = &self.prefetched;
                    self.store.prepare_closure(
                        &wave,
                        |target| {
                            !queue.is_worked(target) && !prefetched.contains_key(&queue_key(target))
                        },
                        room.min(steps * 2).max(wave.len()),
                    )
                };
                if crate::probes::units() {
                    eprintln!(
                        "ADVANCE wave of {} file(s) prepared {} in {:?}",
                        wave.len(),
                        made.len(),
                        prepared_from.elapsed(),
                    );
                }
                for (path, made) in made {
                    self.prefetched.insert(queue_key(&path), made);
                }
            }

            // **Read here rather than out of the wave**, for the file the wave was told to leave alone: with no
            // prepared entry, `index_one` builds the summary itself and can hand it the environment. Every other
            // file still takes the wave's answer.
            let ready = self.prefetched.remove(&key);
            let step_started = std::time::Instant::now();
            done.push(self.index_one(path, priority, depth, ready));
            if crate::probes::units() {
                eprintln!(
                    "[advance] one step took {} ms ({} of {} step(s))",
                    step_started.elapsed().as_millis(),
                    taken + 1,
                    steps
                );
            }
        }

        // Nothing left to read means nothing left to read *ahead*: what is still here was prepared for a queue that
        // has since been emptied by something else, and would be stale by the next time it is looked at.
        if self.queue.pending() == 0 {
            self.prefetched.clear();
        }

        // **The units are the drain's, and `finish_a_slice` builds them for exactly the slice it takes.** This used
        // to root them at `done` — the files this call just read — and the two are different sets: the drain takes
        // its files from `parsed_since_the_last_pass`, a backlog that outlives one call. Rooted at `done`, a slice
        // whose files the units do not cover is re-read in the pass's fallback form (a closure walk per file), and
        // with `advance(64)` that is up to **sixty-four timeline walks per call**: measured through
        // `examples/checks_probe --index-only` on `EmmyLuaCodeStyle`, **224 s** against **20 s** for the same index
        // through the pump, which had already been given the right slice. Passing nothing is not "passing no
        // environment" — `finish_a_slice` builds the four the pass needs, outside a writer this path never holds.
        let units = Vec::new();
        if crate::probes::units() {
            // **Counts, not the names.** A line with sixty-four absolute paths is ~10 KB, and through a redirected
            // stderr that is a write which can block — inside this function, after `started.elapsed()` was taken, so
            // the *caller* measures the block and the line itself does not. That is the whole of a "56 s batch" that
            // this trace reported while the batch's own timer said 7 ms.
            eprintln!(
                "ADVANCE steps={} in {:?} done={}",
                steps,
                started.elapsed(),
                done.len()
            );
        }
        let drain = {
            let finishing = std::time::Instant::now();
            let drain = self.finish_a_slice(&done, units);
            if crate::probes::units() {
                eprintln!(
                    "ADVANCE finish {} file(s) in {:?} (batch total {:?})",
                    done.len(),
                    finishing.elapsed(),
                    started.elapsed(),
                );
            }
            drain
        };

        // **A file this slice read is NOT queued for cooking, and three tests are why.**
        //
        // It was tried, and it is the third time this session that "queue more things to cook" has been the wrong
        // answer:
        //
        // ```text
        //   a_file_nobody_looked_at_is_not_cooked_and_one_a_request_names_is   FAILED
        //   a_header_the_user_never_opened_is_cooked_when_a_request_names_it   FAILED
        //   a_unit_read_reads_the_whole_program_once                           FAILED
        // ```
        //
        // They assert that **a request is what causes a cook**, and they are right: reading a file is not asking a
        // question about it, and a project of ten thousand files must not be rendered because a scan walked past
        // them. The coverage a caller needs for a specific file is [`Session::want_cooked_reading`] — one file, named
        // by whoever wants it — which is what the completion fixtures now call before asking. The reader is what
        // decides what is worth reading the way a compiler reads it; the pump is not.

        // **This path pays for its own drain, here.** `advance` is the non-pump entry point — a test, a batch caller —
        // and it holds no lock at all, so the split that exists to keep the pump's write lock short costs it one
        // call each way. The answer is the same: what `finish_a_slice` planned is worked out and applied.
        let drain_started = std::time::Instant::now();
        let prepared = self.prepare_the_drain(drain);
        let prepared_in = drain_started.elapsed();
        let committed = self.commit_the_drain(prepared);
        if crate::probes::units() {
            eprintln!(
                "ADVANCE drain prepared {:?}, committed {committed} in {:?}",
                prepared_in,
                drain_started.elapsed() - prepared_in,
            );
        }

        // **And a whole program if one is owed**, in the same three phases the pump takes apart — this path has no
        // lock to keep free, so it takes them in a row. The clock and the budget are inside
        // [`Session::read_a_program_now`] (`the_program_to_read`), so a caller that has just edited reads nothing.
        //
        // **Where a batch's seconds go, when they go anywhere.** The timer here exists because everything before it
        // measures in milliseconds: pre-finish, `finish_a_slice` and `prepare_the_drain` are all under 60 ms on the
        // modules fixture, and the batches that take 11 s and 47 s have *nothing else* in them. A program read is a
        // closure walk, a render of every file in it, a parse of the result and a sweep of every declaration —
        // 8.9 s for `std.ixx` measured on its own (`tests/unit_probe.rs`), and this is where a *batch* pays it.
        if self.is_idle() && self.pending_cooking() == 0 && self.refinements_wanted() == 0 {
            let reading = std::time::Instant::now();
            self.read_a_program_now();
            if crate::probes::units() {
                eprintln!("ADVANCE read_a_program_now in {:?}", reading.elapsed());
            }
        }

        done
    }

    /// **Prepare everything a slice of `steps` is going to need, holding nothing but `&self`.**
    ///
    /// The expensive half of indexing a wave — reading the text, lexing, parsing, building a summary — is a pure
    /// function of the file, the configuration and the filesystem (`SummaryStore::prepare`). It changes nothing, so
    /// it does not need the write lock that a query is waiting for, and this is the entry point that lets a caller
    /// pay for it under a **read** lock instead.
    ///
    /// [`Session::commit_a_wave`] is the other half: it takes the same files and puts their summaries in the index,
    /// which is cheap and needs `&mut self`. The pair exists because of what the write lock costs — measured, a slice
    /// of thirty-two files held it for **84, 503, 882, 1190 and 2430 ms**, so every completion arriving inside that
    /// window waited for all of it. With the parse moved out, the same slice holds it for **9–12 ms**.
    pub fn prepare_a_wave(&self, steps: usize) -> PreparedWave {
        // **The files [`Session::commit_a_wave`] is about to pop**, which is what `peek` answers — the queue as it
        // stands now, without taking anything. A file a step discovers later can outrank these and is prepared on a
        // later wave, which is why the committing half still has a slow path.
        let wave: Vec<PathBuf> = self
            .queue
            .peek(steps)
            .into_iter()
            .filter(|path| {
                let key = queue_key(path);
                !self.prefetched.contains_key(&key) && !self.queue.is_worked(path)
            })
            // **And the file that is going to be read through its timeline is not part of it.** A wave parses text;
            // this file is read by [`Session::commit_a_wave`]'s `index_one`, which has the session and can walk the
            // timeline. See [`Session::will_read_it_as_the_program`] for the question and
            // [`Session::the_timeline_to_read_through`] for what it costs.
            .filter(|path| !self.will_read_it_as_the_program(path))
            .collect();

        // **The units, rooted at the files the pass after this one will actually re-read** — which is the slice
        // [`Session::finish_a_slice`] is about to drain, **not** the wave about to be parsed.
        //
        // The first version of this asked for units over `wave`, and it measured **worse** — 2636 ms against
        // 1105 ms — for a reason that is worth writing down, because it is a property of what a unit *is*: a
        // translation unit is a **timeline over a whole include closure**, so rooting one at thirty-two files
        // produces a bigger timeline than rooting one at four, and `definitions()` on it costs more. The cache is
        // keyed by the content of the closure as well, so the two are separate entries and the walk happens twice.
        //
        // What the second pass costs is set by the slice it drains, and the reason is in `re_read_what_a_body_changes`:
        // it scans every `#define` in the project and narrows the scan to the names the re-read files' **texts**
        // mention. So the slice chooses how many names are in play, and the measurement agrees exactly — 1 file is
        // 150 ms, 8 is 1105 ms, and a wave of 32 is 2636 ms.
        //
        // `parsed_since_the_last_pass` is a field, so the slice the commit will drain is known *here*: take the same
        // prefix it will take, and the units cover precisely the files they are used for.
        //
        // **Computed before the empty-wave return below, and that is a measured defect rather than tidiness.** The
        // last stretch of every start-up is the drain phase: the index queue empties, and the pump then commits
        // slice after slice of re-reads, each wave having **nothing to prepare** — so this function returned early
        // and handed the drain `units: []`. `prepare_the_re_read` reads that as "a caller with no timelines" and
        // changes shape: it copies **every indexed file's text** into a map and asks a question about **every
        // `#define` in the project** (see the `eager` arm there). Measured on `EmmyLuaCodeStyle` (1761 files): a
        // warm start was **66 s**, of which 60 s was twelve such calls — 5 s each for four files — while the index
        // itself took 4 s. One unit per drained slice is what the pass was designed around, and the early return was
        // silently withholding it.
        let slice: Vec<PathBuf> = self
            .parsed_since_the_last_pass
            .iter()
            .take(RE_READ_SLICE)
            .cloned()
            .collect();
        // **What the cache already refined, before any timeline is built.** A file whose reading was refined under
        // its own closure needs no unit, no render and no parse: the summary goes into the index at commit time. What
        // is left is the honest work, and the timelines are built for that and nothing else.
        let mut refinements: Vec<crate::FileSummary> = Vec::new();
        for path in &slice {
            if let Some(summary) = self.store.the_stored_refinement(path) {
                refinements.push(summary);
            }
        }
        // **No timelines are built for the pass any more** (phase 2): the reading is composed from the index's macro
        // facts, so the whole-closure walk was the cost of a question it no longer answers. `units` stays in the plan so
        // the callers that still expect one see an empty list rather than a different shape.
        let units = Vec::new();

        if wave.is_empty() {
            return PreparedWave {
                prepared: Vec::new(),
                units,
                refinements,
            };
        }

        let room = (steps * 4)
            .saturating_sub(self.prefetched.len())
            .max(wave.len());
        let queue = &self.queue;
        let prefetched = &self.prefetched;
        PreparedWave {
            prepared: self.store.prepare_closure(
                &wave,
                |target| !queue.is_worked(target) && !prefetched.contains_key(&queue_key(target)),
                room,
            ),
            units,
            refinements,
        }
    }

    /// **Put what [`Session::prepare_a_wave`] prepared into the index**, and queue what it includes.
    ///
    /// The cheap half: no parsing, no reading, no walking — `SummaryStore::commit` per file and `Worklist::add` for
    /// each include it names. Everything the parse needed was paid for under the read lock, and so was the unit walk
    /// that the pass after this one needs.
    /// **Put what [`Session::prepare_a_wave`] prepared into the index**, and queue what it includes.
    ///
    /// The cheap half: no parsing, no reading, no walking — `SummaryStore::commit` per file and `Worklist::add` for
    /// each include it names. Everything the parse needed was paid for under the read lock, and so was the unit walk
    /// that the pass after this one needs.
    ///
    /// **What is left to do comes back as a [`Drain`]**, and this is the second half of the same idea: the pass that
    /// used to run here re-reads files and walks environments, which is 150 ms per file, and it is `&self` work. The
    /// caller takes the drain to [`Session::prepare_the_drain`] under a read lock and brings it back to
    /// [`Session::commit_the_drain`] — so the write lock is held for the inserts and for nothing else.
    pub fn commit_a_wave(&mut self, wave: PreparedWave, steps: usize) -> (Vec<Step>, Drain) {
        let PreparedWave {
            prepared,
            units,
            refinements,
        } = wave;
        for (path, made) in prepared {
            self.prefetched.insert(queue_key(&path), made);
        }
        // **The readings that came from the cache are applied here**, and their files leave the second pass's backlog:
        // there is nothing left to refine in them.
        for summary in refinements {
            self.parsed_since_the_last_pass
                .retain(|queued| queued != &summary.path);
            self.store.apply_a_stored_refinement(summary);
        }

        // **How long one commit may spend inserting** — the write hold this half controls.
        //
        // A count is a guess about a sum that varies with the files a wave happened to hold: measured on
        // `EmmyLuaCodeStyle`, an ordinary header's insert is **1–8 ms** and the largest (`xlocale`,
        // `windows.media.audio.h`) **13–27 ms**, so 24 files were 44.3 ms in one run and 78.4 ms in the next. Spending a
        // budget instead bounds the hold by the work that determines it. The files not popped stay at the front of the
        // queue, already prepared in `prefetched`, and the next slice commits them without parsing them again — so the
        // wave stays large, and the wave is what sets throughput (one second pass per slice).
        const COMMIT_MS: u128 = 20;
        let mut done = Vec::new();
        let committing = std::time::Instant::now();
        for _ in 0..steps {
            if !done.is_empty() && committing.elapsed().as_millis() >= COMMIT_MS {
                break;
            }
            let Some((path, priority, depth)) = self.queue.pop() else {
                break;
            };
            let ready = self.prefetched.remove(&queue_key(&path));
            done.push(self.index_one(path, priority, depth, ready));
        }

        if self.queue.pending() == 0 {
            self.prefetched.clear();
        }

        let drain = self.finish_a_slice(&done, units);

        (done, drain)
    }

    /// **What has to happen once a slice's files are in the index** — the second pass, the cooks, the environments.
    ///
    /// Split out of [`Session::advance`] because there are now **two** ways a slice gets committed — `advance`, which
    /// parses inside the write lock, and [`Session::commit_a_wave`], which parses under a read lock and only commits
    /// here — and both must finish the same way. Measured: leaving this out of `commit_a_wave` fixed the latency
    /// (write lock 9–12 ms against 84–2430) and made every completion **empty**, because the cooking half lives here
    /// and a completion reads the cooked reading.
    fn finish_a_slice(
        &mut self,
        done: &[Step],
        units: Vec<std::sync::Arc<crate::TranslationUnit>>,
    ) -> Drain {
        // **The second pass, at the moment the closure is in hand**. `SummaryStore::get` reads one file with
        // the evidence the index has at that moment, and for MSVC's STL that is no evidence at all: `<vector>`'s
        // `std` scope is written in `yvals_core.h`'s `_STD_BEGIN` — a file `<vector>` *includes*, and a file is read
        // before its includes. So the files this session parsed are read again, once, now that everything they
        // include is in the index. `SummaryStore::index_includes_from` does the same thing for its own walk; this is
        // that pass on the closure this caller built one step at a time.
        //
        // **Only a file that was parsed can have a reading that changed under it**: a summary that came from the
        // disk cache was stored with whatever reading the run that wrote it had, and a file whose text did not
        // change reads the same way. So the warm path — everything reused — pays nothing, and an editing drain pays
        // for the one file the edit produced.
        //
        // **That premise is now false, and the `Reused` half below is the correction.** It was true while a stored
        // summary was a reading of the file's **own text**: the text is in the key, so the same key is the same
        // reading. A summary is now a reading of the text *and of its environment* — `Widget` is in `std` because a
        // header this file includes says so — and that second half is in no key a lookup can compute today. So the
        // disk holds **unrefined** readings (a refined one is refused at write time for exactly this reason, see
        // `prepare_the_re_read`), and a reused summary is one that may still be missing its environment. It has to
        // go through the pass like a parsed one, and the pass decides: a file that mentions no bodied macro is
        // skipped there for nothing, while one that does is rendered.
        //
        // The cost is that a warm start re-reads what a cold one re-reads.
        //
        // **And not the files that were parsed *with* their environment.** This pass exists to repair a reading
        // made without one, and `Step::with_the_environment` is the mark that says the repair has already happened
        // — a file whose summary was built from the closure's macro bodies the first time has nothing a re-parse
        // could change. That is the whole of what the flag is for; see `SummaryStore::prepare_with_the_environment`
        // for why the session can hold an environment for a file at all.
        //
        // **And a file read *with* its environment is taken back off the queue, not merely kept off it.** The filter
        // below has always kept such a file from being added, but `advance` removes nothing: a file read as text cold
        // stays queued after a later edit reads it through its timeline, and under the demand-driven contract nothing
        // drains that stale entry — traced as `/p/api.h` queued twice, `outcome Built environment false` then
        // `outcome Unstored environment true`. The same statement the filter makes, applied to the queue.
        if done.iter().any(|step| step.with_the_environment) {
            let repaired: std::collections::HashSet<PathBuf> = done
                .iter()
                .filter(|step| step.with_the_environment)
                .map(|step| step.path.clone())
                .collect();
            self.parsed_since_the_last_pass
                .retain(|queued| !repaired.contains(queued));
        }
        self.parsed_since_the_last_pass.extend(
            done.iter()
                .filter(|step| {
                    matches!(
                        step.outcome,
                        StepOutcome::Built | StepOutcome::Unstored | StepOutcome::Reused
                    )
                })
                .filter(|step| !step.with_the_environment)
                .map(|step| step.path.clone()),
        );

        // **A slice of them, not all of them.** The pass is the expensive half of this function — measured from the
        // server's own budget warning, one `index slice` held the writer for **10.9 s** where the insert it is named
        // after takes 8–19 ms, and this is what was in the rest of it. A file is re-read here because its reading
        // may have changed once its includes were in the index, so the work is proportional to how many files were
        // parsed since the last pass — which after a cold start is *every file in the closure*.
        //
        // Slicing changes what the unit walk sees: it is asked about fewer files at a time, so it may build a unit
        // for one slice that a later slice would have shared. That costs a cache lookup rather than a rebuild —
        // `translation_unit_of` caches by the content of the closure — and it buys the property this function is
        // now judged by: the writer is held for one slice's worth of work and no more.
        //
        // **The slice is claimed here and re-read outside the writer.** What this function decides is *which* files,
        // which is a drain of a `Vec`; the parse, the environment walk and the encode that follow are 150 ms per file
        // measured, and they are `&self` work — see [`Session::prepare_the_drain`].
        //
        // **The timelines came from the preparing half, rooted at exactly this slice.** They used to be built
        // here, and moving them out is the change that took a **1003 ms** unit walk out of the writer — the
        // largest single thing a drain did. What made the first attempt at that measure *worse* (2636 ms against
        // 1105) was rooting them at the wave instead of at this slice: a unit is a timeline over a whole include
        // closure, so a bigger root set is a bigger timeline and the pass is proportional to it. Rooted here, the
        // walk happens once, at the size it is used at, and under the read lock.
        let files = if self.is_idle() {
            self.take_the_files_wanted_for_refinement(RE_READ_SLICE)
        } else {
            Vec::new()
        };

        // **What the cache already refined, before any timeline is built.** The second pass's answer is a function of
        // the file's own closure, so a stored one is served and its file needs no unit at all — the difference between
        // a second start that re-reads the project and one that reads a handful of files.
        let served = self.store.take_the_stored_refinements(files);
        // **And the rendering of each served file is still owed**, because a cooked record carries facts, not text:
        // `Session::view` parses a rendering, so the file goes to the cook queue rather than being dropped as done.
        // It is the ordinary cook — render and parse — with the composition the record already answered left out.
        //
        // **Measured, and the whole of a warm start's cost**: on the modules fixture the module read is
        // `index_everything()`, this queue is what it drains, and with 289 served files it cost 20.2 s, 112 s and
        // 506 s in three runs on unchanged inputs — while parsing nothing.
        //
        // **A served file is not queued here**, and that is P6's first fix. Its facts are in the index already
        // (`take_the_stored_refinements` inserts them); its *rendering* is the half a view needs, and the ask belongs
        // to the caller that needs one: [`Session::want_cooked_reading`] now queues a cook when the facts are in but
        // no rendering is, so a request for a view of a served file still gets it, while a wave that merely walked
        // the closure pays nothing. Queuing all of them here made a warm start do a render and a parse for every fact
        // it had just read from the store.
        let files = served.still_to_do;

        // **A caller that passed no timelines for a slice it is about to drain.** `advance` roots its units at the
        // files it just read, which is not the slice this pass takes — so on the drain phase (nothing left to read,
        // a backlog in hand) it passed an empty list, and `prepare_the_re_read` changed shape because of it: every
        // indexed file's text copied, every `#define` in the project asked about. See
        // [`Session::prepare_a_wave`] for the same defect on the pump's path, where it was 28 s of a 48 s start-up.
        //
        // Building them here is inside the writer on the pump's path and free of it on `advance`'s — and the pump
        // never reaches this line, because `prepare_a_wave` now always hands it the slice's timelines.
        // **No timelines for the pass** — see `prepare_a_wave`. The composition in `prepare_the_re_read` needs none.
        let _ = &units;

        // **And the files whose cooked reading is out of date**, now that their environments are complete: a file's
        // macros are the ones its includes brought in, and the index queue emptying is the first moment that is
        // true. This is the product's only caller of the cooked reading — every declaration query picks it up from
        // the index, and the files are the ones a reader has open, or has open and just edited.
        //
        // A **slice**, like the indexing steps: cooking a file is a unit walk, a parse and an index, and a session
        // that did a whole closure in one call would hold the writer for seconds while the user types.
        //
        // **The plan this builds is what the caller pays for.** Both of the expensive things a drain does after its
        // inserts are named here and worked out in [`Session::prepare_the_drain`], which takes `&self`: the second
        // pass over `files`, and the rendering of `environments`. What is left in this function is a queue pop, a
        // summary lookup and a `want`.
        // **A slice of one or two files keeps the per-file walk**, and the threshold is the cache rather than the
        // clock: a reading built through a *slice* unit has the root's environment, not the file's own, so it cannot
        // be stored as the file's refinement (`the_closure_fingerprint`). Two session tests pin that a small project
        // serves its refinement to the next session, and a shared unit would break them for a saving of one walk.
        // From `WALKS_WORTH_SHARING` files up, the walks are many and the cacheability of one small file is not the
        // trade it was.
        let slice_units = if files.len() < WALKS_WORTH_SHARING {
            Vec::new()
        } else {
            self.units_for_the_pass(&files)
        };
        let mut drain = Drain {
            files,
            // **One timeline per file of the slice, not one per candidate the pass then finds.** The units the second
            // pass reads its environments out of were left empty here, so every candidate a slice reached fell to
            // `a_timeline_of_its_own` — a whole closure walk each. Measured on the modules fixture: a slice of six
            // files reached **44 candidates** and the re-read cost **46.8 s**, which was the whole module read.
            // `units_for_the_pass` was written for exactly this and was marked dead code because nothing called it.
            units: {
                let mut all = units;
                all.extend(slice_units);
                all
            },
            environments: Vec::new(),
        };

        if self.is_idle() {
            // **NOT WIRED HERE, and now for a better reason than the one this note used to give.**
            //
            // A whole-program read is 5.09 s for a 159-file, 7.07 MB program (`tests/unit_read_probe.rs`), and its
            // answers were re-measured on that project rather than assumed: 159 files, 737 525 tokens, zero errors,
            // 39 080 cooked declarations added, and not one raw fact lost. The three readings the old note recorded
            // as worse do not reproduce; their cause was the unclosed scope that `index_unit_rendering`'s crossing
            // fence now quarantines.
            //
            // What keeps it out of *this* function is the writer: a drain is planned inside `commit_a_wave`, so
            // anything decided here is paid under the write lock. The read itself is
            // [`Session::read_a_program_now`], and the pump takes it as three phases — `Session::gather_a_unit_read`
            // under the read lock, [`UnitReadMaterials::read_the_program`] with **every lock dropped** (4.9 s of the
            // 5.09 s needs no session at all), and `Session::commit_a_unit_reading` under the writer for the inserts.
            // A caller that is already the writer (this function's callers `advance`) takes the three in a row.

            // **What a reader is looking at**, and nothing else: the open files and the files their own text names,
            // plus whatever a request named ([`Session::want_cooked_reading`]). See
            // [`Session::want_the_closure_cooked`] for the measurement that narrowed this down from the whole
            // closure — 11.7 s of every startup for one declaration in a thousand — and
            // [`Session::want_everything_cooked`] for the caller that still wants the whole project read that way.
            for open in self.documents.paths() {
                self.want_the_closure_cooked(&open);
            }

            // **The project is NOT swept here, and three tests are why.**
            //
            // A project opened from disk with nothing open has no file queued, so every `view` in it answers `None`
            // — a real coverage hole, and the raw reading used to *hide* it by answering (wrongly). The obvious fix
            // is to queue the rest of the project once the reader's files are done, and it was tried:
            //
            // ```text
            //   a_file_nobody_looked_at_is_not_cooked_and_one_a_request_names_is   FAILED
            //   a_header_the_user_never_opened_is_cooked_when_a_request_names_it   FAILED
            //   a_unit_read_reads_the_whole_program_once                           FAILED
            // ```
            //
            // Those three assert the deliberate opposite — that a request is what causes a cook, and that the cost
            // is paid by whoever asked. Sweeping the project at every idle drain contradicts them, and they are
            // right: it would put ten thousand cooks in front of the one file the reader is looking at.
            //
            // So the hole is closed **on the asking side** instead: [`Session::view`] queues what it could not
            // answer, and the pump cooks it. A caller gets `None` once and the reading after that, which is the
            // contract the deferral was for. See `Session::is_ready_for_a_request_about` for the caller that wants
            // to know which of the two it is holding.

            // **The cooking queue is *not* drained here**, and that is the fix for the latency the product had.
            //
            // This loop used to be `for path in self.cooking.take(COOK_SLICE) { self.cook(&path); }` — the whole of a
            // cook (unit walk, lex, render, parse, insert) inside the **write** lock. Measured from the server's own
            // log on one open file: the last index slice held the writer for **749 ms**, and after it five seconds
            // passed with no slice logged at all, which is this loop. A completion arriving anywhere in that window
            // waited for all of it — the user's report was *"I type `s` and the completion takes forever"*, and the
            // numbers agree: 1623 / 1049 / 2552 / 685 ms for the first four, 5 ms once the index had settled.
            //
            // The work is not gone, it moved: [`Session::take_cooking_materials`] gathers what a cook needs (a
            // summary lookup and a unit out of the cache, cheap), [`Session::render_them`] does the lex, the render
            // and the parse **under the read lock the pump takes**, and [`Session::commit_them`] inserts under the
            // write one. `finish_a_slice` is called from inside `commit_a_wave`, so anything it drains here is by
            // construction inside the writer.
            //
            // What stays is the queueing: a file a reader has open, and the includes it names, are what has to be
            // cooked first, and that order is what `want_the_closure_cooked` writes into the queue.

            // **…and what a query asked for on its own, which is a *cook* rather than an environment.**
            //
            // `macro_work` is the asking side's queue: a request holds `&self` and cannot touch `cooking` (which
            // needs `&mut self`), so it asks through that lock — exactly as the note on the field says. The pump is
            // the `&mut self` half, and what it owes those requests is a **cooked reading**, because that is what
            // `Session::view` could not answer from: it queues here precisely when `known_rendering_of` missed.
            //
            // Measured before this: a request for a file in a project nobody had opened queued **only** here, this
            // loop handed it to `drain.environments`, and `cooking` stayed empty — so `claim_cooking` returned
            // nothing, no rendering was ever built, and every later request got `None` again. The trace that named
            // it: `cooking: stopping with read 0, cooked 0, backlog 0 | cache []`, with the file's path printed by
            // `view` as the thing it had just asked for.
            //
            // Moved rather than copied: `cooking.want` is idempotent (`Cooking::want` returns false on a repeat), and
            // draining the asking queue is what stops the same file being moved on every pass for ever.
            while let Some(path) = self
                .macro_work
                .lock()
                .ok()
                .and_then(|mut work| work.take(1).into_iter().next())
            {
                self.cooking.want(&path);
            }

            // **…and the macro environments a query asked for**, once each per pass.
            //
            // One per drain rather than four, and the difference from `COOK_SLICE` is measured rather than chosen:
            // cooking a file is a render and a parse of **that file**, while building an environment walks the
            // file's whole closure and copies every file's text — 103.7 ms against a cook's fraction of that on the
            // same headers. This is the budget that keeps a pass responsive while still getting there: the query
            // that asked read the plain view, and the next one about the same text finds the environment.
            //
            // **Named here, rendered outside the writer.** [`Session::rendering_of`] takes `&self` — the lookup, the
            // cached unit, the lex and the cook all read and change nothing — so the 65–190 ms it costs per header is
            // paid by [`Session::prepare_the_drain`], and what is left inside the writer is the `Arc` insert.
            for _ in 0..MACRO_SLICE {
                let Some(path) = self
                    .macro_work
                    .lock()
                    .ok()
                    .and_then(|mut work| work.take(1).into_iter().next())
                else {
                    break;
                };
                drain.environments.push(path);
            }
        }

        drain
    }

    /// **Read one whole program now, if one is owed and allowed** — the inline form of the pump's three phases.
    ///
    /// A program read is three phases around a lock ([`Session::gather_a_unit_read`], the read itself, and
    /// [`Session::commit_a_unit_reading`]), because the 4.9 s in the middle takes no session at all. A caller that
    /// *is* the writer — [`Session::advance`], a test, a batch — has no lock to keep free, so it takes them in a row
    /// and this is that shape. The pump takes them apart and drops the lock between.
    pub fn read_a_program_now(&mut self) -> Option<UnitReading> {
        let root = self.the_program_to_read()?;
        let materials = self.gather_a_unit_read(&root);
        let prepared = materials.and_then(UnitReadMaterials::read_the_program);
        self.commit_a_program(&root, prepared)
    }

    /// **File what a program read produced** — the third phase, and the one place `units_read` is written.
    ///
    /// `prepared` is `None` when the read declined (a root the walk cannot reach, a render that failed) and the root is
    /// **still marked read**: a root that cannot be walked would otherwise be chosen again on the next wake, and a
    /// schedule that keeps choosing the same unbuildable root is one that never goes idle.
    pub fn commit_a_program(
        &mut self,
        root: &Path,
        prepared: Option<PreparedUnitReading>,
    ) -> Option<UnitReading> {
        let Some(prepared) = prepared else {
            // **A declined read is retried — three times.** The walk can be refused for a reason that heals: a
            // dependency the index does not have *yet* (the schedule fires when the queue is empty, but a file the
            // session cannot read at all leaves a hole that stays). Marking it read on the first decline, which this
            // did, made a temporarily-incomplete closure permanent: measured on a real project, three of a project's
            // own roots were never read because one attempt declined (`tests/project_read_probe.rs`).
            //
            // The bound is what keeps a genuinely unbuildable root from being chosen on every wake: three attempts,
            // and the file keeps the reading the per-file path gave it. The number is small because each attempt is a
            // whole-program read's *gather* — a walk — and a fourth would not learn anything the third did not.
            let key = queue_key(root);
            let attempts = self.units_attempted.entry(key).or_default();
            *attempts += 1;
            if *attempts >= PROGRAM_READ_ATTEMPTS {
                self.units_read.insert(queue_key(root));
            }
            return None;
        };
        self.units_read.insert(queue_key(root));
        let committed = self.commit_a_unit_reading(prepared);
        #[cfg(feature = "perf-probe")]
        crate::perf::mark("commit");
        Some(committed)
    }

    /// **Work out the expensive half of a drain, holding nothing but `&self`.**
    ///
    /// The counterpart of [`Session::prepare_a_wave`], for the work that happens *after* a slice's inserts: the second
    /// pass ([`SummaryStore::prepare_the_re_read`]) and the macro environments a query asked for. Both are pure
    /// functions of the session as it stands — the re-read re-parses files it already has the text of, and a rendering
    /// is built from the index and the unit cache — and both were inside the write lock only because the code that
    /// *decided* to do them was. Measured from the server's own budget warning: a slice that committed **no files at
    /// all** held it for 260–572 ms, and this is the whole of what it was doing.
    ///
    /// [`Session::commit_the_drain`] applies the answer, and the pair is what lets the pump give a request the lock
    /// back between the two.
    pub fn prepare_the_drain(&self, drain: Drain) -> PreparedDrain {
        let Drain {
            files,
            units,
            environments,
        } = drain;
        let tracing = crate::probes::cooking();

        // **The two halves are timed apart**, because a start-up regression of **90 %** was found inside this
        // function on a warm cache: 1761 summaries reused from disk in 4 s of real work, and 60 s in twelve calls of
        // this. `prepare_the_re_read` and one environment look the same from outside and have nothing to do with each
        // other — one re-parses four files with their closure's macros in hand, the other renders a file's whole
        // closure — so a single number for the drain names neither.
        let started = std::time::Instant::now();
        let re_read =
            // **`true`: this caller can walk a timeline**, so the pass renders a file no slice unit covers instead of
            // keeping the raw reading of it. See `SummaryStore::prepare_the_re_read`'s `walk_its_own`.
            (!files.is_empty()).then(|| self.store.prepare_the_re_read(&files, &units, false, true));
        if tracing && !files.is_empty() {
            eprintln!(
                "drain: re-read {} file(s) in {} ms",
                files.len(),
                started.elapsed().as_millis()
            );
        }

        // **Remembered under the text it was read from**, which is what the next `view` looks up. The text is taken
        // from the VFS rather than from the rendering: the key is the *file's* content, because that is what a
        // client's buffer changes — and it is read here as well as rendered here, because both are the same read
        // lock and neither changes the session.
        let mut rendered = Vec::new();
        for path in environments {
            let step = std::time::Instant::now();
            let Some(reading) = self.rendering_of(&path) else {
                continue;
            };
            let Some(text) = self.text(&path) else {
                continue;
            };
            // **And the macro environment for the file's own view** - the other half of what a query asked for here.
            // ``Session::view_of_the_file`` reads the text the reader is typing with the macros **in force** for it,
            // and the environment it reads is the one this builds: without this call the map was never written, so
            // every such view was parsed with an *empty* environment and a macro from a header was a name it could
            // not expand. Outside the writer, like the rendering below, and for the same measured reason: building an
            // environment walks the file's whole closure and copies every file's text (103.7 ms on MSVC's headers).
            let _ = self.the_macros_of(&path, &text);
            if tracing {
                eprintln!(
                    "drain: environment for {} in {} ms",
                    path.display(),
                    step.elapsed().as_millis()
                );
            }
            rendered.push((
                (path, crate::cache::content_hash(&text)),
                std::sync::Arc::new(reading),
            ));
        }

        PreparedDrain { re_read, rendered }
    }

    /// **Apply what [`Session::prepare_the_drain`] worked out** — the index inserts, and the remembered renderings.
    pub fn commit_the_drain(&mut self, prepared: PreparedDrain) -> usize {
        for (key, rendered) in prepared.rendered {
            if let Ok(mut known) = self.renderings.lock() {
                let weight = rendered.weight();
                known.insert(key, rendered, weight);
            }
        }

        let Some(plan) = prepared.re_read else {
            return 0;
        };

        // **A refinement that could not be taken is retried, not dropped.** So while there is still
        // work in the queue the file goes back on it, and once the index is complete it is left
        // alone: a dependency nothing will ever index is a fact about the project, not about
        // timing, and the file keeps the reading the first pass gave it.
        let waiting: Vec<PathBuf> = plan.waiting_for_the_index().to_vec();
        // **Every rebuilt reading, not only the storable ones.** What this set says is "the index holds the refined
        // reading of this file now", and a reading rebuilt through a sibling file's timeline is exactly as refined for
        // this session — that it cannot be written down is the cache's question, not this one.
        let refined: Vec<PathBuf> = plan
            .rebuilt
            .iter()
            .map(|(summary, _)| summary.path.clone())
            .collect();
        let re_read = self.store.commit_the_re_read(plan);
        if let Ok(mut known) = self.refined_readings.lock() {
            known.extend(refined);
        }
        if !waiting.is_empty() && self.pending_work() > 0 {
            if crate::probes::cooking() {
                eprintln!(
                    "re-read: {} file(s) go back on the queue — their closures are still filling",
                    waiting.len()
                );
            }
            for path in waiting {
                self.queue.again(path, Priority::Rest, 0);
            }
        }

        re_read
    }

    /// **The translation units a pass over these files reads its environments out of** — one walk each, and cached.
    ///
    /// The project's own translation units are tried first, because one of them covers the most: a project file is
    /// a root whose closure is the program, while a header's own closure is "this header compiled standalone".
    /// A candidate that a unit already built reads is skipped — the question is "which timeline holds this file's
    /// environment", and the first unit that answers `environment_of` is that timeline. On the 138-file project
    /// this returns **one** unit, where the second pass used to walk a closure per file.
    fn units_for_the_pass(&self, files: &[PathBuf]) -> Vec<std::sync::Arc<crate::TranslationUnit>> {
        // **The slice, and not the project.** This used to chain `self.project` in front of `files`, on the argument
        // that a project file is a root whose closure is the whole program and therefore covers the most — and the
        // argument was right about coverage and wrong about cost, because the units built here are a **local** vector
        // while [`MAX_UNITS`] caps what survives the call. So every call walked a closure for essentially every
        // project file, and threw them away.
        //
        // Measured on a real project (198 own sources + a vendored `3rd/`, 1216 project files, no compilation
        // database): `walk` was **54.3 s of a 96.5 s index — 56%** — against `parse` 10.9 s and `sweep` 9.0 s. That
        // is 19 `advance(64)` calls × 1216 candidates ≈ 23 000 closure walks to produce the environments of 1729
        // files, and it made the whole index **serial**: wall 95.6 s against 96.5 s of CPU on sixteen cores.
        //
        // A file's own timeline is the closure of that file compiled standalone, which is the environment its own
        // reading was built with — the thing the second pass is asking for. Where a project root would cover several
        // files of one slice, the `environment_of` test below still skips them, and the walk is paid once for the
        // file that needs it rather than once for every file in the project.
        //
        // Asked by the **index's own spelling** of each path: a unit's frames are keyed by the normalized form, so
        // asking with `C:\…` about a frame spelled `c:/…` answers `None` — which would build a unit per file and
        // put the whole cost back where it was.
        let mut candidates: Vec<PathBuf> = Vec::new();
        let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        for path in files.iter() {
            let path = self
                .store
                .index()
                .summary(path)
                .map(|summary| summary.path.clone())
                .unwrap_or_else(|| path.clone());
            if seen.insert(path.clone()) {
                candidates.push(path);
            }
        }

        // **Walked in parallel, and the dedup kept afterwards.** The walks are independent read-only work on `&self`
        // — a timeline is a function of the closure's summaries and text — and the process was otherwise one core of
        // sixteen: measured on a real project, **1188 walks × ~38 ms = 44.8 s of a 67 s index with CPU/wall ≈ 1.2**
        // (`examples/project_index.rs`, and `Session::walks_by_root` for the count that said the walks are one per
        // file rather than one file walked many times).
        //
        // The sequential loop this replaces skipped a candidate that an **earlier** unit already covered, so it did
        // slightly fewer walks — 1188 of 1729 candidates, so about a third fewer. That saving is what the order
        // below preserves and what running first does not: every candidate is walked, and then the **same** dedup
        // runs in the **same** order over the results. The units handed to the second pass are therefore the ones the
        // sequential version handed it, in the same order, and only the walking moved.
        //
        // Roughly 541 extra walks of ~38 ms is ~20 s of CPU, which is ~1.3 s of wall on this machine — against 44.8 s
        // of serial walking. Any future change here should re-measure rather than reason: the count above is the
        // instrument, and `CPU/wall` in the same probe says whether the cores are actually being used.
        // **Walked one at a time, and that is a measurement rather than a preference.** This was made parallel —
        // one thread per core over contiguous chunks, with the dedup moved after the walks so that the units handed
        // to the second pass stayed identical — on the argument that 1188 walks × ~38 ms is 44.8 s of a 67 s index
        // with `CPU/wall ≈ 1.2`, so fifteen idle cores had to be worth something. It was never measured before
        // being kept, and the first measurement afterwards is a **cold index of a real project going from 67.3 s to
        // 142.7 s**. The shape of that regression is not mysterious in hindsight: `advance(64)` is called about
        // twenty-seven times, each call spawning up to sixteen threads, and each thread's `translation_unit_of`
        // contends for three mutexes (`held_units`, `walks_by_root`, `held_definitions`) — so what was added is
        // thread creation plus lock contention over work that already does its own file I/O.
        //
        // The sequential loop is also the one that walks **fewer** timelines: it skips a candidate an earlier unit
        // already covers, which is roughly 541 of 1729 on that project and is exactly what running first gives up.
        let mut units: Vec<std::sync::Arc<crate::TranslationUnit>> = Vec::new();

        // **A unit already in memory may cover this file, and then there is nothing to walk.**
        //
        // `translation_unit_of` builds a closure timeline per root (~38 ms), and `MAX_UNITS` of them are held. The
        // files a re-read slice names are usually headers of the root that was just walked — the same closure, one
        // step up the graph — so asking the held table first turns "walk this header's own timeline" into "reuse the
        // timeline that already contains it". Only units known to be **complete** are offered: a provisional one is
        // a closure still missing files, and an environment read from it is the defect the pass exists to repair.
        //
        // The key carries the environment (`SummaryKey::env_hash`), so a summary built from one root's environment is
        // filed under it: reusing a unit can produce a *different* entry, never a wrong one.
        let held: Vec<std::sync::Arc<crate::TranslationUnit>> = {
            let table = self.held_units();
            table
                .held
                .values()
                .filter(|held| !held.provisional)
                .map(|held| std::sync::Arc::clone(&held.unit))
                .collect()
        };
        let mut still_to_walk: Vec<PathBuf> = Vec::new();
        for candidate in candidates {
            if let Some(unit) = held
                .iter()
                .find(|unit| unit.environment_of(&candidate).is_some())
            {
                if !units
                    .iter()
                    .any(|known| std::sync::Arc::ptr_eq(known, unit))
                {
                    units.push(std::sync::Arc::clone(unit));
                }
                continue;
            }
            still_to_walk.push(candidate);
        }

        for (candidate, unit) in self.walk_the_timelines(still_to_walk) {
            if units
                .iter()
                .any(|unit| unit.environment_of(&candidate).is_some())
            {
                continue;
            }
            if let Some(unit) = unit {
                units.push(unit);
            }
        }

        units
    }

    /// **[`Session::translation_unit_of`] for many roots at once**, **one at a time**, preserving the order of
    /// `roots`.
    ///
    /// Kept as a named function rather than folded into the caller because that is where the parallel version lived
    /// and where the next attempt will go. **Do not parallelise it again without measuring the thing it is for**:
    /// see the comment at the call site for what the last attempt cost and why its shape regressed.
    #[allow(dead_code)]
    fn walk_the_timelines(
        &self,
        roots: Vec<PathBuf>,
    ) -> Vec<(PathBuf, Option<std::sync::Arc<crate::TranslationUnit>>)> {
        roots
            .into_iter()
            .map(|root| {
                let unit = self.translation_unit_of(&root);
                (root, unit)
            })
            .collect()
    }

    /// **Read one file, and queue what it includes.** One step of the work, as a function because there are now two
    /// callers: the pump ([`Session::advance`]) and [`Session::catch_up`], which needs the same step *now* rather
    /// than at the pump's pace.
    ///
    /// The order inside is the whole of the pump's correctness and is documented at the call site it came from: the
    /// file is loaded into the VFS before it is read (so everything the analysis reads it is also holding), and its
    /// resolved includes are cloned out before the queue is touched (`get` borrows the store, and the queue is a
    /// field of the same struct).
    ///
    /// `ready` is the file already read and parsed, when [`Session::advance`] prepared it alongside its neighbours;
    /// `None` reads it here.
    fn index_one(
        &mut self,
        path: PathBuf,
        priority: Priority,
        depth: usize,
        ready: Option<crate::index::store::Prepared>,
    ) -> Step {
        {
            let loading = std::time::Instant::now();
            self.vfs.load(&path);
            if crate::probes::units() {
                eprintln!(
                    "[step] load {} took {} us",
                    path.display(),
                    loading.elapsed().as_micros()
                );
            }
        }
        let before = self.store.stats();

        // **The environment this session already holds for this file, or one it can walk now.** A unit that is *not*
        // in memory is one whose closure this session has not read — or has read, and lost the timeline of: the units
        // table is bounded by [`MAX_UNIT_BYTES`], and after a project is indexed the file a reader is typing in
        // usually has no entry. That case used to be the ordinary one and it fell back to the file's own text; what
        // decides now is [`Session::the_timeline_to_read_through`], which walks one when the file is the reader's
        // and the program it needs is already read, and answers `None` for everything else.
        //
        // **Asked by the index's own spelling**, because a unit's frames are keyed by the spelling the walk used
        // and asking with any other one answers `None`. `Session::units_for_the_pass` records the same trap from
        // the other side — a header whose name is spelled one way by the session and another by the include graph
        // is a header that gets a unit per spelling.
        //
        // The `Arc` is held on its own because a `MacroView` borrows the unit it came from, and the unit has to
        // outlive it — and it is **cloned**, because the store call below takes `&mut self` at the next step.
        let spelled = self
            .store
            .index()
            .summary(&path)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| path.clone());
        // **And never a provisional one.** `with_the_environment` asserts that the second pass has nothing to repair,
        // and a timeline built while a file the closure needs was still queued is missing exactly what that file
        // defines — so reading a file with it and marking the result would suppress the repair for a reading that
        // needs it. A provisional timeline is a reading of most of the program, which is worth having for a *cook*
        // and is not worth asserting anything about. See [`HeldUnit`].
        let unit = self.the_timeline_to_read_through(&spelled);
        let timed = std::time::Instant::now();
        let timeline_ask = std::time::Instant::now();
        if crate::probes::units() {
            eprintln!(
                "[step] timeline question for {} took {} ms (timeline {})",
                path.display(),
                timeline_ask.elapsed().as_millis(),
                unit.is_some()
            );
        }
        let has_a_timeline = unit
            .as_ref()
            .is_some_and(|unit| unit.environment_of(&spelled).is_some());
        // Read before the wave's work is moved into the store, because the flag below is about which arm ran.
        let read_here = ready.is_none();

        let summary = match ready {
            // A wave prepared this file, and a wave has no unit of its own (`Session::prepare_a_wave` runs before
            // the pass that builds them), so what it produced is what it is and the second pass still looks at it.
            Some(ready) => self.store.commit(&path, ready),
            None => match &unit {
                // **Read once, through the timeline** — instead of once without one and once again in the second
                // pass. The summary this builds *is* the rendering, which is the reading every other feature and the
                // second pass were trying to reach; see `SummaryStore::prepare_with_the_timeline`.
                Some(unit) => {
                    let prepared = self.store.prepare_with_the_timeline(&path, unit);
                    self.store.commit(&path, prepared)
                }
                None => self.store.get(&path),
            },
        };
        if crate::probes::units() {
            eprintln!(
                "[step] {} read in {} ms after a {} ms timeline question (timeline {})",
                path.display(),
                timed.elapsed().as_millis(),
                timeline_ask.elapsed().as_millis(),
                unit.is_some()
            );
        }
        let tail = std::time::Instant::now();
        let includes: Vec<PathBuf> = summary
            .map(|summary| {
                summary
                    .includes
                    .iter()
                    .filter_map(|include| include.resolved.clone())
                    .collect()
            })
            .unwrap_or_default();
        let after = self.store.stats();
        let outcome = outcome_of(before, after);
        if crate::probes::units() {
            eprintln!(
                "[step] {} includes+stats took {} us ({} include(s))",
                path.display(),
                tail.elapsed().as_micros(),
                includes.len()
            );
        }

        // Everything this file includes joins the same half of the list the file came from, one level further out —
        // a header an open file includes is worth reading before the rest of the project, and a header the project's
        // tenth translation unit includes is not.
        let queueing = std::time::Instant::now();
        let include_count = includes.len();
        for include in includes {
            self.queue.add(include, priority, depth + 1);
        }
        if crate::probes::units() {
            eprintln!(
                "[step] {} queued {include_count} include(s) in {} us",
                path.display(),
                queueing.elapsed().as_micros()
            );
        }

        Step {
            path,
            priority,
            depth,
            outcome,
            // **Only when the summary was built here, from this environment.** A file the disk answered for was
            // built by whoever wrote that entry — with an environment or without one, the store cannot say — and a
            // file read without an environment is exactly what the second pass is for. Claiming either would be
            // claiming the second pass has nothing to repair, which is the whole of what this flag asserts.
            with_the_environment: read_here
                && has_a_timeline
                && matches!(outcome, StepOutcome::Built | StepOutcome::Unstored),
        }
    }

    /// **Read one file now, if the editor has changed it since the index read it.**
    ///
    /// # The bug this exists for
    ///
    /// An edit reaches the index in two steps, and they are not the same step: `Session::did_open` /
    /// `did_change` replace the buffer **and drop the file's summary**, and the summary is rebuilt later, when the
    /// pump reaches that file in the queue. Between the two, the analysis holds the new *text* and the old *facts*:
    /// the scope tree is rebuilt from the new text on every parse (so locals are right), while everything that reads
    /// the index is an edit behind.
    ///
    /// Completion is where that shows, and it shows as the wrong list rather than as an error: `full.` needs the
    /// **type** of `full`, the type comes from the index, and a stale index says `full` is not declared — so the
    /// member query declines and the client is handed the names in scope, which is a list of things that cannot
    /// follow a `.`. Measured on a user's file: the popup after `myName.firstName.` was `printf`, `full`, `sum`,
    /// `main` and the keywords.
    ///
    /// # Why this and not a lock
    ///
    /// Blocking the request until the **pump** has caught up would be a lock, and it would be the wrong one: the
    /// pump reads the file's whole include closure, so a request about a file whose first line is
    /// `#include <string>` would wait for thousands of headers — seconds, to answer a keystroke.
    ///
    /// # The two cases, and why both end the same way
    ///
    /// ```text
    /// the file is queued         one file whose answer changed — an edit. Read it now.
    /// the file is *not* queued   the ordinary keystroke, and the first one of a session.
    /// ```
    ///
    /// and then, **either way**, the files it includes that the index has never read: a completion that needs a type
    /// from a header cannot be answered while that header is missing, and "the index has not reached it yet" is not a
    /// state a keystroke should be able to observe. It is the pump's own step (`Session::index_one`), so the
    /// queueing and the cascade are the same code — and a header whose answer is *already* in the index is a cache
    /// hit whenever the pump reaches it, so nothing is read twice.
    ///
    /// That second half is the case a user hit: `full2.` on a fresh session answered **four items** — the names in
    /// scope — because `std::string` was not in the index yet and nothing had read the headers that declare it.
    pub fn catch_up(&mut self, path: &Path) {
        // **The file a request is about is one the second pass may refine** — see `want_refinement`.
        self.want_refinement(path);
        if self.queue.forget(path) {
            self.index_one(path.to_path_buf(), Priority::Open, 0, None);
        }

        // **A slice of its includes, not all of them, and the difference is a hover's latency.** Reading an include
        // is a parse and can be a timeline walk, and this call is made from **inside a request** (a hover, a
        // completion): on a project whose standard-library closure is a hundred headers, the first hover of every
        // file held the write lock for **210–305 ms** — and a write held that long is every request in flight
        // waiting, not this one. Measured over the wire on `EmmyLuaCodeStyle`, per-file first hover:
        //
        // ```text
        // before    40–405 ms, 135 ms average, `a request queued the modules` holding 210–305 ms
        // ```
        //
        // Eight is the same slice the pump uses (`PREPARE_INDEX_SLICE`'s spirit): what a hover needs is the
        // declaration it is pointing at, which is in the file itself or in the first header that declares it, and the
        // rest is queued for the pump — which is woken by the caller (`read_the_modules` ends with `wake`) and reads
        // them on its own slice. Nothing is lost: `needs_catching_up` stays true while any include is unread, so the
        // next request about this file reads eight more and the pump drains the rest in between.
        const INCLUDES_A_REQUEST_READS: usize = 8;
        // **Only the ones nothing has queued yet**, so the inline pass and the pump never read the same header: a
        // queued file is one the pump will reach, and reading it here as well would be the double parse this bound
        // exists to avoid.
        let mut includes: Vec<PathBuf> = self
            .unread_includes_of(path)
            .into_iter()
            .filter(|include| !self.queue.is_queued(include))
            .collect();
        if includes.len() > INCLUDES_A_REQUEST_READS {
            for include in includes.split_off(INCLUDES_A_REQUEST_READS) {
                self.queue.add(include, Priority::Open, 1);
            }
        }
        for include in includes {
            self.index_one(include, Priority::Open, 1, None);
        }
    }

    /// **The files this one includes that the index has never read**, in the order they were written.
    ///
    /// Empty for the ordinary case — a session whose pump has drained has read every include of every file it
    /// indexed — and empty as well for a file with no summary at all, which is a question the caller has just
    /// answered by reading it.
    fn unread_includes_of(&self, path: &Path) -> Vec<PathBuf> {
        let Some(summary) = self.store.index().summary(path) else {
            return Vec::new();
        };

        summary
            .includes
            .iter()
            .filter_map(|include| include.resolved.as_ref())
            .filter(|resolved| self.store.index().summary(resolved).is_none())
            .cloned()
            .collect()
    }

    /// **Is there anything for [`Session::catch_up`] to read?** — the question a caller asks *before* taking the
    /// write lock.
    ///
    /// `catch_up` is two reads and they are both conditional: the file itself when an edit has left its summary
    /// dropped and the pump has not rebuilt it, and the files it includes that the index has never read. For the
    /// ordinary keystroke — and for every request after the first on a file nobody has edited — both answers are
    /// "nothing", and the whole of what the call costs is the lock it takes to find that out.
    ///
    /// Measured over the wire on the latency fixture: with the same shortcut already in place for `prepare`, a
    /// completion's `catch_up` cost **0–1108 ms** for a file whose summary was current and whose includes were all
    /// indexed. Every millisecond of that was the queue.
    ///
    /// Exact rather than approximate: [`Work::is_queued`] is the test `catch_up` itself acts on (and `forget` is
    /// written in terms of it), and [`Session::unread_includes_of`] is the list `catch_up` itself walks. `false`
    /// means "read nothing", and reading nothing changes nothing.
    pub fn needs_catching_up(&self, path: &Path) -> bool {
        self.queue.is_queued(path) || !self.unread_includes_of(path).is_empty()
    }

    /// **Is every file this one can name from a summary in the index** — the question a *colouring* asks.
    ///
    /// [`Session::is_locally_ready`] asks about the file's **direct** includes, which is the right question for a
    /// query that resolves one name (`catch_up` reads exactly what that needs). It is the wrong question for the
    /// semantic tokens, which classify **every identifier in the file** from the index: a name declared ten headers
    /// deep (`std::endl` is in `<ostream>` through `<iostream>`) is unclassified when only the first level has been
    /// read, and the answer a client gets is a file that is mostly drawn in its own grammar colours.
    ///
    /// So the test is the **closure**: every file [`ProjectIndex::visible_files`] reaches, summarized. That walk is
    /// the one every cross-file query already pays (and it is memoized), and the summary lookups are hash reads.
    ///
    /// # Only the files that are *certainly* in the translation unit
    ///
    /// A `Conditional` edge is a file the preprocessor may never include, and requiring its summary is a wait that
    /// may have no end. Measured on neovim (`arglist.c`, a warm `.cppls` cache): `pending() == 0`,
    /// [`Session::is_locally_ready`] `true`, and the closure held exactly two unread files —
    /// `<windows kits>/um/winbase.inl` and `winuser.inl`, both reached through a **conditional** `#include` and never
    /// read by anything. So `is_closure_ready` was `false` and `semanticTokens` answered `null` **for every source
    /// file of the project**, in every session, no matter how long it had been open — a colouring that never starts.
    ///
    /// The names such a file declares are exactly the ones the classifier already reports as uncertain (the fact
    /// carries its guard), so leaving them out of the readiness question costs nothing that was ever certain.
    pub fn is_closure_ready(&self, path: &Path) -> bool {
        if !self.store.index().summary(path).is_some() {
            return false;
        }

        self.store
            .index()
            .visible_files(path)
            .iter()
            .all(|(file, visibility)| {
                *visibility != crate::IncludeVisibility::Unconditional
                    || self.store.index().summary(Path::new(file)).is_some()
            })
    }

    /// **Can a question about this file be answered from what has been read?** — the question a *local* request asks.
    ///
    /// [`Session::pending`] is the project's queue, and it is not this question. What a name means, where it is
    /// declared, what a call's signature is: each of those depends on the file and on the headers it includes, and on
    /// nothing else in the project. A project of five hundred sources whose *other* files are still queued is fully
    /// able to answer about the one the editor has open.
    ///
    /// The measured cost of the confusion: opening a real project queued 1122 files, and every
    /// local request was refused until the last of them — including a second pass that re-reads 773
    /// files at about half a second each, so the refusal lasted **minutes** after the answers were
    /// already available.
    ///
    /// The asymmetry with [`Session::pending`] is deliberate and is the whole of this method: a **project-wide**
    /// question — a reference search, a rename, a workspace symbol — genuinely needs coverage, and those handlers keep
    /// asking [`Session::pending`]. A **local** one does not.
    pub fn is_locally_ready(&self, path: &Path) -> bool {
        self.is_indexed(path) && !self.needs_catching_up(path)
    }

    /// Work until there is nothing left, and say how many files were read.
    ///
    /// Chunked rather than one `advance(usize::MAX)`: the steps of a whole project are a `Vec` nobody wants, and a
    /// caller that wants progress wants it per chunk.
    ///
    /// **Both kinds of work**, which is [`Session::pending_work`]'s count rather than [`Session::is_idle`]'s: a caller
    /// asking for "everything" wants the cooked readings too, and a test that read one and not the other would be
    /// asserting about the parts it happened to wait for.
    ///
    /// # Why the cooking backlog is drained here, and why that is not an optimisation
    ///
    /// `advance` only **marks** files for cooking — `want_the_closure_cooked` runs at the bottom of every slice
    /// whose index queue drained. The draining is [`Session::claim_cooking`] / [`Session::materials_for`] /
    /// [`Session::render_them`] / [`Session::commit_them`], which in production is the pump's loop and here has to
    /// be this one. Without it `pending_work` never falls to zero: the marking is idempotent, so the backlog sits
    /// at a fixed size for ever and this loop spins — **a hang, not a slowdown**, and the reason the analysis
    /// crate's test suite had to be killed rather than run.
    ///
    /// # Why it is a progress test rather than `pending_work() > 0`
    ///
    /// Because a file the index cannot describe can never be cooked, and `want_the_closure_cooked` marks it again
    /// on every slice. That case is real and is recorded at that function ("a queue that refills with files that
    /// have readings never empties… a hang, not a slowdown"). One round that neither read a file nor cooked one is
    /// the end of the work there is.
    /// **The cache database, one generation later** — called once by [`Session::open`] and by tests that open a
    /// store directly, so a collection can tell this run's rows from the last one's.
    pub fn begin_the_cache_generation(&mut self) {
        self.store.begin_generation();
    }

    pub fn index_everything(&mut self) -> usize {
        let mut total = 0;
        let tracing = crate::probes::cooking();

        loop {
            let round_started = std::time::Instant::now();
            let steps = self.advance(64);
            let advanced = round_started.elapsed();
            // **Every file a batch read names is a file to refine.** This is the review's lever: a request asks
            // (`catch_up`), and a batch caller that just read the whole project asks for the whole project. It was held
            // back while the refined reading was weaker than the raw one; the refinement now files its rendering as the
            // file's **cooked** facts (L2) and leaves the summary alone (L1), which is what the design's §2.1 says.
            //
            // # P2 wants this loop gone, and the three tests that stand in its way are named here
            //
            // Making a batch stop asking is a change of *contract* — "index everything" would mean the index and not a
            // compiler's reading of every file — and it was implemented and reverted in one round. What it costs:
            // **659 ms of a 1.54 s `import std;`**, four slices of 13–16 files rendered through a shared timeline, three
            // of which report `0 bodied name(s)` and render anyway. What blocked it, exactly:
            //
            // * `a_refinement_is_served_to_the_next_session` — its second session asserted `re_read_backlog() == 0`
            //   after `index_everything`. Under the new contract the backlog is legitimately 2, and the subject it is
            //   named for (the refinement is *served*) needs an explicit `want_refinement` in both sessions. Two lines,
            //   and the updated form passes;
            // * `a_keystroke_in_a_body_reads_the_file_through_its_timeline` — its setup asserted that the first reading
            //   of a file is the timeline's, which the eager ask is what produced. One `want_refinement` before
            //   `index_everything` keeps the subject;
            // * `a_file_read_back_through_its_own_unit_is_not_read_again_by_the_second_pass` — **not** fixed by an ask.
            //   After an edit it asserted the second pass has nothing to repair, and with the eager loop gone the edited
            //   file is queued again even though it was read with its environment. Either the edit path stops queueing
            //   environment-read files, or this assertion is about a coupling that has to be understood first.
            //
            // So the next attempt starts at that third test, not at this loop.
            //
            // # What that third test turned out to be
            //
            // Not a coupling. `advance` removes nothing from `parsed_since_the_last_pass`, so a file read **as text**
            // cold stays queued after a later edit reads it **through its timeline** — and the filter below, which keeps
            // an environment-read file off the queue, only applies to the slice in hand. Traced: the same `/p/api.h`
            // queued twice, `outcome Built environment false` and then `outcome Unstored environment true`. Under the
            // eager contract this is invisible, because the pass drains the queue anyway. Under the demand-driven one
            // the stale entry sits there for ever, and the test's "has the pass anything to repair" answered yes about
            // a file whose last read had nothing left to repair.
            //
            // The repair is one line — remove from the queue every file this slice read with its environment — and it
            // was tried. It makes that test pass and **breaks two others**, for the reason the eager contract exists:
            // `a_refinement_is_served_to_the_next_session` and
            // `a_keystroke_in_a_body_reads_the_file_through_its_timeline` both get their refinement *because* the file
            // is in the queue when the pass drains it; take it out and there is no first reading to cache or to assert.
            // So the repair belongs to the same change as the loop's removal, and neither lands alone.
            //
            // # Attempt two, and where it stopped
            //
            // Both were landed together and the five tests that depend on the eager ask were updated to ask
            // (`want_refinement`) for what they are about: `a_refinement_is_served_to_the_next_session`,
            // `a_keystroke_in_a_body_reads_the_file_through_its_timeline`, `a_file_read_back_through_its_own_unit…`,
            // `a_refined_reading_is_asked_for_again_after_an_edit` and both `stored_refinement` fixtures. All six passed.
            // The seventh did not: **`a_project_with_no_build_system_is_read_with_the_compilers_environment`**
            // (`tests/toolchain.rs`) reads a file the session discovers rather than one the test names, and an ask for
            // it — issued before `index_everything` — produced no cooked reading at all. That is a question about *when*
            // an ask can be served (the take happens only in an idle round, and a file discovered mid-drain may be
            // taken before its summary exists), not about this loop.
            //
            // So the next attempt starts there: an ask has to survive a file it cannot be served for yet.
            //
            // # Attempt three: the ask survives, and the fixtures still disagree
            //
            // `take_the_files_wanted_for_refinement` was fixed to leave an ask standing until its file has a summary.
            // With that, removing this loop and repairing the queue in `finish_a_slice` gets **eight of the nine**
            // dependants green: both session tests, the keystroke test, the unit-read-back test, `refinement.rs`'s
            // re-ask test, `toolchain.rs`'s discovered-file test (which is what the fix was for) and the two ordinary
            // suites. `tests/stored_refinement.rs` was the ninth, and it was **not** a fixture problem: its ask had not
            // been applied (a shell edit that did not match), and with it `is_refined(widget)` is true on the first
            // round after `index_everything` — measured: `refined true wanted 0 backlog 0 pending 0`. So every
            // dependant of the eager ask is a dependant of *being asked*, and the loop goes.
            let _ = &steps;
            let refined = round_started.elapsed() - advanced;
            let read = steps.len();
            // **Not asked for here, and the reason is the next defect rather than this call.** Every file this pass
            // reads would be a file to refine, which is what "index everything" sounds like it means — and it makes four
            // session tests pass. It also makes the pass **refine a fixture's own file**, and three `checks` tests then
            // see a different reading of it (`c_files_are_not_told_to_write_cpp_style`: `UseNullptr` stops firing, because
            // the re-read of a one-file fixture produces a reading the checks decline). That is a defect in the
            // **refined reading** — the same family as the C++ corpus at 41/52 — so the ask waits for it. A *request*
            // still asks (`catch_up`), which is the contract; a batch "index everything" does not, and the tests that
            // need it are listed in `docs/indexing-review.md` §4.
            total += read;

            // **Every stage of the cook is counted, because a file can be claimed and then lost.**
            //
            // `claim_cooking` pops, and the three stages after it each drop a file by answering `None` —
            // `materials_for` when a file's unit is provisional or its summary is not in the index yet, `render_them`
            // when the render itself declines, and `commit_them` when there is nothing to insert. A pop followed by a
            // drop is a file that is **no longer queued and never cooked**, and the loop below reads that state as
            // "nothing left to do": `read == 0 && cooked == 0` ends the work with a cooking backlog still standing.
            // Measured on the completion fixture: `pending 0, cooking 22` on every round, for six rounds.
            let cook_started = std::time::Instant::now();
            let cooked = {
                let claimed = self.claim_cooking(COOK_SLICE);
                if tracing && !claimed.is_empty() {
                    eprintln!("cooking: claimed {} file(s) {:?}", claimed.len(), claimed);
                }
                let materials = self.materials_for(claimed);
                let wanted = materials.len();
                let rendered = self.render_them(materials);
                let built = rendered.len();
                let committed = self.commit_them(rendered).len();
                if tracing && (built != wanted || committed != built) {
                    eprintln!(
                        "cooking: rendered {built} of {wanted} claimed, committed {committed} \
                         (cooking backlog {})",
                        self.pending_cooking()
                    );
                }
                committed
            };
            let cook = cook_started.elapsed();
            if crate::probes::units() {
                // **Three timers, each taken where it happened** rather than one derived by subtraction: the round
                // that appeared to take 56 s had an `advance` of 7 ms and a cook of 1 µs, so the 56 s was in neither
                // and the derived figure could not say where. The three now print their own walls.
                eprintln!(
                    "DRAIN round: advance {advanced:?} ({read} file(s)), refine {refined:?}, cook {cook:?} ({cooked} committed), round total {:?}",
                    round_started.elapsed(),
                );
            }

            // **The second pass is work, and this loop used to return while it was owed.**
            // `advance` drains up to [`RE_READ_SLICE`] of the backlog per call, and the backlog is
            // filled by a cold start; the loop below ended at `read 0, cooked 0`, which is exactly
            // the state a session is in while it still owes re-reads. Measured on
            // `EmmyLuaCodeStyle` (cold summary cache): `index_everything` returned with **1 205
            // files** owed, 302 slices and **27.3 s** of work left — and every one of those slices
            // reported `nothing bodied`, so the value it bought was not visible in any answer. The
            // cost *was* visible: the next keystroke's pump slice took four of those files and four
            // unit walks, and typing was **65 ms instead of 10** for about three hundred
            // keystrokes. A caller that says "index everything" is owed this too. **The second pass
            // is owed, and this loop does not pay it.** The pump already drains it, four files a
            // slice, and `pending_work` counts it, so the work still happens; what the loop bought
            // was paying for it *before* the session answered anything.
            if read == 0 && cooked == 0 {
                if tracing {
                    // **The cache as the pump leaves it.** If this is non-zero and a request still misses, the key
                    // the writer used and the key the reader computes are two different keys — which is the one
                    // failure a count cannot show and the two hashes beside it can.
                    let cached: Vec<String> = self
                        .renderings
                        .lock()
                        .map(|known| {
                            known
                                .keys()
                                .map(|(path, hash)| format!("{}#{hash}", path.display()))
                                .collect()
                        })
                        .unwrap_or_default();
                    eprintln!(
                        "cooking: stopping with read 0, cooked 0, backlog {} | cache {cached:?}",
                        self.pending_cooking()
                    );
                }
                // **The idle step, and this is the idle.** A session that has just finished indexing is the one
                // moment when collecting the cache is free of a reader: rows nothing references (a file that changed
                // while the last run was writing it, a deleted file, a summary from a previous generation) are
                // dropped, and the file is compacted — measured, a collection left a database *larger* than it
                // started (16.6 MB → 24.6 MB) until compaction returned the pages.
                //
                // Once per call rather than once per loop: `index_everything` is the "index it all" entry point, and
                // a caller that calls it twice is asking twice.
                if let Some(collected) = self.collect_the_cache() {
                    log::debug!(
                        "cache: collected {} summary(ies), {} rendering(s), {} projection(s)",
                        collected.summaries,
                        collected.renderings,
                        collected.projections
                    );
                }
                // **And the memory budget, at the same idle moment.** A project whose summaries exceed
                // [`SUMMARY_BUDGET_BYTES`] forgets the largest of the files nobody is looking at; the ones it forgets
                // are re-read on demand, which is the path a cache miss already takes.
                let forgotten = self.enforce_the_summary_budget(SUMMARY_BUDGET_BYTES);
                if forgotten > 0 {
                    log::debug!("index: forgot {forgotten} summary(ies) over the byte budget");
                }
                return total;
            }
        }
    }

    /// How many files are queued and not yet indexed.
    ///
    /// Counted by **file**, not by queue entry: a path that was discovered once and opened later sits in two places
    /// and is one file to read.
    ///
    /// **The index queue only.** The cooking backlog is [`Session::pending_cooking`], and a caller that wants to
    /// know whether the session has any work at all — a pump deciding whether to keep going — wants their sum,
    /// [`Session::pending_work`].
    pub fn pending(&self) -> usize {
        self.queue.pending()
    }

    /// **Everything the session has left to do**: the index queue, the cooking backlog, and the second pass.
    ///
    /// What a pump loops on. A caller looping on [`Session::pending`] alone would stop while the files it has open
    /// still have no cooked reading — which is the state the closure marking exists to leave, not the state a
    /// finished session is in.
    ///
    /// **The second pass is in here, and it was not.** A cold start parses a file before its includes are in the
    /// index, so its reading has to be built again once the closure is in hand — the backlog
    /// [`Session::re_read_backlog`] counts. That backlog used to be invisible to this count, so a pump stopped with
    /// **1 205 files and 27 s still owed** and the next keystroke's slice paid for four of them: typing measured
    /// **65 ms a keystroke instead of 10** until the debt was gone. A session that owes a re-read is not idle.
    ///
    /// The visibility gate that reads this — `checks_may_be_trusted` in the server, which withholds check findings
    /// until the index has settled — therefore withholds them until the second pass has run. That is the same
    /// argument that put the cooking backlog here: a summary the pass is about to revise is not a summary a check may
    /// call a fact.
    pub fn pending_work(&self) -> usize {
        self.queue.pending() + self.cooking.len() + self.refinements_wanted()
    }

    /// Is everything the session knows about already **read**?
    ///
    /// **The indexing queue's question, not [`Session::pending`]'s**: the two differ by the cooking backlog, and the
    /// difference is load-bearing. A file's cooked reading is built after its includes have been read (their macros
    /// are its environment), and one more thing happens at that same moment — the second parse of a file whose
    /// meaning depended on a header that arrived later. Cooking before that pass would build a reading of an
    /// environment the summaries are about to revise.
    /// **What the queue physically holds against what it says is pending** — a diagnostic, and the measurement behind
    /// §6 of `docs/buglist.md`.
    ///
    /// `pending()` is the number of paths whose standing is `Queued`; the deques also hold entries that were re-armed
    /// into the other half, upgraded, forgotten or worked — `pop` walks them and skips them, and the two numbers
    /// drifting apart is what makes one `advance` step take seconds while `index_one` itself takes milliseconds.
    ///
    /// `(pending, open deque, rest deque)`.
    pub fn queue_sizes(&self) -> (usize, usize, usize) {
        (
            self.queue.pending(),
            self.queue.open.len(),
            self.queue.rest.len(),
        )
    }

    pub fn is_idle(&self) -> bool {
        self.queue.pending() == 0
    }

    /// How many files are waiting to be **cooked** — the half of [`Session::pending`] that is not indexing.
    ///
    /// A caller reporting progress wants the two apart ("63 files to read" and "9 of them read as a compiler would"
    /// are different sentences), and a caller deciding whether it may stop pumping wants the sum.
    pub fn pending_cooking(&self) -> usize {
        self.cooking.len()
    }

    /// **How many files the second pass has still to re-read** — the backlog `advance` drains four at a time.
    ///
    /// A third kind of pending work, and the one no count used to name: [`Session::pending`] is the
    /// index queue and [`Session::pending_cooking`] the cook queue, while the
    /// `parsed_since_the_last_pass` backlog is drained only by a slice's tail and only while the
    /// index queue is empty. Measured on `EmmyLuaCodeStyle` right after `index_everything`
    /// returned, on a cold summary cache: **1 205 files**, which took **302 slices and 27.3 s** to
    /// clear — and every one of those slices reported `nothing bodied`, so the whole tail rebuilt
    /// no summary at all. Until it was cleared, the next keystroke's slice paid for four of those
    /// files and four unit walks: measured **65 ms a keystroke** instead of 10, for about three
    /// hundred keystrokes.
    pub fn re_read_backlog(&self) -> usize {
        self.parsed_since_the_last_pass.len()
    }

    /// **How many files are waiting because somebody asked for them** — the only re-reads a start does not defer.
    pub fn refinements_wanted(&self) -> usize {
        self.refined_on_request
            .lock()
            .map(|wanted| wanted.len())
            .unwrap_or(0)
    }

    /// **Ask for a file's reading to be refined** — what an open or a request does.
    ///
    /// It used to answer only for a file still in `parsed_since_the_last_pass`, on the argument that a file outside it
    /// "has either been refined already or never read". **Both halves of that were wrong**, and the cost was the whole
    /// capability: the pump drains that backlog before a request ever arrives, so every ask was refused, `advance`'s pass
    /// ran on an empty list, and no summary was ever refined. Measured on the C++ corpus: **51/52 type answers before
    /// phase 0, 41/52 after**, with the failing test naming the mechanism
    /// (`a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session`, `Widget` → `NotDeclaredHere`).
    ///
    /// A file that `is_refined` is the one real no-op: asking again would re-read a summary that already has its
    /// environment. Everything else is recorded, and the pump refines it once — `take_the_files_wanted_for_refinement`
    /// removes the ask, so an ask is served once.
    pub fn want_refinement(&self, path: &Path) {
        if self.is_refined(path) {
            return;
        }
        if let Ok(mut wanted) = self.refined_on_request.lock() {
            wanted.insert(path.to_path_buf());
        }
    }

    /// **Take the files that were asked for**, at most `at_most`, and drop them from the backlog.
    ///
    /// The asks are the queue, not the backlog: a file a request named is refined wherever it is, which is what makes
    /// "answer from L1, ask for L2" reachable after a start has drained. A file that is *also* still in
    /// `parsed_since_the_last_pass` is taken out of it, so one file is re-read once.
    ///
    /// # An ask the pass cannot serve is left standing
    ///
    /// The take happens in an **idle** round — `finish_a_slice` calls this when the index queue is empty — but a file
    /// can be asked about before its summary exists: a request for a file the drain has not reached, or one the session
    /// only discovers while indexing. Handing that to the pass consumes the ask and buys nothing, because a re-read
    /// needs a summary to re-read — and then nothing ever asks again.
    ///
    /// That is not hypothetical: it is what stopped the demand-driven contract
    /// (`docs`: the round that removed `index_everything`'s eager ask). `tests/toolchain.rs`'s
    /// `a_project_with_no_build_system_is_read_with_the_compilers_environment` reads a file the session *discovers*,
    /// asked for it before `index_everything`, and got no cooked reading at all — the ask had been consumed by a round
    /// that could not serve it. Left standing, it is served by the round that can.
    fn take_the_files_wanted_for_refinement(&mut self, at_most: usize) -> Vec<PathBuf> {
        let mut taken = Vec::new();
        let Ok(mut wanted) = self.refined_on_request.lock() else {
            return taken;
        };
        // Asks that cannot be served **yet**: out of the set so the loop advances past them, and back in before this
        // returns. Without the round trip one unready ask would block every ready one behind it; without the put-back
        // it would be dropped, which is the defect above.
        let mut not_yet: Vec<PathBuf> = Vec::new();
        while taken.len() < at_most {
            let Some(path) = wanted.iter().next().cloned() else {
                break;
            };
            wanted.remove(&path);
            if self.store.index().summary(&path).is_none() {
                not_yet.push(path);
                continue;
            }
            if let Some(at) = self
                .parsed_since_the_last_pass
                .iter()
                .position(|queued| queued == &path)
            {
                self.parsed_since_the_last_pass.swap_remove(at);
            }
            taken.push(path);
        }
        for path in not_yet {
            wanted.insert(path);
        }
        taken
    }

    // **Drop everything the cook queue is waiting for, and say how much it was.**
    //

    /// **Is this file`s summary about to be revised?** — the second pass has it queued for a re-read.
    ///
    /// A summary parsed before its include closure was indexed has no macro environment, so the facts in it are the
    /// shape-only reading: `_STD` is a name rather than `::std::`, and a class declared under a macro is at the wrong
    /// scope. The second pass repairs that, and until it runs, a **check** reading this summary is reading an answer
    /// that is about to change.
    pub fn is_owed_a_re_read(&self, path: &Path) -> bool {
        let key = crate::file::paths::normalize_path(path, cfg!(windows));
        self.parsed_since_the_last_pass
            .iter()
            .any(|queued| crate::file::paths::normalize_path(queued, cfg!(windows)) == key)
    }

    /// **Does anything this file can see wait for the second pass?** — the closure question behind
    /// [`Session::is_owed_a_re_read`].
    ///
    /// A check about this file resolves names through the headers it includes, so a provisional summary among those
    /// is as misleading as a provisional summary of the file itself. The walk is [`ProjectIndex::visible_files`],
    /// which is the memoized one every cross-file query already pays.
    pub fn its_closure_is_owed(&self, path: &Path) -> bool {
        if self.parsed_since_the_last_pass.is_empty() {
            return false;
        }
        if self.is_owed_a_re_read(path) {
            return true;
        }
        self.store
            .index()
            .visible_files(path)
            .iter()
            .any(|(file, _)| self.is_owed_a_re_read(Path::new(file)))
    }
    /// For the pump`s breaker: a file the cook claims and cannot build leaves the queue`s count where it was, and
    /// a loop that keeps taking it never goes idle (measured once as `2 to cook, 0 left` once a second for as long
    /// as a window stayed open). Dropping is safe rather than lossy: every query that needs one of these files asks
    /// for it again (`want_cooked_reading`, [`Session::want_the_closure_cooked`]), and the next drain takes it with
    /// whatever has changed since.
    ///
    /// Nothing is forgotten about the *files* — only the want. A dropped file that could be built will be built the
    /// next time something names it, and one that could not will not spin the loop meanwhile.
    pub fn abandon_the_cooking_queue(&mut self) -> usize {
        self.cooking.abandon()
    }

    /// **Write the sharing cache into the project's database**, so that the next run starts warm — §8 step 4.
    ///
    /// It is a table rather than a `<cache>/sharing.bin` beside the database, and that is a deliberate move: the file
    /// was written whole and decoded whole, so a start paid for every entry before it could answer anything, and an
    /// interrupted write lost the whole cache. The rows carry their own identity (the file's fingerprint and the
    /// projection of its conditions), so a stale row cannot answer a different question — it simply fails to hit.
    ///
    /// `Err` only when there is no database to write to: a session with the cache turned off is not a failure, but a
    /// caller that asked to save something is owed the difference. `0` entries is the honest answer for a run that
    /// walked nothing.
    pub fn save_the_sharing(&self) -> std::io::Result<usize> {
        if self.store.database().is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "this project has no summary database to write the sharing cache into",
            ));
        }
        Ok(self.store.save_the_contributions())
    }

    /// **Read the sharing cache back** from the project's database.
    ///
    /// A missing table, an unreadable row or a byte this version cannot decode is a cold start, not an error: the
    /// cache is an accelerator and the walk is always correct without it. Say how many entries were restored, because
    /// "it loaded nothing" and "it did not run" are different facts and only one of them is interesting.
    pub fn load_the_sharing(&self) -> usize {
        self.store.load_the_contributions()
    }

    /// Is this file's summary in the index — the question [`Session::pending`] answers for the whole project.
    pub fn is_indexed(&self, path: impl AsRef<Path>) -> bool {
        self.store.index().summary(path.as_ref()).is_some()
    }

    // ---------------------------------------------------------------------------------------------
    // The questions
    // ---------------------------------------------------------------------------------------------

    /// **The file's own tokens — the reading that means nothing and is for positions.**
    ///
    /// For a caller whose question is about the **text**: a fold, a selection range, a bracket, a highlight, the span
    /// of a diagnostic, the bytes a rename would edit. None of those needs a declaration to mean anything, and that
    /// is exactly why this is a different function from [`Session::view`] rather than a fallback inside it.
    ///
    /// Returns the file's text, its line index and its **tokens** — and deliberately **no tree and no scopes**: a
    /// caller that had those would eventually ask them what a name means, and the answer would be about unexpanded
    /// text. A caller that needs meaning calls [`Session::view`] and **defers** when it answers `None`.
    ///
    /// `None` only when the file is neither open nor readable.
    pub fn tokens_of(&self, path: impl AsRef<Path>) -> Option<TokensOf> {
        let file = self.vfs.held(path)?;
        Some(TokensOf {
            path: file.path.clone(),
            source: file.text.clone(),
            line_index: file.line_index.clone(),
            open: file.open,
            tokens: cpp_parser::lex(&file.text, &cpp_parser::LexerConfig::default()).0,
        })
    }

    /// The file as the cursor is in it: the buffer when it is open, the disk otherwise.
    ///
    /// `None` when there is neither — a path that is not open and cannot be read. Everything below takes a view,
    /// so one parse per request serves every question about that request's file.
    ///
    /// The view is **of the session's VFS**: the text and its line index come from the file the VFS is holding, and
    /// the view shares both rather than copying either. What is done here is the parse and the scopes, which are
    /// the two things a position needs and a summary cannot hold.
    /// # Why a file with no rendering answers `None`, which is the whole point
    ///
    /// It used to answer from the file's own tokens — a parse of unexpanded text. That is not a lesser answer, it is
    /// an **untrue** one: C++ source with its macros unexpanded is not a program, and a tree built from it is shaped
    /// like a syntax tree and meaningless as one (measured on MSVC's headers: `<xutility>` 84 error nodes raw
    /// against 0 cooked, `<xstring>` 17 against 0 — see `examples/cooked_parse.rs`).
    ///
    /// So the answer is deferred instead. The work is queued exactly as before, and the caller is told **nothing**
    /// rather than told something false — which is this crate's own rule for an answer it does not have
    /// (`Known::Unknown` over a guess), applied to the reading rather than to a name. A caller that needs a
    /// *position*, a token or a fold does not want this function at all: see [`Session::tokens_of`].
    ///
    /// **The cost of this is real and has to be watched**: a file with no rendering is now answered on the request
    /// *after* the one that asked for it, where before it was answered immediately and wrongly. `pending_cooking`
    /// and the diagnostics channel's `isIncomplete` are how a caller states that, and the alternative is not
    /// "an answer sooner" — it is "a wrong answer sooner".
    /// # Why this takes only `&self`, and what it costs a caller
    ///
    /// A query must not pay a unit walk, a preprocess and a render, so this **asks** rather than builds: it queues
    /// into `macro_work` — the asking side's queue, which the pump drains into a cook — and answers `None`. That is
    /// the whole arrangement, and it is why the ask is a lock rather than a `&mut self`.
    ///
    /// The cost is one case it cannot serve: a file the index has **never described**. Queueing one for a cook is not
    /// enough, because a cook is built out of a summary ([`Session::cook`] answers `None` without one), so such a
    /// file must also be queued for **reading** — which is [`Session::want_cooked_reading`], and that needs
    /// `&mut self`. A caller holding one calls it first and then asks:
    ///
    /// ```text
    ///   session.want_cooked_reading(&path);   // read it *and* cook it
    ///   session.index_everything();           // the pump does both
    ///   let view = session.view(&path);       // the reading
    /// ```
    ///
    /// Measured without that first line, on a fixture that loads a file and pumps: `indexed false, pending 0,
    /// backlog 0` — the ask had nothing to name and every later `view` answered `None` for ever. Making `view`
    /// itself `&mut self` was tried and does not work: `vfs.held(path)` borrows the session immutably for the whole
    /// body, so the mutation cannot sit beside the lookup it depends on.
    pub fn view(&self, path: impl AsRef<Path>) -> Option<FileView> {
        // **The index's own spelling of the path, not the caller's.** A client sends `C:\…`, the resolver wrote
        // `c:/…`, and the rendering cache is keyed by *the index's* spelling — so a lookup by the caller's finds
        // nothing, and the miss is indistinguishable from "this file was never cooked". The same rule
        // [`Session::diagnostics`] and [`Session::cook`] already apply, for the same reason; it was missing here,
        // which is why a view could answer `None` for a file whose rendering was in the cache.
        let requested = path.as_ref();
        let key = self
            .store
            .index()
            .summary(requested)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| requested.to_path_buf());
        // **A miss is otherwise indistinguishable from "never cooked"**, which is what made a file that *had* been
        // cooked answer `None` for every query about it. See `CPPLS_TRACE_VIEW`.
        if crate::probes::view() {
            eprintln!(
                "view: asked {:?} key {:?} summary {} held {}",
                requested,
                key,
                self.store.index().summary(requested).is_some(),
                self.vfs.held(&key).is_some()
            );
        }
        let file = self.vfs.held(&key)?;
        if crate::probes::view() {
            eprintln!(
                "view: file.path {:?} hash {:?}",
                file.path,
                crate::cache::content_hash(&file.text)
            );
        }

        // **The rendering, or a request for it — never the file's own tokens.**
        //
        // A rendering is what a compiler's parser is handed: the file with its macros replaced, so `_STD` is
        // `::std::` and `_STD_BEGIN` is `namespace std {`, and nothing in the text is an invocation any more. That
        // is the reading this view is **of**.
        //
        // Nothing here builds it. A rendering is a unit walk, a preprocess and a render — measured at 65–190 ms for
        // a standard-library header on an indexed project, and far more on a cold one — and a query must not pay
        // that. So a file with no rendering yet is put in the work loop's hands and this call answers `None`; the
        // next query about it finds the rendering built. The same "not now, next time" the diagnostics channel
        // states through `isIncomplete`.
        match self.known_rendering_of(&file.path, &file.text) {
            // **Parsed as the language the file is**, not as the parser's default: see
            // [`FileView::parse_rendering_in`] — `restrict`, C23's literals and the GNU extensions all depend on it.
            //
            // **And only once per (rendering, configuration).** The parse is the expensive half of every request that
            // asks a question about a position — measured at the parser's 6 MB/s, so 120 ms for a 0.74 MB unit — and
            // it was being paid on every call. The key is [`ViewKey`]: the rendered text's hash, the span count, the
            // file's identity, the language, `cuda` and whether the buffer is open. A miss parses; a hit is a few
            // reference-count bumps.
            Some(rendered) => {
                let config = self.config_for_file(&file.path);
                let key = ViewKey {
                    path: file.path.clone(),
                    rendering: Some(RenderingId(rendered.clone())),
                    // The rendering is the text: no second hash of the same bytes, and no `spans` — an object's
                    // identity covers both.
                    text: 0,
                    id: file.id,
                    cuda: config.cuda,
                    language: Some(config.language),
                    open: file.open,
                    // A rendering has no invocations left in it, so its parse takes no macro environment.
                    environment: None,
                };
                if let Some(held) = self.a_view_already_parsed(&key) {
                    if crate::probes::view() {
                        eprintln!("view: {} came from the cache", file.path.display());
                    }
                    return Some(held);
                }
                let view = FileView::parse_rendering_in(file, &rendered, &config.config);
                self.remember_a_view(key, &view);
                Some(view)
            }
            None => {
                if crate::probes::cooking() {
                    // **The two halves of the lookup, printed.** The key is `(path, content_hash(text))`, and a
                    // write and a read that disagree about either half are a miss that looks exactly like "this file
                    // was never cooked". The count says whether the cache has anything at all.
                    eprintln!(
                        "view: no rendering for {} | hash {} | {} entr(ies) cached | open {}",
                        file.path.display(),
                        crate::cache::content_hash(&file.text),
                        self.renderings.lock().map(|known| known.len()).unwrap_or(0),
                        file.open
                    );
                }
                if let Ok(mut work) = self.macro_work.lock() {
                    work.want(&file.path);
                }
                None
            }
        }
    }

    /// **The view of what the file itself writes** — its own tokens, macros and all.
    ///
    /// The counterpart of [`Session::view`], and both are needed because they answer different questions:
    ///
    /// ```text
    /// view                the reading a compiler's parser is handed — offsets in the rendering
    /// view_of_the_file    the text the reader is editing — offsets in the file
    /// ```
    ///
    /// A question about **a position** belongs to the first, because that is where declarations, types and members
    /// are resolved. A question about **a macro** belongs to the second, and it is not a fallback: a rendering has
    /// the macro already replaced, so `#define API …`, every `API` written below it, and the node a rename would
    /// edit are all *gone* from it. Renaming, colouring or hovering a macro is a question about the spelling in the
    /// buffer, and the buffer is this view.
    ///
    /// The two agree on the file's lines wherever no macro expanded, which is most of most files — and where they
    /// disagree, that is exactly the region a macro wrote, so a caller that needs the file's own positions wants
    /// this one regardless.
    /// # Why this answers `None`, and what replaced it
    ///
    /// It used to build a `FileView` by parsing the file's own bytes, and that is the defect this whole layer has
    /// been paying for: **C++ source with its macros unexpanded is not a program.** A rendering has the macro
    /// already replaced, so `#define API …` and every `API` written below it are gone from it — which is why a
    /// question about **a macro** is a question about the buffer. But a question about a macro is a question about a
    /// **spelling**, and a spelling needs no tree: [`Session::tokens_of`] answers it with the lexer's own tokens, and
    /// a caller that needs a *position* or a *fold* wants that too.
    ///
    /// What is left is the honest answer, and it is the same one [`Session::view`] gives: a caller that needs
    /// **meaning** waits for the rendering. `None` therefore means "not now" for this file, and a caller must not
    /// read it as "no such file" — [`Session::is_indexed`] and [`Session::pending_cooking`] are how those are told
    /// apart.
    pub fn view_of_the_file(&self, path: impl AsRef<Path>) -> Option<FileView> {
        // **Held the way the VFS holds it, and asked for it the same way.** The VFS normalizes a path on the way in
        // and on the way out, so a lookup by a caller's spelling finds the file; the *queue* is keyed the same, so
        // the request below names the file the session will actually cook.
        let requested = path.as_ref();
        let file = self.vfs.held(requested)?;
        // Queue it **only if there is nothing to read yet** — the rule `view` states, and the reason this one used to
        // make every request work: it asked unconditionally, so the file was cooked again on every query even though
        // its rendering was in the cache. Measured on a real project with the loop probe
        // (`crates/cpp_ls/tests/loop_probe.rs`): ten diagnostics pulls caused **five** cooks, the same two headers
        // each time (`types.h`, `stl_vector.h`), each a render and a parse — and raising the cache from 8 to 64
        // entries changed the count **not at all**, which is what named the cause: the ask, not the eviction.
        if self.known_rendering_of(&file.path, &file.text).is_none()
            && let Ok(mut work) = self.macro_work.lock()
        {
            work.want(&file.path);
        }

        // **And the plain view, which this used to leave out - the whole of the answer.**
        //
        // The arrangement `macro_environments` describes is *"a query cannot pay the 103.7 ms walk, so it asks for the
        // environment and reads the **plain view**, and the work loop builds what is missing"* - and this function is
        // the "reads the plain view" half, which was never written. It queued the work and returned `None`, so every
        // caller that wanted the text the reader is typing got nothing. Measured, on the handshake suite: the
        // completion handler's one call answered `None` on **every** attempt, the query logged `(answer missing)`, and
        // the server wrote `{"result":null}` to a client that re-asked for thirty seconds.
        //
        // The environment is the one **already read** for this content - `known_macros_of` is a hash and a lookup -
        // and an empty one otherwise, which is the honest answer for a file whose include closure has not been walked.
        let known = self.known_macros_of(&file.path, &file.text);
        // **The same cache as `view`**, because the question is the same one and the cost is the same shape: a parse
        // of this file's own text with an environment that has not moved. The identity is the environment *object* —
        // held in the key, so its address cannot be reused under it — and `None` for the empty one, which is one value
        // however often it is made.
        let key = ViewKey {
            path: file.path.clone(),
            // A buffer view parses the file's own text, which has no rendering behind it.
            rendering: None,
            text: crate::cache::content_hash(&file.text),
            id: file.id,
            cuda: self.config.cuda(),
            // A buffer view's parser is built from `ParserConfig::default()` and the environment alone — see
            // `FileView::parse_with_cuda` — so there is no language level in its identity.
            language: None,
            open: file.open,
            environment: known.clone().map(EnvironmentId),
        };
        if let Some(held) = self.a_view_already_parsed(&key) {
            return Some(held);
        }
        let macros = known.unwrap_or_default();
        let view = FileView::parse_with_cuda(file, macros.as_ref(), self.config.cuda());
        self.remember_a_view(key, &view);
        Some(view)
    }

    /// **The view of a file that a compiler's parser would see**, built if it has to be.
    ///
    /// [`Session::view`] gets the same reading **one query later** without ever blocking on it. This is for a caller
    /// that would rather wait: a batch, a test, or the first query after a file is opened.
    ///
    /// # What the reading is, and why it is the one to want
    ///
    /// A file's own tokens carry macros, and a macro is a spelling rather than a grammar: `_STD widget` is a name
    /// and a name to a reader of one file, and one qualified name to a compiler. The grammar grew a family of rules
    /// for that guess — `MacroCall`, `written_like_a_macro`, `is_a_macro`, a body-shape reader — and every one of
    /// them exists because the parser was being handed text the preprocessor had not finished with. A **rendering**
    /// has none of it left:
    ///
    /// ```text
    /// the file writes     _STD_BEGIN struct widget { … }; _STD_END
    /// the rendering has   namespace std { struct widget { … }; }
    /// ```
    ///
    /// Measured over eight standard-library headers, the rendering reads **24% more declarations with no parse
    /// errors at all**, where the file's own text reported 1 to 84 errors each and recovered from them by inventing
    /// declarations — a call inside a function body read as a declaration, a local read as a file-scope name.
    ///
    /// # The two coordinate systems
    ///
    /// The view's offsets are the **rendering's**. See [`FileView::parse_rendering`], whose note is the contract:
    /// [`FileView::file_offset_of`] and [`FileView::reading_offset_of`] are the way between the two, and a caller
    /// that reports a position to a user must go through them.
    pub fn view_of_the_rendering(&mut self, path: impl AsRef<Path>) -> Option<FileView> {
        let file = self.vfs.held(path)?.clone();
        let rendered = self.rendering_of(&file.path)?;
        // Remembered, so the next `view` — which cannot build one — finds it.
        self.known_rendering_of(&file.path, &file.text);
        Some(FileView::parse_rendering_in(
            &file,
            &rendered,
            &self.config_for(&file.path),
        ))
    }

    /// The rendering for `text`, **if it is already read** — a hash and a lookup, and nothing else.
    fn known_rendering_of(
        &self,
        path: &Path,
        text: &str,
    ) -> Option<std::sync::Arc<crate::preprocess::cooked::RenderedCooked>> {
        let key = (path.to_path_buf(), crate::cache::content_hash(text));
        self.renderings.lock().ok()?.get(&key).cloned()
    }

    /// **The same view, with the macros this file's own includes define, built if it has to be.**
    ///
    /// [`Session::view`] reads a file's tokens and nothing else, and the note on [`FileView::parse`] says why: a
    /// view is a buffer and its index entry, and "nothing here has read the include graph". **A session has.** The
    /// closure is in its index and the text is in its VFS, so the bodies are a walk away — and without them a
    /// buffer is read as a reader of one file reads it, where
    ///
    /// ```cpp
    /// _STD_BEGIN                       // `namespace std {` to a compiler
    /// struct widget { … };
    /// _STD_END
    /// ```
    ///
    /// puts `widget` at **file scope** instead of in `std`. Every MSVC header opens its namespaces this way, so
    /// this is the difference between a declaration being reachable as `std::widget` and not being reachable at
    /// all — and it is the reading the index itself uses for these files ([`crate::index::FileIndexer`] is given
    /// the same environment through `with_macro_bodies`).
    ///
    /// # The eager half, and when to want it
    ///
    /// [`Session::view`] gets the same reading **one query later** without ever blocking on it. This is for a
    /// caller that would rather wait: a batch, a test, or a first query after a file is opened. The cost is the
    /// one below.
    ///
    /// # Cost, measured
    ///
    /// Building the environment walks the file's whole closure and takes a copy of every file's text
    /// ([`Session::closure_with_text`]): on a closure of 151 files that is **103.7 ms**, beside **163.2 µs** for a
    /// view without it. Warm, from [`Session::macro_environments`], it is **186.6 µs** — 1.14× — which is what
    /// makes [`Session::view`]'s arrangement worth having: the walk is paid once, by the work loop, and every query
    /// after it pays a hash and a lookup.
    /// # Removed, and why it is not coming back
    ///
    /// This returned a `FileView` built from the file's own tokens with only the closure's **macro bodies** supplied.
    /// It was the third reading, and it had **zero product call sites** while its documentation elsewhere described
    /// it as the second — `FileIndexer::macro_facts` claimed its parse used `ParserConfig::with_macros_from_includes`
    /// and never did. A reading whose own documentation is wrong and which nothing calls is not a fallback, it is a
    /// trap for the next caller: supplying `#define` bodies changes what `_STD_BEGIN` expands to without taking the
    /// `#if` branches, so its scope tree was a third answer to a question that already had two.
    ///
    /// Deleted rather than kept as a fourth `None`. A caller that wants meaning has [`Session::view`] and waits; a
    /// caller that wants a spelling has [`Session::tokens_of`] and does not need a tree at all.
    /// The environment for `text`, **if it is already read** — a hash and a lookup, and nothing else.
    fn known_macros_of(
        &self,
        path: &Path,
        text: &str,
    ) -> Option<std::sync::Arc<cpp_parser::MacroEnvironment>> {
        let key = (path.to_path_buf(), crate::cache::content_hash(text));
        self.macro_environments.lock().ok()?.get(&key).cloned()
    }

    /// The macros `path`'s own includes define, as both readers of a body want them.
    ///
    /// The walk is [`crate::summary::macros_from_the_closure_with_bodies`] — the same one
    /// [`crate::SummaryStore`] uses when it has no unit timeline — fed from this session's own closure and
    /// overlay, so an unsaved buffer is what the macros come from. The result is remembered against `text`, and a
    /// second call with the same text is a lookup.
    fn the_macros_of(
        &self,
        path: &Path,
        text: &str,
    ) -> Option<std::sync::Arc<cpp_parser::MacroEnvironment>> {
        if let Some(known) = self.known_macros_of(path, text) {
            return Some(known);
        }

        let summary = self.store.index().summary(path)?;
        let (closure, by_key) = self.closure_with_text(path);
        let mut definitions = crate::summary::MacroDefinitions::default();

        let evidence = crate::summary::macros_from_the_closure_with_bodies(
            summary,
            |wanted| {
                let key = normalize_path(wanted, cfg!(windows));
                let (defined_in, text) = closure.get(*by_key.get(&key)?)?;
                Some((self.store.index().summary(defined_in)?, text.as_str()))
            },
            self.store.index().macros(),
            &mut definitions,
        );

        let made = std::sync::Arc::new(
            cpp_parser::MacroEnvironment::from_included_macros(evidence.macros)
                .with_bodies_in_force(evidence.conditional_bodies),
        );

        // **Bounded**: typing in one file changes its content hash on every keystroke, so an unbounded map here is
        // one environment per keystroke — and an environment of a header-heavy closure is thousands of names.
        let weight = cpp_parser::MacroFacts::len(made.as_ref());
        if let Ok(mut known) = self.macro_environments.lock() {
            known.insert(
                (path.to_path_buf(), crate::cache::content_hash(text)),
                made.clone(),
                weight,
            );
        }
        Some(made)
    }

    /// **What to report about one file, said in the file's own coordinates** — the answer the diagnostics channel
    /// publishes.
    ///
    /// Two readings can answer, and which one does is the whole point of this method:
    ///
    /// * the file has a **cooked** reading (the index holds one, built by [`Session::cook`]) → its parse errors, the
    ///   ones the parser found in the text a compiler actually parses. A declaration a macro writes stops being a
    ///   guess, and a branch nobody takes stops producing errors at all;
    /// * it does not → the file's own text, parsed here ([`Session::view`]).
    ///
    /// # Why the two are not merged
    ///
    /// Unioning them would publish, for a file that was cooked, exactly the errors the cooked reading exists to
    /// remove: the raw reading sees text the preprocessor never shows the compiler, so its errors include the ones
    /// inside untaken branches and the ones a macro's own shape provokes. Whoever has to choose — and this is the
    /// choice — should choose the reading that matches what the compiler sees.
    ///
    /// # What is *not* claimed
    ///
    /// A cooked answer is not automatically "clean": the rendering is the compiler's text, and a grammar gap is a
    /// grammar gap (MSVC's `sourceannotations.h` fails in both readings for a reason that is not the macros').
    /// Errors the rendering has and this file cannot show — text written in another file's macro body — are counted
    /// into [`FileDiagnostics::unplaced`] rather than dropped in silence, because "the list is empty" must not mean
    /// two different things.
    ///
    /// # Staleness, which is the caller's to handle
    ///
    /// A cooked reading is dropped when the file or its environment changes ([`ProjectIndex::forget_cooked`]),
    /// so between an edit and the next drain this falls back to the raw reading — and a caller that publishes that
    /// answer will publish the coarser one. The LSP's diagnostic service answers that by re-diagnosing once the
    /// queue drains; a caller that publishes and forgets would show the raw errors until the next edit.
    /// **Why this toolchain cannot see these modules — and this file's own partitions — one note each, pointing at
    /// the `import` line that wrote it.**
    ///
    /// The sentence the analysis owes a reader who wrote `import std;` and got nothing: "no file in this project
    /// declares module `std`" is true and useless, because the file *does* exist on the machine — it is outside the
    /// project, in a directory only the compiler knows about, and which directory that is **differs per compiler**.
    /// See [`crate::Toolchain::module_note`], where the per-compiler facts are written down with the measurement
    /// behind each one.
    ///
    /// # Partitions, which are the same failure and a different sentence
    ///
    /// `import :area;` names a partition of the module **this file declares**, so the question is not "which file
    /// declares module `area`" — nothing does, and asking it is why a partition import used to look like an import
    /// of a module nobody has. It is "which file declares partition `area` of `shapes`", and what the compiler
    /// needs is not a module built but the module's interface compiled *against* that partition's `.ifc`
    /// ([`crate::Toolchain::partition_note`], and `target/build_partitions.bat` for the three commands that
    /// establish it).
    ///
    /// Only a partition of the file's **own** module is asked about: `import :part;` in a file that declares no
    /// module is a different mistake — there is no module for the partition to belong to — and it is left to the
    /// reader of the syntax rather than described as a missing file.
    ///
    /// Empty when nothing is unreadable, which is the ordinary case for a project whose modules are its own files.
    ///
    /// # Where the range comes from, and why the tree rather than the summary
    ///
    /// A range is what makes the note *about* the line the reader wrote, and the summary does not keep one — it
    /// records the module names, because the visibility walk needs names and nothing else. So the file's own tree is
    /// read here, which the caller has already built ([`crate::FileView`]): the cost is one walk of a tree that
    /// exists, and the alternative is a second field on every import of every file in the project for a diagnostic
    /// that appears on a handful of them.
    ///
    /// # Why the notes are not [`FileDiagnostics`]
    ///
    /// They are not errors, and `FileDiagnostics` is the *errors* — a type whose severity is fixed at one value
    /// because everything in it is a parse failure. These are information, and merging them would mean a field whose
    /// only two values are "what everything else in the list is" and "this one", which is a field that says nothing.
    pub fn notes_about_the_modules(&self, view: &crate::FileView) -> Vec<ModuleNote> {
        let info = crate::ModuleInfo::from_tree(&view.root);
        let own_module = info.module_name.clone();
        let mut notes = Vec::new();

        for declaration in &info.imports {
            // **The note's range is a fact about the file the reader wrote, not about the rendering.** Its consumer is
            // the diagnostics channel, which turns it into a position with `position_in_file(held, note.start)` — the
            // *file's* text — so a range in the rendering's coordinates lands on the wrong line: measured,
            // `an_import_nothing_declares_gets_a_note_on_the_line_that_wrote_it` read `left: 0, right: 46`, the note
            // having started at the top of a rendering whose comments, blank lines and includes are gone. This is the
            // outbound half of the same crossing every other query makes at the seam, and a note that cannot be placed
            // in the file is dropped rather than shown somewhere wrong.
            // **Each end falls back on its own**, rather than the note being dropped when one of them has no span in
            // the rendering: a note is worth showing even if only one end can be placed, and dropping it turned a
            // wrong-line note into no note at all (measured: `a_partition_the_project_does_not_have_gets_a_note_about_its_module`
            // read `left: 0, right: 1`).
            let start = view
                .file_offset_of(ReadingOffset::new(declaration.range.start_offset))
                .map_or(declaration.range.start_offset, |at| at.get());
            let end = view
                .file_offset_of(ReadingOffset::new(declaration.range.end_offset()))
                .map_or(declaration.range.end_offset(), |at| at.get());
            let range = (start, end);

            // A partition of this file's own module: a file has to declare the module, and the project has to be
            // missing the partition's file, for there to be anything to say.
            if let Some(partition) = declaration.target.partition_name() {
                let Some(module) = own_module.as_deref() else {
                    continue;
                };

                if self.the_partition_is_read(module, partition) {
                    continue;
                }

                notes.push(ModuleNote {
                    message: match &self.toolchain {
                        Some(toolchain) => toolchain.partition_note(module, partition),
                        None => format!(
                            "no file this project contains declares partition `:{partition}` of module `{module}` \
                             — a partition is not a module, and until its file is read the names it exports are \
                             unknown rather than absent"
                        ),
                    },
                    start: range.0,
                    end: range.1,
                });

                continue;
            }

            let Some(module) = declaration.target.module_name() else {
                continue;
            };

            // A module this project declares is not a note: the reader can go to it, and telling them where their
            // compiler keeps modules would be an answer to a question they did not ask.
            if self.store.index().interface_unit_of(module).is_some() {
                continue;
            }

            let message = match &self.toolchain {
                Some(toolchain) => toolchain.module_note(module),
                // No compiler answered, so there is no per-compiler fact to give — and a sentence naming the
                // wrong compiler's switches would be worse than one naming none.
                None => format!(
                    "no file this project contains declares module `{module}` — a module outside the project is \
                     one only the compiler can see, and until then the names it exports are unknown rather than \
                     absent"
                ),
            };

            notes.push(ModuleNote {
                message,
                start: range.0,
                end: range.1,
            });
        }

        notes
    }

    /// **Did any file this session read turn out to be the interface unit of `module:partition`?**
    ///
    /// Asked of the **summaries** rather than of a resolution, so that the answer is "the project has this file"
    /// rather than "the naming convention proposes one": a partition's file is read by the index like any other
    /// module unit, and the reading records which module and partition it declares
    /// ([`crate::ModuleReading::partition`]).
    ///
    /// A linear scan over the summaries, which is the same shape as every other question the index cannot key — and
    /// it runs once per partition import of one file, which is a handful.
    fn the_partition_is_read(&self, module: &str, partition: &str) -> bool {
        self.store.index().summaries().any(|summary| {
            summary.modules.module.as_deref() == Some(module)
                && summary.modules.partition.as_deref() == Some(partition)
                && summary.modules.is_interface
        })
    }

    pub fn diagnostics(&self, path: impl AsRef<Path>) -> Option<FileDiagnostics> {
        let path = path.as_ref();
        let _whole = DiagTrace::new("diagnostics");

        // The index's own spelling of the path, for the reason `cook` documents: a client's `C:\…` and the
        // resolver's `c:/…` are the same file, and a lookup by the client's spelling finds nothing.
        let key = self
            .store
            .index()
            .summary(path)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| path.to_path_buf());

        // **The cooked reading first, and without parsing anything.** Its errors are already placed in the file
        // (that is what `index_rendering` did), so answering from it costs a lookup — while the raw answer needs the
        // file's own tree, which is a parse per request. A reading the index holds is a reading of the text the VFS
        // is holding now: a change to the file or to its environment drops it (`forget`, `forget_cooked`).
        //
        // **The module notes are taken from the same tree, when there is one.** A caller that asks
        // [`Session::diagnostics`] and then [`Session::notes_about_the_modules`] with a view of its own parses the
        // file twice, once for each answer, and the file's tree is the expensive half of both. So the notes are
        // attached here — from the tree this call already has, or from a view it reads once — and the method that
        // takes a view stays for a caller that has one and wants nothing else.
        let mut notes = Vec::new();

        if let Some(cooked) = self.store.index().cooked_reading(&key) {
            // The tree the raw answer would have needed, if the session can still read the file: the notes are about
            // the imports as they are *written*, and a file that has gone missing since it was indexed has no text to
            // point at. Reading it here rather than making the caller ask again is what keeps one request to one
            // parse — and the view is a parse of a file the VFS is already holding, because the cooked reading could
            // not exist for a file nothing read.
            //
            // The **checks** read it too, and for the same reason: they are asked of the file's facts and its tree,
            // and both come from this one view. A file that cannot be read back is a file with no checks, which is
            // the same answer as a file no check had anything to say about.
            let view = {
                let _t = DiagTrace::new("the file's tree (view)");
                self.view(path)
            };
            if let Some(view) = &view {
                let _t = DiagTrace::new("module notes");
                // **A note about a module says what the session has *read*, so it waits for the reading.**
                //
                // `no file this project contains declares module `std`` was published on the first pull after a
                // window opened and gone by the next edit, because on that pull the index had not yet read the
                // project`s module units — the answer "no file declares it" was true of what had been read and
                // false of the project. The same timing rule the checks already obey applies: nothing is said
                // while the session still has work, because absence of evidence is not evidence here.
                notes = if self.pending_work() == 0 {
                    self.notes_about_the_modules(view)
                } else {
                    Vec::new()
                };
            }
            let checks = match &view {
                Some(view) => self.checks_about(&key, view),
                None => Vec::new(),
            };

            return Some(FileDiagnostics {
                reading: DiagnosticReading::Cooked,
                unplaced: cooked.unplaced,
                // **A file that says it cannot be read on its own.** Some headers are *fragments*: their body is a
                // piece of an initializer that another file opens and closes around them. libstdc++'s
                // `bits/text_encoding-data.h` is one — a list of `{ 3, "US-ASCII" }` rows, guarded by
                // `#error "This is not a public header, do not include it directly"`. Read as a translation unit it
                // is **895** parse errors, every one of them a consequence of the brace that has no opening; a
                // compiler says the same thing in one line, which is what its own `#error` is for.
                //
                // So the message is published and the cascade is not. Two signals together, and both are facts: the
                // file writes an `#error` line, **and** its rendering begins with a `{` or a `}` — a token no
                // declaration starts with. A file with an `#error` in a branch nobody takes, or a fragment with a
                // typo elsewhere, keeps its own errors: the pair has to hold.
                errors: match view.as_ref().and_then(Self::a_fragment_and_its_message) {
                    Some((start, end, message)) => vec![FileDiagnostic {
                        start,
                        end,
                        message,
                        code: crate::sema::check::DiagnosticCode::SyntaxError,
                    }],
                    None => cooked
                        .diagnostics
                        .iter()
                        .map(|error| FileDiagnostic {
                            start: error.range.start_offset,
                            end: error.range.end_offset(),
                            message: error.message.clone(),
                            // **Every diagnostic carries its rule.** These came from the parser, so the rule is
                            // `syntax-error` — stated here rather than left for the client to guess from the message.
                            code: crate::sema::check::DiagnosticCode::SyntaxError,
                        })
                        .collect(),
                },
                notes,
                checks,
            });
        }

        // **No cooked reading means no answer — and it also means no *view*, which is why there is no second arm.**
        //
        // This used to fall through to `self.view(path)` and report the errors of the file's own text. That is gone
        // for the reason [`DiagnosticReading::Cooked`] records: an untaken branch is not compiled, so an error found
        // inside one is an error about a program that does not exist. Measured on the fixture in
        // `a_branch_nobody_takes_reports_nothing_once_the_file_is_cooked`: the same `#if 0` branch produced **1**
        // error as the file's own text and **0** once cooked.
        //
        // The arm is deleted rather than kept returning `None`, because a file with a rendering always has a cooked
        // reading (`index_rendering` files one for every rendering it is given), so the fall-through was already
        // unreachable — and unreachable code that reports diagnostics is the shape of the defect this removed.
        None
    }

    /// **A file that declares itself a fragment**, with the message it wrote for that case.
    ///
    /// The two signals, both facts and both required: the file **writes an `#error` line**, and the **first token of its
    /// rendering is a brace**. A fragment's body begins with the `{` of the initializer its including file opens;
    /// nothing else starts a translation unit that way, and a file that has an `#error` in a branch nobody takes still
    /// starts with a declaration. See the call site for the header this was measured on.
    fn a_fragment_and_its_message(view: &crate::FileView) -> Option<(usize, usize, String)> {
        let written = view.written_text().unwrap_or(&view.source);
        let (start, end, message) = Self::error_directive_line(written)?;

        // The rendering's first non-trivia token, read from the tree rather than from the text: comments and directives
        // are already gone from it, so a `{` here is the file's first real token.
        let first = view
            .root
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .map(|token| cpp_parser::CppTokenKind::from(token.kind()))
            .find(|kind| {
                !matches!(
                    kind,
                    cpp_parser::CppTokenKind::Whitespace | cpp_parser::CppTokenKind::Newline
                )
            })?;
        if !matches!(
            first,
            cpp_parser::CppTokenKind::LeftBrace | cpp_parser::CppTokenKind::RightBrace
        ) {
            return None;
        }

        Some((start, end, message))
    }

    /// The first `#error` line of a written text: where it is, and what it says.
    fn error_directive_line(text: &str) -> Option<(usize, usize, String)> {
        let mut at = 0;
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if let Some(rest) = trimmed.strip_prefix('#') {
                let rest = rest.trim_start();
                if let Some(message) = rest.strip_prefix("error") {
                    let message = message.trim().trim_matches('"').trim();
                    return Some((at + indent, at + line.trim_end().len(), message.to_string()));
                }
            }
            at += line.len();
        }
        None
    }

    /// The checks' answer for one file — see [`crate::sema::check`].
    ///
    /// An empty list when the index holds no summary for the path, which is the same answer as "no check had
    /// anything to say": a check is a claim about facts the analysis has, and a file it has no facts about is a
    /// file it has nothing to claim.
    ///
    /// Nothing is parsed here. Every check is answered from the summary the reading already produced, so asking
    /// for diagnostics is not a reason for a parse to happen — it is a reason to read what one produced.
    fn checks_about(&self, key: &Path, view: &crate::FileView) -> Vec<crate::sema::check::Finding> {
        let index = self.store.index();
        let Some(summary) = index.summary(key) else {
            return Vec::new();
        };

        // **The model is built here, and it is what the checks are handed.** One per `diagnostics` call, which is
        // the lifetime that makes its memo need no invalidation rule — see [`Session::semantic_model`]. The view is
        // already a parse this call paid for, so the model costs a `Box` and an empty `BTreeMap`.
        //
        // Owned by `Checks` rather than borrowed, because the model is a *view* — a handful of references and a memo
        // — so moving it into the value that uses it costs nothing and removes a borrow that could not outlive this
        // function's own frame.
        // **The second pull of an unchanged file answers from the first one's findings.** The key names every input a
        // finding can depend on: this file's key (content, context, environment), the closure's fingerprint (a
        // `#define` in a header it includes), the store's revision (a summary dropped anywhere), the configuration, the
        // file's language and whether the project is Unreal code.
        let state = self.the_state_of_a_pull(key, summary);
        if let Ok(memo) = self.checks_memo.lock()
            && let Some((remembered, findings)) = memo.as_ref()
            && *remembered == state
        {
            return findings.clone();
        }
        let model = self.semantic_model(view);

        // **The config, and the run.** The rules come from `.cppls.toml`'s `[diagnostics]` section when a project
        // writes one, and every rule is on otherwise — see `CheckConfig::default`. The filtering lives in
        // `CheckContext::add`, so that no check has to know a rule was switched off.
        //
        // `enable` is "only these" and `disable` is "not these", with `disable` winning: the pair a project wants —
        // `enable` for one that starts from nothing, `disable` for the single rule that is noise. Unknown names are
        // logged rather than ignored: a rule a user believes is on and that never runs is the failure this
        // mechanism exists to prevent.
        let config = crate::sema::check::CheckConfig::from_names(
            &self.project_config().config.diagnostics.enable,
            &self.project_config().config.diagnostics.disable,
        );
        for name in config.unknown() {
            log::warn!("`[diagnostics]` names a rule this build does not have: `{name}`");
        }
        let inside = {
            let mut inside: Vec<std::path::PathBuf> = vec![self.root.clone()];
            inside.extend(
                self.project
                    .iter()
                    .filter_map(|file| file.parent().map(std::path::Path::to_path_buf)),
            );
            // **A library is the project's too**, as far as a suggestion is concerned: its headers are what a target
            // that depends on it may add an `#include` for. See `missing_include`.
            inside.extend(
                self.project_config()
                    .config
                    .libraries
                    .iter()
                    .map(|library| library.root(&self.root)),
            );
            inside.sort();
            inside.dedup();
            inside
        };
        let findings = crate::sema::check::run(crate::sema::check::CheckContext::new(
            key,
            summary,
            model,
            &view.source,
            // **Both texts**: the checks read ranges from the tree (the rendering) and from the summary (the
            // file), and mixing the two is a measured defect - see `CheckContext::written`.
            view.written_text().unwrap_or(&view.source),
            view.tree.get_errors(),
            view,
            config,
            // **The language this file is compiled as**, which is what makes a C-only rule quiet in C: the compile flags say
            // `-std=c11`, and `cstyle_cast` is then not a rule about this file at all.
            self.config_for(key).language_for(key),
            // **Whether the project is Unreal Engine code** — see `[unreal] enable`. Read here rather than inside a
            // check so that the switch has one home.
            self.project_config()
                .config
                .unreal
                .as_ref()
                .is_some_and(|unreal| unreal.is_on()),
            // **Which files this project may add an `#include` for** — the workspace root and the directories the
            // project's own files are in. (The second half is not redundancy: a session over a *virtual* filesystem —
            // a test, a remote client — has paths that do not start with its root, and the rule must work there too.)
            // Once `[library]` exists its paths are added here as well. See `CheckContext::inside`.
            &inside,
        ));
        // Stored, then answered: the next pull of this state is a lookup.
        if let Ok(mut memo) = self.checks_memo.lock() {
            *memo = Some((state, findings.clone()));
        }
        findings
    }

    /// **Everything a pull's findings are a function of**, as one number.
    fn the_state_of_a_pull(&self, key: &Path, summary: &crate::FileSummary) -> u64 {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(format!("{:?}", summary.key).as_bytes());
        bytes.extend_from_slice(
            &self
                .store
                .the_closure_fingerprint(key)
                .unwrap_or(0)
                .to_le_bytes(),
        );
        bytes.extend_from_slice(&self.store.revision().to_le_bytes());
        let config = self.project_config();
        for rule in config
            .config
            .diagnostics
            .enable
            .iter()
            .chain(config.config.diagnostics.disable.iter())
        {
            bytes.extend_from_slice(rule.as_bytes());
            bytes.push(0x1f);
        }
        bytes.extend_from_slice(format!("{:?}", self.config_for(key).language_for(key)).as_bytes());
        bytes.push(u8::from(
            config
                .config
                .unreal
                .as_ref()
                .is_some_and(|unreal| unreal.is_on()),
        ));
        crate::cache::fnv1a64(&bytes)
    }

    /// **Answer the pull for the files a reader has open, before it is asked for.**
    ///
    /// A pull's first answer for a new reading is the expensive one — measured on `CodeFormat.cpp`, **36.6 ms** (18.1 ms of
    /// inference) for the pull after an edit and **109 ms** for the first pull of a session, against **2.2 ms** for a
    /// repeat. Those numbers are the first answer for a state, and the pump is the only party that knows when a state
    /// exists and nobody is waiting for its answer: it has just committed the reading. So it asks the same question the
    /// client will ask, and the `checks_memo` it fills is what the client's pull finds.
    ///
    /// **At most two files** — a reader has one open file and the header beside it — and only what `diagnostics` can
    /// answer at all: an uncooked reading is a `None` here and a `None` there, so the warm costs a lookup.
    pub fn warm_the_pulls(&self) -> usize {
        let mut warmed = 0;
        for path in self.documents.paths().into_iter().take(2) {
            if self.diagnostics(&path).is_some() {
                warmed += 1;
            }
        }
        warmed
    }

    /// **The query entry point for one file** — a [`crate::sema::model::SemanticModel`] over `view`, with the
    /// session as its resolver.
    ///
    /// This is the thing the four `type_of_expression` call sites were missing, and the difference is one closure:
    ///
    /// ```text
    ///   before   type_of_expression(index, &mut |_: &Path| None, …)   inference gives up at the first header
    ///   now      session.semantic_model(&view)                         inference can read that header's tree
    /// ```
    ///
    /// # Why it takes only `&self`, and why the memo is still sound
    ///
    /// The resolver it installs *is* the session: the model may need another file's tree, and obtaining one is
    /// [`Session::view_of_the_file`] — a read through the VFS, and a parse. That is a **read**, so the model borrows
    /// the session shared, which is what lets a caller hold the model and still ask the session other questions
    /// beside it. The first version took `&mut self` on the theory that a parse is a mutation; it is not — the VFS
    /// is already behind its own lock — and the mutable borrow bought nothing while forbidding the model and its
    /// session from being used together.
    ///
    /// The memo is sound for a different reason than the borrow: its answers are keyed by node ranges in `view`,
    /// and `view` is the caller's — an edit replaces the view rather than mutating it, so a model cannot outlive the
    /// text its answers are about. There is no invalidation rule to write because there is nothing to invalidate.
    pub fn semantic_model<'s>(
        &'s self,
        view: &'s FileView,
    ) -> crate::sema::model::SemanticModel<'s> {
        // **The resolver: a path in, that file's own reading out.** `view_of_the_file` and not `view`, because a
        // declaration's type is written in the file it was written in — this file's *rendering* is not that file's
        // tree, and answering a question about `<vector>` with a rendering of the file being edited would answer in
        // the wrong coordinates.
        //
        // The session is reached through a **shared** reborrow, and that is what lets the model hold both at once:
        // the resolver only needs `&Session` (obtaining a file's tree is a read of the VFS and a parse, not a
        // mutation of the analysis), so it does not conflict with the index reference taken beside it.
        let session: &'s Session<F> = self;
        let index: &'s crate::ProjectIndex = session.store.index();

        // **The views this model has already been given, by path.** The resolver parses the file it is asked about,
        // and one diagnostic pass asks about the same few headers again and again: measured on EmmyLuaCodeStyle's
        // `LuaParser/src/File/LuaSource.cpp`, `type_of` took **55 ms** per expression and the parse of the header
        // that declares `std::size_t` was most of it — repeated for every expression, because the view was dropped
        // after the answer.
        //
        // Kept **here and not in the session**: a `FileView` holds rowan nodes, which are not `Send`, and a session
        // is shared across threads. This closure lives as long as the model — one diagnostic pass — which is exactly
        // the lifetime in which the repetition happens, and no other thread can see these readings.
        let seen: std::cell::RefCell<std::collections::HashMap<PathBuf, Option<FileView>>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
        let resolver = Box::new(move |path: &Path| -> Option<FileView> {
            if let Some(known) = seen.borrow().get(path) {
                return known.clone();
            }
            let started = std::time::Instant::now();
            let made = session.view_of_the_file(path);
            if crate::probes::timing() {
                eprintln!(
                    "[resolver] {:>5} ms  {} ({})",
                    started.elapsed().as_millis(),
                    path.file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string()),
                    if made.is_some() { "read" } else { "none" }
                );
            }
            seen.borrow_mut().insert(path.to_path_buf(), made.clone());
            made
        });

        crate::sema::model::SemanticModel::new(
            index,
            &view.root,
            &view.scopes,
            &view.source,
            &view.path,
            resolver,
        )
    }

    /// The text the analysis reads for a path — the buffer when it is open, the file otherwise.
    ///
    /// [`Session::view`] without the parse, for a consumer that wants the text rather than the tree: a hover that
    /// shows a declaration the cursor is not in, a search over a file the index already holds. `None` when there is
    /// neither a buffer nor a readable file.
    ///
    /// Read through the VFS, so the file is held afterwards and a second question about it — its lines, its text —
    /// costs nothing.
    pub fn text(&self, path: impl AsRef<Path>) -> Option<String> {
        Some(self.vfs.held(path)?.text.to_string())
    }

    /// Read a file in and hold it, answering with the id the VFS gave it.
    ///
    /// The **writer** path's way to hold a file: a notification, an indexing step, a caller that knows it is about
    /// to ask several questions about one file. After this, [Session::view] and [Session::text] answer for it.
    pub fn load(&mut self, path: impl AsRef<Path>) -> Option<crate::FileId> {
        self.vfs.load(path)
    }

    /// The files the session is holding: their text and their line indexes.
    ///
    /// Exposed because "which text is this analysis working from" is a question a caller diagnosing a wrong answer
    /// asks, and because it is the one place that knows how much of a project has been read *as text* rather than
    /// as facts.
    pub fn files(&self) -> &Vfs<SessionFiles<F>> {
        &self.vfs
    }

    /// **The translation unit `path` belongs to**, walked once and then reused.
    ///
    /// # Why this is the whole point of plan A
    ///
    /// A file's macro environment is a *position in one walk of the unit*
    /// ([`crate::TranslationUnit`]): the walk visits every file the unit enters, records where each was entered, and
    /// every later question — "what is `_STD_BEGIN` in `<xstring>`" — is a lookup in that timeline rather than a
    /// closure walk of its own. Measured before this existed (the census, 255 files of the Windows SDK):
    /// **17 618 794 conditional facts evaluated, 147 s of a 174 s run**, because each file's environment was built
    /// as if it were the only one.
    ///
    /// [`Session::cook`] built one anyway — *per cooked file* — so cooking the 138 files of one real project's
    /// closure walked that closure 138 times. Three levels of reuse, cheapest first:
    ///
    /// ```text
    /// this session, this unit        the in-memory map below          — no walk, no read, no decode
    /// a previous run, same content   TranslationUnitCache on disk     — a read and a hash of the closure
    /// otherwise                      the walk, once                   — and it is written for the next run
    /// ```
    ///
    /// # What invalidates it
    ///
    /// ```text
    /// the in-memory map    this session's own edits — and only the edited file's entry (Session::buffer_changed)
    /// the disk entry       whatever RootKey says, which for a file being typed into is its *preamble*
    /// ```
    ///
    /// The disk rule is the one that used to be wrong. It hashed every file of the closure whole, the root
    /// included, so **the file a person was typing into could never be reused** — its own hash moved on every
    /// keystroke — and every cook of it paid a walk of its whole closure. See [`crate::tu_cache::RootKey`]: the
    /// root is keyed on everything a walk can read out of it, which its body is not.
    fn translation_unit_of(&self, path: &Path) -> Option<std::sync::Arc<crate::TranslationUnit>> {
        self.unit_and_state_of(path).map(|(unit, _)| unit)
    }

    /// [`Session::translation_unit_of`], and **whether the timeline is of the whole program** — see [`HeldUnit`].
    ///
    /// The flag is needed by the one caller that must not use a provisional timeline at all:
    /// [`Session::cooking_materials`], because a rendering is filed and never revisited, where a timeline is
    /// re-walked the moment the queue drains.
    fn unit_and_state_of(
        &self,
        path: &Path,
    ) -> Option<(std::sync::Arc<crate::TranslationUnit>, bool)> {
        let key = queue_key(path);

        // **The guard is bound and dropped before the body**, not held across it: `held_units()` is a `Mutex`, and
        // a `MutexGuard` temporary in an `if let` scrutinee lives until the whole statement ends — so taking it
        // again inside the body (the `remove` below) is a re-entrant lock on one thread, which is a deadlock and
        // not an error the type can report.
        let held = self.held_units().get(&key);
        if let Some((provisional, unit)) = held {
            // **A provisional timeline is dropped once there is nothing left to arrive.** It was walked while a
            // file its closure needed was still queued; if the queue has since drained, that file has been read
            // and this is a timeline of a smaller program — see [`HeldUnit`]. Until then it is served, because a
            // reading of most of the program is what the index has and the alternative is a walk per step.
            if !provisional || self.queue.pending() > 0 {
                self.note_a_unit(Which::FromMemory);
                return Some((unit, provisional));
            }
            self.held_units().remove(&key);
        }

        let context = self.store.context_hash(path);
        // nothing is read from a timeline cache — see `cached` below
        // (`root_key_of` is gone with the timeline cache — see the note at `cached` below)

        // Read out of the cache under a timer, then matched: a stage timer inside a `match` scrutinee is a block
        // clippy asks to hoist, and hoisting it is what the timer wants anyway — it should cover the read only.
        // **No timeline cache on disk.** `tu/` stored built `TranslationUnit`s and was the last of the old
        // per-directory files (`cache.redb` holds the summaries and the unit renderings now). A timeline is
        // cheap to rebuild — 38 ms — and rebuilding it is the only way its environment is guaranteed to be the one
        // the current index describes; a stored one outlives the files it was built from. So the walk runs.
        let cached: Option<crate::TranslationUnit> = None;

        let (unit, provisional) = match cached {
            Some(unit) => {
                self.note_a_unit(Which::FromDisk);
                // An entry on disk is never provisional: `put` refuses to write one. See [`HeldUnit`].
                //
                // Boxed into an `Arc` because both arms of this match must agree, and the walk arm shares its value
                // with every later root whose closure is the same — see [`Session::walked_by_content`].
                (std::sync::Arc::new(unit), false)
            }
            None => {
                // **The file the index cannot describe in full, and the order that answers it.**
                //
                // A file a reader is editing has no summary — [`Session::did_change`] drops it — so
                // the walk has no root to start from, and until this existed the file's timeline
                // was unobtainable exactly while the reader was working on it. Its **directives**
                // are the part of it that can be read without a parse, and they are also the part
                // the walk needs first, because they name the includes.
                //
                // The fallback is for a file a reader is looking at, and only for one: a directive reading is a lex
                // and a directive scan, which is cheap but not free, and the project's other ten thousand files have
                // no claim on it — they are read as text and refined by the second pass.
                let owned_root;
                let index = self.store.index();
                let root = match index.summary(path) {
                    Some(summary) => summary,
                    None if self.documents.is_open(path) => {
                        owned_root = self.store.directive_summary(path)?;
                        // **A timeline of the whole program or none at all.** A walk stops at a file it cannot look
                        // up and treats it as defining nothing, which is the right reading for a file outside the
                        // analysis and the wrong one for a file nobody has read yet: the rendering made from such a
                        // timeline is the macro left unexpanded, which is worse than the file's own text (see
                        // [`HeldUnit`]). A file whose program is not read yet is left to the raw path, and the second
                        // pass renders it once the closure is in hand.
                        self.the_closure_of_the_directives(path, &owned_root)?;
                        owned_root.as_ref()
                    }
                    None => return None,
                };

                let read_started = std::time::Instant::now();
                let (closure, closure_by_key) = { self.closure_with_text_from(path, Some(root)) };
                let read_took = read_started.elapsed();
                let closure_bytes: usize = closure.iter().map(|(_, text)| text.len()).sum();
                let walk_started = std::time::Instant::now();

                // **The session's own definition cache**, not a fresh one: the walk reads back every `#define` a
                // file sees, and the same definition reaches hundreds of files — `<vector>` and `<string>` both see
                // `_STD_BEGIN`, whose text is in `yvals_core.h`. One cache per *walk* is one parse per definition
                // per file, which the census measured at 12.2 s of a 40 s run. Invalidated **per path** when a file
                // changes, since the keys are offsets in that file.
                // **Held rather than taken**, for the reason the units table carries: this cache is what made the
                // unit walk need `&mut self`, and `&mut self` is what put the walk inside the writer. A lock of its
                // own is enough — the walk is the only thing that ever holds it for long.
                #[cfg(feature = "perf-probe")]
                crate::perf::mark("closure");
                let mut definitions = self.held_definitions();
                #[cfg(feature = "perf-probe")]
                crate::perf::mark("definitions");

                // **Has this exact closure been walked already in this run?** A `TranslationUnit` is a function of
                // its closure's content and the context, so two roots that agree on both have the same timeline and
                // the second one is 68 s of guard evaluation for an answer already in hand. Measured on a real
                // project: 2110 roots, 786 distinct closures, **46% of walks removable**. See
                // [`Session::walked_by_content`].
                let identity = self.closure_identity(&closure, context);
                let shared = self
                    .walked_by_content
                    .lock()
                    .ok()
                    .and_then(|mut held| held.get(&identity).cloned());

                let (unit, provisional) = if let Some(shared) = shared {
                    #[cfg(feature = "perf-probe")]
                    crate::perf::mark("walk");
                    // A shared timeline is never provisional: only a finished walk is ever stored, for the same
                    // reason the disk entry refuses a provisional one.
                    self.note_a_unit(Which::FromMemory);
                    (shared, false)
                } else {
                    // An `Arc` from the start, because a shared timeline hands the *same* value to every root that
                    // asks for it: `TranslationUnit` is not `Clone` and does not need to be — the units table has
                    // always held `Arc`s, and this is the same value reaching more callers.
                    // **Is the closure still growing?** Asked *before* the walk, because a unit whose closure can still
                    // grow is incomplete by construction: it must neither fill the sharing cache nor read from it.
                    let provisional = self.closure_is_still_growing(&closure, &closure_by_key);
                    let walked = std::sync::Arc::new({
                        // **The root's own summary comes from `root`, not from the index.** For a file the index
                        // cannot describe — the one a reader is editing, whose directives are all there is — the
                        // lookup would otherwise answer `None` for the very file the walk starts at, and a timeline
                        // with no root is a timeline of an empty program.
                        let root_key = queue_key(path);
                        crate::TranslationUnit::walk(
                            root,
                            |wanted| {
                                let (held, text) =
                                    closure.get(*closure_by_key.get(&queue_key(wanted))?)?;
                                let summary = if queue_key(held) == root_key {
                                    root
                                } else {
                                    index.summary(held)?
                                };
                                Some((summary, text.as_str()))
                            },
                            index.macros(),
                            &mut definitions,
                            // **Off, and this is why.** The sharing replays a file's cached contribution at the position
                            // it was recorded at in a *different* walk, and a preprocessor's positions are not portable
                            // between programs: measured on `import std;`, `<limits>` lost its `<cwchar>` and
                            // `<xstddef>` includes — the second defines the `_MEMBER_CALL_*` macros — so
                            // `<xtr1common>` was cooked without them and the rendering kept their invocations as text.
                            // `CPPLS_WALK_SHARE=1` turns it back on for an A/B measurement.
                            !provisional && self.share_contributions,
                            self.store.contributions(),
                        )
                    });
                    let walk_took = walk_started.elapsed();

                    // **Is this timeline of the whole program, or of as much of it as has been read?** A file the
                    // closure could not offer is either outside the analysis — the permanent reading, and the one
                    // the walk's own early return documents — or *not yet read*, which is a fact about the queue and
                    // not about the file. See [`HeldUnit`] for what storing the second kind cost.

                    let mut put_took = std::time::Duration::ZERO;
                    if !provisional {
                        // Written for the **next run**.
                        let put_started = std::time::Instant::now();
                        // (the disk cache is gone — see the note at `cached` above)
                        put_took = put_started.elapsed();

                        // …and offered to the **rest of this one**, under the same condition: a provisional
                        // timeline is one the queue will make obsolete, and handing it to a second root would
                        // spread the smaller program rather than save a walk.
                        //
                        // The weight is the timeline's own size, because that is what the budget is about: two
                        // roots of a header-heavy project differ in closure size by an order of magnitude and a
                        // count would treat them the same.
                        let weight = walked.heap_bytes();
                        if let Ok(mut held) = self.walked_by_content.lock() {
                            held.insert(identity, std::sync::Arc::clone(&walked), weight);
                        }
                    }

                    // **Counted here and not after the match**: reaching this arm is what a walk *is*, and a call
                    // that was served off the disk reaches the other one. Counting after the match counts both,
                    // which is the mistake this line was written with.
                    self.note_a_unit(Which::Walked);
                    // …and counted **by root**, which the total above cannot say. See [`Session::walks_by_root`].
                    if let Ok(mut walks) = self.walks_by_root.lock() {
                        *walks.entry(queue_key(path)).or_insert(0) += 1;
                    }

                    if crate::probes::timing() {
                        eprintln!(
                            "[unit] read {:>4} ms, walk {:>4} ms, put {:>4} ms, {} file(s) / {} bytes  {}",
                            read_took.as_millis(),
                            walk_took.as_millis(),
                            put_took.as_millis(),
                            closure.len(),
                            closure_bytes,
                            path.file_name()
                                .map(|name| name.to_string_lossy().to_string())
                                .unwrap_or_else(|| path.display().to_string())
                        );
                    }

                    (walked, provisional)
                };

                (unit, provisional)
            }
        };

        // Already an `Arc` from both arms of the match above — the units table has always held `Arc`s, and a shared
        // timeline is the same value reaching more than one root.
        self.held_units()
            .insert(key, std::sync::Arc::clone(&unit), provisional);
        Some((unit, provisional))
    }

    /// **Does this session hold a macro environment for `path` that is worth reading the file with?**
    ///
    /// Asked by [`Session::advance`] before it takes a file's summary out of the wave: a wave parses a file as a
    /// pure function of its text, which cannot place a scope that a macro in **another** file opens, and this
    /// session may already know better. See [`HeldUnit`] for why a provisional timeline does not count — it is
    /// missing whatever the queue has not read, so reading a file with it and marking the result would suppress the
    /// second pass for a reading that needs it.
    ///
    /// Asked by the index's own spelling of the path, because a unit's frames are keyed by the spelling the walk
    /// used — the same trap [`Session::units_for_the_pass`] records from the other side.
    fn holds_an_environment_for(&self, path: &Path) -> bool {
        let spelled = self
            .store
            .index()
            .summary(path)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| path.to_path_buf());
        let held = self.held_units().get(&queue_key(&spelled));
        held.is_some_and(|(provisional, _)| !provisional)
    }

    /// **Will this file be read as the program it is in?** — the question a wave asks before it parses a file as
    /// text, and the union of the two ways a file gets a macro environment here.
    ///
    /// ```text
    /// the timeline is in hand                      the pump's ordinary case: a body edit keeps it
    /// it can be walked now — see below             what this closes: the unit was evicted, or this is the
    ///                                              first read after `did_open`
    /// ```
    ///
    /// [`Session::advance`] and [`Session::prepare_a_wave`] both ask it for every file they are about to hand to a
    /// wave, because a wave parses a file as a pure function of its text — which cannot place a scope that a macro
    /// in **another** file opens, and would then be *written to the disk cache* as the file's reading. A file this
    /// answers `true` for is left out of the wave and read by [`Session::index_one`] instead, where the timeline is.
    ///
    /// # A held timeline is worth it only for a file somebody is looking at
    ///
    /// `a_timeline_can_be_walked_for` already says so in its first condition, and `holds_an_environment_for` did
    /// not — and that asymmetry is what made a **batch** pay a program read per file. Measured on the modules
    /// fixture with `import std;`: the drain after the module unit was queued had nine `advance(64)` batches, seven
    /// of them 20–60 ms and one of them **45.9 s for 33 files** — the batch the pass had just built timelines for,
    /// so every one of them took this arm and `index_one` re-walked a closure for a file nobody had opened.
    ///
    /// The reading itself is not lost: a file read as text is refined by the second pass, which is the pipeline the
    /// rest of the project relies on, and a *request* for an open file still gets the timeline (`want_cooked_reading`
    /// is what a request calls).
    fn will_read_it_as_the_program(&self, path: &Path) -> bool {
        if !self.documents.is_open(path) {
            return false;
        }
        self.holds_an_environment_for(path) || self.a_timeline_can_be_walked_for(path)
    }

    /// **Can a timeline for this file be walked right now, without one being in hand?**
    ///
    /// Three conditions, and each is one of the costs this has to be worth:
    ///
    /// * **A reader is looking at it.** A walk reads the whole closure and evaluates every conditional in it — 38 ms
    ///   on a real project's — and it is worth that for the file somebody is typing in and for no other. The rest of
    ///   the project is read as text and refined by the second pass, which is what the pipeline already does.
    /// * **It has no summary, or its whole closure is read.** A file with no summary is one that was just edited or
    ///   just opened: its *directives* are the only part of it the index can describe, and they are enough to say
    ///   whether the program it needs is in the index ([`Session::the_closure_of_the_directives`]).
    /// * **Nothing is left queued inside that closure**, which is the same statement: a file whose summary was
    ///   dropped has no entry, and the check above fails on it — so a cold start answers `false` for everything and
    ///   pays nothing but the directive scan.
    fn a_timeline_can_be_walked_for(&self, path: &Path) -> bool {
        if !self.documents.is_open(path) {
            return false;
        }

        match self.store.index().summary(path) {
            Some(_) => self.is_closure_ready(path),
            None => self
                .store
                .directive_summary(path)
                .is_some_and(|directives| {
                    self.the_closure_of_the_directives(path, &directives)
                        .is_some()
                }),
        }
    }

    /// **The timeline `path` may be read through now** — the one in hand, or the one that can be walked.
    ///
    /// `None` means the file falls back to its own text (and the second pass refines it later), which is the cold
    /// start's path and stays it: an environment that is not the whole program renders *worse* than the raw text.
    fn the_timeline_to_read_through(
        &self,
        path: &Path,
    ) -> Option<std::sync::Arc<crate::TranslationUnit>> {
        if let Some((false, unit)) = self.held_units().get(&queue_key(path))
            && unit.environment_of(path).is_some()
        {
            return Some(unit);
        }

        if !self.a_timeline_can_be_walked_for(path) {
            return None;
        }

        // **This is the 3.6 s, and it is required behaviour** — see the two contract tests below, which fail if it is
        // removed: `a_file_opened_after_the_index_was_built_is_read_through_a_walked_timeline` and
        // `a_keystroke_in_a_body_reads_the_file_through_its_timeline`. Measured on `LSPHandle.cpp`
        // (`tests/codestyle_probe.rs::what_keeps_the_pump_busy`): with this arm skipped, one `advance` step after an
        // edit falls from **3.73 s to 8.75 ms** — so the cost is *acquiring* the timeline (disk decode of a
        // whole-program rendering, or a walk), and it is paid **per keystroke** because the bounded units table has
        // evicted this file's entry during the project read.
        //
        // The fix is therefore not here: it is to keep the **open file's timeline hot** (pinned against eviction), so
        // the contract holds at milliseconds. See `docs/buglist.md` §6.
        let (unit, provisional) = self.unit_and_state_of(path)?;
        (!provisional && unit.environment_of(path).is_some()).then_some(unit)
    }

    /// **Could this closure still grow?** — the question a timeline's cache entry has to answer, and the one the
    /// walk itself cannot.
    /// # The two meanings of a file the closure does not hold
    ///
    /// [`crate::TranslationUnit::walk`] stops at a file the closure cannot offer, and its early return says why
    /// that is right: a system header nobody indexed is **outside the analysis**, and treating it as "may define
    /// anything" was measured to collapse the corpus's conditional evidence from millions of facts to tens of
    /// thousands. That reading is permanent, and a timeline built under it is the whole program.
    ///
    /// While an index is running the same absence means **not yet**: the file is in the queue and will be read.
    /// The timeline is then of *part* of the program — and the difference is visible here and nowhere else,
    /// because the queue is this type's.
    ///
    /// So a target that is missing **and queued** is a target that is coming. `queue.is_queued` is exact rather
    /// than a guard against the future: a path in the work list is a path whose summary was dropped or never built,
    /// and every one of them is read before the queue empties.
    fn closure_is_still_growing(
        &self,
        closure: &[(PathBuf, String)],
        in_closure: &HashMap<String, usize>,
    ) -> bool {
        let index = self.store.index();

        closure.iter().any(|(path, _)| {
            index.summary(path).is_some_and(|summary| {
                summary.includes.iter().any(|include| {
                    include.resolved.as_ref().is_some_and(|target| {
                        !in_closure.contains_key(&queue_key(target)) && self.queue.is_queued(target)
                    })
                })
            })
        })
    }

    /// Count one answer from [`Session::translation_unit_of`] — see [`UnitStats`] for what the three mean.
    fn note_a_unit(&self, which: Which) {
        use std::sync::atomic::Ordering::Relaxed;

        let counter = match which {
            Which::FromMemory => &self.unit_from_memory,
            Which::FromDisk => &self.unit_from_disk,
            Which::Walked => &self.unit_walked,
        };
        counter.fetch_add(1, Relaxed);
    }

    /// **What this session's translation units have cost it.** See [`UnitStats`].
    ///
    pub fn unit_stats(&self) -> UnitStats {
        use std::sync::atomic::Ordering::Relaxed;

        UnitStats {
            from_memory: self.unit_from_memory.load(Relaxed),
            from_disk: self.unit_from_disk.load(Relaxed),
            walked: self.unit_walked.load(Relaxed),
        }
    }

    /// **How many times each root's timeline has been walked**, most-walked first — see [`Session::unit_stats`] for
    /// the total this breaks down.
    ///
    /// The question it exists for: a total of two hundred walks is either one root walked two hundred times or two
    /// hundred roots walked once, and those want opposite fixes. Measured on a real project
    /// (`examples/project_index.rs`), opening one file took `walk` from 10.6 s to 45.8 s over the same 1729 files —
    /// so the walks are not spread evenly, and this says which root is paying.
    pub fn walks_by_root(&self) -> Vec<(PathBuf, u64)> {
        let Ok(walks) = self.walks_by_root.lock() else {
            return Vec::new();
        };
        let mut walked: Vec<(PathBuf, u64)> = walks
            .iter()
            .map(|(path, count)| (PathBuf::from(path), *count))
            .collect();
        walked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        walked
    }

    // **What a unit rooted at `path` is keyed on** — the file's preamble, when its text is in reach.
    //
    // A file that cannot be read is [`RootKey::WholeText`], which is the rule that can only ever be less

    /// **What makes two timelines the same timeline: the closure's content, hashed.**
    ///
    /// Used as the in-run key of [`Session::walked_by_content`]. Two roots whose closures hold the same files with
    /// the same text produce the same walk, whatever the roots are called and whatever order the includes were
    /// discovered in — so the files are hashed **by content and in a canonical order**, which is what makes the
    /// answer independent of both.
    ///
    /// The context hash is mixed in because the same closure really can walk differently under a different
    /// configuration — `-D`s, `-I`s, the standard — and that is the reason `tu_cache` keeps the two apart on disk
    /// as well.
    ///
    /// **Sorted, not in closure order**: `closure_paths` is a breadth-first discovery order, which is a property of
    /// how the graph was read rather than of what is in it. Two roots that reach the same headers through a different
    /// chain have the same closure and would otherwise hash differently — which is the whole case this exists for.
    fn closure_identity(&self, closure: &[(PathBuf, String)], context: u64) -> (u64, u64) {
        let mut contents: Vec<u64> = closure
            .iter()
            .map(|(_, text)| crate::cache::content_hash(text))
            .collect();
        contents.sort_unstable();

        // A plain FNV over the sorted hashes: this is a **key**, not a security boundary. The file count is mixed in
        // at the end for the one case a hash of a sorted list cannot separate on its own — a prefix of a list whose
        // remaining entries all hashed to zero, which is not reachable in practice but costs nothing to exclude.
        let mut identity: u64 = 0xcbf2_9ce4_8422_2325;
        for hash in &contents {
            identity ^= *hash;
            identity = identity.wrapping_mul(0x0000_0100_0000_01b3);
        }
        identity ^= (contents.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (context, identity)
    }

    /// The units table, with the lock taken — the one place the poison rule is written down.
    fn held_units(&self) -> std::sync::MutexGuard<'_, UnitTable> {
        self.units
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The macro definition cache, with the lock taken — same poison rule, see [`Session::held_units`].
    ///
    /// It is behind a lock for the same reason the units table is: this cache was what made a unit walk need
    /// `&mut self`, and `&mut self` put the walk inside the writer where every request waits for it.
    fn held_definitions(&self) -> std::sync::MutexGuard<'_, crate::MacroDefinitions> {
        self.definitions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// **Cook one translation unit into a stream, without indexing it** — the compiler's own reading of the program,
    /// as this session would parse it.
    ///
    /// This is [`Session::read_the_unit`]'s first half, and it exists as a call of its own because the reading is
    /// worth having **before** anything is done with it: it is the one artifact in this crate that can be put beside
    /// a real preprocessor's output, and that comparison is what
    /// [`crate::align`] is for. A caller that only wanted the facts calls [`Session::read_the_unit`] and never sees
    /// this; a caller checking whether the reading is *right* needs exactly this and nothing else.
    ///
    /// # What comes back
    ///
    /// Every file the unit's walk reached, stitched in include order with the branches the conditions chose already
    /// taken — [`crate::RenderedUnit`], whose `spans` say which file each token **stands in** and where in that file
    /// a consumer should act on it. Nothing is parsed and nothing is filed: the same stream, asked for twice, is the
    /// same stream.
    ///
    /// # Why the text comes through the session's overlay
    ///
    /// A file the reader has open and has **not saved** is the file that should be read — an editor's reading of a
    /// program that ignores the buffer in front of the user is a reading of some other program. A file the provider
    /// cannot read at all is simply absent, and the stream counts it as a hole
    /// ([`crate::RenderedUnit::missing`]) rather than as an empty file, because those are different programs.
    ///
    /// `None` when the file has no summary or the walk cannot be built — nothing has read it yet, which is a state
    /// and not an error. A caller with a path the index spells differently should hand over the index's own spelling;
    /// [`Session::read_the_unit`] is the one that has to do that, because it puts the result in the index.
    /// **A translation unit`s macro tables, built by folding effects** — see [`crate::effect`].
    ///
    /// This is the model`s alternative to cooking an include closure: every file in the program contributes its
    /// *effect*, an effect is a function of the few macro names the file tests, and folding them is a hash lookup
    /// per include. What it answers is the table each file was read under, which is what the second pass has been
    /// walking timelines to reconstruct.
    ///
    /// `None` when the root has no summary — nothing has read it, which is a state rather than an error.
    pub fn effect_walk(
        &self,
        root: &Path,
    ) -> Option<(HashMap<PathBuf, u64>, crate::effect::Walked)> {
        self.store.index().summary(root)?;
        let reads = SessionReads {
            files: &self.files,
            index: self.store.index(),
        };
        // **Both caches are shared between walks.** Without the surfaces every walk re-lexes every file (measured:
        // 1.05 s against the 38 ms timeline walk this would replace); without the effects every condition is
        // re-evaluated, which is the rest of it.
        let mut cache = self
            .effects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // **The effect table is bounded by count, not by bytes**: each effect is a small list of folds, and the
        // map outlives every walk, so a long session over a big project would otherwise keep one entry per
        // (file, environment) pair it ever saw — see [`crate::cache_lru::EFFECT_ENTRIES`].
        if cache.len() >= cache::cache_lru::EFFECT_ENTRIES {
            cache.clear();
        }
        let seed = crate::effect::Macros::new();
        let surfaces = self
            .surfaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (signatures, walked) = crate::effect::Chain::new(&reads, &mut cache)
            .with_surfaces(&surfaces)
            .at_signatures(root, &seed);
        Some((signatures, walked))
    }

    pub fn render_the_unit(&self, root: &Path) -> Option<crate::RenderedUnit> {
        let unit = self.translation_unit_of(root)?;
        let (sources, key) = self.the_units_inputs(&unit);

        // **The same inputs, already rendered.** A reader asks for the same unit again and again — every keystroke
        // that lands on a name, every drain — and a render is a walk, a cook of every file in include order and a
        // stitched stream of megabytes. The plan's M4 is this call: the second answer must not cost what the first
        // did.
        if let Ok(known) = self.rendered_unit.lock()
            && let Some((seen, cached)) = known.as_ref()
            && *seen == key
        {
            return Some(cached.clone());
        }

        // **The database may already have this program.** The key is a hash over every `(path,
        // content)` the closure entered, so a rendering filed under it is this rendering — and
        // reading it back is a decode instead of a cook. Measured: the cook is **899 ms** for a 1
        // 991 163-byte program, which is the whole reason a unit read could not be repeated; the
        // decode is the difference between a second unit read costing a second and costing a
        // moment.
        if let Some(cached) = self.store.cached_unit(key) {
            if crate::probes::units() {
                eprintln!("unit render: key {key:016x} served from the cache");
            }
            if let Ok(mut known) = self.rendered_unit.lock() {
                *known = Some((key, cached.clone()));
            }
            return Some(cached);
        }
        if crate::probes::units() {
            eprintln!(
                "unit render: key {key:016x} MISSED ({} source(s) to cook)",
                sources.len()
            );
        }

        let definitions = unit.definitions();
        let seed = MacroTable::from_marked(self.store.index().macros());
        // **The include search, so `__has_include` is answerable.** Without it the operator answers `Unknown`, which
        // is honest but decides nothing, and `#if __has_include(<span>)` is a shape real headers are full of, so a
        // cook that cannot answer it reads a branch on no evidence. The resolver's own search is what an `#include`
        // would use, which is what makes the two agree.
        let search = crate::preprocess::cooked::Search::new(&self.files, &self.config);
        let stream =
            { unit.cook_the_unit(&sources, &definitions, Some(&seed), true, Some(&search)) };

        // **Kept under the key, provisionally or not — the key is what makes that safe.** It folds every `(path,
        // content)` the walk entered, so a rendering of a closure that was still filling has a key the finished
        // closure cannot repeat: the row is never hit again, it is merely wasted, and a collection drops it once no
        // file references it.
        self.store.remember_unit(key, &stream);
        if let Ok(mut known) = self.rendered_unit.lock() {
            *known = Some((key, stream.clone()));
        }
        Some(stream)
    }

    /// **Does this stored rendering still describe the files the walk entered?**
    ///
    /// The same fold `the_units_inputs` makes — every file the **walk** entered (`RenderedUnit::walked`), whose text is
    /// readable, sorted by path, `path|hash;` — against the key the rendering is filed under. An edit between two asks
    /// makes the two disagree, which is a miss rather than a stale answer.
    fn the_rendering_still_describes(&self, rendered: &crate::RenderedUnit, stored: u64) -> bool {
        let mut inputs: Vec<(&std::path::Path, u64)> = Vec::new();
        for path in &rendered.walked {
            let Some(text) = self.files.read(path) else {
                return false;
            };
            inputs.push((path.as_path(), crate::cache::content_hash(&text)));
        }
        inputs.sort_unstable_by(|left, right| left.0.cmp(right.0));
        let mut folded = String::new();
        for (path, hash) in &inputs {
            folded.push_str(&path.to_string_lossy());
            folded.push('|');
            folded.push_str(&hash.to_string());
            folded.push(';');
        }
        crate::cache::content_hash(&folded) == stored
    }

    /// **The texts a unit's cook reads, and the key its rendering is filed under.**
    ///
    /// Extracted because two callers need exactly this pair and neither should compute it twice: the render
    /// ([`Session::render_the_unit`]) and the gather ([`Session::gather_a_unit_read`]).
    ///
    /// # The key is the inputs, not the root path
    ///
    /// A file the reader has open and has **not saved** comes from the overlay, so an edit changes the program without
    /// changing its name, and a cache keyed on the name would answer with a stream built from text the files no longer
    /// have. Folding every file's *content hash* in is what makes a hit mean "the same input", which is the only thing
    /// that makes reusing the output sound.
    ///
    /// Sorted, so two runs that read the same files in a different order of *this loop* still agree; the include order
    /// that matters is already fixed inside the stream by the walk.
    fn the_units_inputs(&self, unit: &crate::TranslationUnit) -> (HashMap<PathBuf, String>, u64) {
        let mut sources: HashMap<PathBuf, String> = HashMap::new();
        for path in unit.files() {
            if let Some(text) = self.files.read(path) {
                sources.insert(path.to_path_buf(), text);
            }
        }

        let mut inputs: Vec<(&std::path::Path, u64)> = sources
            .iter()
            .map(|(path, text)| (path.as_path(), crate::cache::content_hash(text)))
            .collect();
        inputs.sort_unstable_by(|left, right| left.0.cmp(right.0));
        let key = {
            let mut folded = String::new();
            for (path, hash) in &inputs {
                folded.push_str(&path.to_string_lossy());
                folded.push('|');
                folded.push_str(&hash.to_string());
                folded.push(';');
            }
            crate::cache::content_hash(&folded)
        };

        (sources, key)
    }

    /// **Everything a program read needs, gathered under the lock that is already held.**
    ///
    /// The point of this type is *where the time is spent*, and it is measured: of the 5.09 s a program read costs,
    /// the **walk and the closure's texts are 44 ms** and they are the only part that needs the session — the cook
    /// (1.25 s), the parse and the sweep (3.66 s) are a function of the unit, the texts and the configuration.
    /// [`UnitReadMaterials::read_the_program`] is that part, and it takes no session at all, so the caller may drop
    /// every lock while it runs — which is what lets a real project's program be read without a keystroke waiting.
    ///
    /// The cheap lookups happen here rather than in `read_the_program`, because they are cache reads the store owns:
    /// the rendering by the closure's key, and (when the rendering is already known) the facts by the rendering's own
    /// hash.
    ///
    /// `None` when the file has no summary or the walk cannot be built — nothing has read it yet, which is a state
    /// and not an error.
    pub fn gather_a_unit_read(&self, root: &Path) -> Option<UnitReadMaterials<SessionFiles<F>>> {
        let started = std::time::Instant::now();
        #[cfg(feature = "perf-probe")]
        crate::perf::mark("read start");
        // The index's own spelling again, for the same reason [`Session::cook`] takes it: the unit's frames are
        // keyed by the normalized path.
        let root = self.store.index().summary(root)?.path.clone();
        let context = self.store.context_hash(&root);

        // **The key without the walk, when this session has already walked this program.** The rendering records the
        // walk's own file list (`RenderedUnit::walked`), so the key it is filed under can be reproduced from the
        // rendering and checked against those files' current texts — an edit between the two asks is a miss.
        if let Some(units_key) = self.the_units_key_of(&root, context)
            && let Some(rendered) = self
                .rendered_unit
                .lock()
                .ok()
                .and_then(|known| {
                    known
                        .as_ref()
                        .filter(|(seen, _)| *seen == units_key)
                        .map(|(_, cached)| cached.clone())
                })
                .or_else(|| self.store.cached_unit(units_key))
            && self.the_rendering_still_describes(&rendered, units_key)
        {
            if crate::probes::units() {
                eprintln!(
                    "unit gather: key {units_key:016x} served without the walk in {:?} ({} walked, {} token file(s))",
                    started.elapsed(),
                    rendered.walked.len(),
                    rendered.files.len(),
                );
            }
            // **What an importer of this module sees** — recorded here as well as on the walk path, because this is
            // the path a warm session takes and the answer must not depend on which one read the unit.
            self.remember_what_the_unit_saw(&root, &rendered);
            let program = crate::cache::content_hash(&rendered.text);
            let known = self
                .unit_index
                .lock()
                .ok()
                .and_then(|memo| {
                    memo.as_ref()
                        .filter(|(seen, _)| *seen == program)
                        .map(|(_, cached)| cached.clone())
                })
                .or_else(|| self.store.cached_indexed_unit(program));
            return Some(UnitReadMaterials {
                root,
                key: SummaryKey::new(0, context),
                units_key,
                unit: None,
                sources: HashMap::new(),
                macros: self.store.index().macros().clone(),
                config: self.config.clone(),
                files: self.files.clone(),
                database: self.store.database(),
                program: Some(program),
                rendered: Some(rendered),
                known,
            });
        }

        let unit = self.translation_unit_of(&root)?;
        let walked = started.elapsed();
        let (sources, units_key) = self.the_units_inputs(&unit);
        let hashed = started.elapsed() - walked;
        let key = SummaryKey::new(0, context);

        // **The rendering, if this session or the database already has it.** Without it the program's hash is unknown
        // and neither cache can be asked — so the facts are looked up in `read_the_program`, after the cook.
        let rendered = self
            .rendered_unit
            .lock()
            .ok()
            .and_then(|known| {
                known
                    .as_ref()
                    .filter(|(seen, _)| *seen == units_key)
                    .map(|(_, cached)| cached.clone())
            })
            .or_else(|| self.store.cached_unit(units_key));
        if crate::probes::units() {
            eprintln!(
                "unit gather: key {units_key:016x} {} — walked {walked:?}, key {hashed:?} ({} file(s))",
                if rendered.is_some() {
                    "served"
                } else {
                    "MISSED"
                },
                unit.files().count(),
            );
        }

        let (program, known) = match &rendered {
            Some(rendered) => {
                let program = crate::cache::content_hash(&rendered.text);
                let known = self
                    .unit_index
                    .lock()
                    .ok()
                    .and_then(|memo| {
                        memo.as_ref()
                            .filter(|(seen, _)| *seen == program)
                            .map(|(_, cached)| cached.clone())
                    })
                    .or_else(|| self.store.cached_indexed_unit(program));
                (Some(program), known)
            }
            None => (None, None),
        };
        // **What an importer of this module sees** — the rendering's own file list, recorded where the visibility walk
        // can reach it. See `ProjectIndex::unit_closures`.
        if let Some(rendered) = &rendered {
            self.remember_what_the_unit_saw(&root, rendered);
        }

        Some(UnitReadMaterials {
            root,
            key,
            units_key,
            unit: Some(unit),
            sources,
            macros: self.store.index().macros().clone(),
            config: self.config.clone(),
            files: self.files.clone(),
            database: self.store.database(),
            program,
            rendered,
            known,
        })
    }

    /// **Record what an importer of this module sees** — the unit's whole rendering, by module name.
    ///
    /// A module is not a header, and the visibility walk cannot get this from `#include` edges: it evaluates each guard
    /// against a state that is not the unit's, so a header the unit included behind `_HAS_CXX20` — MSVC's `xstring`
    /// behind `<string>` — is dropped. Measured: `report.cppm` importing `std` saw **151 files and no
    /// `std::string`**, while `main.cpp` saw 173 only because its own `#include` pulled the same tree in textually.
    ///
    /// The rendering has already decided every one of those guards, once, for the module, so its `walked` list is the
    /// answer — recorded on the index (`ProjectIndex::unit_closures`) where the visibility walk finds it.
    fn remember_what_the_unit_saw(&self, root: &Path, rendered: &crate::RenderedUnit) {
        self.remember_the_walked_closure(root, &rendered.walked);
    }

    /// [`Session::remember_what_the_unit_saw`] from the list alone — the form a **cold** read has, because the
    /// rendering is cooked inside [`UnitReadMaterials::read_the_program`] and only the walked list comes back out.
    fn remember_the_walked_closure(&self, root: &Path, walked: &[PathBuf]) {
        if walked.is_empty() {
            return;
        }
        let Some(module) = self
            .store
            .index()
            .summary(root)
            .and_then(|summary| summary.modules.module.clone())
        else {
            return;
        };
        let files: Vec<String> = walked
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect();
        self.store.index().remember_the_unit_closure(&module, files);
    }

    /// **A unit read worked out, and nothing changed** — the expensive half, under `&self`.
    ///
    /// The whole of it: gather ([`Session::gather_a_unit_read`], which needs the session for the walk) and then read
    /// ([`UnitReadMaterials::read_the_program`], which does not). A caller that has a *lock* to keep free — the pump —
    /// calls the two apart; this is the caller that does not care.
    ///
    /// The counterpart of [`Session::commit_a_unit_reading`], and the split exists for the reason every other
    /// prepare/commit pair in this session does: a unit read is a walk, a cook of every file in include order, a
    /// **parse of the whole program** and a sweep of every declaration in it — **5.09 s** for a 159-file,
    /// 737 525-token program on a real project, and **85 ms** for the same program again.
    pub fn prepare_a_unit_reading(&self, root: &Path) -> Option<PreparedUnitReading> {
        let materials = self.gather_a_unit_read(root)?;
        // **Remember which key this root's rendering came out under** — in this session, and in the project's database,
        // so the *next* session can skip the walk too. Recorded here rather than inside `read_the_program`, which has no
        // session to write to and whose materials know the key.
        let spelling = self
            .store
            .index()
            .summary(root)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| root.to_path_buf());
        let context = self.store.context_hash(&spelling);
        let units_key = materials.units_key();
        if let Ok(mut known) = self.unit_roots.lock() {
            known.insert((spelling.clone(), context), units_key);
        }
        self.store
            .remember_unit_root(&format!("{context}:{}", spelling.display()), units_key);
        materials.read_the_program()
    }

    /// **The units key a root's rendering was filed under** — this session's memory first, the project's database
    /// second. See [`Session::unit_roots`] and `cache_db`'s `UNIT_ROOTS`.
    fn the_units_key_of(&self, root: &Path, context: u64) -> Option<u64> {
        if let Some(known) = self
            .unit_roots
            .lock()
            .ok()
            .and_then(|known| known.get(&(root.to_path_buf(), context)).copied())
        {
            return Some(known);
        }
        self.store
            .cached_unit_root(&format!("{context}:{}", root.display()))
    }

    /// Remember what a program's parse came out as — the one-entry memo `Session::unit_index` holds.
    fn remember_the_unit_index(&self, program: u64, indexed: &crate::IndexedUnit) {
        if let Ok(mut memo) = self.unit_index.lock() {
            *memo = Some((program, indexed.clone()));
        }
    }

    /// **How many items one completion may answer with** — `[completion] max_items` in `.cppls.toml`, clamped.
    ///
    /// Read per query rather than stored at assembly: the project file is re-read when it changes, and a budget
    /// captured at start-up would go on answering the old number. The clamp is `1..=20 000` — twenty thousand items is
    /// a megabyte of JSON per keystroke, and a budget of zero would answer "there is nothing here".
    fn the_completion_budget(&self) -> usize {
        self.project_config()
            .config
            .completion
            .max_items
            .unwrap_or(crate::completion::ITEM_BUDGET)
            .clamp(1, MAX_COMPLETION_ITEMS)
    }

    /// **Put what [`Session::prepare_a_unit_reading`] worked out into the index** — one `insert_cooked` per file, and
    /// the only part of a unit read that needs `&mut self`.
    pub fn commit_a_unit_reading(&mut self, prepared: PreparedUnitReading) -> UnitReading {
        #[cfg(feature = "perf-probe")]
        crate::perf::mark("before the commit");
        let PreparedUnitReading {
            root,
            program,
            indexed,
            walked,
        } = prepared;
        // **What an importer of this module sees** — the closure the cold path only had here, and the warm path also
        // records at gather time. See [`ProjectIndex::unit_closures`](crate::ProjectIndex).
        self.remember_the_walked_closure(&root, &walked);
        // **And the memo, which is the difference between a second program read costing 3.66 s and costing a clone.**
        // It is filled here rather than in `read_the_program` because the memo is the session's; a program read off
        // the lock still writes the database rows, so a session that never commits this one (a dropped batch) loses
        // the fast path for one read and nothing else.
        self.remember_the_unit_index(program, &indexed);

        // **The gate is gone, and what replaced it is a repair rather than a refusal.** A parse error is filed
        // against the file it is in; a brace the parser paired across two files is neutralised in the stream and
        // counted (`repaired`), and the file that owned it is read on its own as well — see
        // `index_unit_rendering`, which is where both happen.
        //
        // What used to be here was `if indexed.crossings == 0` around the whole loop below, and it was the single
        // most expensive rule in this crate: one mis-paired brace anywhere in a five-million-byte program left the
        // index exactly as empty as it started. That is how `std::format` came to have no members while the file
        // that declares them was in the program, and it is the failure the plan's §4 rule 1 is written against —
        // *"no layer may give up an answer it already has because it is unsure about something else."* The
        // declarations of the files that did **not** leak were never in doubt, and refusing them answered a
        // question nobody had asked.
        let mut placed = 0usize;
        if crate::probes::units() {
            let names: Vec<String> = indexed
                .files
                .iter()
                .map(|(path, _)| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default()
                })
                .filter(|name| name == "format" || name == "string" || name == "vector")
                .collect();
            eprintln!(
                "unit commit: {} file(s); interesting: {names:?}",
                indexed.files.len()
            );
        }
        for (path, cooked) in indexed.files {
            placed += 1;
            if crate::probes::units()
                && path.file_name().and_then(|name| name.to_str()) == Some("format")
            {
                let in_std = cooked
                    .declarations
                    .iter()
                    .filter(|fact| fact.name.as_str() == "format")
                    .map(|fact| fact.scope.clone().unwrap_or_default())
                    .take(4)
                    .collect::<Vec<_>>();
                eprintln!(
                    "unit commit: <format> {} declaration(s); format in {in_std:?}",
                    cooked.declarations.len()
                );
            }
            // **And the queue is told, so no per-file step reads it again.**
            self.queue.mark_worked(&path);
            // **The summary is left alone, and that is a measurement rather than an omission.** The tempting next
            // step here is "replace the file's summary declarations with the program's reading of it", and it is
            // wrong for a reason `tests/unit_read_probe.rs` shows: after this pipeline has run, a file's summary is
            // *already* a rendering — the second pass re-read every file whose declarations a directive split or a
            // body macro moved. What differs between the two lists is not raw-versus-rendering, it is **file-read-
            // on-its-own versus file-read-in-this-program**: 1 095 declarations over 177 files are in one and not the
            // other, because `_CRT_…` and `_GLIBCXX_…` answer differently when the file is compiled standalone and
            // when it is compiled inside `Selection.cpp`. So replacing one list with the other would answer for the
            // wrong translation unit — the problem §5 step two (a key with `env_hash`) exists to solve, not one a
            // rewrite can.
            // **Filed under the program that read it**, not under the file: the same header read by two translation
            // units is two readings, and a lookup asks for the asking file's one
            // (`ProjectIndex::the_reading_for`). See `Readings` in `index/project.rs`.

            self.store
                .index_mut()
                .insert_cooked_in_a_program(&path, program, cooked);
            // **A file a program read is refined.** The reading this just filed *is* the compiled one — the strongest
            // answer this crate produces — and `want_refinement` asks `is_refined`, which until now knew only about
            // the two paths that write under the file's own `env_hash` (a served record, and a per-file refinement).
            // A file from a program read therefore looked unrefined, and `index_everything`'s loop asked for every one
            // of them, which puts them in the second pass's backlog and pays a unit walk per file in `prepare_a_wave`.
            //
            // **Measured, and it did not move the module read**: marking them closed the backlog, and the two slow
            // `advance` batches of the modules fixture (49–67 s for 33 files, 44 s for none) were unchanged — so the
            // cost there is `index_one`'s own unit walk, not this backlog. The mark stays because it is true and
            // because the backlog it removes is real work for a drain that would otherwise walk files it has just
            // read; the batch cost is P6's next measurement, not this line's.
            if let Ok(mut refined) = self.refined_readings.lock() {
                refined.insert(path.clone());
            }
        }

        UnitReading {
            root,
            files: placed,
            declared: 0,
            tokens: indexed.tokens,
            files_with_tokens: indexed.files_with_tokens,
            missing: indexed.missing,
            unplaced: indexed.unplaced,
            unbalanced: indexed.unbalanced,
            braces: indexed.braces,
            errors: indexed.errors,
        }
    }

    /// **Read one translation unit as a program, and file its facts under every file it read.**
    ///
    /// # What this is for
    ///
    /// Everything else in this session reads a **file**: its own text, its own closure, its own environment. This is
    /// the one call that reads a **program** — the closure stitched into one stream in include order
    /// ([`crate::TranslationUnit::cook_the_unit`]), parsed **once**
    /// ([`crate::FileIndexer::index_unit_rendering`]) — and hands every declaration it finds to the file it was
    /// written in.
    ///
    /// That is what makes the index hold "what the compiler saw" for files **nobody has opened**: without it a
    /// header's cooked facts exist only for the files some request happened to name, so a cross-file answer depends
    /// on whether a reader had looked at the header yet.
    ///
    /// # The two halves, and where they are paid
    ///
    /// [`Session::prepare_a_unit_reading`] does the work and takes `&self`;
    /// [`Session::commit_a_unit_reading`] is the inserts. This function is the two together, for a caller that
    /// already holds the write lock and does not care — a test, a batch, `read_a_looked_at_unit`. The pipeline pays
    /// them apart, so that the five seconds a program takes to read does not hold the lock a keystroke waits for.
    ///
    /// # Why it was **not** wired into the pump until now
    ///
    /// Because an earlier attempt made three readings worse and could not say why (`declarations_in("std")` 2008 →
    /// 1959, and two `definition` counts). `tests/unit_read_probe.rs` re-measured that on a real project and it does
    /// not reproduce: 159 files, 737 525 tokens, **zero** errors, 39 080 cooked declarations added, and not one raw
    /// fact lost. The cause was the one `index_unit_rendering`'s crossing fence now quarantines — a file whose
    /// unclosed scope used to swallow the rest of the program.
    pub fn read_the_unit(&mut self, root: &Path) -> Option<UnitReading> {
        let gathering = std::time::Instant::now();
        let prepared = self.prepare_a_unit_reading(root)?;
        let gathered = gathering.elapsed();
        let committing = std::time::Instant::now();
        let reading = self.commit_a_unit_reading(prepared);
        if crate::probes::units() {
            eprintln!(
                "unit read: prepare {gathered:?} (walk + cook + parse), commit {:?} ({} file(s) filed)",
                committing.elapsed(),
                reading.files
            );
        }
        Some(reading)
    }

    /// **Is a whole program going to be read from this drain?** — the one rule that keeps a unit read off the path
    /// a keystroke takes.
    ///
    /// The split between [`Session::prepare_a_unit_reading`] and [`Session::commit_a_unit_reading`] moves the cost
    /// off the *write* lock, and that is not enough on its own: the preparing half holds the **read** lock for five
    /// seconds, and a `didChange` takes the write lock — so a reader typing into the file being read would wait for
    /// the whole program. Nothing in a session may start one while somebody is typing, and this is what says so.
    ///
    /// Two conditions, and the second is what makes it testable rather than merely polite:
    ///
    /// * **a reader has the file open** — a project nobody is looking at is not worth a program (`read_the_unit` is
    ///   still the API for a caller that wants one, and `read_a_unit_of_the_project` for a caller that wants all);
    /// * **no edit for [`UNTIL_A_UNIT_MAY_BE_READ`]** — the instant [`Session::buffer_changed`] records. A person
    ///   typing defers it indefinitely, which is the point: the reading is a *comfort* that makes the whole closure
    ///   queryable, and typing is the thing the session exists to answer.
    fn a_unit_may_be_read_now(&self) -> bool {
        self.last_edit.elapsed() >= UNTIL_A_UNIT_MAY_BE_READ
    }

    /// **Move the editing clock back**, so that a test can ask about a schedule that waits ten seconds without
    /// sleeping for them — the two thresholds in this file are a person's typing rhythm, and a test that waited for
    /// the longer one would be a test nobody runs.
    #[cfg(test)]
    pub(crate) fn pretend_a_pause(&mut self, for_how_long: std::time::Duration) {
        self.last_edit = std::time::Instant::now()
            .checked_sub(for_how_long)
            .unwrap_or_else(std::time::Instant::now);
    }

    pub fn the_wait_before_a_program_may_be_read(&self) -> Option<std::time::Duration> {
        // **One clock.** An open file becomes readable one second after the reader stops typing, and nothing else is
        // read as a program at all — see [`UNTIL_A_UNIT_MAY_BE_READ`].
        self.the_next_unit_to_read()
            .map(|_| UNTIL_A_UNIT_MAY_BE_READ.saturating_sub(self.last_edit.elapsed()))
    }

    /// **The next file whose whole program is worth reading** — an open file the program of which nobody has read,
    /// whose program is small enough that reading it cannot make a keystroke wait
    /// ([`THE_LARGEST_PROGRAM_READ_AT_ONCE`]), and — the clock — which nothing has been typed into for a moment.
    ///
    /// Open files only, in the order the client sent them: a project with two hundred translation units does not
    /// read two hundred programs because one file is open. `units_read` is what makes it once per program, and it is
    /// keyed by the root rather than by the program's bytes because the root is what the schedule iterates.
    ///
    /// A file that is open but **over budget** is never marked read, so it is re-considered on every drain — that is
    /// a closure walk of hash lookups (the texts are held), and it is what lets the program be read later if the file
    /// is edited down or the configuration shrinks it. The alternative — marking it read — would make the budget
    /// permanent for a file that merely started large.
    ///
    /// [`Session::the_next_unit_to_read`] is the same question without the clock, for a caller that is itself the
    /// signal that now is a good moment (`Session::read_a_looked_at_unit`).
    pub fn the_program_to_read(&self) -> Option<PathBuf> {
        // **Open files only.** A project with two hundred translation units does not read two hundred programs because
        // one file is open — the files nobody has open keep the reading their own text gives them, which is what
        // `clangd` does too: it parses a unit when a file is opened, not for the whole project in the background.
        let open = self.the_next_unit_to_read()?;
        self.a_unit_may_be_read_now().then_some(open)
    }

    /// The root a program read would be for, ignoring the clock — see [`Session::the_program_to_read`].
    fn the_next_unit_to_read(&self) -> Option<PathBuf> {
        // **The module interface units first, then the open documents.** A module is a *dependency* of the files that
        // import it: until `bits/std.cc` has been read as a translation unit, every header in its closure answers from
        // its raw summary, and an `import std;` file's completions are wrong for as long as that lasts. They are
        // units of the project too, and they are nobody's open document.
        let mut wanted: Vec<PathBuf> = self
            .programs_wanted
            .iter()
            .filter(|key| !self.units_read.contains(*key))
            .map(PathBuf::from)
            .collect();
        wanted.sort();
        if let Some(unit) = wanted.into_iter().next() {
            // The size budget does not apply to a module unit: its closure is the standard library and is larger than
            // the budget by construction, so refusing it is refusing the module.
            return Some(unit);
        }

        let open = self.documents.paths().into_iter().find(|path| {
            !self.units_read.contains(&queue_key(path))
                && !self.the_program_is_too_large_to_read_at_once(path)
                // **A file that imports a module is not read as a program.** Its `import std;` is not an `#include`:
                // the unit walk would read the file *alone* — measured, 1 file and 33 tokens — and replace its
                // rendering with one that has no `std` in it at all, after which every completion in the file is
                // empty. The module's own unit is what supplies those declarations, and it is read instead
                // (`programs_wanted`, above); the file keeps the reading its own text gives it, with the module's
                // facts reached through `module_interfaces`.
                && self
                    .store
                    .index()
                    .summary(path)
                    .is_none_or(|summary| summary.modules.imports.is_empty())
        });
        if open.is_some() {
            return open;
        }

        // **Then the module interface units**, which are units of the project too and are nobody's open document. The
        // size budget does not apply to them: it exists to keep a *keystroke* from waiting for a program the reader is
        // not looking at, and a module unit's read is what `import std;` answers from — `bits/std.cc`'s closure is
        // larger than the budget by construction (it is the whole standard library), so refusing it is refusing the
        // module.
        let mut wanted: Vec<PathBuf> = self
            .programs_wanted
            .iter()
            .filter(|key| !self.units_read.contains(*key))
            .map(PathBuf::from)
            .collect();
        wanted.sort();
        wanted.into_iter().next()
    }

    // **The next translation unit of the project nobody has read** — gone.
    //
    // `clangd` does not do this either: it parses a translation unit when a file is opened or asked
    // about. A file nobody has open keeps the reading its own text gives it, and the reader's own
    // files still get the compiler's reading one second after they stop typing.
    //
    // What is lost is only ⑲'s widest case — the same header read two ways by two programs *neither of which is
    // open*. Everything a reader can look at keeps its per-program reading.

    /// **Is this program over [`THE_LARGEST_PROGRAM_READ_AT_ONCE`]?** — asked before the read, so a keystroke never
    /// waits for one that is too big.
    ///
    /// The size is the source text of the closure, which is what the read is linear in and what is cheap to know: the
    /// walks the index already did give the file list, and the VFS is holding the texts (a file it is not holding is
    /// counted as empty here and read by the walk itself). A file the index cannot describe has no closure and is
    /// therefore not too large — the read will fail on its own terms.
    fn the_program_is_too_large_to_read_at_once(&self, root: &Path) -> bool {
        let bytes: usize = self
            .closure_paths_from(root, None)
            .iter()
            .map(|path| self.text(path).map_or(0, |text| text.len()))
            .sum();
        bytes > THE_LARGEST_PROGRAM_READ_AT_ONCE
    }

    /// **The project's translation units, read once each** — what the pump calls when its queue drains.
    ///
    /// One per call rather than all of them, for the reason the pump is sliced at all: a unit read is a walk, a
    /// render and a parse, and a session that did a hundred of them in one call would hold the writer for as long
    /// as a compiler takes to build the project. [`Session::pending_work`] counts the ones still unread, so a
    /// caller looping on it keeps coming back.
    pub fn read_a_unit_of_the_project(&mut self) -> Option<UnitReading> {
        let root = self
            .project
            .iter()
            .find(|root| !self.units_read.contains(&queue_key(root)))
            .cloned()?;

        let reading = self.read_the_unit(&root);
        self.units_read.insert(queue_key(&root));
        reading
    }

    /// **A unit of one file a reader is looking at**, read as a program — one per call.
    ///
    /// The policy is [`Session::cook`]'s: a reading is built when something looks at the file. What changed is the
    /// **unit** the reading is built for — not "this file and its direct includes", but the whole translation unit
    /// the file is read in, parsed once, with every declaration in it filed under the file it was written in.
    ///
    /// One per call, like every other step here, and bounded by the number of *open* files rather than by the
    /// project: a project with two hundred translation units does not read two hundred programs because one file is
    /// open. [`Session::read_a_unit_of_the_project`] is the caller that wants all of them, and the pipeline's own
    /// schedule is [`Session::a_unit_may_be_read_now`] — this is the caller that says "now" regardless of the clock.
    /// **Is a module unit still waiting to be read as a program?** — the question a request asks before deciding that
    /// there is nothing to do.
    ///
    /// A unit the index already *names* is not a unit the index has *read*: the pump's queue puts it in the project and
    /// asks for a cook, and the program read is a separate step. A caller that skipped the step because the module was
    /// named answered a hover from the raw summaries — measured, "`std::shared_ptr<std::string>` has no member
    /// `c_str`" while the completion at the same cursor offered 187 of `std::string`'s members.
    pub fn a_module_unit_is_waiting_for_its_program(&self) -> bool {
        self.programs_wanted
            .iter()
            .any(|key| !self.units_read.contains(key.as_str()))
    }

    /// **Which class a `.` or `->` written against a type reaches** — the completion's own reader, for a caller that
    /// has a type and needs the class whose members the operator names.
    ///
    /// The hover used to ask `members_of` about the *written* type, so `ptr->c_str` on a `std::shared_ptr<std::string>`
    /// looked `c_str` up in `shared_ptr` — measured, "`std::shared_ptr<std::string>` has no member `c_str`" while the
    /// completion at the same cursor offered 187 of `std::string`'s members. One reader for the question is what makes
    /// the two agree; see [`crate::completion::the_class_after_the_operator`].
    pub fn the_class_after_the_operator(
        &self,
        view: &crate::FileView,
        written: &str,
        arrow: bool,
    ) -> Option<String> {
        let written = crate::sema::types::parse_type_spelling(written);
        // **The pointee, by `element_type` with the type's own arguments substituted** — `shared_ptr<Node>` reaches
        // `Node`, where the completion's resolver answered `std::shared_ptr` because `element_type` is written `_Tp`. See
        // [`crate::index::project::the_pointee_of_a_handle`]; the resolver below stays for a raw pointer and for an
        // `operator->` whose return type is a class.
        if arrow && let crate::sema::types::Type::Named { name, arguments } = &written {
            // **The class as the index files it.** A member is recorded under the class's qualified name, and a type
            // deduced from a `std::` function is spelled without the namespace: `make_shared` answers
            // `shared_ptr<Node>` while the class is `std::shared_ptr`, whose `element_type` is the member being read.
            let class = match name.contains("::") {
                true => name.clone(),
                false => crate::index::project::qualified_from_the_index(
                    self.store.index(),
                    &view.path,
                    name,
                )
                .unwrap_or_else(|| name.clone()),
            };
            if let Some((pointee, _)) = crate::index::project::the_pointee_of_a_handle(
                self.store.index(),
                &view.scopes,
                &view.root,
                &view.path,
                &class,
                arguments,
            ) {
                return Some(pointee);
            }
        }
        crate::completion::the_class_after_the_operator(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            &written,
            arrow,
        )
        .map(|(class, _)| class)
    }

    /// **Read one queued module unit as a program, now** — the compiler's reading of a module, for a caller that is
    /// about to answer a question the module supplies a name for.
    ///
    /// A request cannot wait for the pump: a hover on `ptr->c_str` in a file that says `import std;` arrived before the
    /// unit had been read as a program, and the member list of `std::shared_ptr<std::string>` was built from the
    /// closure's **raw** headers — measured, the hover said "`std::shared_ptr<std::string>` has no member `c_str`"
    /// while the completion at the same cursor offered 187 of `std::string`'s members. The unit read is what makes the
    /// two agree, and it is a one-off per session: `units_read` keeps the pump from doing it again.
    ///
    /// `false` when nothing is queued, so a caller can loop until the queue is empty.
    pub fn read_a_module_unit(&mut self) -> bool {
        let Some(key) = self
            .programs_wanted
            .iter()
            .find(|key| !self.units_read.contains(*key))
            .cloned()
        else {
            return false;
        };
        let path = std::path::PathBuf::from(&key);
        // **The whole closure must be summarised before the program is read.** A walk over files the index has not read
        // contributes what it can — and then the reading is filed and marked read, so the *partial* program reading
        // shadows every summary from then on. Measured: in the editor `definitions("std::println", demo.cpp)` answered
        // **0** while the same query in a session that indexed the closure first answered **6**; the difference was
        // exactly this — the request path read `bits/std.cc` before its 400 headers had summaries, so `<print>`'s body
        // was missing from the reading the query then used.
        //
        // The queue is what reads them: `want_the_closure_cooked` walks the includes and queues them, and the drain is
        // the same one the request path already pays for a module (measured: 1.7 s of summaries for `std`, once).
        if self.store.index().summary(&path).is_none() {
            self.load(&path);
        }
        self.want_the_closure_cooked(&path);
        let mut rounds = 0;
        while self.pending() > 0 && rounds < 2_000 {
            self.advance(256);
            rounds += 1;
        }
        let reading = self.read_the_unit(&path);
        // **Marked read only when it was.** A reading that could not be built has to stay retryable, or the unit keeps
        // its raw summaries for the rest of the session.
        if reading.is_some() {
            self.units_read.insert(key);
        }
        reading.is_some()
    }

    pub fn read_a_looked_at_unit(&mut self) -> Option<UnitReading> {
        let root = self.the_next_unit_to_read()?;

        let reading = self.read_the_unit(&root);
        // **A reading that could not be built is retried, not marked done.** This used to mark the root read whatever
        // happened, and for a module unit that was fatal: the unit's summary is queued rather than read when the pump
        // first names it, so the first program read found nothing, marked it read, and the whole closure kept its raw
        // summaries for the rest of the session — measured as a hover that said "`std::shared_ptr<std::string>` has no
        // member `c_str`" while the completion at the same cursor offered 187 of `std::string`'s members. Three
        // attempts and it is treated as unreadable, which is the policy `commit_a_program` already had.
        let key = queue_key(&root);
        let attempts = self.units_attempted.entry(key.clone()).or_insert(0);
        *attempts += 1;
        if reading.is_some() || *attempts >= 3 {
            self.units_read.insert(key);
        }
        reading
    }
    /// **Queue the interface units of the modules the open files import**, for the pump to read like any other file.
    ///
    /// The counterpart of [`Session::read_the_modules_a_file_imports`], and the difference is who pays. That one
    /// reads the whole closure **now**, holding the write lock from the first interface unit to the last file it
    /// drags in — measured at **6 151 ms** for `std` on a cold machine. A request that is already waiting on an
    /// answer can afford that; the pump cannot, because its design is that the write lock is held for milliseconds at
    /// a time — and a *diagnostic* cannot either, because a diagnostic is a read and must not mutate the index.
    ///
    /// So this only **queues**: the unit lands in the project and asks to be cooked, and the ordinary slices read it
    /// with the budget the loop already applies. It is called from the pump when the queue has drained, so the files
    /// it adds are read before the end-of-drain refresh rather than after it.
    ///
    /// `std` is the ordinary case and the reason this exists: libstdc++ ships its interface unit as
    /// `<c++ include>/bits/std.cc`, MSVC as `<VC>/Tools/MSVC/<version>/modules/std.ixx`, and neither is part of any
    /// include closure — nothing in the project includes it, so nothing would ever read it on its own.
    ///
    /// Returns how many units were queued, so the caller can go round the loop again instead of publishing an answer
    /// computed before they were read.
    pub fn queue_the_modules_the_open_files_import(&mut self) -> usize {
        let mut queued = 0;
        for path in self.documents.paths() {
            let wanted = self.modules_the_index_cannot_name(&path);
            if wanted.is_empty() {
                continue;
            }

            for unit in self.module_interface_units_of(&wanted, &path) {
                // **Cooked as well as read**: the names a module exports are in its *rendering* — `bits/std.cc`
                // opens with `#include <bits/stdc++.h>` — and a summary of the raw text is the reading without a
                // macro expansion, which for a module source is the reading without most of the names.
                self.add_project_files([unit.clone()]);
                // **And a translation unit, so it is read as one.** The per-file cook beside this is the cheap half — it
                // renders the unit's own text, with `#include` as a boundary — and it is what makes the names appear
                // quickly; the *program* read is what makes them **right**, because it renders every header in the
                // closure inside this unit's environment. Recorded rather than run: the pump reads a unit at its own
                // budget (`the_next_unit_to_read`).
                self.programs_wanted.insert(queue_key(&unit));
                self.want_cooked_reading(&unit);
                queued += 1;
            }
        }
        queued
    }

    /// **Queue the interface units of the modules `path` imports** — the cheap half of
    /// [`Session::read_the_modules_a_file_imports`], for a caller on a request path.
    ///
    /// The read that method does is the **whole closure**, and it is not small: measured on a real project
    /// (`test_module`, `src/std_module_demo.cpp`, `import std;`), it held the analysis's write lock for **1 717 ms**
    /// — `bits/std.cc` → `bits/stdc++.h`, four hundred files — and every diagnostic pull behind it was answered
    /// **1.4 s** late by an index with nothing left to do.
    ///
    /// A query may not spend that. It does not have to: the unit only has to be **queued**, and the pump reads it in
    /// slices under the budget it applies to every other file. The answer to the query that asked is then "not yet",
    /// which is what the protocol's `isIncomplete` is for and is true.
    ///
    /// Returns how many units were queued.
    pub fn queue_the_modules_a_file_imports(&mut self, path: &Path) -> usize {
        // **`[index] modules = false` means exactly that** — no unit read, and therefore no minutes of background CPU
        // after a cache invalidation. A project that turned it off gets no `std` from `import std;`, which is the
        // trade it asked for, and the queries that would have used it answer "not read" instead of a fragment.
        if self.project_config().config.index.modules == Some(false) {
            return 0;
        }
        let wanted = self.modules_the_index_cannot_name(path);
        if wanted.is_empty() {
            return 0;
        }

        let found = self.module_interface_units_of(&wanted, path);
        for unit in &found {
            // The project list is what the pump's scan reads; the cooking queue is what makes the *rendering* be
            // built, and a module source's names are in its rendering (`bits/std.cc` writes `#include
            // <bits/stdc++.h>`, and its `export namespace std { using std::… }` blocks after it).
            self.add_project_files([unit.clone()]);
            self.want_cooked_reading(unit);
            // **And a translation unit, read as one.** The cook above renders the text of the unit itself only
            // (`#include` is a boundary), so the library's headers keep their raw summaries — and a raw summary of
            // `shared_ptr_base.h` files `__shared_ptr_access` as a nested class of `_Sp_counted_ptr_inplace`. The
            // request path reads it as a program (`read_a_module_unit`) before answering, because a hover that arrives
            // first says "no member" about a name the completion at the same cursor offers.
            self.programs_wanted.insert(queue_key(unit));
            // **And queued for the index**, without which nothing ever reads the unit at all: `add_project_files`
            // only records that the file is part of the project, and a file nothing queues keeps no summary — so the
            // pump's program read declined three times and marked the unit unreadable, and `ptr->` stayed at the raw
            // list. The read-now variant queues it the same way.
            self.queue.again(unit.clone(), Priority::Open, 0);
        }
        found.len()
    }

    /// **The module interface units `path` imports, read in** — the files behind `import m;` that are not part of
    /// the project.
    ///
    /// `import mathlib;` reaches `mathlib.ixx` because the project scan indexed it: the visibility walk holds
    /// summaries, and a summary of the *importing* file names a module rather than a file, so the index has to know
    /// which file declares that module. That is `ProjectIndex::module_interfaces`, and it is filled from the files
    /// the project took in — so a module whose interface unit is **outside the project** is a module nothing can see.
    ///
    /// # The case that makes this concrete, and the measurement behind the design
    ///
    /// `import std;` (C++23). MSVC ships the *source* of `std` as `<VC>/Tools/MSVC/<version>/modules/std.ixx` —
    /// 3 194 bytes of `export module std;` followed by an `#include` of every standard header — so the answer is a
    /// file this analysis can read like any other interface unit, and nothing about it is special except that it is
    /// not in the project. Measured on the fixture `tests/fixtures/modules/std_only.cpp` (a file whose only import is
    /// `import std;`), before this existed: `std::string`, `std::vector` and `std::cout` all `None`, and the
    /// completion after `std::` empty. Measured after it, on the same fixture: the **first** call reads 401 files in
    /// **6 151 ms** (nothing cached, which is the first time on a machine), the warm call is **547.7 ms**, and a call
    /// with everything already in the index reads 0 files in **0.004 ms** — `std::string` resolves to `<xstring>` and
    /// `std::cout` to `<iostream>`.
    ///
    /// # Why it is a call and not a step of the pump
    ///
    /// Six seconds of indexing is not work a language server may do for every workspace it opens, and it is not work
    /// to do when nobody asks for a name from `std`. So it is a **request-driven** read, like
    /// [`Session::read_the_unit`]: the caller that is about to answer a name query on `path` says so, and pays for it
    /// once — the next call finds the interface unit already in the index and does nothing.
    ///
    /// # Recursion is a queue, not a stack
    ///
    /// Reading a module in is not reading **one file**. MSVC's `std.ixx` is 3 194 bytes whose entire body is an
    /// `#include` of every standard header, and *those* are where the names are, so a call that stopped at the
    /// interface unit would answer `None` for `std::string` with the module read — measured, and the reason the
    /// interface units are queued in the **open** half and the queue is drained after them rather than left to the
    /// pump.
    ///
    /// `MAX_MODULES_READ_AT_ONCE` bounds the rounds rather than the files: an interface unit that imports a module
    /// whose interface unit imports it back is a cycle, and a cycle terminates on its own (a path already in the
    /// index is never read again) — so the cap is for the graph that is merely large, and the honest answer for the
    /// rest is "not read yet".
    ///
    /// Returns how many interface units were read in, so a caller can say whether anything changed.
    pub fn read_the_modules_a_file_imports(&mut self, path: &Path) -> usize {
        let mut read_in = 0;
        // **Where this call's seconds go**, under the same switch the unit trace uses — the question it exists for is
        // "which of the four phases is the one that costs a minute", and no single number outside can answer it: the
        // same call measured 25 s, 44 s, 112 s and 506 s on unchanged inputs, against a 24 MB cache, so the answer is
        // not the cache and not the closure.
        let tracing = crate::probes::units();
        let started = std::time::Instant::now();
        let mut last = started;

        for _ in 0..MAX_MODULES_READ_AT_ONCE {
            // The module names the index cannot point at a file for, from the file itself and from every file the
            // previous round brought in — a fresh walk each round, because a round is a handful of hashmap lookups
            // and the alternative is a worklist that has to be kept in step with the index.
            let wanted = self.modules_the_index_cannot_name(path);
            if tracing {
                eprintln!(
                    "module read: wanted {:?} after {:?}",
                    wanted,
                    last.elapsed()
                );
                last = std::time::Instant::now();
            }

            if wanted.is_empty() {
                break;
            }

            let found = self.module_interface_units_of(&wanted, path);
            if tracing {
                eprintln!(
                    "module read: resolved {} file(s) {:?} after {:?}",
                    found.len(),
                    found,
                    last.elapsed()
                );
                last = std::time::Instant::now();
            }
            if found.is_empty() {
                break;
            }

            read_in += found.len();
            self.add_project_files(found.clone());

            for unit in found {
                // **The interface unit is a translation unit, so it is read the way a compiler reads one.** This used
                // to queue the unit for a *per-file* cook and drain the index behind it — and a per-file cook renders
                // one file with `#include` as a boundary, so every header in the closure kept its **raw** summary,
                // where both branches of every `#if` are present in the text at once and no compiler would ever see
                // that stream.
                //
                // Measured on `shared_ptr_base.h` read raw: `_Sp_counted_ptr_inplace`'s class body never closes —
                // the class head is written three times under `#if`/`#else` — so `__shared_ptr_access`, hundreds of
                // lines later, is filed as a *nested* class of it, and every member of the real `__shared_ptr_access`
                // is unreachable: `std::shared_ptr<std::string>::operator->` cannot be found, and a completion after
                // `ptr->` offers the `shared_ptr`'s own four names instead of `std::string`'s sixty-eight.
                //
                // `read_the_unit` is the compiler's reading: it walks the closure, renders each file **in this
                // unit's environment** (`#if` decided once, macros expanded), parses that once, and files every file's
                // declarations from it — `insert_cooked_in_a_program`, which is the reading a query about that file
                // asks for (`ProjectIndex::the_reading_for`). Its cost is the closure walk, which this call already
                // paid: **6 151 ms** for `std` on a cold machine, 547.7 ms warm.
                //
                // It is *recorded* here rather than run here because the walk needs the unit's summary, which is
                // queued below and read by the drain — the first round would find nothing. `the_next_unit_to_read`
                // hands it to the pump as a program like any other, which is also what keeps a module's six seconds
                // out of the write lock a request is waiting on.
                self.programs_wanted.insert(queue_key(&unit));
            }

            // **Everything this made reachable, read** — the interface units, the headers they include, and the
            // second pass over the files whose scopes came out of a macro body in one of those headers, which is
            // what `advance` runs when the queue drains (`std::basic_string` is spelled `basic_string` at file scope
            // until `yvals_core.h` has been read).
            //
            // The whole queue, not only these files: `Session::advance` takes the front of the open half and the
            // open half is the project, so a drain that spared the rest would leave the module half-read. The cost is
            // the same work the pump was going to do anyway, moved to the moment a reader asked for an answer that
            // needs it.
            self.index_everything();

            // **And then the module's own unit, read as the program it is** — the step that was only ever *recorded*.
            //
            // The comments above explain at length why the unit is queued rather than read here: the walk needs the
            // summaries the drain is about to read, and a module's six seconds do not belong under a request's write
            // lock. Both are true, and both are about *the pump*: a session with a work loop will pick this up on its
            // next slice. A caller that has no pump — a batch, a test, this probe — asked for `import std;` and gets
            // headers and no module.
            //
            // Measured before this: the module read resolves `std.ixx`, queues it, drains 401 files, and answers
            // **0** of the module's exports — `std::format` is not offered after `import std;`, and reading three
            // *other* programs first does not change that (`probe: reading programs does not make the standard
            // module's exports visible`). The unit read is the missing step: `read_the_unit` walks the closure, renders
            // each file in the unit's environment, parses it once and files every declaration from it
            // (`insert_cooked_in_a_program`), which is where `export namespace std { using std::format; … }` lands.
            //
            // It costs what the comments above say — 6.2 s for `std` on a cold machine, 0.55 s warm — and it is paid
            // **here, after the drain**, so the summaries the walk needs are all in the index by the time it starts.
            let due: Vec<PathBuf> = {
                let mut due: Vec<PathBuf> = self
                    .programs_wanted
                    .iter()
                    .filter(|key| !self.units_read.contains(*key))
                    .map(PathBuf::from)
                    .collect();
                due.sort();
                due
            };
            for unit in due {
                let reading = std::time::Instant::now();
                let read = self.read_the_unit(&unit);
                if tracing {
                    eprintln!(
                        "module read: unit {} in {:?} ({} file(s))",
                        unit.display(),
                        reading.elapsed(),
                        read.map_or(0, |reading| reading.files),
                    );
                }
            }
        }

        read_in
    }

    /// **The module names reachable from `path` that the index cannot point at a file for** — the list
    /// [`Session::read_the_modules_a_file_imports`] reads in, and the question
    /// [`Session::imports_a_module_that_is_not_read`] asks.
    ///
    /// One implementation of both, because they are the same question: the caller that wants to know whether there
    /// is work must not be able to disagree with the caller that does it.
    fn modules_the_index_cannot_name(&self, path: &Path) -> Vec<String> {
        self.modules_in_view_of(path)
            .into_iter()
            .filter(|module| self.store.index().interface_unit_of(module).is_none())
            .collect()
    }

    /// **Would [`Session::read_the_modules_a_file_imports`] read anything?** — the question a caller asks *before*
    /// taking the write lock.
    ///
    /// The call is a **request-driven** read precisely so that it happens once: the second time, the interface unit
    /// is in the index and the answer is "nothing to do" — measured at **0.004 ms** for the work. What is not
    /// 0.004 ms is the write lock it takes to discover that, and the pump holds the session for read while it parses
    /// an include closure.
    ///
    /// So a file that imports nothing — which is nearly every file — asks this, gets `false`, and never queues for
    /// the lock at all. A file that does import a module pays the lock exactly once, which is the call's whole point.
    pub fn imports_a_module_that_is_not_read(&self, path: &Path) -> bool {
        !self.modules_the_index_cannot_name(path).is_empty()
    }

    /// **Which file each of these module names is declared in** — resolved the way `scan_imports` resolves them.
    /// The naming convention, the importing file's own directory, and each include path's sibling `modules`
    /// directory (which is where a standard module lives: `<VC>/Tools/MSVC/<version>/modules/std.ixx` is *beside* the
    /// include directory, not inside it). Going through [`ModuleScanner`] rather than a second implementation is what
    /// keeps "which file is module `m`" a question with one answer in this crate.
    ///
    /// A file already in the index is dropped here: the caller is asking because the index cannot name a file for the
    /// module, and a candidate that the index *does* hold a summary of would mean the map and the summaries disagree.
    fn module_interface_units_of(&self, wanted: &[String], importing: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = Vec::new();
        let mut interner = PathInterner::new(self.files.is_case_insensitive());
        let mut scanner = ModuleScanner::new(&self.files, &self.config);

        // **The directory, not the file.** `ModuleScanner::resolve_module`'s naming convention
        // looks for a module "beside the importing file", which it does by joining the name onto
        // the directory it is given — and this call passed the importing *file*, so every candidate
        // became `<dir>/main.cpp/math.cppm`, which is not a path any filesystem has. The result was
        // not an error but a silence: `read_the_modules_a_file_imports` answered **0** for every
        // file, `imports_a_module_that_is_not_read` stayed true for ever, and a request in a module
        // file therefore took the session's write lock on **every keystroke** to discover that
        // there was nothing to read. The other two callers of this resolution (`scan_imports`,
        // `module_reading`) already pass a directory, which is why the standard-module tests
        // passed: their module is found through an include path's sibling `modules/` directory, and
        // the broken first entry was never the one that answered.
        let directory =
            crate::paths::parent_normalized(importing, self.files.is_case_insensitive());

        for module in wanted {
            if let ImportOutcome::Resolved(unit) =
                scanner.resolve_module(module, &directory, &mut interner)
                && let Some(entry) = scanner.units().iter().find(|entry| entry.file == unit)
            {
                found.push(entry.path.clone());
            }
        }

        found.retain(|unit| self.store.index().summary(unit).is_none());
        found.sort();
        found.dedup();
        found
    }

    /// **Every module name reachable from `path` through `import`s** — the file's own, and those of every file that
    /// came into view because of one.
    ///
    /// The same walk the visibility rules use ([`ProjectIndex::visible_files_with_modules`]) would be the wrong
    /// question here: what this needs is not "which files are in view" but "which module names might name a file
    /// nobody has read", and that is answered by following the import edges from summaries the index holds.
    fn modules_in_view_of(&self, path: &Path) -> Vec<String> {
        let mut wanted: Vec<String> = Vec::new();
        let mut pending: Vec<PathBuf> = vec![path.to_path_buf()];
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some(current) = pending.pop() {
            if !seen.insert(queue_key(&current)) {
                continue;
            }

            let Some(summary) = self.store.index().summary(&current) else {
                continue;
            };

            for imported in &summary.modules.imports {
                if !wanted.iter().any(|held| held == imported.as_ref()) {
                    wanted.push(imported.to_string());
                }

                // …and the file that module is declared in, when the index knows it: a module brought in by a
                // module is as much a file nobody has read as one the file wrote itself.
                if let Some(interface) = self.store.index().interface_unit_of(imported) {
                    pending.push(PathBuf::from(interface));
                }
            }

            // **A module implementation unit implicitly imports its own interface unit**, so `module math;` alone is
            // a reason to read `math.cppm` — the visibility walk crosses the same edge. Without this the file is
            // never read in, because it imports nothing: measured on `test_module/src/math.cpp`, whose every name
            // resolved to nothing until the interface unit was in the index.
            if !summary.modules.is_interface
                && summary.modules.partition.is_none()
                && let Some(module) = &summary.modules.module
                && !wanted.iter().any(|held| held == module.as_ref())
            {
                wanted.push(module.to_string());

                if let Some(interface) = self.store.index().interface_unit_of(module) {
                    pending.push(PathBuf::from(interface));
                }
            }
        }

        wanted
    }

    /// How many units of **looked-at** files have not been read as programs yet.
    ///
    /// # Why this is **not** part of [`Session::pending_work`]
    ///
    /// It was, for one round, and that was a **hang**: a counter of work is a promise that the pump will do it, and
    /// the only step that marks a unit read ([`Session::read_a_looked_at_unit`]) is not wired into the pump — so
    /// `pending_work` never reached zero and [`Session::index_everything`]'s `while pending_work() > 0` spun at full
    /// speed for ever, doing nothing each round. A test binary pegged a core and had to be killed.
    ///
    /// The rule that follows is the one this type already learned once (§8's "a queue that exists for one path must
    /// be invisible to the other"): **a work counter may only count work the pump performs.** A question about
    /// something the pump does not do is a *question*, and it is asked by name.
    pub fn unread_units(&self) -> usize {
        self.documents
            .paths()
            .iter()
            .filter(|path| !self.units_read.contains(&queue_key(path)))
            .count()
    }

    /// Read one file the way a compiler reads it**, and let the index answer for what came out.    ///
    /// One call that is the whole foundation, in the order the layers were built: walk the file's own translation
    /// unit ([`crate::TranslationUnit::walk`], over the summaries the index already holds and the text this session
    /// is holding), read the unit's definitions once, cook the file against **its** environment
    /// ([`crate::FileMacros`]) and the compilation's own definitions ([`MacroTable::from_marked`]), render, index
    /// the rendering and map every range back into the file ([`crate::FileIndexer::index_rendering`]), and hand the
    /// declarations to the index ([`crate::ProjectIndex::insert_cooked`]).
    ///
    /// # Why it is worth doing, and when
    ///
    /// The raw reading — the file's own text — cannot see what a macro declares, and it cannot see the *scope* a
    /// namespace-opening macro puts a declaration in: MSVC's `_STD_BEGIN` is `namespace std {` in `yvals_core.h`,
    /// so every declaration in `<string>` is `std::`-qualified to a compiler and at file scope to a reader. Measured
    /// on the 255-file SDK corpus, 3 250 declaration names exist **only** after expansion, and the index answers for
    /// 4 of 40 sampled ones before this call and 40 of 40 after.
    ///
    /// It is **not free** (a unit walk plus a parse of the rendering per file), so the session does it for the files
    /// a reader is actually looking at, at the moment its queue drains: [`Session::cook_the_open_files`] is what the
    /// indexing loop calls, and this is what a caller that wants one file cooked says.
    ///
    /// `None` when the file has no summary or no text — nothing has read it yet, which is a state and not an error.
    pub fn cook(&mut self, path: impl AsRef<Path>) -> Option<CookedReading> {
        let materials = self.cooking_materials(path.as_ref())?;
        let rendered = self.render_a_cooked(&materials)?;
        self.commit_a_cooked(rendered)
    }

    /// **Everything a cook needs that is cheap to gather** — and it takes `&self`, which is the whole point of it.
    ///
    /// The expensive half of cooking a file — lexing it, rendering it, parsing the rendering — is a function of the
    /// file's own text and its environment, and it changes nothing. So it is [`Session::render_a_cooked`], which takes
    /// `&self`, and this is the short part that has to happen first: resolve the path, ask the index for the summary,
    /// take the translation unit **out of the cache** (a hit whenever the file has been indexed) and read the text.
    ///
    /// Split for the same reason [`Session::prepare_a_wave`] is: the write lock is what a query waits for. Measured,
    /// a cooking slice of *zero* index files held it for up to **1527 ms**, and the parts measured here are
    /// `unit 15–42 ms` (the cache lookup) against `own 9–269 ms` (the render and parse).
    ///
    /// **And `&self` rather than `&mut self`, which it was until the measurement said so.** Every step of it reads:
    /// the summary lookup, `context_hash`, the unit (behind its own lock, deliberately — see `Session::units`), the
    /// text, the seed. What needed `&mut self` was the *queue pop*, and that is [`Session::claim_cooking`] — so a
    /// slice is claimed under the write lock, which is a `Vec` operation, and gathered here under the read one.
    /// Measured from the server's own budget warning, the gather was **58–76 ms** of every drain's write hold and the
    /// pop was nothing.
    pub fn cooking_materials(&self, path: &Path) -> Option<CookingMaterials> {
        let path = self.store.index().summary(path)?.path.clone();
        let key = SummaryKey::new(0, self.store.context_hash(&path));

        // What the file's own text already declares — the names the cooked reading adds are the ones missing here.
        let already_declared: std::collections::HashSet<String> = self
            .store
            .index()
            .summary(&path)
            .into_iter()
            .flat_map(|summary| summary.declarations.iter())
            .map(crate::DeclFact::qualified_name)
            .collect();

        // **Not from a provisional timeline.** A rendering made from an environment missing whatever the queue has
        // not read yet is a rendering with the macro left unexpanded — and unlike the timeline, the rendering
        // *sticks*: it is filed as the file's cooked reading, and nothing re-cooks a file that has one. Leaving the
        // file uncooked keeps it wanted (`want_the_closure_cooked` asks for every open file without a reading), so
        // a later drain renders it when the timeline is the whole program. See [`HeldUnit`].
        // **A header is not the root of its own translation unit**, and cooking one as if it were is a *wrong* answer
        // rather than a smaller one.
        //
        // `wincon.h` writes `WINBASEAPI WINBOOL WINAPI SetConsoleOutputCP(UINT wCodePageID);` and includes neither
        // `windef.h` nor `windows.h` — those arrive from the **includer**, and `windows.h` includes them before it.
        // So a timeline rooted at `wincon.h` itself has no `WINAPI` and no `WINBASEAPI` in force, the cook leaves the
        // specifiers spelled out, and [`crate::sema::types::read_specifiers`] takes the **last** name of the sequence
        // (the rule that fixes `_NODISCARD _CONSTEXPR20 reference back()`) — which here is `WINAPI`. Measured: the
        // hover on `SetConsoleOutputCP` said "a function returning `WINAPI`", the declaration says `WINBOOL`, and the
        // *wrong* one won, because a cooked reading outranks the file's own summary.
        //
        // Two rules, in this order:
        //
        // 1. **A unit that already contains the file is the environment to use.** The project's translation unit is
        //    normally held (it was walked when its file was read), and its timeline is exactly the state at the
        //    header's inclusion — no walk, no guess.
        // 2. **Otherwise a file that can be the root of its own program is walked as one** — a `.cpp` with a compile
        //    command, a file nothing includes, a buffer the reader has open. Its timeline is the whole closure.
        // 3. **Otherwise the environment is composed from the index** — `the_indexed_environment`, the same reading the
        //    second pass makes and the same one the *summary* of this file is rebuilt from. This arm is new, and it is
        //    what a header gets: it used to `return None`, because the alternative was an **empty** environment and a
        //    rendering made from one is a wrong answer. A composed environment is not empty, so the reason that rule
        //    existed is gone and the rule with it.
        //
        // **The measurement that made rule 3 exist.** After phase 2 stopped building units for the pass, rule 2 was
        // reached for every header, rule 3 was `return None`, and `render_a_cooked`'s `unit.environment_of(path)?`
        // answered `None`: `include/techniques.h` came out `NOT COOKED` and the C++ corpus went **51/52 → 41/52** type
        // answers, 10/10 → 9/10 member answers. A capability that depends on a *cache* holding some other file's
        // timeline is not a capability; the environment has to be a function of the index.
        //
        // **Rules 1 and 2 come first, and that is measured too.** Composing *everything* was tried first and left the
        // corpus at 41/52: a timeline sees the guarded `#define`s a closure's facts do not (`FactGuard::Special`), so a
        // composed environment is weaker exactly where a standard library puts its specifiers. Composition is the
        // fallback that makes a header cookable at all, not a replacement for the walk.
        let held = {
            let held_units = self.held_units();
            held_units
                .values()
                .find(|held| !held.provisional && held.unit.environment_of(&path).is_some())
                .map(|held| std::sync::Arc::clone(&held.unit))
        };
        let can_be_a_root = {
            let included = !self.store.index().includers_of(&path).is_empty();
            let has_a_command = self
                .config
                .database
                .as_ref()
                .is_some_and(|database| database.command_for(&path).is_some());
            !included || has_a_command || self.documents.is_open(&path)
        };
        // The definition cache is the run's, not this file's: a definition's text does not depend on which file asked
        // for it, and without it every cook re-lexes and re-parses the whole closure's bodies (`configuration_from_
        // environment_and`'s own note measures 12.2 s of a 40 s census on exactly that).
        let mut parsed = self
            .parsed_definitions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut composed = || {
            self.store
                .the_indexed_environment(&path)
                .map(|environment| {
                    crate::preprocess::cooked::configuration_from_environment_and(
                        &environment,
                        true,
                        &mut parsed,
                    )
                })
        };
        let walked = if held.is_none() && can_be_a_root {
            self.unit_and_state_of(&path).map(|(unit, _)| unit)
        } else {
            None
        };
        // **A timeline answers positionally and stays preferred**; the composed configuration is the fallback for a
        // file no walk covers. Converting both to a `Configuration` was tried and is *not* equivalent: the two
        // `semantic.rs` tests about a directive-split return type failed with it, because a materialised table is the
        // state a file starts with rather than the state at each line.
        let (unit, configuration) = match held.or(walked) {
            Some(unit) => (Some(unit), None),
            None => (None, Some(composed()?)),
        };
        drop(parsed);

        let text = self.files.read(&path)?;
        let text_hash = crate::cache::content_hash(&text);
        let seed = MacroTable::from_marked(self.store.index().macros());

        Some(CookingMaterials {
            path,
            key,
            unit,
            configuration,
            text,
            text_hash,
            seed,
            already_declared,
        })
    }

    /// **Lex, render and parse one file's text** — the expensive half of cooking, and it takes `&self`.
    ///
    /// Everything it reads is owned by its argument, so a caller may hold it under a **read** lock while a query runs
    /// beside it. [`Session::commit_a_cooked`] is the other half: one insert, `&mut self`.
    ///
    /// The starting macro state and the seed are moved in rather than borrowed because the caller has to let go of the
    /// session between the two halves — that is the whole point of the split — and a borrow of `&self` cannot survive it.
    pub fn render_a_cooked(&self, materials: &CookingMaterials) -> Option<RenderedCook> {
        let CookingMaterials {
            path,
            key,
            unit,
            configuration,
            text,
            text_hash,
            seed,
            already_declared,
        } = materials;

        let (tokens, _) = { cpp_parser::lex(text, &cpp_parser::LexerConfig::default()) };
        // **Two ways to start a rendering, and the walk's is the accurate one.**
        //
        // A `MacroView` is a *position* in a walk, so it answers a `#define` that arrives mid-file and a directive that
        // splits a declaration — which is what a compiler sees, and what every reading this session built before this
        // change used. It requires a **held translation unit**, and after phase 2 stopped building units for the pass no
        // header had one: `include/techniques.h` came out `NOT COOKED` and the C++ corpus went 51/52 → 41/52 type
        // answers. A capability that depends on a *cache* holding another file's timeline is not a capability, so when
        // no walk covers the file its environment is **composed from the index**
        // (`configuration_from_environment_and`, over the run's shared `ParsedDefinitions`) and rendered as
        // `Over(configuration, seed)`.
        //
        // **The fallback is not a substitute**, and the difference is measured: converting *both* to a `Configuration`
        // failed the two `tests/semantic.rs` tests about a directive-split return type, because a materialised table is
        // the state a file *starts* with rather than the state at each line.
        let rendered = match (unit, configuration) {
            (Some(unit), _) => {
                let definitions = unit.definitions();
                let macros = crate::preprocess::cooked::FileMacros::new(
                    unit.environment_of(path)?,
                    &definitions,
                    Some(seed),
                    true,
                );
                crate::preprocess::cooked::cook_with(text, &tokens, &macros).render()
            }
            (None, Some(configuration)) => {
                let start = crate::preprocess::cooked::Over::new(&configuration.table, seed);
                crate::preprocess::cooked::cook_with(text, &tokens, &start).render()
            }
            (None, None) => return None,
        };
        let indexer = FileIndexer::new(&self.files, &self.config);
        let indexed = indexer.index_rendering(path, &rendered, *key);
        let reading = CookedReading {
            declarations: indexed.summary.declarations.len(),
            only_after_expansion: indexed
                .summary
                .declarations
                .iter()
                .filter(|fact| !already_declared.contains(&fact.qualified_name()))
                .count(),
            diagnostics: indexed.diagnostics.len(),
            unplaced: indexed.unplaced,
            mapped: indexed.mapped,
        };
        Some(RenderedCook {
            text_hash: *text_hash,
            indexed,
            reading,
        })
    }

    /// **Put a rendered reading into the index** — one insert, and it is the whole of what needs `&mut self`.
    ///
    /// # The one check, and the defect it answers
    ///
    /// The rendering was made from the text the session held when the cook was **claimed**, and the file can be
    /// edited between the claim and this call. Committing then would file a reading of text nobody has, *under the
    /// current text* — and nothing re-cooks a file that has a cooked reading, so the wrong answer is permanent until
    /// the next edit. Measured through the protocol: a `didChange` deleting a `;`, then one pull —
    /// `reading Cooked, 0 error(s)` for a file that does not parse. So a reading whose text is no longer the file's
    /// is **dropped**, and the file is asked for again: the edit that made it stale already queued a cook (see
    /// [`Session::buffer_changed`]), and asking again here costs a hash lookup.
    pub fn commit_a_cooked(&mut self, rendered: RenderedCook) -> Option<CookedReading> {
        let RenderedCook {
            text_hash,
            indexed,
            reading,
        } = rendered;
        let path = indexed.summary.path.clone();

        // **The check.** `self.text` is the file's text *now*, and the hash it is compared against is the one the
        // rendering was made from. A file that has become unreadable is the same answer — nothing to file.
        let current = self
            .text(&path)
            .map(|text| crate::cache::content_hash(&text));
        if current != Some(text_hash) {
            if crate::probes::cooking() {
                eprintln!(
                    "commit_a_cooked: dropped {} | the text changed while it was being rendered",
                    path.display()
                );
            }
            self.want_cooked_reading(&path);
            return None;
        }

        // **The rendering is remembered, not dropped.** This is the half that was missing, and its absence is why
        // `Session::view` could answer `None` for a file whose rendering had *just* been built and parsed: the facts
        // went into the index and the tree went in the bin, so nothing could reconstruct the reading until some
        // other path happened to remember one. With this, "the file has been read the way a compiler reads it" and
        // "the reading is available to a query" are the same state — which is what the rest of this layer assumes.
        //
        // Keyed by the hash of the text it was **made from** — the same one the check above just compared — rather
        // than by a second read of the file, so the entry cannot describe one text under another's key.
        if let Ok(mut known) = self.renderings.lock() {
            let key = (path.clone(), text_hash);
            if crate::probes::view() {
                eprintln!("commit: path {:?} hash {:?}", path, text_hash);
            }
            let before = known.len();
            let weight = indexed.rendered.weight();
            known.insert(key, std::sync::Arc::clone(&indexed.rendered), weight);
            if crate::probes::cooking() {
                eprintln!(
                    "commit_a_cooked: inserted {} | cache {} -> {}",
                    path.display(),
                    before,
                    known.len()
                );
            }
        } else if crate::probes::cooking() {
            // **The other way this can silently not happen**: the cache lock is poisoned, which leaves it exactly as
            // it was while the facts go into the index — "read" and "available" apart again, which is the state this
            // whole change is about.
            eprintln!(
                "commit_a_cooked: NOT inserted {} | lock {}",
                path.display(),
                self.renderings.lock().is_ok()
            );
        }
        {
            // **The reading goes to the disk as well as the index**, which is what makes a second start free: the
            // next session's `take_the_stored_refinements` serves this file instead of rebuilding it — measured at
            // 45 ms rebuilt against 1.2 ms served, see `docs/refinement.md` §3.2. Written *before* the index insert
            // because the write borrows the store and the insert needs it mutably; both are cheap.
            let cooked: crate::index::CookedFile = indexed.into();
            self.store.put_the_cooked_reading(&path, &cooked);
            self.store.index_mut().insert_cooked(&path, cooked);
        }
        Some(reading)
    }

    /// **Which files the next cooking slice is for** — the queue pop, and the only part of taking a slice that needs
    /// the write lock.
    ///
    /// The materials themselves are [`Session::cooking_materials`], which takes `&self`: a summary lookup, a unit out
    /// of the cache and the text. Naming the files here and gathering them there is what lets the pump hold the read
    /// lock for `unit 15–42 ms` per file rather than the write lock a query waits for.
    ///
    /// It takes the queue's front, so a caller that gathers, renders and commits them in order has done exactly what
    /// `finish_a_slice`'s loop used to do — with everything but the pop outside the write lock.
    pub fn claim_cooking(&mut self, how_many: usize) -> Vec<PathBuf> {
        self.cooking.take(how_many)
    }

    /// **The materials for a claimed slice**, skipping the files the index cannot describe yet.
    ///
    /// A file the cook queue names but the index cannot describe is dropped rather than re-queued:
    /// `want_cooked_reading` and the closure sweep both ask for it again, and a queue that re-adds its own failures
    /// is a queue that never empties — the note on `want_the_closure_cooked` records that hang.
    /// **Remember the files a cooking slice could not build**, so the closure walk stops asking for them.
    ///
    /// `asked` is what the slice claimed and `built` is what `materials_for` could gather from it; the difference is
    /// the set that would otherwise be wanted again on every drain. See [`Session::cooking_refused`], and
    /// [`Session::want_cooked_reading`] for the one caller that clears a refusal.
    pub fn refuse_the_cooks_that_built_nothing(&mut self, asked: &[PathBuf], built: &[PathBuf]) {
        let Ok(mut refused) = self.cooking_refused.lock() else {
            return;
        };
        for path in asked {
            if !built.iter().any(|made| made == path) {
                refused.insert(queue_key(path));
            }
        }
    }

    pub fn materials_for(&self, paths: Vec<PathBuf>) -> Vec<CookingMaterials> {
        paths
            .into_iter()
            .filter_map(|path| {
                let built = self.cooking_materials(&path);
                // **A claimed file that yields no materials is a file that is now neither queued nor cooked.**
                // `claim_cooking` popped it and this answered `None`, so it is gone from both places and the loop
                // that decides whether there is work left reads `cooked == 0` as "nothing to do". Naming the reason
                // is what separates "the summary is not in the index yet" (a state) from "the unit is provisional"
                // (also a state) from a defect.
                if built.is_none() && crate::probes::cooking() {
                    let indexed = self.store.index().summary(&path).is_some();
                    let (unit, provisional) = match self.unit_and_state_of(&path) {
                        Some((_, provisional)) => ("held", provisional),
                        None => ("none", false),
                    };
                    eprintln!(
                        "cooking: {} yields no materials | summary indexed {indexed} | unit {unit} \
                         provisional {provisional}",
                        path.display()
                    );
                }
                built
            })
            .collect()
    }

    /// **Lex, render and parse a slice's worth of materials** — the expensive half, under a read lock.
    pub fn render_them(&self, materials: Vec<CookingMaterials>) -> Vec<RenderedCook> {
        let tracing = crate::probes::cooking();
        materials
            .into_iter()
            .filter_map(|materials| {
                let path = materials.path.clone();
                let built = self.render_a_cooked(&materials);
                // **The last silent drop.** A claimed file whose render declines is gone from the queue and was
                // never cooked, which the pump reads as "no work left".
                if built.is_none() && tracing {
                    eprintln!("cooking: {} did not render", path.display());
                }
                built
            })
            .collect()
    }

    /// **Put a slice's worth of rendered readings into the index** — one insert each, under the write lock.
    pub fn commit_them(&mut self, rendered: Vec<RenderedCook>) -> Vec<CookedReading> {
        rendered
            .into_iter()
            .filter_map(|reading| self.commit_a_cooked(reading))
            .collect()
    }

    // **Everything a cook needs, gathered under the write lock and then used without it.**
    //
    // Owned rather than borrowed on purpose: the caller has to let go of the session between
    // [`Session::cooking_materials`] and [`Session::render_a_cooked`], so nothing here may borrow it. The unit is an
    // `Arc` the cache already holds, and the seed is a table built from the index's own macros.

    /// **The rendering of one file** — what the preprocessor produces from it, as text.
    ///
    /// The other half of [`Session::cook`], and the artifact a parser should be reading if the crate is to follow a
    /// compiler's reading rather than its own: `cook` renders a file and **parses the rendering**, then keeps the
    /// declarations it found and throws the text away. This is that text, for a caller that wants the reading
    /// itself — a probe comparing the two readings of a file, and any future consumer that answers from the
    /// rendering instead of from the file's own tokens.
    ///
    /// `None` under exactly the conditions [`Session::cook`] returns `None`: the file is not in the index, or its
    /// closure has not been read, so there is no environment to expand it against.
    ///
    /// # Why it is built the same way `cook` builds it
    ///
    /// The two must agree token for token, or a caller comparing them would be comparing a rendering against a
    /// *different* rendering. They share the steps — the unit, the file's macros, `cook_with` — and this method
    /// exists rather than `cook` returning its text because the text is large (a header's rendering is megabytes)
    /// and every caller of `cook` but this one discards it.
    pub fn rendered_text_of(&self, path: impl AsRef<Path>) -> Option<String> {
        Some(self.rendering_of(path)?.text)
    }

    /// **The rendering of one file, with its spans** — the text and the map that says where each token of it was
    /// written.
    ///
    /// The same artifact as [`Session::rendered_text_of`] with the half that makes it usable for a **view**:
    /// [`crate::FileView::parse_rendering`] needs the spans, because a rendering's offsets are its own and a client
    /// speaks the file's.
    ///
    /// **`&self`, and that is load-bearing.** Every step of it — the lookup, the unit (cached, behind its own lock),
    /// the read, the lex, the cook — reads the session and changes nothing, so a caller can build a rendering under
    /// the **read** lock instead of the write one a query is waiting for. The drain's macro-environment slice used to
    /// be the largest thing left inside the writer after the parse moved out, at 65–190 ms per header.
    pub fn rendering_of(
        &self,
        path: impl AsRef<Path>,
    ) -> Option<crate::preprocess::cooked::RenderedCooked> {
        let path = self.store.index().summary(path.as_ref())?.path.clone();
        let unit = self.translation_unit_of(&path)?;
        let text = self.files.read(&path)?;
        let seed = MacroTable::from_marked(self.store.index().macros());
        let (tokens, _) = cpp_parser::lex(&text, &cpp_parser::LexerConfig::default());
        let unit_definitions = unit.definitions();
        let macros = crate::preprocess::cooked::FileMacros::new(
            unit.environment_of(&path)?,
            &unit_definitions,
            Some(&seed),
            true,
        );
        Some(crate::preprocess::cooked::cook_with(&text, &tokens, &macros).render())
    }

    /// **Read one file the way the compiler does, and put that reading in the index.**
    ///
    /// Its key carries the environment the rendering was cooked under, so a header included by two
    /// translation units with different macros gets two readings rather than one that is wrong for
    /// one of them.
    ///
    /// Returns the summary it committed. Costs the cook (a cached rendering is a hash and a lookup) plus the parse of
    /// the rendered text; `tests/rendering_real.rs` measures both halves.
    pub fn index_the_rendering(&mut self, path: impl AsRef<Path>) -> Option<crate::FileSummary> {
        let path = self.store.index().summary(path.as_ref())?.path.clone();
        let unit = self.translation_unit_of(&path)?;
        let environment = unit.environment_of(&path)?;
        let written = self.files.read(&path)?;
        let rendered = match self.known_rendering_of(&path, &written) {
            Some(rendered) => rendered,
            None => std::sync::Arc::new(self.rendering_of(&path)?),
        };
        self.store
            .index_the_rendering(&path, &rendered, Some(&environment))
            .cloned()
    }
    /// **A declaration's fact, read from the *rendering*** — the reading where a directive is resolved.
    ///
    /// For a fact whose [`DeclFact::split_by_a_directive`] is set the summary declines to name a
    /// type: the file's own text states one per branch and the written reading cannot know which is
    /// live.
    ///
    /// **Cost.** The rendering itself is the artifact a cook produces — a hash and a lookup when it is cached — and
    /// the summary of its text is computed here, so a caller pays one parse of the rendered text per split
    /// declaration it asks about. That is why this is asked from a hover on a hand-written `#if` rather than from a
    /// path a keystroke takes.
    ///
    /// [`DeclFact::split_by_a_directive`]: crate::DeclFact::split_by_a_directive
    pub fn fact_from_the_rendering(
        &self,
        path: impl AsRef<Path>,
        name: &str,
        scope: Option<&str>,
    ) -> Option<crate::DeclFact> {
        let path = self.store.index().summary(path.as_ref())?.path.clone();
        // **The cached rendering first.** `rendering_of` builds one — a unit walk, a lex and a cook — and the session
        // already keeps them by content hash; asking for the artifact it holds is the difference between a lookup and
        // 4 ms (`tests/rendering_real.rs`: the second ask was 3.9 ms with this missing, 23.8 → 19.1 before the summary
        // cache existed).
        let text = self.files.read(&path)?;
        let rendered = match self.known_rendering_of(&path, &text) {
            Some(rendered) => rendered,
            None => {
                // Built and **kept**: the key is the one `known_rendering_of` reads, so the next ask is a lookup.
                let built = std::sync::Arc::new(self.rendering_of(&path)?);
                if let Ok(mut known) = self.renderings.lock() {
                    let weight = built.weight();
                    known.insert(
                        (path.clone(), crate::cache::content_hash(&text)),
                        built.clone(),
                        weight,
                    );
                }
                built
            }
        };
        // **The summary of the rendered text, cached** — see `rendering_summaries`. A hover on a declaration
        // a directive splits asks this, and before the cache existed every ask paid one parse of the rendering.
        let key = (path.clone(), crate::cache::content_hash(&rendered.text));
        let remembered = self
            .rendering_summaries
            .lock()
            .ok()
            .and_then(|mut known| known.get(&key).cloned());
        let summary = match remembered {
            Some(summary) => summary,
            None => {
                let built = std::sync::Arc::new(crate::summarize(
                    &path,
                    &rendered.text,
                    crate::SummaryKey::new(0, 0),
                ));
                if let Ok(mut known) = self.rendering_summaries.lock() {
                    let weight = built.declarations.len() + built.macros.len();
                    known.insert(key, built.clone(), weight);
                }
                built
            }
        };
        summary
            .declarations
            .iter()
            .find(|fact| {
                fact.name == name && fact.scope.as_deref() == scope && !fact.split_by_a_directive
            })
            .cloned()
    }

    /// Cook every **open** file — what the indexing loop does when its queue drains.
    ///
    /// The moment is the point: a file's environment is only complete once everything it includes has been read, and
    /// "the queue is empty" is exactly that moment. The files are the open ones because those are what a reader is
    /// looking at, and cooking costs a unit walk and a parse per file.
    ///
    /// Returns what each file read as, for a caller that reports it.
    pub fn cook_the_open_files(&mut self) -> Vec<(PathBuf, CookedReading)> {
        let open = self.documents.paths();
        let mut done = Vec::new();
        for path in open {
            if let Some(reading) = self.cook(&path) {
                done.push((path, reading));
            }
        }
        done
    }

    /// The closure of `path` **with the text of each file**, in the order the graph is walked, and a map from the
    /// key every path is compared by to its place in that list.
    ///
    /// Read from the index's own edges, so it is the same closure the visibility answers use, and through this
    /// session's overlay, so an unsaved buffer is what gets cooked. One entry per file, and a file whose text this
    /// session cannot get is left out — the walk then treats it as a file that defines nothing, which is what an
    /// unreadable include is.
    ///
    /// **The map is the walk's lookup**, and it is why this returns two things: the unit walk asks "give me this
    /// path's summary and text" once per file it visits, and answering that by scanning the list with `same_file` is
    /// quadratic in the closure — a thousand files of standard library, for each of a hundred files being cooked.
    fn closure_with_text(&self, root: &Path) -> (Vec<(PathBuf, String)>, HashMap<String, usize>) {
        let seed = self.store.index().summary(root);
        self.closure_with_text_from(root, seed)
    }

    /// [`Session::closure_with_text`] for a root whose summary the caller has **instead of** the index.
    ///
    /// The one caller that has one is the file a reader is editing: its summary is dropped by the edit, so the
    /// index can describe its *directives* and nothing else — and those are what names its includes. See
    /// [`Session::unit_and_state_of`], where the order `directives → unit → parse` is written out.
    fn closure_with_text_from(
        &self,
        root: &Path,
        seed: Option<&FileSummary>,
    ) -> (Vec<(PathBuf, String)>, HashMap<String, usize>) {
        let mut out: Vec<(PathBuf, String)> = Vec::new();
        let mut by_key: HashMap<String, usize> = HashMap::new();

        for path in self.closure_paths_from(root, seed) {
            if let Some(text) = self.text(&path) {
                by_key.insert(queue_key(&path), out.len());
                out.push((path, text));
            }
        }

        (out, by_key)
    }

    /// The files `root` includes, **transitively**, `root` first and then by level — the order a reader would read
    /// them in, and the order the cooked readings are built in.
    ///
    /// The edges come from the index, which is the only place that knows them: a summary records where each
    /// `#include` resolved to, and a file the index has not read contributes no edges. A file is listed once,
    /// whatever number of paths reach it, which is what keeps a header included by everything from being cooked
    /// per includer.
    ///
    /// `seed` is the caller's own reading of the **root**, for the one caller whose root the index cannot describe —
    /// the file a reader is editing. See [`Session::closure_with_text_from`].
    fn closure_paths_from(&self, root: &Path, seed: Option<&FileSummary>) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        // **By the key every other queue in this crate compares by.** This used to ask `same_file` against every
        // file already listed, which normalizes two paths per comparison — and the closure of a standard-library
        // header is around a thousand files, so the walk was quadratic in it: measured as 100 s to cook a 108-file
        // corpus against 4.5 s for 255 files, because the closure here is the whole standard library and there it is
        // not. One hash per file is the same answer without the arithmetic.
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: std::collections::VecDeque<PathBuf> = std::collections::VecDeque::new();
        queue.push_back(root.to_path_buf());

        // **The root's edges come from `seed` when there is one**, because the index has none for it: a file that
        // was just edited describes nothing but its directives, and its directives are exactly its includes.
        let mut edges: Option<&FileSummary> = seed;

        while let Some(path) = queue.pop_front() {
            // One entry per **file**: two spellings of it are one file to cook, and a closure that listed both would
            // cook it twice, once under a name the index does not use.
            if !seen.insert(queue_key(&path)) {
                continue;
            }
            out.push(path.clone());

            let summary = match edges.take() {
                Some(seed) => Some(seed),
                None => self.store.index().summary(&path),
            };
            if let Some(summary) = summary {
                for include in &summary.includes {
                    if let Some(resolved) = &include.resolved {
                        queue.push_back(resolved.clone());
                    }
                }
            }
        }

        out
    }

    /// **The closure a file's own directives name, and whether every file in it has been read.**
    ///
    /// `None` when one of them has not.
    ///
    /// The root itself is exempt: the whole reason this exists is a file whose summary the index does not have.
    fn the_closure_of_the_directives(
        &self,
        root: &Path,
        directives: &FileSummary,
    ) -> Option<Vec<PathBuf>> {
        let index = self.store.index();
        let root_key = queue_key(root);
        let reached = self.closure_paths_from(root, Some(directives));

        reached
            .iter()
            .all(|path| queue_key(path) == root_key || index.summary(path).is_some())
            .then_some(reached)
    }

    /// Which declaration the name at `offset` means, using this file's scopes and then the index.
    ///
    /// The *one* projection of [`Session::definitions`]: `Yes` only when the answer is a single declaration, so a
    /// consumer that can show only one location gets nothing rather than an arbitrary one. A client that can show a
    /// list — `textDocument/definition` — should ask that instead.
    pub fn definition(&self, view: &FileView, offset: ReadingOffset) -> Known<ProjectDefinition> {
        let found = definition_across_files(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            offset.get(),
        );
        let found = self.with_the_reflection(view, found);
        // **The answer came from L1, so ask for L2** — phase 1 of `docs/indexing-design.md`. A declaration the reader
        // jumped into is a file they are now looking at, and its refined reading is what its own members, types and
        // macro-shaped declarations are answered from. Asking here costs a `HashSet` insert under its own lock; the pump
        // then refines that one file (measured: **93.6 ms**, one timeline) while this answer is already on screen.
        if let Known::Yes(found) = &found {
            self.want_refinement(&found.file);
        }
        found
    }

    /// **Is this file's reading the refined one?** — `false` for a summary that is still the shape-only reading of the
    /// file's own text, which is what a start leaves behind (see `want_refinement`).
    pub fn is_refined(&self, path: &Path) -> bool {
        // Two ways to be refined, and both are real: a reading served from the refinement cache keeps its environment
        // key, while one the pass built in this run is inserted under the file's own key (the store resets it, so that
        // the entry the first pass wrote stays addressable). The set is what makes the second kind visible.
        if self
            .refined_readings
            .lock()
            .is_ok_and(|refined| refined.contains(path))
        {
            return true;
        }
        self.store.is_marked_refined(path)
            || self
                .store
                .index()
                .summary(path)
                .is_some_and(|summary| summary.key.env_hash != 0)
    }

    /// **The reflection annotation the summary recorded**, when a same-file answer came from the binding instead.
    ///
    /// A definition inside the file being read is built from the scope tree, which holds the declaration's shape and no
    /// annotation: `UPROPERTY(EditAnywhere)` is an empty macro and was expanded away before the tree existed. The
    /// **summary** of the same file recorded it from the text, so the two answers are merged here rather than duplicating
    /// the text walk in the binding path. A member declared in *another* file already carries it — that fact came from
    /// the index.
    fn with_the_reflection(
        &self,
        view: &FileView,
        found: Known<ProjectDefinition>,
    ) -> Known<ProjectDefinition> {
        let Known::Yes(mut definition) = found else {
            return found;
        };
        if definition.fact.reflection.is_some() {
            return Known::Yes(definition);
        }
        let Some(in_the_file) =
            view.file_offset_of(ReadingOffset::new(definition.fact.name_range.start_offset))
        else {
            return Known::Yes(definition);
        };
        let Some(summary) = self.parsed_summary(&view.path) else {
            return Known::Yes(definition);
        };
        // **The name and its offset**, because an overload set shares the first and not the second.
        let recorded = summary.declarations.iter().find(|fact| {
            fact.name == definition.fact.name && fact.name_range.start_offset == in_the_file.get()
        });
        if let Some(recorded) = recorded {
            definition.fact.reflection = recorded.reflection.clone();
        }
        Known::Yes(definition)
    }

    /// **Every declaration the name at `offset` refers to** — an overload set, a redeclaration, or one entity.
    ///
    /// See [`crate::ProjectIndex::definitions`] for what the list means and why one namespace collapses to one
    /// entry. This is the query behind `textDocument/definition`: measured on one real file, **46** identifiers
    /// answered `Ambiguous` through the single-answer form, and every one of them had a usable list behind it.
    pub fn definitions(&self, view: &FileView, offset: ReadingOffset) -> Known<ProjectDefinitions> {
        definitions_across_files(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            offset.get(),
        )
    }

    /// **Every place the symbol at `offset` is written** — what `textDocument/references` asks.
    ///
    /// # Why it resolves first rather than searching for the spelling
    ///
    /// A reference list is a claim about the **whole project**, and the protocol gives it no way to say "and there
    /// may be more". So the question is not "where is this word written" but "where is *this declaration* named",
    /// and the two differ wherever a project has two things spelled alike — a local shadowing a file-scope name, a
    /// member of one class and a free function of the same name. The cursor is therefore resolved the way
    /// go-to-definition resolves it, and only then is the name searched for. A cursor that resolves to nothing
    /// answers `Unknown(NotDeclaredHere)` rather than an empty list, which is the difference between "the analysis
    /// cannot tell you" and "there are none".
    ///
    /// The search itself is [`crate::index::references::symbol_references`], whose note states the candidate rule
    /// and the one case it does not cover.
    pub fn symbol_references(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        budget: crate::ReferenceBudget,
    ) -> Known<crate::SymbolReferences> {
        let Known::Yes(found) = self.definitions(view, offset) else {
            // **Which name the cursor is on**, for the refusal's message: a caller that gets `NotDeclaredHere("")`
            // learns nothing, and the name is what a person would look up next.
            let written = crate::sema::resolve::name_position_at(&view.root, offset.get())
                .map(|name| name.written.clone())
                .unwrap_or_default();
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(written)));
        };

        // **One declaration, and the list is all of them.** `definitions` answers a namespace as one entry and an
        // overload set as several; a reference list over an overload set is the union, and every entry shares the
        // spelling, so the first is the one that names the symbol.
        let Some(first) = found.found.first() else {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("")));
        };

        let symbol = crate::SymbolToFind::of(&first.fact, first.file.clone());

        crate::index::references::symbol_references(
            self.store.index(),
            &self.files,
            &symbol,
            budget,
        )
    }

    /// Which `#define` or `#undef` settles the macro name at `offset`.
    /// **The macro a use names, asked in the file's own coordinates.**
    ///
    /// `macro_definition` takes the **rendering's** offset, and a macro has no footprint there: the cook expanded every use,
    /// so the tree at `int n = MAX;` says `((a) > (b) ? (a) : (b))` and the reader's `MAX` is not in it. The writer's text still
    /// has it — `written_text()` — and the macro's definition ranges are file offsets, so this is the query the two agree on.
    ///
    /// Measured: at a use of a file's own `#define MAX(a, b)`, `macro_definition` answered `NotDeclaredHere("MAX")` because the
    /// rendering offset it was given (`"MAX"`) named a position the *file* does not have.
    pub fn macro_at_a_file_offset(
        &self,
        view: &FileView,
        offset: crate::file::coordinates::FileOffset,
    ) -> Known<ProjectMacro> {
        // **The file's own text, from whichever view this is** — a handler may hold the *plain* view, whose root is the
        // file's own text, and asking only for `written_text()` made every macro question in the editor answer "no
        // written text". See `FileView::the_written_text`.
        let Some(written) = view.the_written_text() else {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("no written text")));
        };
        let at = offset.get();
        // The word under the cursor, in the file's own bytes — the same walk `hover`'s `name_at_the_cursor` makes for a fix.
        let is_a_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        let bytes = written.as_bytes();
        let mut start = at.min(bytes.len());
        while start > 0 && is_a_word(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = at.min(bytes.len());
        while end < bytes.len() && is_a_word(bytes[end]) {
            end += 1;
        }
        let name = &written[start..end];
        if name.is_empty() {
            return Known::Unknown(UnknownReason::UnparsableName);
        }
        self.store.index().macro_definition(name, &view.path, at)
    }

    /// **A `#define`/`#undef` this file writes itself**, read from its text — the answer to a macro question about a
    /// file the index has not caught up with yet.
    ///
    /// [`Session::macro_at_a_file_offset`] asks the **index**, whose summaries are pushed by the work loop: a file that
    /// was opened a moment ago has a view and no summary, so a hover on that file's own `#define MAX(a, b)` answered
    /// nothing at all. Measured through the real server on a two-file C fixture: `CORPUS_STANDARD` at its definition
    /// answered `Unknown(NotDeclaredHere)` while the text plainly writes it.
    ///
    /// Returns the **directive's text** — `#define CORPUS_STANDARD __STDC_VERSION__`, continuation lines joined — which
    /// is what a reader hovering a definition wants to see, and `None` when the name is not defined (or undefined) in
    /// this file before `offset`.
    pub fn macro_definition_text(&self, path: &Path, offset: usize, name: &str) -> Option<String> {
        let text = self.text(path)?;
        the_directive_defining(name, &text, offset)
    }

    /// **Every module this project can name**, for a completion after `import`.
    ///
    /// Two sources, cheapest first, and neither parses anything:
    ///
    /// ```text
    /// the index      `interface_unit_of` — exact, for a module whose interface unit has been read
    /// the file list  the project's own `.cppm`/`.ixx`/… files, named by the convention the resolver proposes with —
    ///                so a name offered here is a name `import` will resolve, even before the unit is read
    /// ```
    ///
    /// A file named `math.cppm` is module `math`; a dotted one (`std.compat.ixx`) is `std.compat`. That is the
    /// convention [`crate::ModuleScanner`] searches by, and taking it from the *files* rather than from a scan is
    /// what keeps this cheap enough to answer on every keystroke.
    pub fn module_names(&self) -> Vec<(String, PathBuf)> {
        let mut found: Vec<(String, PathBuf)> = self
            .store
            .index()
            .interface_units()
            .into_iter()
            .map(|(name, unit)| (name, PathBuf::from(unit)))
            .collect();

        // **Only the extensions that are a module unit by naming convention.** `INTERFACE_UNIT_EXTENSIONS` also
        // holds `cpp`/`cc`/`cxx`, because a `.cpp` *may* declare a module — but a completion built from that list
        // offers `main` for `main.cpp` and `math` three times for a project with `math.cpp` and `math.cppm`.
        // Measured on the C++23 fixture: six items for two modules. A `.cpp` that really is an interface unit is
        // found by the **index** once it is read, which is the exact source and the one above.
        const A_MODULE_FILE: &[&str] = &["cppm", "ixx", "ccm", "cxxm"];
        for path in &self.project {
            let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
                continue;
            };
            if !A_MODULE_FILE
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
            {
                continue;
            }
            let Some(name) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.is_empty())
            else {
                continue;
            };
            found.push((name.to_string(), path.clone()));
        }

        // **One row per name**, and the index's comes first: `sort_by` is stable, so a name the index knows keeps
        // its exact unit and the file-list entry for the same name is dropped.
        found.sort_by(|left, right| left.0.cmp(&right.0));
        found.dedup_by(|later, earlier| later.0 == earlier.0);
        found
    }

    /// **The module names a reader can type at this cursor** — the answer to a [`crate::completion::context::CompletionContext::ModuleName`]
    /// position.
    ///
    /// A list of names, not of declarations: nothing declares a module name, so the items carry the name as their
    /// label and the unit as their detail, and the client replaces the partial word the reader has typed.
    fn module_completions(
        &self,
        written: &str,
        range: cpp_parser::SourceRange,
    ) -> crate::CompletionSet {
        let mut items: Vec<crate::completion::CompletionItem> = Vec::new();

        for (name, unit) in self.module_names() {
            if !written.is_empty()
                && !name
                    .to_ascii_lowercase()
                    .starts_with(&written.to_ascii_lowercase())
            {
                continue;
            }
            items.push(crate::completion::CompletionItem {
                label: name.clone(),
                insert: name.clone(),
                snippet: false,
                // A module name is a scope name, which is the kind a theme draws a qualified-name root with.
                kind: crate::completion::ItemKind::Namespace,
                detail: Some(format!("module {}", unit.display())),
                identity: None,
                filter: None,
                fact: None,
                replace: range,
                inclusion: None,
                additional_edits: Vec::new(),
            });
        }

        crate::CompletionSet {
            scope: String::new(),
            prefix: written.to_string(),
            replace: range,
            truncated: false,
            // A module name is a file on disk, not a fact the index has to have read: nothing here can be missing.
            incomplete: false,
            items,
        }
    }

    /// **The integer value of the constant declared at `offset`**, when this layer can fold it — an enumerator, or a
    /// `const`/`constexpr` variable. See [`crate::sema::constant`], which does the folding and says what it refuses.
    ///
    /// The macro question — `UCHAR_MAX` is `0xff` — is answered here rather than there because a macro may be defined
    /// in a header: the index has the body of every `#define` it has read, and the constants of a real header are
    /// written in terms of the ones `<climits>` brings in.
    pub fn constant_value_at(&self, view: &FileView, offset: ReadingOffset) -> Option<i64> {
        let path = view.path.clone();
        crate::sema::constant::value_at(view, offset.get(), &|name| {
            self.macro_body_at(&path, offset.get(), name)
        })
    }

    /// **An object-like macro's body, as text**, for a name visible at `offset` in this file — `0xff` for
    /// `UCHAR_MAX`. `None` for a function-like macro (its body is not a value) and for a name that is not a macro
    /// here.
    pub fn macro_body_at(&self, path: &Path, offset: usize, name: &str) -> Option<String> {
        let environment = self.store.index().macro_environment(name, path);
        let Known::Yes(found) = environment.at(offset) else {
            return None;
        };
        let fact = found.fact();
        if fact.function_like {
            return None;
        }
        // **The body is a range in the defining file**, because a replacement list is text in *that* file and the fact
        // deliberately stores a shape rather than a copy of the tokens (see `MacroFact::body_range`).
        let range = fact.body_range?;
        let text = self.text(found.file())?;
        let body = text.get(range.start_offset..range.end_offset())?;
        (!body.trim().is_empty()).then(|| body.trim().to_string())
    }

    pub fn macro_definition(&self, view: &FileView, offset: ReadingOffset) -> Known<ProjectMacro> {
        macro_across_files(self.store.index(), &view.root, &view.path, offset.get())
    }

    /// **A macro the *compiler* predefines** — `__STDC_VERSION__`, `__GNUC__`, `_WIN32` — with its value.
    ///
    /// These are not in any file, so no summary holds them and [`Session::macro_at_a_file_offset`] answers nothing for
    /// one; a reader hovering `#ifdef __STDC_VERSION__` was told nothing at all, which reads as "this name is unknown"
    /// for the one kind of name a compiler defines by itself.
    ///
    /// # Where the answer comes from, in order
    ///
    /// ```text
    /// 1. the standard's own macros             `__STDC__`, `__STDC_VERSION__`, `__STDC_HOSTED__`, `__cplusplus`
    /// 2. the toolchain's `-dM` table           `__GNUC__`, `_WIN32`, `__linux__`
    /// ```
    ///
    /// **The standard's first, and that is not an optimisation.** A test project with no `compile_commands.json` and no
    /// compiler on `PATH` has no toolchain at all, and `__STDC_VERSION__` is defined by *every* C compiler — the *value*
    /// is what a compiler decides, and the file's language already says which standard it is reading. Measured: a
    /// handshake test on a two-file C project answered no hover for `__STDC_VERSION__`, because the toolchain was the
    /// only source and it was absent.
    ///
    /// `path` is the file the question is about, because the standard is per file.
    pub fn predefined_macro(&self, name: &str, path: &std::path::Path) -> Option<String> {
        let language = self.config_for(path).language_for(path);
        if let Some(value) = crate::session::the_standards_own_macro(name, &language) {
            return Some(value);
        }

        let toolchain = self.toolchain.as_ref()?;
        if toolchain.builtin_macros.is_empty() {
            return None;
        }
        toolchain
            .macros()
            .into_iter()
            .find(|(defined, _)| *defined == name)
            .map(|(_, value)| value.unwrap_or_default().to_string())
    }

    /// **What a macro invocation at `offset` expands to** — the replacement list with the invocation's arguments put in.
    ///
    /// `None` when the name is not a macro there, when it is a function-like macro **not followed by a call** (writing
    /// its name alone does not expand it), or when the argument list cannot be read (an unbalanced `(` in the text).
    ///
    /// The substitution is textual, and deliberately: `#x` and `a ## b` are not applied, and the result's own macro names
    /// are not expanded again. A consumer that shows this must say which rules it applied — see the `hover` card, which
    /// does — because a reader comparing it with a compiler's answer would otherwise be looking at two different things.
    pub fn the_expansion_of_a_macro(
        &self,
        view: &FileView,
        offset: crate::file::coordinates::FileOffset,
    ) -> Option<String> {
        let Known::Yes(found) = self.macro_at_a_file_offset(view, offset) else {
            return None;
        };
        let fact = &found.fact;
        if fact.kind != crate::MacroKind::Definition {
            return None;
        }

        // **The body's text, from the file that writes it.** A macro's fact has file offsets, so the text is the
        // defining file's own — which is this one for a macro written here, and a header otherwise.
        let body_range = fact.body_range?;
        let defining =
            if crate::file::paths::normalize_path(&found.file, self.files.is_case_insensitive())
                == crate::file::paths::normalize_path(&view.path, self.files.is_case_insensitive())
            {
                view.the_written_text().map(|text| text.to_string())
            } else {
                self.view_of_the_file(&found.file)
                    .map(|other| other.source.to_string())
            }?;
        let body = defining
            .get(body_range.start_offset..body_range.end_offset())?
            .trim();

        // An **object-like** macro is its replacement list, whatever follows the name.
        if !fact.function_like {
            return Some(body.to_string());
        }

        // **The parameters, from the `#define` line.** The fact records `function_like` and the body's range, not the
        // names: they are the text between the name and the body, and reading it is the same rule the parser applied.
        let between = defining.get(fact.range.end_offset()..body_range.start_offset)?;
        let open = between.find('(')?;
        let close = between.rfind(')')?;
        if close < open {
            return None;
        }
        let parameters: Vec<&str> = between[open + 1..close]
            .split(',')
            .map(str::trim)
            .filter(|parameter| !parameter.is_empty())
            .collect();

        // **The argument list, read from the text at its `(`** — `(2, 5) rest` → `["2", "5"]`. The nesting and the
        // literals are tracked because both appear in real arguments: `f(g(1, 2), 3)` has two arguments and
        // `f("a,b", c)` has two as well. A list with no closing paren is `None` — the file is being edited and the text
        // is not a call yet.
        fn the_arguments_of_a_call(text: &str) -> Option<Vec<String>> {
            let bytes = text.as_bytes();
            if bytes.first() != Some(&b'(') {
                return None;
            }
            let mut depth = 0i32;
            let mut quote: Option<u8> = None;
            let mut arguments: Vec<String> = Vec::new();
            let mut current_start = 1usize;
            let mut at = 0usize;
            while at < bytes.len() {
                let byte = bytes[at];
                match quote {
                    Some(ending) => {
                        if byte == b'\\' {
                            at += 2;
                            continue;
                        }
                        if byte == ending {
                            quote = None;
                        }
                    }
                    None => match byte {
                        b'"' | b'\'' => quote = Some(byte),
                        b'(' | b'[' | b'{' => depth += 1,
                        b')' | b']' | b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                let last = text[current_start..at].trim();
                                if !last.is_empty() || !arguments.is_empty() {
                                    arguments.push(last.to_string());
                                }
                                return Some(arguments);
                            }
                        }
                        b',' if depth == 1 => {
                            arguments.push(text[current_start..at].trim().to_string());
                            current_start = at + 1;
                        }
                        _ => {}
                    },
                }
                at += 1;
            }
            None
        }

        // **The invocation, from the file the cursor is in.** The name's own token is read from the bytes at `offset`,
        // and the arguments are what follows the `(` that must come next for an expansion to happen at all.
        let written = view.the_written_text()?;
        let bytes = written.as_bytes();
        let is_a_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        let mut start = offset.get().min(bytes.len());
        while start > 0 && is_a_word(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = offset.get().min(bytes.len());
        while end < bytes.len() && is_a_word(bytes[end]) {
            end += 1;
        }
        if written.get(start..end)? != fact.name {
            return None;
        }
        let after = &written[end..];
        let paren = after.find(|character: char| !character.is_whitespace())?;
        if !after[paren..].starts_with('(') {
            return None;
        }

        let arguments = the_arguments_of_a_call(&written[end + paren..])?;
        if arguments.len() != parameters.len() && !(parameters.len() == 1 && parameters[0] == "...")
        {
            return None;
        }

        // **A textual substitution, on identifier boundaries.**
        let mut expanded = String::with_capacity(body.len());
        let body_bytes = body.as_bytes();
        let mut at = 0usize;
        while at < body_bytes.len() {
            if !is_a_word(body_bytes[at]) || (at > 0 && is_a_word(body_bytes[at - 1])) {
                expanded.push(body_bytes[at] as char);
                at += 1;
                continue;
            }
            let word_start = at;
            while at < body_bytes.len() && is_a_word(body_bytes[at]) {
                at += 1;
            }
            let word = &body[word_start..at];
            match parameters.iter().position(|parameter| *parameter == word) {
                Some(position) => expanded.push_str(arguments.get(position)?.trim()),
                None => expanded.push_str(word),
            }
        }
        Some(expanded)
    }

    /// **Every `import m;` this file writes, with the file each one reaches** — the module's answer to what
    /// [`crate::IncludeFact`] is for `#include`.
    ///
    /// The range is the **module name** and not the declaration, for the same reason a document link covers
    /// `vector` rather than `#include <vector>`: a link is a clickable span, and the name is the clickable thing.
    ///
    /// # Where the answer comes from, in order
    ///
    /// ```text
    /// the index        `interface_unit_of(m)` — exact, and free, for a module whose unit has been read
    /// the scanner      the naming convention beside this file and along the include paths — which is how the
    ///                  FIRST request in a module file learns where the module lives, before any of it is indexed
    /// ```
    ///
    /// `None` for a module neither can name, which is the honest answer: the link leads nowhere, and an underline
    /// that leads nowhere is worse than none. A **partition** (`import :part;`) and a **header unit**
    /// (`import <vector>;`) are not module names and are skipped here — the header unit is a header, and the
    /// partition belongs to the file's own module.
    pub fn module_imports_of(&self, view: &FileView) -> Vec<ModuleImport> {
        let mut imports: Vec<ModuleImport> = Vec::new();
        let directory =
            crate::paths::parent_normalized(&view.path, self.files.is_case_insensitive());
        let mut interner = PathInterner::new(self.files.is_case_insensitive());
        let mut scanner: Option<ModuleScanner<'_, OverlayFiles<OpenDocuments, F>>> = None;

        for node in view.root.descendants() {
            if cpp_parser::CppSyntaxKind::from(node.kind()) != cpp_parser::CppSyntaxKind::ImportDecl
            {
                continue;
            }
            let Some(name_node) = node.descendants().find(|inner| {
                cpp_parser::CppSyntaxKind::from(inner.kind())
                    == cpp_parser::CppSyntaxKind::ModuleName
            }) else {
                continue;
            };

            let name = name_node.text().to_string().trim().to_string();
            if name.is_empty() {
                continue;
            }
            let range = cpp_parser::source_range(name_node.text_range());

            let resolved = match self.store.index().interface_unit_of(&name) {
                Some(path) => Some(PathBuf::from(path)),
                None => {
                    let scanner = scanner
                        .get_or_insert_with(|| ModuleScanner::new(&self.files, &self.config));
                    match scanner.resolve_module(&name, &directory, &mut interner) {
                        ImportOutcome::Resolved(unit) => scanner
                            .units()
                            .iter()
                            .find(|entry| entry.file == unit)
                            .map(|entry| entry.path.clone()),
                        _ => None,
                    }
                }
            };

            imports.push(ModuleImport {
                name,
                range,
                resolved,
            });
        }

        imports
    }

    /// **Where the `#include` under the cursor points** — the fourth answer a jump can give.
    ///
    /// The three the analysis already had are a declaration (the scope walk), an overload set (the index by name)
    /// and a macro (`#define`). A **header name** is none of them: nothing in the file declares `vector`, the scope
    /// walker never sees `<vector>` (the grammar folds it into one token inside a directive), and the macro table
    /// has no entry for it. So `textDocument/definition` on `#include <vector>` had nothing to answer with, and a
    /// reader who ctrl-clicked it was told there is no definition — which is true of every question that was asked
    /// and false about the file it names.
    ///
    /// The answer is the resolved path, from the same summary field the resolver wrote when the file was read, so
    /// this is a lookup rather than a second implementation of the search: see [`crate::header_at`].
    /// # The offset is the **file's**, because a header name is a directive's argument
    ///
    /// `#include "near.h"` is a fact about the text the reader wrote, and a rendering has the *included file's*
    /// content where that line was — so neither the directive nor the name exists in the tree a `FileView` of a
    /// rendering parses. This query therefore takes a `FileOffset` and parses the written text, exactly as
    /// `Session::documentation` does for comments, `Session::selection_chain` for selections and
    /// `Session::folding_ranges` for folds.
    pub fn header_at(&self, view: &FileView, offset: FileOffset) -> Known<crate::HeaderTarget> {
        let index = self.store.index();
        let Some(written) = view.written_text() else {
            // No rendering: this view *is* a parse of the file's own text, so its offsets are already the file's.
            return crate::header_at(index, &view.root, &view.path, offset.get());
        };
        let tree = cpp_parser::CppParser::parse(written, cpp_parser::ParserConfig::default());
        crate::header_at(index, &tree.get_red_root(), &view.path, offset.get())
    }

    /// Everywhere the macro name written at `offset` is used, across the project — the query a rename starts from.
    ///
    /// The name comes from the cursor rather than from the caller, because that is what a client has: a position,
    /// not a spelling. Everything after that is [`macro_references`], which reads file **text** — through this
    /// session's overlay, so an unsaved buffer is searched as the user typed it rather than as it is on disk.
    ///
    /// # What the answer depends on, in a session
    ///
    /// The candidates are the files the index holds. A file whose summary was dropped by an edit and has not been
    /// read again is not one of them, so its own uses are missing from the answer until [`Session::advance`] gets
    /// to it — which is the same lazy-indexing boundary every query here has, and the same rule applies: **do not
    /// show "no references" while [`Session::pending`] is non-zero.**
    ///
    /// A cursor on an ordinary identifier — a variable, a function — answers `Unknown(NotDeclaredHere)`, which says
    /// exactly what is true: nothing defines that name as a macro. This query is about macros, and references of a
    /// *name* need the scope of every candidate file, which is a parse per candidate rather than a lex.
    pub fn macro_references(
        &self,
        view: &FileView,
        offset: ReadingOffset,
    ) -> Known<MacroReferences> {
        // `name_at_including_directives` rather than `name_at`: the place a user asks this question is the name
        // itself, and half the time that name is written in a `#define` — which is tokens in a directive, not a
        // name node the scope walker sees.
        let Some((name, _)) =
            crate::sema::resolve::name_at_including_directives(&view.root, offset.get())
        else {
            return Known::Unknown(UnknownReason::UnparsableName);
        };

        macro_references(
            self.store.index(),
            &self.files,
            &name,
            ReferenceBudget::default(),
        )
    }

    /// What to offer after a member access at `offset`: the object's type, its members, and the edit.
    /// **The cursor is a FILE offset**, as it is for `Session::completions` and `Session::name_completions`: the three
    /// completion queries speak the coordinates a client sends and each crosses the seam itself. This one did not, and
    /// the cost was a panic rather than a wrong list — `rowan` was handed a file offset for a rendering tree.
    pub fn member_completions(
        &self,
        view: &FileView,
        offset: FileOffset,
    ) -> Known<MemberCompletions> {
        let Some(offset) = view.reading_offset_of(offset) else {
            // A cursor the rendering does not have - in a region a macro replaced, or past its end.
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("")));
        };
        member_completions_at(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            offset.get(),
        )
    }

    /// What to offer at a cursor with no member access: the scope's names, then the index's.
    pub fn name_completions(&self, view: &FileView, offset: FileOffset) -> Known<NameCompletions> {
        // **The cursor is a FILE offset; the tree is the RENDERING's.** These two coincided while a view was a parse
        // of the file's own text, so no caller had to know they were two things — and the moment `view` became the
        // reading a compiler is handed they stopped coinciding, by exactly the macro expansion. Measured on the
        // completion fixture: `Bad offset: range 0..6 offset 12`, where 6 is the rendering's length (`int x;`) and 12
        // is a position in the 18-byte file.
        //
        // The translation belongs **here** rather than in each handler: every caller holds a view and a cursor, the
        // pair is always the same pair, and a handler that forgot would get a wrong answer rather than an error —
        // which is exactly what happened three times while this boundary went in. See [`FileView`]'s note on the two
        // coordinate systems: an offset in the file goes in through `reading_offset_of`, and a range in the rendering
        // comes back out through `file_offset_of`.
        // **Prose first, and asked of the file's own text.** A comment and a literal are things a rendering has none
        // of, so a cursor in one arrives at the scope walk as an ordinary position and the answer is the file's names,
        // offered into a sentence. Measured: `a_comment_and_a_literal_offer_nothing` failed against this query while
        // `completions` was already refusing.
        if let Some(written) = view.written_text()
            && crate::file::view::in_prose(written, offset.get())
        {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("")));
        }

        let Some(offset) = view.reading_offset_of(offset) else {
            // The cursor is not in the rendering at all — in a region a macro replaced, or past its end. There is no
            // name to offer, and `Unknown` says so rather than answering about the nearest thing that is.
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("")));
        };
        crate::index::project::name_completions_traced(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            offset.get(),
        )
    }

    /// **The completion list for a cursor** — the layer that decides *which* question the cursor is asking.
    ///
    /// The two queries above answer "what is visible here" and "what does this type have"; this one reads the
    /// shape at the cursor, picks between them, adds the language's own vocabulary (keywords, snippets, directives,
    /// headers), and orders the result by how near each declaration is to the reader. See
    /// [`crate::completion`] for the whole of the reasoning; the short version is that "every name reachable
    /// through the include graph" is a *correct* answer and a useless one, and the difference is the order.
    ///
    /// # The one thing this needs that the session does not already hold
    ///
    /// An `#include`'s answer is a **file name**, and nothing in the index or the tree knows one: the index holds
    /// the files somebody has read, and a reader typing `#include <chro` wants the header that is on the search
    /// path whether or not any file has included it. So the search path is walked once, when the session is built,
    /// and the project's own headers are listed from [`Session::project_files`] — see
    /// [`crate::completion::header_index`].
    ///
    /// # `is_incomplete` is the caller's to decide
    ///
    /// This returns [`CompletionSet::truncated`](crate::CompletionSet::truncated) — "the budget cut this list" —
    /// and it is deliberately **not** [`Session::pending`]. The two are different claims and a client reacts to them
    /// differently: pending work means "ask me again as you type, a name may be missing for the only reason that
    /// its file has not been read yet", while a capped list means the opposite ("do not ask again, this is the best
    /// I have"). A caller that merged them would leave a client retrying a list that cannot change.
    /// # What the two halves cost
    ///
    /// The header index is **cloned** rather than borrowed — see [`Session::headers`] — and on a project the size of
    /// the latency fixture that clone measures **0 ms**, against 350–1270 ms for the query beside it while the index
    /// was filling. It is named here because it is the obvious suspect for a per-keystroke cost and it is not one.
    pub fn completions(&self, view: &FileView, offset: FileOffset) -> crate::CompletionSet {
        // **The list, then the edits that go with it.** A name offered from a header this file does not include is only
        // usable once the `#include` is there, so every item that carries an [`Inclusion`](crate::completion::Inclusion)
        // gets its insertion here — the position is a line in the **written** text, which the completion itself never
        // sees (it is computed on the rendering).
        let mut offered = self.completions_without_the_edits(view, offset);
        self.attach_the_inclusions(&mut offered, &view.path);
        offered
    }

    /// **The files this file could include to gain a name it does not have** — the completion side of
    /// `missing-include`.
    ///
    /// Only **strongly related** files, which is what keeps the list small and the offer believable: a file in the same
    /// directory, a header or module interface under a `[[library]]` the project names, or one under this file's own
    /// include directories. A file already reached by an `#include` is not a candidate (its names are visible), and
    /// `[completion] suggest_unincluded = false` turns the whole thing off.
    fn the_files_this_file_could_include(
        &self,
        path: &std::path::Path,
    ) -> Vec<crate::completion::Unincluded> {
        if !self
            .project_config()
            .config
            .completion
            .suggest_unincluded
            .unwrap_or(true)
        {
            return Vec::new();
        }
        let include_paths = self.config_for(path).include_paths;
        let reached = self.store.index().include_closure(path);
        let directory = path.parent().map(|parent| parent.to_path_buf());
        let libraries: Vec<PathBuf> = self
            .project_config()
            .config
            .libraries
            .iter()
            .map(|library| library.root(&self.root))
            .collect();

        const MOST: usize = 64;
        let mut found = Vec::new();
        for file in &self.project {
            if found.len() >= MOST {
                break;
            }
            if file == path {
                continue;
            }
            let extension = file
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_lowercase());
            let is_header = matches!(
                extension.as_deref(),
                Some("h" | "hpp" | "hxx" | "h++" | "inl" | "ipp")
            );
            let is_interface = matches!(extension.as_deref(), Some("cppm"));
            if !is_header && !is_interface {
                continue;
            }
            // **Already reached** by an `#include`, transitively: its names are visible, so there is nothing to add.
            let key = crate::file::paths::normalize_path(file, cfg!(windows));
            if reached.contains(&key) {
                continue;
            }
            let related = directory.as_deref() == file.parent()
                || libraries.iter().any(|root| file.starts_with(root))
                || include_paths
                    .iter()
                    .any(|held| file.starts_with(&held.directory));
            if !related {
                continue;
            }
            let inclusion = match self
                .store
                .index()
                .summary(file)
                .and_then(|summary| summary.modules.module.clone())
            {
                Some(module) => crate::completion::Inclusion::Import(module.to_string()),
                None => crate::completion::Inclusion::Include(
                    crate::completion::the_include_spelling(file, path, &include_paths),
                ),
            };
            found.push(crate::completion::Unincluded {
                file: file.clone(),
                inclusion,
            });
        }
        found
    }

    /// **Attach the `#include`/`import` each item carries**, as an edit at the file's include block.
    ///
    /// The completion layer knows *what* to add ([`crate::completion::Inclusion`]) and the session knows the file's
    /// text, which is where the *position* lives — after the last `#include` for one kind, after the module declaration
    /// or last `import` for the other. An item whose inclusion cannot be placed (a file whose text is not the one the
    /// completion was computed on) keeps its name and loses its edit, which is the harmless direction: the reader sees
    /// the name and no include, rather than an edit at an offset that may have moved.
    fn attach_the_inclusions(&self, offered: &mut crate::CompletionSet, path: &std::path::Path) {
        if !offered.items.iter().any(|item| item.inclusion.is_some()) {
            return;
        }
        let Some(text) = self.vfs.held(path).map(|held| held.text.to_string()) else {
            return;
        };
        let after_includes = after_the_includes(&text);
        let after_imports = after_the_module_imports(&text);
        for item in &mut offered.items {
            let Some(inclusion) = &item.inclusion else {
                continue;
            };
            let at = match inclusion {
                crate::completion::Inclusion::Include(_) => after_includes,
                crate::completion::Inclusion::Import(_) => after_imports,
            };
            item.additional_edits
                .push((cpp_parser::SourceRange::new(at, 0), inclusion.line()));
        }
    }

    /// **The completion before the `#include` edits are attached** — see [`Session::completions`].
    fn completions_without_the_edits(
        &self,
        view: &FileView,
        offset: FileOffset,
    ) -> crate::CompletionSet {
        // **The headers this file could add**, and what accepting one of their names has to edit — see
        // `Session::the_files_this_file_could_include`.
        let unincluded = self.the_files_this_file_could_include(&view.path);
        // **The cursor is a FILE offset; the tree is the RENDERING's** — see [`Session::name_completions`], where the
        // full note is. A cursor that is not in the rendering has nothing to offer, and an empty set is the honest
        // answer rather than a list computed from the nearest position that *is* in it.
        // **The shapes that live in the file''s own text, decided before anything crosses the seam.**
        //
        // A rendering has none of them in it: a `#include` line is replaced by what it includes, a `#define` by
        // nothing, a comment is removed before the parse. So a cursor in one arrives at the context reader as whatever
        // the renderer put in that place - measured, `#inc` reached the tree at offset 0, so the replacement range for
        // `include` came back at the **start of the file**.
        //
        // Their answers need nothing from the reading either, which is why this is a shortcut rather than a special
        // case: directive names are a constant table, a header name comes from the search path, prose has no
        // suggestion to make, and a `#define`''s argument is the program''s names. The offset handed over is the
        // **file''s**, so the replacement range a client edits is in the buffer it is showing.
        //
        // **`offset` here is still the parameter**: one statement lower it is the *reading''s*, and for `"#inc\nint
        // x;\n"` the cursor at file offset 4 maps into the rendering at 0 - the directive is gone - so the written
        // text was asked at 0, before the `#`, and the list came from the vocabulary''s `include` *snippet*.
        // **A module name is the file's own question, asked first.** `import ` names a file, so the answer needs the
        // *project* rather than the reading — and unlike a directive, the context is the same in both readings
        // (`import math;` survives the preprocess untouched), so the file's text is where it is read from. Asked
        // before the `written_text` branch below because a view of the file's own text has no `written_text` at all,
        // and that branch is what a *rendering* is answered through.
        {
            let source: &str = view.written_text().unwrap_or(&view.source);
            if let Some((written, range)) =
                crate::completion::context::module_name_from_the_text(source, offset.get())
            {
                return self.module_completions(&written, range);
            }
        }

        if let Some(written) = view.written_text() {
            // **A documentation comment is prose to the renderer and a language of its own to a reader.**
            //
            // The refusal below is right for every other question — a `w.` in a comment is a member access only to
            // a tree — but a comment produces **no tokens at all** in the rendering (the cook removes it before the
            // parse), so the doc-comment position cannot be read from the reading at any offset. It is asked of the
            // written text instead, and the offset it answers with is the **file's**, which is the buffer the reader
            // is editing: same shortcut as a `#include`, and for the same reason.
            //
            // Asked before `in_prose`, which is the line that would otherwise turn the whole of a comment away.
            let tree = cpp_parser::CppParser::parse(written, cpp_parser::ParserConfig::default());
            let written_root = tree.get_red_root();
            if let Some(context @ crate::completion::CompletionContext::DocComment { .. }) =
                crate::completion::doc_comment_at(&written_root, offset.get())
            {
                return crate::completion::completion_with(
                    self.store.index(),
                    &view.scopes,
                    &view.root,
                    &view.path,
                    offset.get(),
                    &self
                        .headers
                        .read()
                        .map(|headers| headers.clone())
                        .unwrap_or_default(),
                    context,
                    self.config.cuda(),
                    self.the_completion_budget(),
                    &[],
                );
            }

            if crate::file::view::in_prose(written, offset.get()) {
                return crate::CompletionSet::default();
            }
            let context = crate::completion::context_at(&written_root, offset.get());
            match context {
                // **The program is in the rendering; the buffer is in the file.** The `replace` range is carried by
                // the context (read from the written text) and the *lookup* needs a reading offset - the one the
                // cursor maps to, or the rendering''s end when it maps nowhere (measured: `one::` at the end of a
                // file has no position in the rendering at all, and the answer was an empty set).
                crate::completion::CompletionContext::DirectiveArgument { .. }
                | crate::completion::CompletionContext::Qualified(_) => {
                    let in_the_reading = view
                        .reading_offset_of(offset)
                        .map_or(view.source.len(), |at| at.get());
                    return crate::completion::completion_with(
                        self.store.index(),
                        &view.scopes,
                        &view.root,
                        &view.path,
                        in_the_reading,
                        &self
                            .headers
                            .read()
                            .map(|headers| headers.clone())
                            .unwrap_or_default(),
                        context,
                        self.config.cuda(),
                        self.the_completion_budget(),
                        &[],
                    );
                }
                // **`MacroBody` is here too**: a `#define` line is in the file and not in the rendering, and the
                // answer needs no reading position at all — the parameters come from the line, the macros from the
                // summary.
                context @ (crate::completion::CompletionContext::DirectiveName { .. }
                | crate::completion::CompletionContext::Include { .. }
                | crate::completion::CompletionContext::MacroBody { .. }
                | crate::completion::CompletionContext::Condition { .. }) => {
                    return crate::completion::completion_with(
                        self.store.index(),
                        &view.scopes,
                        &view.root,
                        &view.path,
                        offset.get(),
                        &self
                            .headers
                            .read()
                            .map(|headers| headers.clone())
                            .unwrap_or_default(),
                        context,
                        self.config.cuda(),
                        self.the_completion_budget(),
                        &[],
                    );
                }
                // **`Nothing` is an answer, not a miss**: prose, and a directive that takes no argument (`#endif `).
                crate::completion::CompletionContext::Nothing => {
                    return crate::CompletionSet::default();
                }
                _ => {}
            }
        }

        let Some(offset) = view.reading_offset_of(offset) else {
            return crate::CompletionSet::default();
        };

        // The header index through its lock, and **an empty one on a poisoned lock**: a completion is a suggestion
        // list, and a thread that panicked while adding project files is no reason to fail the request — the
        // declarations below are the answer the reader came for.
        let headers = self
            .headers
            .read()
            .map(|headers| headers.clone())
            .unwrap_or_default();

        crate::completion::completion_at(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            offset.get(),
            &headers,
            self.config.cuda(),
            self.the_completion_budget(),
            &unincluded,
        )
    }

    /// **The headers the search path can name** — what a `#include` is completed from.
    ///
    /// A clone rather than a borrow, because the answer is behind a lock that is written when the project's file
    /// list grows. See [`Session::completions`] for the whole reason.
    pub fn headers(&self) -> crate::HeaderIndex {
        self.headers
            .read()
            .map(|headers| headers.clone())
            .unwrap_or_default()
    }

    /// **The documentation comment above a declaration** — what a hover shows when a file writes one.
    ///
    /// `file` and `offset` are the declaration's own coordinates, which is why they are arguments rather than
    /// something read from `view`: the fact a query resolved carries them, and the declaration can be in a file
    /// other than the one the cursor is in.
    ///
    /// # The one file that is not re-parsed
    ///
    /// The declaration in the file being edited is answered from the view the request already built, so a hover in
    /// the current file costs nothing extra. A declaration in another file needs that file's tree, which this
    /// session does not keep — a parse per question is the honest price of not caching a tree per file — and it is
    /// paid **only when the text before the declaration could hold a comment at all**
    /// ([`crate::file::view::might_be_documented`]). A header with no documentation above the declaration is
    /// therefore never parsed to find that out.
    ///
    /// The answer is the parser's node, not text: what a comment *says* — delimiters, line markers, `@param` — is
    /// the documentation layer's reading, and every consumer asking this question wants that reading rather than a
    /// copy of it.
    pub fn documentation(
        &self,
        view: &FileView,
        file: &Path,
        offset: usize,
    ) -> Option<cpp_parser::CppDocComment> {
        if file == view.path {
            // **The file's own text, for exactly the reason the branch below parses another file's.**
            //
            // A rendering is what a compiler reads, and a compiler reads no comments: they are removed before the
            // parse, so a `DocComment` node cannot exist in `view.root` and this lookup could only ever answer
            // `None` for a documented declaration. Measured: `Bad offset: range 0..6 offset 9` on
            // `"/// Doc.\nint x;\n"`, whose rendering is the six bytes of `int x;` — the comment *and* its line are
            // gone, so the offset a caller has (the file's) is past the tree as well as beside the comment.
            //
            // The other file's branch already accepts this price — "a parse per question is the honest price of not
            // caching a tree per file" — and the current file is the one a person is most likely to hover, so the
            // same price is paid here rather than a different answer being given for it. `might_be_documented` is
            // asked first for the same reason it is asked there: a declaration with nothing above it costs no parse.
            let Some(written) = view.written_text() else {
                // No rendering: this view *is* a parse of the file's own text, and its comments are its own nodes.
                return crate::file::view::documentation_at(&view.root, offset);
            };
            if !crate::file::view::might_be_documented(written, offset) {
                return None;
            }
            let tree = cpp_parser::CppParser::parse(written, cpp_parser::ParserConfig::default());
            return crate::file::view::documentation_at(&tree.get_red_root(), offset);
        }

        let declared_in = self.vfs.held(file)?;
        if !crate::file::view::might_be_documented(&declared_in.text, offset) {
            return None;
        }

        let tree =
            cpp_parser::CppParser::parse(&declared_in.text, cpp_parser::ParserConfig::default());
        crate::file::view::documentation_at(&tree.get_red_root(), offset)
    }

    /// **The type of the expression at `offset`**, as far as this analysis can tell — what a hover shows when the
    /// cursor is not on a name.
    ///
    /// The three answers are three different facts and a caller should keep them apart:
    ///
    /// * [`Known::Yes`] with the expression's text, its type, and the file the type came from — `this` in a member
    ///   function, `*p`, `f(x)`, `w.size`, a plain name;
    /// * [`Known::No`] when the cursor is in **no expression at all** — a `#define`, the punctuation between two
    ///   statements, a declaration's `const`. The question does not apply, which is not the same as an answer that
    ///   is missing;
    /// * [`Known::Unknown`] with the infer layer's own reason when the expression is there but its type is not
    ///   knowable here: a template that was never instantiated, a name nothing declares, a type computed rather
    ///   than read.
    ///
    /// # What "the expression" is
    ///
    /// The **innermost** one containing the offset ([`crate::sema::resolve::expression_at`]), so a cursor on
    /// `this` in `this->size` asks about `this`. A cursor in whitespace inside a statement is in no expression, and
    /// that is also the honest answer.
    /// **The type of this expression** — [`Session::type_at`] for a node the caller already holds.
    ///
    /// The offset form asks about the **innermost** expression containing a byte, which is the right question for a
    /// cursor and the wrong one for a caller that already has the expression: `Pair<int, int>{1, 2}`'s last byte is
    /// inside the braced list, so the offset question answered the *list*'s type for the object whose members were
    /// being asked about — and a hover said "`int` has no member `sum`" for a `Pair<int, int>`.
    pub fn type_of_the_expression(
        &self,
        view: &FileView,
        expression: &cpp_parser::CppSyntaxNode,
    ) -> Known<ExpressionType> {
        let written = expression.text().to_string().trim().to_string();
        match crate::index::project::type_of_expression(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            expression,
            0,
        ) {
            Known::Yes((type_of, file)) => Known::Yes(ExpressionType {
                expression: written,
                type_of: type_of.to_string(),
                file,
            }),
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::No,
        }
    }

    pub fn type_at(&self, view: &FileView, offset: ReadingOffset) -> Known<ExpressionType> {
        let Some(expression) = crate::sema::resolve::expression_at(&view.root, offset.get()) else {
            return Known::No;
        };

        let written = expression.text().to_string().trim().to_string();

        match crate::index::project::type_of_expression(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            &expression,
            0,
        ) {
            Known::Yes((type_of, file)) => Known::Yes(ExpressionType {
                expression: written,
                type_of: type_of.to_string(),
                file,
            }),
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::No,
        }
    }

    /// **The member of the class the cursor is inside that `name` names** — C++'s unqualified member lookup.
    ///
    /// The hover's question, for a name the scope tree and the index cannot place: `shared_from_this()` inside a class
    /// deriving `std::enable_shared_from_this<Node>` is a member of the **base**, and no scope in the file declares it.
    /// `members_of` answers it, substituted, and this is the one-reader wrapper the hover uses. `None` when the cursor is
    /// not inside a class member, or the name is not a member of that class.
    pub fn member_of_the_enclosing_class(
        &self,
        view: &FileView,
        offset: ReadingOffset,
    ) -> Option<ProjectDefinition> {
        let callee = crate::sema::resolve::name_node_at(&view.root, offset.get())?;
        crate::index::project::a_member_of_the_enclosing_class(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            &callee,
            false,
        )
    }

    /// **The parameter names to draw at the calls in `range`** — see [`crate::inlay`].
    ///
    /// `range` is the range the client asked about, in the file's own coordinates. Everything this needs beyond
    /// the file in hand is the *callee's* file, which is why it is a session method: this is how a header a call
    /// reaches into is parsed, and it is parsed once per declaring file however many calls reach it.
    ///
    /// # Why the declaring file is read **as itself**, and not through [`Session::view`]
    ///
    /// Because `callee.name_offset` is an offset in the declaring file's **own text** — that is what a summary
    /// records — and `view` hands back the compiler's *rendering* whenever one is cached. A rendering has no line
    /// breaks and no comments, so the same number names a different place in it, `parameter_list_of` finds no
    /// declarator there, and the call yields **no hint**.
    ///
    /// Measured, and it is the whole of a report that said the hints came and went: an edit drops the rendering, so
    /// the request made **immediately** after a keystroke read the file as itself and hinted correctly; two seconds
    /// later the pump had rebuilt the rendering, the same call resolved to the same declaration with the same offset,
    /// and the parameter list could no longer be read. The refresh the pump sends then asks the client to ask again —
    /// and the second answer was the same broken one, which is why the hints did not come back.
    pub fn inlay_hints(
        &self,
        view: &FileView,
        range: cpp_parser::SourceRange,
    ) -> Vec<ParameterHint> {
        let mut hints = crate::inlay::parameter_hints(self.store.index(), view, range, |path| {
            self.view_of_the_file(path)
        });

        // **The range going in is the rendering's; the offsets coming out are the file's.**
        //
        // That asymmetry is not an accident and it is worth stating, because it is the shape of this seam: the walk
        // has to read a **tree**, so the range it is given is an offset in the rendering, while a hint is drawn in the
        // reader's own buffer — and the handler places it with `position_in_file(file, hint.offset)`, the *file's* line
        // index, as does `tests/inlay.rs`. Moving the offset here rather than in the handler is the same choice
        // `Session::classified_names` makes one function up, and for the same measured reason: the two consumers
        // agreeing is the contract, and a hint placed at an offset the cook shifted is a hint on **another line**.
        //
        // A hint the file cannot place is dropped, exactly as the handler drops one it cannot place ("a position the
        // file cannot place produces no hint rather than one at a guessed place").
        for hint in &mut hints {
            let Some(placed) = view.file_offset_of(ReadingOffset::new(hint.offset)) else {
                continue;
            };
            hint.offset = placed.get();
        }

        hints
    }

    /// **The types the file does not write** — `auto`, a range-for's element, each name of a structured binding, and
    /// the return type an `auto` function deduces. See [`crate::inlay::type_hints`] for the three shapes and the three
    /// refusals.
    ///
    /// The type of an expression and the return type of a declaration are both **this** session's questions, which is why
    /// the walk lives in `inlay` and the answers are handed to it: one reader of "what type is this", the same one the
    /// hover uses.
    ///
    /// The offsets are moved back into the file's coordinates exactly as [`Session::inlay_hints`] does, and for the same
    /// reason — a hint is drawn in the reader's own buffer.
    pub fn type_hints(
        &self,
        view: &FileView,
        range: cpp_parser::SourceRange,
    ) -> Vec<crate::inlay::TypeHint> {
        let mut hints = crate::inlay::type_hints(
            view,
            range,
            |at| match self.type_at(view, ReadingOffset::new(at)) {
                Known::Yes(found) => Some(found.type_of),
                _ => None,
            },
            // **The index's fact, not the binding's.** A name in this file is answered from the *binding* builder, which
            // records the return type the file wrote — and an `auto` return has none. The deduction
            // (`deduced_returns_of`) runs in the index's builder, so the fact that says `int` for `auto one()` is the
            // summary's; the local fact stays as the fallback for a file the index has not caught up with.
            |at| {
                let local = self.definition(view, ReadingOffset::new(at)).value();
                let indexed = local.as_ref().and_then(|found| {
                    self.store
                        .index()
                        .definition(&found.fact.qualified_name(), &view.path)
                        .value()
                });
                indexed
                    .and_then(|found| found.fact.returns)
                    .or_else(|| local.and_then(|found| found.fact.returns))
            },
        );
        for hint in &mut hints {
            let Some(placed) = view.file_offset_of(ReadingOffset::new(hint.offset)) else {
                continue;
            };
            hint.offset = placed.get();
        }
        hints
    }

    /// **What each name in this file is** — the classification a semantic highlighter draws colours from.
    ///
    /// Read from the file's own bindings and macro directives, and from the index for a spelling the file does not
    /// declare itself; a name nothing can be said about is left out rather than guessed at. See
    /// [`crate::semantic`] for the four questions, in the order they are asked, and for the measurement behind
    /// asking the index **once per spelling** rather than once per identifier.
    ///
    /// Nothing here is a position question, so the answer does not depend on a cursor — but it does depend on the
    /// index, and a file whose includes have not been read yet answers with **fewer** classifications rather than
    /// with different ones. A caller that publishes these should therefore publish them again once
    /// [`Session::pending`] reaches zero.
    pub fn classified_names(&self, view: &FileView) -> Vec<crate::semantic::Name> {
        use cpp_parser::CppTokenKind;
        let mut names = crate::semantic::classified_names(self.store.index(), view);

        // **The predefined macros, which no file declares.** `__LINE__`, `__FILE__`, `__DATE__`, `__TIME__`,
        // `__cplusplus` and the compiler's own five hundred are defined by the *compiler*, so the walk above — which
        // classifies a name by the declaration it resolves to — has nothing to say about them, and an editor drew
        // `__LINE__` as "standard token type: Other" (reported, with the inspector's own reading). The two tables are
        // this session's: the standard's own (a constant, per language) and the toolchain's `-dM` output.
        //
        // The tokens are the **rendering's**, like the walk's, and this runs before the placement loop for the same
        // reason: a range still in rendering coordinates is what that loop translates.
        let language = self.config_for(&view.path).language_for(&view.path);
        let mut claimed: std::collections::HashSet<usize> =
            names.iter().map(|name| name.range.start_offset).collect();
        for token in view.tree.get_tokens() {
            if token.kind != CppTokenKind::Identifier {
                continue;
            }
            // A predefined macro is spelled with leading underscores on both sides; nothing else in the file needs the
            // two table lookups below.
            let Some(text) = view.get_token_text(token.range) else {
                continue;
            };
            if !text.starts_with('_') {
                continue;
            }
            let start = token.range.start_offset;
            if claimed.contains(&start) {
                continue;
            }
            // **Three sources, because no one of them holds them all**: the standard's constant macros
            // (`__cplusplus`), the standard's position-dependent ones (`__LINE__`, which `-dM` never prints), and the
            // toolchain's `-dM` table (`__GNUC__`). Reported from an editor: `__LINE__ << __FILE__ << __DATE__ <<
            // __TIME__` was drawn as plain identifiers.
            let is_predefined = the_standards_own_macro(text, &language).is_some()
                || a_macro_whose_value_depends_on_the_use(text)
                || self.predefined_macro(text, &view.path).is_some();
            if !is_predefined {
                continue;
            }
            claimed.insert(start);
            names.push(crate::semantic::Name {
                range: token.range,
                kind: crate::semantic::NameKind::Macro,
                declaration: false,
                modifiers: Vec::new(),
                provenance: crate::semantic::Provenance::Macro,
            });
        }

        // **The ranges are moved out of the rendering and into the file, because that is what both consumers read.**
        //
        // The walk reads `view.root`, so every range it answers is an offset in the **rendering** — and the two
        // consumers of this query take it for an offset in the **file**: the semantic-tokens handler computes the
        // client's line and column with `position_in_file(file, name.range.start_offset)` and measures the token with
        // `&file.text[range]`, and `tests/semantic.rs` slices the fixture's own text the same way. Two independent
        // consumers agreeing is the contract; the query was the side that was wrong, and it is user-visible: a colour
        // drawn at an offset the cook shifted is a colour on **another word**.
        //
        // Measured on the suite: `the_header's_class` read `[("l", Function), ("de \"b.", Type), ("\n", Parameter)]`
        // — the tokens of a rendering sliced out of the file. A name whose start the file cannot place is **dropped**,
        // which is the same choice the handler makes one layer up ("dropping one token is the only option that keeps
        // the walk valid"): a name in a region the rendering does not cover — an untaken `#if` branch, the body of a
        // macro — has no file position at all, and keeping it would put a token at an offset that means something else.
        let mut placed = Vec::with_capacity(names.len());
        for mut name in names {
            let Some(start) = view.file_offset_of(ReadingOffset::new(name.range.start_offset))
            else {
                continue;
            };
            let end = view
                .file_offset_of(ReadingOffset::new(name.range.end_offset()))
                .unwrap_or(start);
            name.range =
                cpp_parser::SourceRange::new(start.get(), end.get().saturating_sub(start.get()));
            placed.push(name);
        }
        names = placed;

        // **And the identifiers only the file's own text has.**
        //
        // The walk iterates `view.tree.get_tokens()` — the **rendering's** tokens — so a name the cook removed is not
        // in it at all: an identifier in an untaken `#if` branch, or in the body of a function-like macro. Those are
        // names the reader is looking at, and the module's own words are that this reads "the file's own bindings and
        // macro directives". `a_macro_in_an_untaken_branch_is_still_a_macro` is the test that says so, and it answered
        // `None` against `Some(Macro)`.
        //
        // A parse of the file's own text sees them, **in the file's coordinates already** — so nothing here is
        // translated, and the two passes meet in one numbering. Deduplicated by where the name starts, with the
        // rendering's answer winning: it is the one that carries the macro and binding facts.
        //
        // The guard is "the two readings differ", because a file the cook did not change needs no second pass — and
        // the cost when they do differ is a second walk, which the `Classify` stage timer in
        // `examples/semantic_probe.rs` measures.
        if view
            .written_text()
            .is_some_and(|written| written != &*view.source)
            && let Some(own) = self.view_of_the_file(&view.path)
        {
            // **Where the two passes disagree about one name, the file's own text wins.** Measured both ways: keeping
            // the rendering's answer for a shared start lost `(Parameter, false)` — a use the cook rewrote — and
            // keeping *both* broke `the_classifications_are_sorted_and_disjoint`. A colour is drawn on the reader's
            // text, so for two answers about one offset the one read from that text is the one to draw.
            let mut at_offset: std::collections::HashMap<usize, usize> = names
                .iter()
                .enumerate()
                .map(|(index, name)| (name.range.start_offset, index))
                .collect();
            for name in crate::semantic::classified_names(self.store.index(), &own) {
                match at_offset.get(&name.range.start_offset) {
                    Some(&index) => names[index] = name,
                    None => {
                        at_offset.insert(name.range.start_offset, names.len());
                        names.push(name);
                    }
                }
            }
        }

        // **And re-sorted, because the crossing is not order-preserving.** The walk answers in the rendering's order,
        // and a token a macro produced maps back to *where the macro was invoked* — which can be before a name that
        // preceded it in the rendering. The consumer builds a **delta walk** (`SemanticTokens`, where every token is
        // relative to the one before it), so an out-of-order list does not merely look odd: the deltas would be
        // negative. Sorted here rather than by the caller, because this is where the order stopped being the
        // rendering's.
        names.sort_by_key(|name| (name.range.start_offset, name.range.end_offset()));

        names
    }

    /// **Every format string in this file, with what each part of it is** — see [`crate::format`].
    ///
    /// ```cpp
    /// std::println("值 = {:.3f}", x);    `{:.3f}` is a specifier and `f` a conversion
    /// printf("%-8.2f|%*d", y, w);        a specifier, a conversion, and a width taken from an argument
    /// scanf("%3d %[a-z]", &n, name);     and the scanset
    /// ```
    ///
    /// # Why the *written* reading, and only the written reading
    ///
    /// A colour is painted on the text the reader has in the buffer, and the answers here are **file offsets**. A
    /// rendering's offsets are its own (see [`FileView::file_offset_of`]), and a format string is a *literal*: the cook
    /// never expands one, but it does move everything after the macros it did expand. So this reads
    /// [`Session::view_of_the_file`] and refuses a rendering — the same rule `push_doc_commands` one layer up states
    /// for a comment, and for the same reason.
    ///
    /// # Why the tokens rather than the tree
    ///
    /// The format string is there either way, and where the parser put it is not a question this
    /// pass has an opinion about: the call is "a `(` whose arguments are…", which the token stream
    /// says exactly. See [`crate::format::literals_in`].
    pub fn format_literals(
        &self,
        view: &FileView,
        config: &crate::format::FormatConfig,
    ) -> Vec<crate::format::FormatLiteral> {
        // **A rendering is not this pass's text.** Its tree's tokens are the cook's, and a range in it names a place
        // in a string the reader has never seen.
        if view.written_text().is_some() {
            return Vec::new();
        }

        // **A name the file does not qualify is resolved, and only where it decides something**: `format("…")` after
        // `using namespace std;` is the standard one, and a project's own `format` is not.
        //
        // The instrument is the **binding** and its scope, not `definitions`: that query answers "where do I jump to"
        // and fills `DeclFact::scope` with the qualifier *the cursor wrote*, which for a bare name is no scope at
        // all — measured, a bare `format(…)` resolved to `format` with `scope: None` in both spellings of the using
        // (`using namespace std;` and `using std::format;`). What this needs is the namespace the declaration sits
        // in, which is the scope the binding lives in, and `ScopeTree::qualified_name_of` spells it.
        //
        // A **using-declaration** is the one shape that still answers with the wrong thing: `using std::format;`
        // binds the name to the using-declaration itself (which is where go-to-definition goes), so the spelling
        // resolves to `format` at file scope and is refused. `using namespace std;` is followed properly, and a
        // qualified `std::format(…)` needs neither.
        let mut qualified_name = |offset: usize| -> Option<String> {
            let reading = view.reading_offset_of(FileOffset::new(offset))?;
            let Known::Yes(binding) =
                crate::sema::resolve::definition_at(&view.scopes, &view.root, reading.get())
            else {
                return None;
            };
            let name = binding.name.identifier_text()?;
            Some(match view.scopes.qualified_name_of(binding.scope) {
                Some(scope) if !scope.is_empty() => format!("{scope}::{name}"),
                _ => name.to_string(),
            })
        };

        crate::format::literals_in(
            &view.source,
            view.tree.get_tokens(),
            config,
            &mut qualified_name,
        )
    }

    /// **The ranges to select at a cursor, in the file's own coordinates** — the third fact that belongs to the text
    /// the *reader* is looking at rather than to the text the compiler reads.
    ///
    /// A chain answers in the coordinates of the tree it walks, and every view this session holds is of a
    /// **rendering**. Translating in and out does not repair that: a cursor in whitespace maps *forward* to the next
    /// token — right for a cursor — so a walk of the rendering begins at a token that starts **after** the cursor and
    /// builds a rung that does not contain the position it was built for. Measured: the cursor at file offset 9 in
    /// `namespace ns` produced a chain whose innermost rung began at 10.
    ///
    /// Folding answers the same question the same way for the same reason, and so does documentation for comments.
    /// **Three facts, one rule**: a selection, a fold and a doc comment are things the reader sees in their buffer,
    /// so they are asked of the buffer.
    pub fn selection_chain(
        &self,
        view: &FileView,
        offset: FileOffset,
    ) -> Vec<cpp_parser::SourceRange> {
        let Some(written) = view.written_text() else {
            // No rendering: this view *is* a parse of the file's own text, so its offsets are already the file's.
            return view.selection_chain(ReadingOffset::new(offset.get()));
        };
        let tree = cpp_parser::CppParser::parse(written, cpp_parser::ParserConfig::default());
        crate::file::view::selection_chain_of(&tree.get_red_root(), offset.get())
    }

    /// **The file's foldable regions** — see [`crate::folding`].
    ///
    /// Read from the **buffer's own tokens and directives**, which is the same source the outline uses and for the
    /// same reason: folding is about the file in front of the reader, so an unsaved edit is folded as it is typed,
    /// and a declaration in a branch nobody takes is a region like any other. Nothing here consults the index, the
    /// cooked reading, or any other file — a fold is a fact about this text.
    pub fn folding_ranges(&self, view: &FileView) -> Vec<crate::folding::Fold> {
        // **The file in front of the reader, which is not the text this view parses any more.**
        //
        // The promise above — "the buffer's own tokens and directives" — was free while a view *was* the file's own
        // text: `view.source` and the written text were one string. They stopped being one when the raw parse was
        // deleted and a view became the compiler's reading, and this function was left reading the wrong one. A
        // rendering has no `#include` line, no comment and no `#if` region in it — those are what a preprocessor
        // removes — and it usually has fewer line breaks, so any fold it could still see is thrown away by the rule
        // that a region on one line hides nothing. Measured over the wire: `textDocument/foldingRange` on a
        // four-region fixture answered **`[]`**.
        //
        // Folding is a question about *layout* rather than about meaning — where the braces are, which lines a
        // comment covers, where an `#if` closes — so reading the file's own text is not the "semantic view of
        // unexpanded C++" that was deliberately removed; it is the only text those regions exist in. The lexer runs
        // here rather than being kept because only this query wants these tokens, and only when a client asks.
        let Some(written) = view.written_text() else {
            // No rendering: this view *is* the file's own text, and its tokens are already the right ones.
            return crate::folding::folding_ranges(&view.source, view.tree.get_tokens());
        };
        let (tokens, _) = cpp_parser::lex(written, &cpp_parser::LexerConfig::default());
        crate::folding::folding_ranges(written, &tokens)
    }

    /// **The signatures of the call the cursor is inside** — see [`crate::signature`].
    ///
    /// A list, because a call to an overloaded function has more than one declaration and the protocol's
    /// `SignatureHelp.signatures` is a list: `std::format(` is four of them, and answering one — or none, which is
    /// what this did while the name looked ambiguous — is a popup with nothing in it. The reader picks; this layer
    /// claims nothing about which overload a half-typed argument list means.
    ///
    /// The parameters come from the *callee's* declarations, so a call into a header parses that header for this
    /// answer — **once per file**, however many overloads it holds.
    ///
    /// The documentation above each declaration is read here rather than by the caller: it is the same declaration
    /// the signature came from, and asking twice would be two lookups for one popup.
    pub fn signatures_at(
        &self,
        view: &FileView,
        offset: ReadingOffset,
    ) -> Vec<crate::signature::CallSignature> {
        let mut signatures =
            crate::signature::signatures_at(self.store.index(), view, offset.get(), |path| {
                self.view(path)
            });

        for signature in &mut signatures {
            // **The declaration's offset is the *index's*, and the index reads the cooked rendering.** So
            // `declared_at` is a **reading** offset, while `Session::documentation` looks for the comment in the text
            // the reader **wrote** - and the two differ by exactly what the cook removed, which for a documented
            let declared = self.view(&signature.declared_in);
            let Some(in_the_file) = declared.as_ref().and_then(|declared| {
                declared.file_offset_of(ReadingOffset::new(signature.declared_at))
            }) else {
                continue;
            };
            signature.documentation =
                self.documentation(view, &signature.declared_in, in_the_file.get());
        }

        signatures
    }

    /// **The declarations a call at this offset can reach** — indices into `parameter_lists`.
    ///
    /// The same resolution the argument checks and the signature popup use, asked over the declarations a caller already
    /// holds: a hover has the facts of the name under the cursor and needs to know which of them the call can mean.
    ///
    /// ```text
    ///   one index     the resolution decided (`Choice::One`)
    ///   several       every one of them can accept the arguments, and nothing decides between them by type
    ///   None          the offset is not on a call, or no declaration can accept it
    /// ```
    ///
    /// The **several** case is the one a hover needs: printing the chosen declaration beside five others that cannot take
    /// this argument list answers a question the reader did not ask.
    pub fn the_declarations_a_call_means(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        parameter_lists: &[Option<String>],
        template_parameters: &[Vec<String>],
    ) -> Option<Vec<usize>> {
        let (types, candidates) =
            self.the_candidates_of_a_call(view, offset, parameter_lists, template_parameters)?;
        match crate::sema::overload::choose(&types, &candidates) {
            crate::sema::overload::Choice::One(chosen) => Some(vec![chosen]),
            // **Nothing decides between them**, so the viable set is the answer: every one of them can accept this call,
            // and the ones arity alone rules out are gone.
            crate::sema::overload::Choice::Ambiguous => Some(
                crate::sema::overload::viable_candidates(&types, &candidates),
            ),
            crate::sema::overload::Choice::None => None,
        }
    }

    /// **The one declaration a call at this offset means**, when the resolution decides — see
    /// [`Session::the_declarations_a_call_means`] for the answer that also covers the ambiguous case.
    pub fn the_declaration_a_call_means(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        parameter_lists: &[Option<String>],
        template_parameters: &[Vec<String>],
    ) -> Option<usize> {
        let (types, candidates) =
            self.the_candidates_of_a_call(view, offset, parameter_lists, template_parameters)?;
        match crate::sema::overload::choose(&types, &candidates) {
            crate::sema::overload::Choice::One(chosen) => Some(chosen),
            _ => None,
        }
    }

    /// **The declarations and the argument types of the call at `offset`** — the one walk the two questions above share.
    fn the_candidates_of_a_call(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        parameter_lists: &[Option<String>],
        template_parameters: &[Vec<String>],
    ) -> Option<(
        Vec<crate::Known<String>>,
        Vec<crate::sema::overload::Candidate>,
    )> {
        let token = cpp_parser::token_at(&view.root, offset.get())?;
        let call = token.parent_ancestors().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::CallExpr
        })?;
        let arguments = crate::inlay::arguments_of(&call)?;

        // The engine answers with a parsed `Type` and the file it came from; the resolver ranks spellings, which is the
        // same information the facts carry.
        let session: &Session<F> = self;
        let index: &crate::ProjectIndex = session.store.index();
        let resolver =
            Box::new(move |path: &Path| -> Option<FileView> { session.view_of_the_file(path) });
        let model = crate::sema::model::SemanticModel::new(
            index,
            &view.root,
            &view.scopes,
            &view.source,
            &view.path,
            resolver,
        );
        let types: Vec<crate::Known<String>> = arguments
            .iter()
            .map(|argument| match model.type_of(argument) {
                crate::Known::Yes((a_type, _)) => crate::Known::Yes(a_type.to_string()),
                crate::Known::No => crate::Known::No,
                crate::Known::Unknown(reason) => crate::Known::Unknown(reason),
            })
            .collect();

        let candidates: Vec<crate::sema::overload::Candidate> = parameter_lists
            .iter()
            .enumerate()
            .map(|(at, parameters)| {
                let (declared, variadic) = parameters
                    .as_deref()
                    .map(crate::sema::overload::parameters_in)
                    .unwrap_or_default();
                // **A template candidate is ranked as what the call instantiates**, when the caller
                // could read its header: `take(T*)` and `take(T)` say nothing about an `int*`, and
                // deduced they say `int*` and `int`.
                let names = template_parameters.get(at).cloned().unwrap_or_default();
                // Kept for the partial ordering, which compares what the *declarations* wrote, not what the call
                // substituted — and the substitution below consumes `declared`.
                let declared_for_ordering = declared.clone();
                let (types, is_a_template) =
                    match crate::sema::deduction::deduce(&names, &declared, &types) {
                        Some(arguments) => (
                            declared
                                .iter()
                                .map(|spelling| {
                                    crate::sema::types::substituted_spelling(
                                        spelling, &names, &arguments,
                                    )
                                })
                                .collect(),
                            true,
                        ),
                        None => (declared, !names.is_empty()),
                    };
                let candidate = crate::sema::overload::Candidate::new(
                    types,
                    variadic,
                    format!("declaration {at}"),
                );
                if is_a_template {
                    candidate.with_declaration(declared_for_ordering, names.clone())
                } else {
                    candidate
                }
            })
            .collect();

        Some((types, candidates))
    }

    /// **A file's facts, read from its own view** — the answer an index summary gives, without waiting for one.
    ///
    /// A request can beat the index: the file is open, its tree is parsed, and the summary that records a reflection
    /// annotation (`UCLASS(Blueprintable)`) is built by the pump some time later. A card that needs one field the
    /// binding does not carry cannot wait, so it asks the view — the same `build_facts` the summary is built from, over
    /// the same tree, which is why the two cannot disagree about what the file says.
    ///
    /// One parse and one fact-building pass, the price [`Session::outline`] already pays per request.
    pub fn facts_of_the_view(&self, view: &FileView) -> Vec<crate::DeclFact> {
        let preprocessing = crate::preprocess(&view.source, view.tree.get_tokens());
        let errors: Vec<cpp_parser::SourceRange> = view
            .tree
            .get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range))
            .collect();
        crate::build_facts(&view.scopes, &preprocessing, &view.root, &errors, true).0
    }

    /// **The declaration's own node**, for a fact that names one — the syntax a rendering needs.
    ///
    /// A [`DeclFact`] records the words of a declaration and not the structure of it: whether a class was written
    /// `class`, `struct` or `union` is the node's kind, and a template head's parameters are the node's children. A
    /// caller that has only the fact can render everything a fact carries and must guess the rest, so the card asks for
    /// the node where the fact says the declaration is.
    ///
    /// `None` when the file cannot be read (a header the project never loaded), when the offset is not inside a
    /// declaration, or when the declaration there is not the one asked about — all three are the caller's answer
    /// "render from the fact alone", never an error.
    pub fn the_declaration_node(
        &self,
        file: &Path,
        offset: usize,
    ) -> Option<cpp_parser::CppDeclaration> {
        let view = self.view_of_the_file(file)?;
        let token = cpp_parser::token_at(&view.root, offset)?;
        use cpp_parser::CppAstNode as _;
        token
            .parent_ancestors()
            .find_map(cpp_parser::CppDeclaration::cast)
    }

    /// **The template arguments a call fixes**, for a declaration the caller already holds.
    ///
    /// Deduction needs the call's argument types, which is what a `SemanticModel` is for; the names come from the
    /// declaration's own text ([`crate::sema::deduction::template_parameters_before`]) and the parameters from its fact.
    /// `None` when the declaration is not a template, when the offset is not on a call, or when the call does not fix
    /// every parameter — the three answers for which nothing should be shown as an instantiation.
    pub fn the_deduced_arguments(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        template_parameters: &[String],
        parameters: &[String],
    ) -> Option<Vec<String>> {
        if template_parameters.is_empty() {
            return None;
        }
        let token = cpp_parser::token_at(&view.root, offset.get())?;
        let call = token.parent_ancestors().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::CallExpr
        })?;
        let arguments = crate::inlay::arguments_of(&call)?;

        let session: &Session<F> = self;
        let index: &crate::ProjectIndex = session.store.index();
        let resolver =
            Box::new(move |path: &Path| -> Option<FileView> { session.view_of_the_file(path) });
        let model = crate::sema::model::SemanticModel::new(
            index,
            &view.root,
            &view.scopes,
            &view.source,
            &view.path,
            resolver,
        );
        let types: Vec<crate::Known<String>> = arguments
            .iter()
            .map(|argument| match model.type_of(argument) {
                crate::Known::Yes((a_type, _)) => crate::Known::Yes(a_type.to_string()),
                crate::Known::No => crate::Known::No,
                crate::Known::Unknown(reason) => crate::Known::Unknown(reason),
            })
            .collect();

        crate::sema::deduction::deduce(template_parameters, parameters, &types)
    }
    /// The members of a type, as a file's own scopes and the index together know them.
    ///
    /// `written_type` is a spelling — `Widget`, `ns::Widget`, `const Widget&` — and following it to a class is the
    /// same work [`Session::member_completions`] does for an object expression.
    pub fn members_of(&self, view: &FileView, written_type: &str) -> Known<MemberList> {
        let listed = members_of(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            written_type,
        );
        // **The class's arguments, written through its members** — see [`substituted_members`].
        let listed = substituted_members(self.store.index(), &view.path, written_type, listed);
        self.the_generated_members(written_type, &view.path, listed)
    }

    /// **The members `GENERATED_BODY()` writes that a macro cannot** — `Super` and `ThisClass`.
    ///
    /// Unreal's build tool writes `typedef <Base> Super;` and `typedef <TheClass> ThisClass;` into the generated code,
    /// so a project whose generated headers are present has them as ordinary members and this adds nothing. A project
    /// that is read from a **simulation** — the macro expanded to the name-independent half, `StaticClass()` and
    /// `GetPrivateStaticClass()` — has the annotation and not these, which is the case this exists for: measured on
    /// `UE-like`, `Super::BeginPlay()` and `ThisClass` were names nothing declared.
    ///
    /// The signal is `StaticClass`, which only the generated code (or a simulation of it) declares, and the absence of
    /// `ThisClass`, which says the generated code is not here. Outside `[unreal]` nothing is added: an ordinary C++
    /// class with a `StaticClass` member must not grow a `Super` it never declared.
    fn the_generated_members(
        &self,
        written_type: &str,
        path: &std::path::Path,
        listed: Known<crate::index::MemberList>,
    ) -> Known<crate::index::MemberList> {
        let unreal = self
            .project_config()
            .config
            .unreal
            .as_ref()
            .is_some_and(|unreal| unreal.is_on());
        if !unreal {
            return listed;
        }
        let Known::Yes(mut list) = listed else {
            return listed;
        };
        let declares_static_class = list
            .members
            .iter()
            .any(|member| member.fact.name == "StaticClass");
        let already_generated = list
            .members
            .iter()
            .any(|member| member.fact.name == "ThisClass");
        if !declares_static_class || already_generated {
            return Known::Yes(list);
        }
        let Known::Yes(declared) = self.store.index().definition(written_type, path) else {
            return Known::Yes(list);
        };
        // The synthetic members point at the class's own declaration: that is where the macro that generates them is
        // written, and it is the honest place for a hover to open.
        let class_fact = declared.fact.clone();
        let class_name = class_fact.name.clone();
        for (name, type_of) in crate::project::ue::generated_members(&class_name, &class_fact.bases)
        {
            let mut fact = class_fact.clone();
            fact.name = name.to_string();
            fact.kind = crate::DeclKind::Type;
            fact.type_of = Some(type_of);
            // A generated member is not a member of a **namespace**: it belongs to the class it was generated for.
            fact.scope = Some(class_name.clone());
            list.members.push(crate::index::ProjectMember {
                file: declared.file.clone(),
                fact,
                declared_in: class_name.clone(),
                depth: 0,
                ambiguous: false,
            });
        }
        Known::Yes(list)
    }

    /// **A type name as a declaration wrote it, resolved where it was written — through a base if it has to.**
    ///
    /// [`ProjectIndex::definition_where_written`] answers for a name written in a scope: it follows an alias,
    /// and it looks outward through the enclosing scopes the way C++ does. What it cannot answer is a member the
    /// class **inherits**, because that is not a lookup in the index at all — it is a walk of the base chain,
    /// and the walk lives here, where the file's scope tree and tree are, rather than in the index.
    ///
    /// Measured, and it is what this wire exists for:
    ///
    /// ```text
    /// std::allocator_traits::pointer        NotDeclaredHere   the name is inherited, not declared
    /// members_of("std::allocator_traits")   44 members        …and `pointer` is one of them
    /// ```
    ///
    /// `pointer` is declared in `_Normal_allocator_traits`, which `allocator_traits` names only inside a
    /// `conditional_t<…>` — so finding it needs the alias step, the outward scope step **and** the base walk,
    /// and this is the one entry point that has all three.
    ///
    /// # A diamond is `Ambiguous`, and that answer comes from the walk rather than from here
    ///
    /// A member two bases declare at once is not uniquely resolved by the language, and the walk records it as
    /// [`ProjectMember::ambiguous`]. Picking one would be a jump to an entity the user cannot tell from the
    /// other, which is the answer this project refuses to give.
    pub fn definition_of_a_written_type(
        &self,
        view: &FileView,
        written: &str,
        in_scope: Option<&str>,
        at: usize,
    ) -> Known<ProjectDefinition> {
        let index = self.store.index();

        // **A declaration inside a body has no scope to be named by, and the file's own scopes can supply
        // one.** [`DeclFact::scope`] is `None` for a local — a function body contributes no segment to a
        // qualified name — so a name it wrote has nothing to be looked up from, and a local `Widget` in
        // `namespace app` is a name that cannot be found. The view's scope chain at the declaration's own
        // offset says which namespaces enclose it.
        //
        // **Literal namespaces only, and that is the honest limit here.** A view is built with no macro
        // evidence (see [`FileView::parse`]), so `namespace app {` is a scope in it and `_STD_BEGIN` — whose
        // replacement list is in a header nothing here has read — is not. A name written inside a namespace the
        // file spells out is therefore resolvable, and one inside a namespace a macro opened is not; the second
        // needs the fact to carry the namespace, which is a change to what a summary stores rather than to what
        // this query asks.
        let enclosing = match in_scope {
            Some(scope) => Some(scope.to_string()),
            None => enclosing_namespace_at(view, at),
        };

        let direct = index.definition_where_written(written, enclosing.as_deref(), &view.path);
        if !matches!(direct, Known::Unknown(UnknownReason::NotDeclaredHere(_))) {
            return direct;
        }
        let in_scope = enclosing.as_deref();

        // **The class half, then the member.** The class is handed to [`Session::members_of`] **as written**,
        // because that query is the one that already knows how to turn a spelling into a class: it follows an
        // alias, it looks outward through the enclosing scopes, and it walks the bases. Resolving it here first
        // would be a second implementation of that, and it was one — asking
        // `ProjectIndex::definition_where_written` for `std::allocator_traits` answers `Unknown(Ambiguous(…))`,
        // because the class is forward-declared **and** defined, and an `Ambiguous` read as "no class" is the
        // same mistake this file's own `is_declared` and `direct_members` each had to have fixed.
        let Some((class, member)) = written.rsplit_once("::") else {
            return direct;
        };

        // **Qualified by the scope first, then as written** — and the order is the whole of the care here. A
        // class named unqualified inside another one is written relative to it, so `_Mybase` in
        // `std::_Vb_const_iterator` means `std::_Vb_const_iterator::_Mybase` and not the bare name. Trying the
        // bare spelling first was the first attempt, and it stopped the walk on the wrong class: `members_of`
        // answered `Yes` for `_Mybase` — there is a class by that name somewhere — and the loop took that as the
        // class to look the member up in, so the scope-qualified spelling was never reached. Measured, that is
        // `_Mybase::_Mycont` at `std::_Vb_const_iterator`: unresolved with the bare spelling tried first, and
        // `Yes` when the same name is asked in full.
        let mut tried: Vec<String> = Vec::new();
        if let Some(scope) = in_scope.filter(|_| !class.starts_with("::") && !class.contains("::"))
        {
            tried.push(format!("{scope}::{class}"));
        }
        tried.push(class.to_string());

        let mut list = None;
        for spelling in &tried {
            if let Known::Yes(found) = self.members_of(view, spelling) {
                list = Some(found);
                break;
            }
        }

        let Some(list) = list else {
            return direct;
        };

        let mut found = list
            .members
            .iter()
            .filter(|found| found.fact.name == member);

        let Some(only) = found.next() else {
            // Nothing in the chain declares it, and the honest answer is the one the direct lookup gave: the
            // name is not here, rather than a name nobody wrote.
            return direct;
        };

        // **Two answers, or one answer the walk already called ambiguous.** A member that two bases declare at
        // once is not uniquely resolved by the language, and a jump to either is a jump to an entity the user
        // cannot tell from the other.
        if found.next().is_some() || only.ambiguous {
            return Known::Unknown(UnknownReason::Ambiguous(Box::from(written)));
        }

        Known::Yes(ProjectDefinition {
            file: only.file.clone(),
            fact: only.fact.clone(),
        })
    }

    /// **The file's declarations as a tree** — what an outline, a breadcrumb bar or a folding range is drawn from.
    ///
    /// # One source: the file's own text
    ///
    /// An outline is about the **file**, not about the program: a declaration in a branch nobody takes belongs in an
    /// outline, and a type a macro declares does not. So this reads the view it is given — the file's own parse
    /// ([`Session::view_of_the_file`]) — and never the index's summary.
    ///
    /// **That used to be worded "the index's summary when it has one, and the buffer's own parse
    /// when it does not", and the difference is no longer a fallback but a wrong answer.** While a
    /// summary was the file's own text reading, the two sources agreed about everything an outline
    /// shows. Measured in `cpp_ls`'s `the_outline_shows_the_file_and_not_the_compiler`, which is
    /// what says the two readings must not be mixed here.
    ///
    /// # What it costs, and why that is not a regression
    ///
    /// One parse of the file's own bytes per request (the buffer, which the VFS already holds), and no dependence on a
    /// rendering existing — so the "summary is gone between a keystroke and the next drain" case that the fallback
    /// existed for is now the only case, and the panel never waits for a cook.
    pub fn outline(&self, view: &FileView) -> Vec<OutlineSymbol> {
        let preprocessing = crate::preprocess(&view.source, view.tree.get_tokens());
        let errors: Vec<cpp_parser::SourceRange> = view
            .tree
            .get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range))
            .collect();
        let (facts, _guards) =
            crate::build_facts(&view.scopes, &preprocessing, &view.root, &errors, true);

        crate::outline_of(&facts)
    }
}

/// Which reading answered a file's diagnostics — see [`Session::diagnostics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticReading {
    /// The text a **compiler** parses: the file's bytes with its translation unit's macros expanded and the branches
    /// nobody takes left out.
    ///
    /// **There was a `Raw` variant beside this one and it is gone.** It meant "the file's own bytes, unexpanded —
    /// what a reader editing the file sees, macros and untaken branches included", and it was reachable: a file the
    /// index held no cooked reading for was answered from its own text. That answer was not a lesser one, it was an
    /// **untrue** one — an untaken branch is not compiled, so an error reported inside one is an error about a
    /// program that does not exist. Measured on the fixture below: the same `#if 0` branch produced 1 error as the
    /// file's text and 0 once cooked.
    ///
    /// A caller that needs diagnostics for an unexpanded file has nothing to read: it asks
    /// [`Session::diagnostics`], gets `None`, and comes back — the same deferral [`Session::view`] answers with, and
    /// for the same reason. The variant is deleted rather than left reachable-but-unused because a variant nothing
    /// produces is an invitation to match on it.
    Cooked,
}

/// The type of the expression at a cursor — see [`Session::type_at`].
///
/// Three fields rather than one, because a consumer that shows the type needs to say *about what*: the expression's
/// own text is what the user recognises, and the file is where the declaration the type was read from lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpressionType {
    /// The expression's own text, as the file writes it — `this`, `*p`, `make(1)`, `w.size`.
    pub expression: String,
    /// The type, spelled the way the declaration it was read from spells it. Not a canonical type: an alias is an
    /// alias name, and `const Widget&` is those three tokens, because that is what the file says.
    pub type_of: String,
    /// The file that declaration is in.
    pub file: PathBuf,
}

/// One thing to report, with **file** offsets and the message as the parser wrote it.
///
/// Offsets rather than a line and a column because the line index belongs to the layer that holds the text
/// ([`Session::view`]) — and a range, because an error that covers one character and one that covers a declaration
/// are different claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiagnostic {
    pub start: usize,
    pub end: usize,
    pub message: String,
    /// **Which rule produced this**, as the enum the analysis defines — see
    /// [`crate::sema::check::DiagnosticCode`].
    ///
    /// Carried rather than derived from the message, because a client needs a *stable* name: `Diagnostic::code`
    /// is what an editor shows and what a settings file turns a rule off by, and a consumer that parsed the
    /// sentence would be reading prose as an identifier.
    pub code: crate::sema::check::DiagnosticCode,
}

/// **Something the analysis knows that is not an error** — see [`Session::notes_about_the_modules`].
///
/// The same offsets as [`FileDiagnostic`] and a different kind of claim: an import whose module this project does not
/// contain is not a mistake in the file, it is a fact about where the module lives. A consumer shows it as
/// information, and a type that merged the two would have to carry a severity field whose only honest values are
/// "error, for the errors" and "information, for this" — which is the same as having two types, with one of them
/// harder to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleNote {
    pub message: String,
    pub start: usize,
    pub end: usize,
}

/// What a diagnostics channel should say about one file — see [`Session::diagnostics`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiagnostics {
    /// Which reading answered. Nothing about the list depends on it; a caller that explains itself to a user
    /// ("this file is read as the compiler reads it") needs it.
    pub reading: DiagnosticReading,
    /// How many errors the reading found that **cannot be shown here** — text written in another file's macro body,
    /// or a token whose spelling exists nowhere. Only a cooked reading can have any, and a caller that reports
    /// [`FileDiagnostics::errors`] as the whole truth should mention this number when it is not zero.
    pub unplaced: usize,
    pub errors: Vec<FileDiagnostic>,
    /// **Things the analysis knows about this file that are not errors** — see
    /// [`Session::notes_about_the_modules`].
    ///
    /// Here rather than in a call of its own because both answers come out of the same tree, and a consumer that
    /// asked twice would pay for the file's parse twice. Empty is the ordinary case.
    pub notes: Vec<ModuleNote>,
    /// **What the analysis says is wrong with the file** — see [`crate::sema::check`].
    ///
    /// The third channel, and the one that is neither the parser's nor a note: a construct the grammar accepts
    /// and the language does not. Every entry is a claim the analysis can stand behind — a check reports only
    /// `Known::Yes` or `Known::No`, and never an absence of an answer — so a consumer may show all of them
    /// without hedging, which is the property the layer exists to have.
    pub checks: Vec<crate::sema::check::Finding>,
}

/// **What reading a file the way a compiler reads it produced** — see [`Session::cook`].
///
/// Counts rather than a summary, because the summary has already gone where it belongs: the index holds the
/// declarations and the errors (that is the point of the call), and what a caller — a status line, a test, a log —
/// wants to know is whether the reading found anything the file's own text did not, what it had to say about it, and
/// whether every range could be placed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CookedReading {
    /// How many declarations the cooked reading found.
    pub declarations: usize,
    /// How many of them the file's own text does not declare — the names that exist only after expansion.
    pub only_after_expansion: usize,
    /// How many errors the parse of the rendering reported **and this file can show** — see
    /// [`crate::IndexedRendering::diagnostics`].
    pub diagnostics: usize,
    /// How many it reported that this file cannot show, because the text they are about is not written here (a
    /// grammar error inside another file's macro body, or a pasted token that exists nowhere).
    pub unplaced: usize,
    /// How the ranges mapped back into the file; see [`crate::MapReport`].
    pub mapped: crate::MapReport,
}

/// How many translation units the session keeps in memory.
///
/// A unit is a timeline over a whole include closure, so it is the session's largest single object; the disk cache
/// holds the rest ([`crate::TranslationUnitCache`]) and a unit that falls out of here is one decode away. **Eight**
/// is enough for the units a reader is working in — the open file it is looking at and the headers beside it — and
/// small enough that a project of a thousand sources does not hold a thousand timelines.
///
/// It was sixteen, and the measurement that halved it is at [`MAX_UNIT_BYTES`]: a session that opened twenty files
/// held sixteen timelines for 40.4 MB and re-asked for them **twice**.
///
/// **A count is not a budget.** Measured on `EmmyLuaCodeStyle`, a unit is **6.2 MB** after the
/// second pass — eight of them are 50 MB, and a header-heavier project would have eight units
/// of 25 MB without this constant changing. So the table is bounded by **bytes** as well:
/// [`MAX_UNIT_BYTES`].
const MAX_UNITS: usize = 8;

/// **How many bytes of translation units the session keeps** — the budget the count above cannot express.
///
/// An entry larger than the whole budget is still kept (the walk that just produced it is the one the caller is
/// about to use), and the eviction order prefers the smallest closure first so the units that answer the most
/// questions survive.
///
/// # Why 128 MB became 64, measured
///
/// The table is the session's largest single object and it was sized when the **second pass** walked units for every
/// file it re-read — a phase-2 change removed that caller, and what uses the table now is a reader cooking one file
/// at a time. Measured with `tests/memory_probe.rs` on `EmmyLuaCodeStyle`, twenty files opened, cooked and cooked
/// again:
///
/// ```text
/// sixteen timelines held                 40.4 MB
/// twenty files cooked a second time        2 hits, 23 walks   (the rendering cache answers the rest)
/// ```
///
/// So the tail of sixteen is not earning 40 MB. Eight keeps the reader's working set — the file it is looking at and
/// the headers beside it — and halves the bill; a timeline that falls out is a 38 ms walk, and the rendering cache
/// in front of it is what a re-cook usually hits anyway.
const MAX_UNIT_BYTES: usize = 64 * 1024 * 1024;

/// **How many timelines the closure-dedup table keeps.** The byte budget is the real bound; this is the backstop
/// against a project of thousands of tiny closures — where 512 entries are a budget of their own, on a project whose
/// closures are a few kilobytes each. 32 is what a reader's recent history needs; the block below has the measurement
/// that took both numbers down.
const MAX_WALKS_BY_CONTENT_ENTRIES: usize = 32;

/// **How many bytes of closure timelines the dedup table keeps** — see [`Session::walked_by_content`].
///
/// Before this bound the table was a `HashMap` that never dropped anything: measured on a cold start it held
/// **384 entries / 222 MB** after the second pass, and every edit of a root adds a new content identity.
///
/// # Why 64 MB became 16 MB, measured
///
/// The cache's justification was a mass indexer: "2110 roots, 786 distinct closures, **46 % of walks removable**".
/// That caller no longer exists — phase 2 stopped building units for the second pass — and what remains is a reader
/// cooking one file at a time, where a hit needs two roots whose *closure content* is identical. Measured with
/// `tests/memory_probe.rs` on two real projects:
///
/// ```text
/// EmmyLuaCodeStyle   20 files opened and cooked   20 walks, 0 from memory   46.6 MB held
/// clice             20 files opened and cooked   20 walks, 0 from memory    0.6 MB held
/// ```
///
/// So on both projects the table's hit rate is **zero** and its bill on the first is 46.6 MB, of which 40.4 MB is
/// the same allocation the unit table also holds. A cap of 16 MB keeps what the cache is good at — the sibling roots
/// that do share a closure — and bounds the waste at a quarter of what it was. The *useful* table here is the unit
/// table, which holds a root's own timeline for its own cooks and pulls; this one is speculation.
const MAX_WALKS_BY_CONTENT_BYTES: usize = 16 * 1024 * 1024;

/// **How many files' configurations a session memoizes** — see [`Session::config_for_file`].
///
/// A reader meets a few dozen files; 256 is far past that and small enough that the memo is a rounding error in the
/// memory report. Reaching it clears the whole memo rather than evicting one entry.
const CONFIG_FOR_FILE_ENTRIES: usize = 256;

/// How many rounds [`Session::read_the_modules_a_file_imports`] may read module interface units in.
///
/// A round reads every module an already-read file imports and could not name a file for, so the number of rounds is
/// the **depth** of the module graph rather than its size — one for `import std;`, two for a module that imports a
/// module that imports a module. Sixteen is far past anything a real project reaches and bounds a graph that is
/// merely wrong: an interface unit that imports a module whose interface unit imports it back is a cycle, and a cycle
/// terminates on its own (a path already read is never read again), so this is the guard for the case the cycle rule
/// does not cover — a module graph that keeps producing *new* files, which only a scan that resolves module names to
/// the wrong files can do.
const MAX_MODULES_READ_AT_ONCE: usize = 16;

/// The in-memory translation units, least recently used out first.
#[derive(Default)]
struct UnitTable {
    held: std::collections::HashMap<String, HeldUnit>,
    clock: u64,
    /// The sum of `HeldUnit::weight` — what the table holds, in the same estimate the memory report prints.
    bytes: usize,
}

impl UnitTable {
    /// The units this session is holding, borrowed — for a caller asking *about* them rather than for one by name.
    /// The cook is such a caller: "does any held unit contain this header" is a question about all of them and about
    /// none of them in particular.
    fn values(&self) -> impl Iterator<Item = &HeldUnit> {
        self.held.values()
    }

    fn get(&mut self, key: &str) -> Option<(bool, std::sync::Arc<crate::TranslationUnit>)> {
        self.clock += 1;
        let held = self.held.get_mut(key)?;
        held.used = self.clock;
        Some((held.provisional, held.unit.clone()))
    }

    fn insert(
        &mut self,
        key: String,
        unit: std::sync::Arc<crate::TranslationUnit>,
        provisional: bool,
    ) {
        self.clock += 1;

        // **Both budgets, and the byte one is the real one.** A count of sixteen says nothing when a unit is 0.1 MB
        // in one project and 25 MB in another; the count stays as the backstop that keeps a project of tiny
        // closures from pinning hundreds of timelines.
        let weight = unit.heap_bytes();
        if let Some(replaced) = self.held.remove(&key) {
            self.bytes = self.bytes.saturating_sub(replaced.weight);
        }
        while self.held.len() + 1 > MAX_UNITS || self.bytes + weight > MAX_UNIT_BYTES {
            if self.held.is_empty() {
                // One unit larger than the whole budget is still the one the caller just asked for.
                break;
            }
            // **The smallest closure goes first, and only then the oldest.** A unit is a timeline over its whole
            // closure, so a project root's unit answers `environment_of` for hundreds of headers while a header's
            // own answers for a handful — and the second pass asks "is this file covered" for every file it
            // re-reads. LRU kept whichever was walked last and threw the big ones away: measured on a cold start,
            // the tail walked **365 units for 63 slices** (~6 per slice of 16 files), because every sibling needed
            // its own timeline while the root that covered all of them had been evicted.
            let smallest = self
                .held
                .iter()
                .min_by_key(|(_, held)| (held.weight, held.used))
                .map(|(key, _)| key.clone());
            let Some(smallest) = smallest else {
                break;
            };
            if let Some(removed) = self.held.remove(&smallest) {
                self.bytes = self.bytes.saturating_sub(removed.weight);
            }
        }

        self.bytes = self.bytes.saturating_add(weight);
        self.held.insert(
            key,
            HeldUnit {
                used: self.clock,
                provisional,
                unit,
                weight,
            },
        );
    }

    /// **Forget one file's unit** — what an edit to that file invalidates, and nothing else.
    ///
    /// `clear` was the only removal for most of this table's life, and it is the wrong width: see
    /// [`Session::buffer_changed`]. `clear` stays for the one case that really is whole-table — a closed buffer,
    /// whose text every unit in the table may have been built from.
    fn remove(&mut self, key: &str) {
        if let Some(removed) = self.held.remove(key) {
            self.bytes = self.bytes.saturating_sub(removed.weight);
        }
    }

    fn clear(&mut self) {
        self.held.clear();
        self.bytes = 0;
    }

    /// What the table holds: `(entries, bytes)`.
    fn accounting(&self) -> (usize, usize) {
        (self.held.len(), self.bytes)
    }

    /// **The timelines themselves, for the one caller that has to know which of them is shared.**
    ///
    /// [`UnitTable`] and `walked_by_content` hold the *same* `Arc<TranslationUnit>` whenever a timeline was both held
    /// for a root and remembered for sharing, which is the common case — so a report that adds
    /// `accounting().1` to the sharing cache's weight counts one allocation twice. This is how that is measured
    /// instead of assumed: the caller compares pointers. See [`Session::memory_report`].
    fn timelines(&self) -> impl Iterator<Item = &std::sync::Arc<crate::TranslationUnit>> {
        self.held.values().map(|held| &held.unit)
    }
}

/// One held timeline, and **whether it was built while the program was still being read**.
///
/// # The defect this flag exists for
///
/// A walk enters every file its closure offers and stops at one the closure does not hold. Usually that is the
/// right reading *and the permanent one*: a system header nobody indexed is **outside the analysis**, which is what
/// [`crate::TranslationUnit::walk`]'s early return says and what was measured when it was written (treating it as
/// "may define anything" collapses the corpus's conditional evidence from millions of facts to tens of thousands).
///
/// But while an index is running, the same absence means **not yet** — the file is in the queue and will be read —
/// and a timeline built without it is a timeline of a *smaller program*. Nothing in the timeline says which of the
/// two it is, so storing it asserts the wrong one. Measured, with `examples/keystroke.rs`'s fixture family:
///
/// ```text
///   one step of indexing            WALK "/p/api.h" frames=["/p/api.h"]      the closure held two files
///   …written to the disk cache      an entry whose recorded closure is the one frame it entered
///   every later session             served it — the key cannot tell a short closure from a complete one
///   the environment it answered     without `#define BEGIN_NS namespace one {`
///   so the reading of the file      `struct Widget` at file scope, and `one::Widget` NotDeclaredHere
/// ```
///
/// # What provisional buys, and what it costs
///
/// A provisional timeline is used for **this** run, exactly as before — the index is being built, and a reading of
/// most of the program beats none. It is not written to the disk cache, and it is dropped as soon as the queue is
/// empty, so the next walk is over the whole closure and *that* one is stored.
///
/// The cost is one re-walk per index burst. The alternative — never caching while anything is queued — is worse,
/// because a big project's first minute is exactly when the cache is worth most.
struct HeldUnit {
    /// The table's own clock, for eviction.
    used: u64,
    /// Built while a file the closure needed was still queued. See this type's documentation.
    provisional: bool,
    unit: std::sync::Arc<crate::TranslationUnit>,
    /// What this unit costs, in the estimate [`crate::TranslationUnit::heap_bytes`] gives — the number the table's
    /// byte budget is deducted from. Stored rather than recomputed on eviction: the estimate walks every event.
    weight: usize,
}

/// Which of a file's two directive comparisons an edit failed — see [`Session::directives_moved`].
#[derive(Debug, Clone, Copy)]
struct DirectivesMoved {
    /// What the file tells its includers changed.
    environment: bool,
    /// Where the file's directives sit changed, so a timeline over it is out of date.
    layout: bool,
}

/// The files waiting to be read **the way a compiler reads them**, in the order they will be.
///
/// A queue with a membership set, the same shape [`Work`] has and for the same reason: a file may be wanted twice
/// (an open file that is also a dependent of something that changed) and must be cooked once, and the *whole
/// project* is marked at every drain once the open files are done — a scan whose cost has to be linear in the
/// number of files rather than quadratic in it. The order is the point of the queue: a reader is waiting for the
/// file they are looking at, and for the header their edit invalidated, before they are waiting for the rest.
#[derive(Debug, Default)]
struct Cooking {
    /// The order, front first.
    wanted: VecDeque<PathBuf>,
    /// Membership, by the queue's own key — the normalization every other queue in this crate compares by.
    held: HashSet<String>,
}

impl Cooking {
    /// Want `path` cooked, **at the front**: something is about to look at it, and its answer is the next thing
    /// asked for.
    fn want(&mut self, path: &Path) -> bool {
        if !self.held.insert(queue_key(path)) {
            return false;
        }

        // The spelling is kept as the caller gave it: the cook resolves it against the index's own spelling anyway
        // (`Session::cook` asks the index for the path it filed the file under), and rewriting it here would be a
        // second opinion about identity.
        self.wanted.push_front(path.to_path_buf());
        true
    }

    /// Is `path` already waiting to be cooked? The **membership set** [`Cooking::want`] inserts into, asked without
    /// taking anything — see [`Session::is_ready_for_a_request_about`].
    fn wants(&self, path: &Path) -> bool {
        self.held.contains(&queue_key(path))
    }

    /// Take up to `how_many` files to cook, in order.
    fn take(&mut self, how_many: usize) -> Vec<PathBuf> {
        let mut taken = Vec::new();

        for _ in 0..how_many {
            let Some(path) = self.wanted.pop_front() else {
                break;
            };
            self.held.remove(&queue_key(&path));
            taken.push(path);
        }

        taken
    }

    fn len(&self) -> usize {
        self.wanted.len()
    }

    /// **Drop the whole queue, and say how many files it held** — see [`Session::abandon_the_cooking_queue`].
    ///
    /// Both halves: the order and the membership set, which is what `want` consults before pushing. Clearing only one
    /// would leave the queue claiming a file it no longer holds.
    fn abandon(&mut self) -> usize {
        let held = self.wanted.len();
        self.wanted.clear();
        self.held.clear();
        held
    }
}

/// The queue, and what each path in it is doing there.
///
/// Two deques because the order has two halves — open files and what they reach, then the rest of the project —
/// and because a notification can move a path from one half to the other. The map is what keeps one file from being
/// worked twice: a header `#include`d by forty files is one entry, and an entry left behind by an upgrade is
/// skipped rather than read again.
#[derive(Debug, Default)]
struct Work {
    /// Files the user is looking at, and (through `add`'s inheritance of the half) what they include.
    open: VecDeque<(PathBuf, usize)>,
    /// The project scan's files, and what they include.
    rest: VecDeque<(PathBuf, usize)>,
    /// Where each path stands, by normalized spelling.
    standing: HashMap<String, Standing>,
    /// How many *files* are queued — the count [`Work::pending`] reports, maintained rather than recomputed.
    queued: usize,
}

/// What the queue knows about one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// In one of the deques, in this half.
    Queued(Priority),
    /// Worked. A second entry for it — left behind by an upgrade, or discovered by a second include — is skipped.
    Worked,
}

impl Work {
    /// Queue a path if it is not already queued or worked.
    ///
    /// A path in the **rest** half that is now wanted in the open half is upgraded rather than ignored: that is the
    /// case the two halves exist for — a header the project scan listed before the user opened it — and ignoring it
    /// would leave the file the user is looking at behind everything else in the project.
    fn add(&mut self, path: PathBuf, priority: Priority, depth: usize) -> bool {
        let key = queue_key(&path);

        match self.standing.get(&key) {
            None => {
                self.standing.insert(key, Standing::Queued(priority));
                self.push(path, priority, depth);
                self.queued += 1;
                true
            }
            Some(Standing::Queued(Priority::Rest)) if priority == Priority::Open => {
                self.standing.insert(key, Standing::Queued(Priority::Open));
                self.push(path, priority, depth);
                // The count does not move: the file was already counted once, and the entry it left in the rest
                // half is skipped when it surfaces.
                true
            }
            Some(_) => false,
        }
    }

    /// Queue a path whose **answer** has changed — an edit, or a file the client says changed.
    ///
    /// Different from [`Work::add`] in both directions: it is queued even if it was already worked, and it goes to
    /// the **front** of its half. Both are about the same thing — the user is waiting for this one — and the cost
    /// of being wrong is one redundant step, because a file whose text did not actually change is a cache hit.
    fn again(&mut self, path: PathBuf, priority: Priority, depth: usize) {
        self.rearm(path, priority, depth, true);
    }

    /// Queue a path whose standing is void for a reason that is not urgent — every key in the project, after the
    /// configuration changed.
    ///
    /// The same re-arming as [`Work::again`] and the back of the half instead of the front: there is no file among
    /// ten thousand of them that the user is waiting for more than the others, and pushing each one to the front
    /// would leave the list in the reverse of the order it was scanned in.
    fn requeue(&mut self, path: PathBuf, priority: Priority, depth: usize) {
        self.rearm(path, priority, depth, false);
    }

    /// Queue a path and forget what the queue knew about it, at either end of its half.
    fn rearm(&mut self, path: PathBuf, priority: Priority, depth: usize, urgent: bool) {
        let key = queue_key(&path);

        // An entry still in a deque keeps its count; it will be skipped when it surfaces, because this insert
        // replaces its standing. A path that was worked, or never seen, is a new file to read.
        if !matches!(self.standing.get(&key), Some(Standing::Queued(_))) {
            self.queued += 1;
        }

        self.standing.insert(key, Standing::Queued(priority));

        match (priority, urgent) {
            (Priority::Open, true) => self.open.push_front((path, depth)),
            (Priority::Open, false) => self.open.push_back((path, depth)),
            (Priority::Rest, true) => self.rest.push_front((path, depth)),
            (Priority::Rest, false) => self.rest.push_back((path, depth)),
        }
    }

    /// **Forget a path whose answer is already in hand** — the queue entry a synchronous re-read makes unnecessary.
    ///
    /// Returns whether there was anything to forget, which is also how a caller learns that the file *was* stale:
    /// a path in the queue is a path whose summary was dropped and not yet rebuilt, so "the queue had it" and "the
    /// index is one edit behind" are the same statement.
    ///
    /// The path becomes [`Standing::Worked`] rather than being left unknown, because it *has* been worked — by
    /// whoever read it synchronously — and a later `add` for it (an include discovered by another file) must not
    /// queue a second read of a file the store already answered for.
    fn forget(&mut self, path: &Path) -> bool {
        if !self.is_queued(path) {
            return false;
        }

        self.standing.insert(queue_key(path), Standing::Worked);
        self.queued -= 1;
        true
    }

    /// **Is `path` waiting to be read?** — the test [`Work::forget`] is built on, asked on its own because a caller
    /// that only wants to know whether there is work to do must be able to ask it without doing any.
    ///
    /// One implementation, two callers: [`Work::forget`] decides what to forget with it and
    /// [`Session::needs_catching_up`] decides whether to take the write lock with it.
    fn is_queued(&self, path: &Path) -> bool {
        matches!(
            self.standing.get(&queue_key(path)),
            Some(Standing::Queued(_))
        )
    }

    /// The next file to work, and the half it came from.
    ///
    /// Skips the entries an upgrade left behind: a path whose standing is in the other half, or has already been
    /// worked, is not work.
    fn pop(&mut self) -> Option<(PathBuf, Priority, usize)> {
        loop {
            let (path, priority, depth) = match self.open.pop_front() {
                Some((path, depth)) => (path, Priority::Open, depth),
                None => {
                    let (path, depth) = self.rest.pop_front()?;
                    (path, Priority::Rest, depth)
                }
            };

            let key = queue_key(&path);
            match self.standing.get(&key) {
                Some(Standing::Queued(held)) if *held == priority => {
                    self.standing.insert(key, Standing::Worked);
                    self.queued -= 1;
                    return Some((path, priority, depth));
                }
                _ => continue,
            }
        }
    }

    fn push(&mut self, path: PathBuf, priority: Priority, depth: usize) {
        match priority {
            Priority::Open => self.open.push_back((path, depth)),
            Priority::Rest => self.rest.push_back((path, depth)),
        }
    }

    fn pending(&self) -> usize {
        self.queued
    }

    /// Has this path already been worked?
    fn is_worked(&self, path: &Path) -> bool {
        matches!(self.standing.get(&queue_key(path)), Some(Standing::Worked))
    }

    /// **Mark a file read without popping it** — what a translation-unit read does for every file it covered.
    ///
    /// A unit read parses one program and files the facts of every file in it, so the per-file queue must not read
    /// those files again. The queue entry (if any) is left where it is and skipped by `pop`, which asks `standing`
    /// rather than the deque; the count is what a caller sees, so it comes down here.
    ///
    /// A path the queue has never heard of is recorded as worked rather than ignored: a later `add` then leaves it
    /// alone, which is the honest answer — the unit read covered it.
    fn mark_worked(&mut self, path: &Path) {
        let key = queue_key(path);
        match self.standing.get(&key) {
            Some(Standing::Queued(_)) => {
                self.standing.insert(key, Standing::Worked);
                self.queued -= 1;
            }
            Some(Standing::Worked) => {}
            None => {
                self.standing.insert(key, Standing::Worked);
            }
        }
    }

    /// The next `how_many` files [`Work::pop`] would answer with, **without taking them** — in the order it would.
    ///
    /// The queue as it stands now: a file a step discovers later can outrank these, which is why the caller treats
    /// the answer as a hint about what to prepare and pops for real.
    fn peek(&self, how_many: usize) -> Vec<PathBuf> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut ahead = Vec::new();

        let halves = [(&self.open, Priority::Open), (&self.rest, Priority::Rest)];
        for (half, priority) in halves {
            for (path, _) in half {
                if ahead.len() >= how_many {
                    return ahead;
                }

                let key = queue_key(path);
                let queued_here = matches!(
                    self.standing.get(&key),
                    Some(Standing::Queued(held)) if *held == priority
                );
                if queued_here && seen.insert(key) {
                    ahead.push(path.clone());
                }
            }
        }

        ahead
    }
}

/// **The namespaces enclosing `at`, as the file itself spells them** — see
/// [`Session::definition_of_a_written_type`], which asks this for a declaration that has no scope of its own.
///
/// The chain is walked outward from the innermost scope and then reversed, so the answer reads outermost first:
/// a declaration in `namespace a { namespace b { void f() { … } } }` is looked up from `a::b`, then `a`, which
/// is the order C++ looks in and the order [`crate::ProjectIndex::definition_where_written`] already walks once
/// it has a starting point. `None` when nothing encloses the offset but the file: a declaration at file scope
/// is qualified by nothing, and an empty string would be a scope no name is in.
fn enclosing_namespace_at(view: &FileView, at: usize) -> Option<String> {
    let scope = view.scopes.scope_at(at)?;

    let mut segments: Vec<&str> = Vec::new();
    for id in view.scopes.scope_chain(scope) {
        let Some(data) = view.scopes.scope(id) else {
            continue;
        };
        if data.kind == crate::ScopeKind::Namespace
            && let Some(name) = data.name.as_deref()
        {
            segments.push(name);
        }
    }

    if segments.is_empty() {
        return None;
    }

    segments.reverse();
    Some(segments.join("::"))
}

/// A path as the queue compares them — the same normalization the store and the index use.
fn queue_key(path: &Path) -> String {
    normalize_path(path, cfg!(windows))
}

/// **Does this session know the whole of what its compilation defines?**
///
/// The licence for one claim — *a name none of the witnesses mentions is not defined* — which is what turns
/// `#ifdef NAME` from `Unknown` into a branch, and with it every conditional region in every header the project
/// reaches. See the note at the call site ([`Session::assemble`]) for what it is worth measured.
///
/// # Why the compiler's own answer is the licence
///
/// A condition is answered against three witnesses: the compiler's predefined names, the command line's `-D`s, and
/// the file's own `#define`s. The second and third are in files — the build description and the source — and the
/// first exists **nowhere**: `_MSC_VER`, `__cplusplus` and five hundred more are built into the compiler, and
/// `-dM`/`/PD` is the only way to see them. So a session that has that table knows everything there is to know
/// about the command line *except what a build description nobody can read said*, and one that does not have it
/// knows neither half.
///
/// Empty is the signal, and it is not "this compiler predefines nothing": [`Toolchain::builtin_macros`] says so —
/// a compiler that could not be asked (no `/Zc:preprocessor`, no resources for the language it is running in) and
/// the last-resort toolchain built from the system's conventional header directories both arrive here with an
/// empty table and a [`Toolchain::note`] explaining it.
///
/// A compile database is **not** part of the test, although it was the whole of it before: it is a source of
/// flags, not a witness to the built-ins, and tying the claim to it made every project with no build system read
/// MSVC's standard library as if nothing in it were compiled. See [`Session::assemble`].
fn the_compilation_is_known(toolchain: Option<&Toolchain>) -> bool {
    toolchain.is_some_and(|found| !found.builtin_macros.is_empty())
}

/// **The files of every library this project depends on**, restricted to what a dependency is used through.
///
/// Headers (`.h`, `.hpp`, `.hxx`, `.h++`, `.inl`, `.ipp`) under each library's include directories, and module
/// interfaces (`.cppm`) under its module directories. **No `.cpp`**: see `LibrarySection` for what that costs and why
/// the cost is the right one — a dependency's implementation is not part of what this project compiles, and indexing it
/// would multiply the index by every dependency's sources on every cold start.
///
/// A missing directory is skipped rather than reported: a library whose path does not exist is a configuration problem
/// the discovery layer already reports, and a session built from a provider that cannot list directories (an in-memory
/// project) has nothing to walk.
fn library_files_to_read(
    libraries: &[crate::project::LibrarySection],
    workspace: &std::path::Path,
) -> Vec<PathBuf> {
    const HEADER_EXTENSIONS: &[&str] = &["h", "hpp", "hxx", "h++", "inl", "ipp"];
    /// How deep a dependency's public headers are walked. Deep enough for `include/pkg/detail/x.h`, bounded so that a
    /// library whose path is a whole disk cannot turn a start-up into a walk of it.
    const MAX_DEPTH: usize = 12;
    const MAX_FILES: usize = 20_000;

    let mut found: Vec<PathBuf> = Vec::new();
    for library in libraries {
        let mut roots: Vec<PathBuf> = library.include_directories(workspace);
        roots.extend(library.module_directories(workspace));
        let mut stack: Vec<(PathBuf, usize)> = roots.into_iter().map(|root| (root, 0)).collect();
        while let Some((directory, depth)) = stack.pop() {
            if depth > MAX_DEPTH || found.len() >= MAX_FILES {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    stack.push((path, depth + 1));
                    continue;
                }
                let extension = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(|extension| extension.to_lowercase());
                let wanted = match extension.as_deref() {
                    Some("cppm") => true,
                    Some(other) => HEADER_EXTENSIONS.contains(&other),
                    None => false,
                };
                if wanted {
                    found.push(path);
                }
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// **Where an `#include` for this file belongs**, as a byte offset into the written text: right after the last
/// top-level `#include` line, or the top of the file when there is none.
///
/// The same rule the code action uses ([`FixContext::after_the_includes`](../../cpp_ls) in the server), expressed in the
/// offsets a `TextEdit` needs.
fn after_the_includes(text: &str) -> usize {
    let mut end = 0usize;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("#include") {
            end = offset + line.len();
        }
        offset += line.len();
    }
    end
}

/// **Where an `import` belongs**: after the module declaration or the last top-level import, in the file's offsets.
fn after_the_module_imports(text: &str) -> usize {
    let mut end = 0usize;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let imports = trimmed.starts_with("import ") && trimmed.trim_end().ends_with(';');
        let declares_a_module = (trimmed.starts_with("module ")
            || trimmed.starts_with("export module "))
            && trimmed.trim_end().ends_with(';');
        if imports || declares_a_module {
            end = offset + line.len();
        }
        offset += line.len();
    }
    end
}

/// Fold a project's configuration into the filter a session reads through.
///
/// The exclusions are applied **on top of** whatever the caller passed, because the two lists are different claims that
/// are both true at once: the caller's is the editor's ("these files are not what I am working on"), the project's is
/// the file's ("these files are not source"). The cache directory goes in here too, so that the rule keeping a watcher
/// from re-indexing the cache's own writes follows the configuration rather than the default — one place, and the
/// store reads the same name from the same report.
fn apply_project_config(filter: WatchFilter, report: &ConfigReport) -> WatchFilter {
    let mut filter = filter;

    for pattern in &report.config.workspace.exclude {
        if let Ok(pattern) = crate::PathPattern::new(pattern) {
            filter = filter.ignore_pattern(pattern);
        }
    }

    if let Some(cache_dir) = &report.config.index.cache_dir {
        filter = filter.with_cache_directory(cache_dir);
    }

    // **A UE project's build trees are not source.** `Intermediate` holds the generated headers — which the analysis
    // must still *read* when a source includes one, and must never analyse as a translation unit — and `Binaries`,
    // `Saved` and `DerivedDataCache` hold outputs. Measured on a real UE checkout: `Intermediate/Build` is most of the
    // file count under the project, and indexing it produces findings about generated code nobody edits.
    //
    // Excluded by **pattern** rather than by a special case in the walk, because that is what this filter is for and
    // because a project that keeps its generated code somewhere else can say so with `[workspace] exclude`.
    if report
        .config
        .unreal
        .as_ref()
        .is_some_and(|unreal| unreal.is_on())
    {
        for pattern in [
            "Intermediate/**",
            "**/Intermediate/**",
            "Binaries/**",
            "Saved/**",
            "DerivedDataCache/**",
        ] {
            if let Ok(pattern) = crate::PathPattern::new(pattern) {
                filter = filter.ignore_pattern(pattern);
            }
        }
    }

    filter
}

/// The sources a project owns: the compile database's files, or a scan of the root.
///
/// The database when it names files that are actually here — a checked-in `compile_commands.json` is written on
/// somebody else's machine, so its absolute paths may name nothing locally, and a list of files that do not exist
/// is worse than a scan. The scan is a fallback and not a merge: a database that resolves is the project's own
/// statement of which files are compiled, and adding every other file under the root to it would index test
/// fixtures and dead code.
///
/// # The scan reads the filesystem, not the provider
///
/// Deliberate, and the one place in this module where the two disagree. A project scan is a question about a
/// **directory** — what is in it — and a provider interface that answered it would be a filesystem listing by
/// another name: `MemoryFiles` has no directories, and a buffer set has no "everything else". A caller with
/// synthetic files says so with [`Session::with_config`] plus its own seeds; a session over the real disk, which
/// is the only kind [`Session::open`] builds, gets the walk.
fn project_files(
    files: &impl FileProvider,
    filter: &WatchFilter,
    root: &Path,
    database: Option<&CompileCommands>,
    extra_extensions: &[String],
    max_files: usize,
) -> Vec<PathBuf> {
    let mut found = Vec::new();

    if let Some(database) = database {
        for command in &database.commands {
            // A relative path in a database is relative to the entry's `directory`, which is where the build ran.
            let path = match (&command.directory, command.file.is_relative()) {
                (Some(directory), true) => directory.join(&command.file),
                _ => command.file.clone(),
            };

            if files.exists(&path) && !filter.is_ignored(&path) {
                found.push(path);
            }
        }
    }

    if found.is_empty() {
        scan(root, filter, &mut found, extra_extensions, max_files);
    }

    // Sorted and deduplicated: the walk's order is the filesystem's, and an index whose contents depend on which
    // directory entry the OS returned first is one whose behaviour cannot be compared between two runs.
    found.sort();
    found.dedup();
    found
}

/// Every source under `root`, bounded and with symlinked directories left alone.
///
/// The bound is the caller's (`index.max_files`, or [`MAX_PROJECT_FILES`]). Symlinks are not followed **as
/// directories**: a link back up the tree is an infinite walk, and the cheap rule that rules it out — the entry's
/// own type, which does not resolve the link — is also the one that keeps a project from indexing the same files
/// through two paths.
fn scan(
    root: &Path,
    filter: &WatchFilter,
    found: &mut Vec<PathBuf>,
    extensions: &[String],
    max_files: usize,
) {
    let mut pending = vec![root.to_path_buf()];

    while let Some(directory) = pending.pop() {
        if found.len() >= max_files {
            return;
        }

        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };

        for entry in entries.flatten() {
            if found.len() >= max_files {
                return;
            }

            let path = entry.path();
            if filter.is_ignored(&path) {
                continue;
            }

            let Ok(kind) = entry.file_type() else {
                continue;
            };

            if kind.is_symlink() {
                continue;
            }

            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && is_a_source_file(&path, extensions) {
                found.push(path);
            }
        }
    }
}

/// Does this path's name say it is a source? See [`SOURCE_EXTENSIONS`] for why a scan may ask this and an include
/// may not.
///
/// `extra` is the project's own list (`workspace.source_extensions` in `.cppls.toml`), **added** to the engine's:
/// a project that writes `.cuh` is saying "and also these", and a project that wanted to *replace* the list would
/// be one analysing C++ without `.cpp`.
fn is_a_source_file(path: &Path, extra: &[String]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let lower = extension.to_ascii_lowercase();
            SOURCE_EXTENSIONS.contains(&lower.as_str())
                || extra
                    .iter()
                    .any(|listed| listed.trim_start_matches('.').eq_ignore_ascii_case(&lower))
        })
}
#[cfg(test)]
mod tests;
/// How many rendering summaries a session remembers, and how much they may weigh.
///
/// The weight is `declarations + macros`, which is what a summary is made of, so the bound is "a handful of the
/// largest headers": a rendering summary of `bits/basic_string.h` is about 1 100 declarations and one of
/// `stl_bvector.h` about 440. Sixteen of the largest is a few tens of thousands of entries.
const RENDERING_SUMMARY_ENTRIES: usize = 16;
const RENDERING_SUMMARY_BYTES: usize = 40_000;
