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

use crate::PathInterner;
use crate::cache::SummaryKey;
use crate::file::coordinates::{FileOffset, ReadingOffset};
use crate::file::paths::{DiskFiles, FileProvider, OverlayFiles, normalize_path};
use crate::file::vfs::Vfs;
use crate::file::view::{FileView, TokensOf};
use crate::include::config::{CompileCommands, CompilerConfig, project_config_from_flags};
use crate::include::toolchain::{self, DiskCommands, Environment, Toolchain};
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
use crate::summary::{OutlineSymbol, UnitReading};
use crate::symbol::{Known, UnknownReason};

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
const RE_READ_SLICE: usize = 4;
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
/// `walked` is the number `docs/status.md` stage 1 is judged by: typing in a file's **body** must not
/// make it move, because the body is not an input to the file's own timeline. Before the root of a unit was keyed
/// on its preamble it moved on every keystroke, and no other number in this crate would have shown it.
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
    unit_index: Option<(u64, crate::IndexedUnit)>,
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
    rendered_unit: Option<(u64, crate::RenderedUnit)>,
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
    walked_by_content: std::sync::Mutex<
        std::collections::HashMap<(u64, u64), std::sync::Arc<crate::TranslationUnit>>,
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
        std::collections::HashMap<(PathBuf, u64), std::sync::Arc<cpp_parser::MacroEnvironment>>,
    >,
    /// **Files whose macros something has asked for and not yet got.**
    ///
    /// The other half of the arrangement [`Session::macro_environments`] describes: a query cannot pay 103.7 ms,
    /// so it asks for the environment and reads the plain view, and the **work loop** builds it — the same shape
    /// [`Session::want_cooked_reading`] and `Cooking` have, for the same reason. Behind a lock because the asking
    /// happens in a query (`&self`) and the building happens in the loop (`&mut self`).
    macro_work: std::sync::Mutex<Cooking>,
    /// **The renderings built for `view`**, remembered against the content they were read from.
    ///
    /// The same arrangement as [`Session::macro_environments`] and for the same reason: a rendering is a unit walk
    /// and a preprocess, a query cannot pay it, and the file it is asked about does not change between two
    /// keystrokes. Keyed by the **content hash** rather than the path, because a buffer being typed in has a new
    /// text on every keystroke and a rendering of the old text would be a wrong answer rather than a stale one.
    renderings: std::sync::Mutex<
        std::collections::HashMap<
            (PathBuf, u64),
            std::sync::Arc<crate::preprocess::cooked::RenderedCooked>,
        >,
    >,
    /// **The files a cook has already refused**, by the queue's own key.
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

        let toolchain = toolchain::discover_with(
            &files,
            &DiskCommands,
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
        session.sweep_the_cache_in_the_background();
        session
    }
}

/// **Everything a cook needs, gathered under the write lock and then used without it.**
///
/// Owned rather than borrowed on purpose: the caller has to let go of the session between
/// `Session::cooking_materials` and `Session::render_a_cooked`, so nothing here may borrow it. The unit is an
/// `Arc` the cache already holds, and the seed is a table built from the index's own macros.
pub struct CookingMaterials {
    /// The index's own spelling of the path, which is what every other layer keys on.
    pub path: PathBuf,
    /// The summary key the rendered reading is filed under.
    pub key: SummaryKey,
    /// The translation unit whose environment this file's macros come from.
    pub unit: std::sync::Arc<crate::TranslationUnit>,
    /// The file's own text.
    pub text: String,
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
        let project = project_files(
            &files,
            &filter,
            &root,
            database,
            &project_config.config.workspace.source_extensions,
            project_config
                .config
                .index
                .max_files
                .unwrap_or(MAX_PROJECT_FILES),
        );

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
        // **Measured on the workspace that asked for this** (`C:\Users\zc\Desktop\cpp_project`: one `main.cpp`,
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
            project: project.clone(),
            queue: Work::default(),
            parsed_since_the_last_pass: Vec::new(),
            cooking: Cooking::default(),
            definitions: std::sync::Mutex::new(crate::MacroDefinitions::default()),
            units: std::sync::Mutex::new(UnitTable::default()),
            walked_by_content: std::sync::Mutex::new(std::collections::HashMap::new()),
            units_read: std::collections::HashSet::new(),
            unit_index: None,
            rendered_unit: None,
            directive_signatures: std::collections::HashMap::new(),
            prefetched: HashMap::new(),
            headers,
            macro_environments: std::sync::Mutex::new(std::collections::HashMap::new()),
            macro_work: std::sync::Mutex::new(Cooking::default()),
            renderings: std::sync::Mutex::new(std::collections::HashMap::new()),
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

    /// The configuration every summary in this session is keyed against.

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
    /// `None` is the file cache, which is still the default until the database is measured on a real project; see
    /// `docs/refactor-cache-and-startup.md` M1.
    pub fn summary_database_sizes(&self) -> Option<crate::cache_db::DatabaseSizes> {
        self.store.summary_database_sizes()
    }

    /// **Collect and compact the summary database** — the idle step, never a query.
    pub fn collect_the_cache(&mut self) -> Option<(crate::cache_db::Collected, bool)> {
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

        CacheSizes {
            renderings,
            rendering_bytes,
            units: self.held_units().held.len(),
            summaries,
            facts,
            macros,
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

    /// **Sweep the summary cache on a thread of its own**, once, when a project is opened.
    ///
    /// What it removes is only what could never be served again — see [`crate::cache::prune`] — so it changes no
    /// answer and needs nothing from the session but the directory's name. It is the product path only
    /// ([`Session::open`]), not [`Session::with_config`]: a test that counts the files in a cache directory does
    /// not want a second party deleting them, and a session over a provider that is not the disk has no directory
    /// to sweep. The thread is detached and its result unread: a sweep that fails has left a cache that is larger
    /// than it should be, which is where it started.
    fn sweep_the_cache_in_the_background(&self) {
        let cache = self.store.cache_directory().to_path_buf();
        let _ = std::thread::Builder::new()
            .name("cppls-cache-sweep".to_string())
            .spawn(move || {
                crate::cache::prune(&cache, crate::cache::CACHE_BUDGET_BYTES);
            });
    }

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
        }
        self.units_read.remove(&queue_key(path));
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
        for dependent in self.store.index().dependents_of(path) {
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

            // **Only the files that had a reading are asked for another one.** Dropping a stale reading is what this
            // loop is for; re-marking a file that never had one would cook a file nobody has looked at, which is the
            // 11.7 s the cooking policy was narrowed to avoid — paid here one header edit at a time. A file with no
            // reading is answered from its own text, and gets one when something looks at it.
            let had_a_reading = self.store.index().cooked_declarations(&dependent).is_some();
            self.store.index_mut().forget_cooked(&dependent);
            if had_a_reading {
                self.cooking.want(&dependent);
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
        // **A request clears a refusal**, because it is a statement that someone needs the answer now. The pump's
        // closure walk does not clear it — that is the retry loop this exists to break (see `cooking_refused`).
        if let Ok(mut refused) = self.cooking_refused.lock() {
            refused.remove(&queue_key(path));
        }
        // **And read it, if nothing has.** A reading is built out of a summary ([`Session::cook`] answers `None`
        // without one), and a file a request names may be one no pump has reached — a header outside every
        // translation unit, a file the workspace scan did not list. Wanting it cooked and not reading it is a
        // promise the session never keeps: the next request gets the same raw reading and the `isIncomplete` flag
        // goes on telling the client to come back for an answer that is not coming.
        if self.store.index().summary(path).is_none() {
            self.queue.add(path.to_path_buf(), Priority::Open, 0);
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
    /// It asks the two questions the mutation itself asks, of the same state: the VFS's own `held` table, the
    /// index's own cooked-reading slot, and the cooking queue's own membership set ([`Cooking::wants`], which reads
    /// the set [`Cooking::want`] inserts into). Nothing here re-derives a fact another way — the failure this crate
    /// has already paid for twice (see `docs/status.md` §2) is two implementations of one question, and a predicate
    /// that answered a *different* question would be the third.
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
            // **A file whose macro environment this session holds is left out of the wave.** The wave parses on the
            // read path, where a summary is a pure function of the text — which is exactly what cannot place a scope
            // a macro in **another** file opens. For the one file this session knows better about, the wave would
            // build the wrong reading *and write it to the disk cache*, and then `index_one`'s own attempt would be
            // served that entry as a hit: measured, dropping the wave's answer afterwards turned the step into
            // `Reused` and left the environment unused, which is worse than either.
            //
            // So it is not a root of this wave, and its includes are prepared by the iteration that reads it.
            // **This is what makes §5.2's steps 1–3 reachable at all**: they live at `index_one`'s `None` arm, and
            // the pump's path always reached it with `Some`, so the mechanism was correct, tested, and dead.
            let holds_the_environment = self.holds_an_environment_for(&path);
            if left > 1 && !holds_the_environment && !self.prefetched.contains_key(&key) {
                let mut wave = vec![path.clone()];
                wave.extend(
                    self.queue
                        .peek(left - 1)
                        .into_iter()
                        .filter(|queued| !self.prefetched.contains_key(&queue_key(queued))),
                );

                let room = (steps * 4).saturating_sub(self.prefetched.len());
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
                for (path, made) in made {
                    self.prefetched.insert(queue_key(&path), made);
                }
            }

            // **Read here rather than out of the wave**, for the file the wave was told to leave alone: with no
            // prepared entry, `index_one` builds the summary itself and can hand it the environment. Every other
            // file still takes the wave's answer.
            let ready = self.prefetched.remove(&key);
            done.push(self.index_one(path, priority, depth, ready));
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
            eprintln!(
                "ADVANCE steps={} done={:?} closure(api.h)={:?}",
                steps,
                done.iter()
                    .map(|step| step.path.clone())
                    .collect::<Vec<_>>(),
                self.closure_with_text(Path::new("/p/api.h"))
                    .0
                    .iter()
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>()
            );
        }
        let drain = self.finish_a_slice(&done, units);

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
        let prepared = self.prepare_the_drain(drain);
        self.commit_the_drain(prepared);

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
        let units = self.units_for_the_pass(&slice);

        if wave.is_empty() {
            return PreparedWave {
                prepared: Vec::new(),
                units,
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
        let PreparedWave { prepared, units } = wave;
        for (path, made) in prepared {
            self.prefetched.insert(queue_key(&path), made);
        }

        let mut done = Vec::new();
        for _ in 0..steps {
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
        mut units: Vec<std::sync::Arc<crate::TranslationUnit>>,
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
        // **And not the files that were parsed *with* their environment.** This pass exists to repair a reading
        // made without one, and `Step::with_the_environment` is the mark that says the repair has already happened
        // — a file whose summary was built from the closure's macro bodies the first time has nothing a re-parse
        // could change. That is the whole of what the flag is for; see `SummaryStore::prepare_with_the_environment`
        // for why the session can hold an environment for a file at all.
        self.parsed_since_the_last_pass.extend(
            done.iter()
                .filter(|step| matches!(step.outcome, StepOutcome::Built | StepOutcome::Unstored))
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
            let take = self.parsed_since_the_last_pass.len().min(RE_READ_SLICE);
            self.parsed_since_the_last_pass.drain(..take).collect()
        } else {
            Vec::new()
        };

        // **A caller that passed no timelines for a slice it is about to drain.** `advance` roots its units at the
        // files it just read, which is not the slice this pass takes — so on the drain phase (nothing left to read,
        // a backlog in hand) it passed an empty list, and `prepare_the_re_read` changed shape because of it: every
        // indexed file's text copied, every `#define` in the project asked about. See
        // [`Session::prepare_a_wave`] for the same defect on the pump's path, where it was 28 s of a 48 s start-up.
        //
        // Building them here is inside the writer on the pump's path and free of it on `advance`'s — and the pump
        // never reaches this line, because `prepare_a_wave` now always hands it the slice's timelines.
        if units.is_empty() && !files.is_empty() {
            units = self.units_for_the_pass(&files);
        }

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
        let mut drain = Drain {
            files,
            units,
            environments: Vec::new(),
        };

        if self.is_idle() {
            // **NOT WIRED, ON PURPOSE — and the reason has changed.** It used to be "the answers get worse": with a
            // unit read, `declarations_in("std")` *fell* by 49 and `std::size_t` went from "resolved in a header" to
            // "the index has no such name". That reason is **gone** — it was one file's unclosed scope swallowing
            // the rest of the program, which the crossing fence in `index_unit_rendering` now quarantines. Measured
            // again with the fence in place (`examples/unit_read.rs`, one process, before and after):
            //
            //     declarations_in("std")     1959 → 2452   (+493)
            //     definition: in a header      53 → 56
            //     definition: no such name     12 →  8
            //     per file                    every file's raw count unchanged, cooked 0 → N   (nothing lost)
            //     the one new `Ambiguous`     std::size_t, which was `NotDeclaredHere` before
            //
            // What keeps the switch off now is **cost, measured**: a unit read is 2.7 s on the 138-file project
            // (render 1.30 s, parse of the rendering 0.69 s, sweep 0.42 s, fence 0.14 s) and it is one indivisible
            // step, so calling it here holds the writer — the token stream, hovers, every request — for all of it,
            // on the first drain after a file is opened. That is the same budget [`Session::want_the_closure_cooked`]
            // was narrowed to protect (11.7 s of every startup), and the value it buys is the one that on-demand
            // cooking already reaches for the files a question names.
            //
            // So the capability is **requested, not scheduled**: [`Session::read_the_unit`] and
            // [`Session::read_a_looked_at_unit`] are the API, `units_read` keeps it to once per file per directive
            // change, and a caller that wants the whole program read as a compiler reads it says so.
            //
            // self.read_a_looked_at_unit();
            //
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
            (!files.is_empty()).then(|| self.store.prepare_the_re_read(&files, &units, false));
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

    /// **Apply what [`Session::prepare_the_drain`] worked out** — the index insert, and the remembered renderings.
    pub fn commit_the_drain(&mut self, prepared: PreparedDrain) -> usize {
        for (key, rendered) in prepared.rendered {
            if let Ok(mut known) = self.renderings.lock() {
                known.insert(key, rendered);
            }
        }

        match prepared.re_read {
            Some(plan) => self.store.commit_the_re_read(plan),
            None => 0,
        }
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
        for (candidate, unit) in self.walk_the_timelines(candidates) {
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
            self.vfs.load(&path);
        }
        let before = self.store.stats();

        // **The environment this session already holds for this file, if it holds one.** A unit that is *not* in
        // memory is one whose closure this session has not read, and an index step is not the place to start: the
        // walk is minutes of work on a real project, and starting it here would put it in front of the file it is
        // meant to be reading. `None` is therefore the ordinary cold case and means today's path, unchanged.
        //
        // **Asked by the index's own spelling**, because a unit's frames are keyed by the spelling the walk used
        // and asking with any other one answers `None`. `Session::units_for_the_pass` records the same trap from
        // the other side — a header whose name is spelled one way by the session and another by the include graph
        // is a header that gets a unit per spelling.
        //
        // The `Arc` is held on its own because a `MacroView` borrows the unit it came from, and the unit has to
        // outlive it.
        let spelled = self
            .store
            .index()
            .summary(&path)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| path.clone());
        let held = self.held_units().get(&queue_key(&spelled));
        // **And not a provisional one.** `with_the_environment` asserts that the second pass has nothing to repair,
        // and a timeline built while a file the closure needs was still queued is missing exactly what that file
        // defines — so reading a file with it and marking the result would suppress the repair for a reading that
        // needs it. A provisional timeline is a reading of most of the program, which is worth having for a *cook*
        // and is not worth asserting anything about. See [`HeldUnit`].
        let unit = held
            .filter(|(provisional, _)| !*provisional)
            .map(|(_, unit)| unit);
        let view = unit.as_ref().and_then(|unit| unit.environment_of(&spelled));
        // Read before the wave's work is moved into the store, because the flag below is about which arm ran.
        let read_here = ready.is_none();

        let summary = match ready {
            // A wave prepared this file, and a wave has no unit of its own (`Session::prepare_a_wave` runs before
            // the pass that builds them), so what it produced is what it is and the second pass still looks at it.
            Some(ready) => self.store.commit(&path, ready),
            None => match &view {
                // **Read once, with the environment** — instead of once without it and once again in the second
                // pass. See `SummaryStore::prepare_with_the_environment`.
                Some(view) => {
                    let prepared = self.store.prepare_with_the_environment(&path, view, view);
                    self.store.commit(&path, prepared)
                }
                None => self.store.get(&path),
            },
        };
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

        // Everything this file includes joins the same half of the list the file came from, one level further out —
        // a header an open file includes is worth reading before the rest of the project, and a header the project's
        // tenth translation unit includes is not.
        for include in includes {
            self.queue.add(include, priority, depth + 1);
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
                && view.is_some()
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
        if self.queue.forget(path) {
            self.index_one(path.to_path_buf(), Priority::Open, 0, None);
        }

        for include in self.unread_includes_of(path) {
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
    pub fn is_closure_ready(&self, path: &Path) -> bool {
        if !self.store.index().summary(path).is_some() {
            return false;
        }

        self.store
            .index()
            .visible_files(path)
            .iter()
            .all(|(file, _)| self.store.index().summary(Path::new(file)).is_some())
    }

    /// **Can a question about this file be answered from what has been read?** — the question a *local* request asks.
    ///
    /// [`Session::pending`] is the project's queue, and it is not this question. What a name means, where it is
    /// declared, what a call's signature is: each of those depends on the file and on the headers it includes, and on
    /// nothing else in the project. A project of five hundred sources whose *other* files are still queued is fully
    /// able to answer about the one the editor has open.
    ///
    /// The measured cost of the confusion: opening a real project queued 1122 files, and every local request was
    /// refused until the last of them — including a second pass that re-reads 773 files at about half a second each,
    /// so the refusal lasted **minutes** after the answers were already available. See `docs/status.md` §九 and
    /// `docs/cold-start-cpu.md`.
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
            let read = self.advance(64).len();
            total += read;

            // **Every stage of the cook is counted, because a file can be claimed and then lost.**
            //
            // `claim_cooking` pops, and the three stages after it each drop a file by answering `None` —
            // `materials_for` when a file's unit is provisional or its summary is not in the index yet, `render_them`
            // when the render itself declines, and `commit_them` when there is nothing to insert. A pop followed by a
            // drop is a file that is **no longer queued and never cooked**, and the loop below reads that state as
            // "nothing left to do": `read == 0 && cooked == 0` ends the work with a cooking backlog still standing.
            // Measured on the completion fixture: `pending 0, cooking 22` on every round, for six rounds.
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

            // **The second pass is work, and this loop used to return while it was owed.** `advance` drains up to
            // [`RE_READ_SLICE`] of the backlog per call, and the backlog is filled by a cold start; the loop below
            // ended at `read 0, cooked 0`, which is exactly the state a session is in while it still owes re-reads.
            // Measured on `EmmyLuaCodeStyle` (cold summary cache): `index_everything` returned with **1 205 files**
            // owed, 302 slices and **27.3 s** of work left — and every one of those slices reported `nothing bodied`,
            // so the value it bought was not visible in any answer. The cost *was* visible: the next keystroke's pump
            // slice took four of those files and four unit walks, and typing was **65 ms instead of 10** for about
            // three hundred keystrokes. A caller that says "index everything" is owed this too.
            // **The second pass is owed, and this loop does not pay it.** `8e976e1` folded it in here, and the
            // measurement is `docs/edit-latency.md` §「冷启动的第二遍」: a cold start of `EmmyLuaCodeStyle` went from
            // **12.6 s to 135.6 s**, because the pass walks a unit per re-read file — **1 621 walks** for 1 761
            // files, at ~75 ms each. The pump already drains it, four files a slice, and `pending_work` counts it, so
            // the work still happens; what the loop bought was paying for it *before* the session answered anything.
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
        self.queue.pending() + self.cooking.len() + self.parsed_since_the_last_pass.len()
    }

    /// Is everything the session knows about already **read**?
    ///
    /// **The indexing queue's question, not [`Session::pending`]'s**: the two differ by the cooking backlog, and the
    /// difference is load-bearing. A file's cooked reading is built after its includes have been read (their macros
    /// are its environment), and one more thing happens at that same moment — the second parse of a file whose
    /// meaning depended on a header that arrived later. Cooking before that pass would build a reading of an
    /// environment the summaries are about to revise.
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
    /// A third kind of pending work, and the one no count used to name: [`Session::pending`] is the index queue and
    /// [`Session::pending_cooking`] the cook queue, while the `parsed_since_the_last_pass` backlog is drained only by
    /// a slice's tail and only while the index queue is empty. Measured on `EmmyLuaCodeStyle` right after
    /// `index_everything` returned, on a cold summary cache: **1 205 files**, which took **302 slices and 27.3 s** to
    /// clear — and every one of those slices reported `nothing bodied`, so the whole tail rebuilt no summary at all.
    /// Until it was cleared, the next keystroke's slice paid for four of those files and four unit walks: measured
    /// **65 ms a keystroke** instead of 10, for about three hundred keystrokes. See `docs/edit-latency.md`.
    pub fn re_read_backlog(&self) -> usize {
        self.parsed_since_the_last_pass.len()
    }

    /// **Drop everything the cook queue is waiting for, and say how much it was.**
    ///
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

    /// **Write the sharing cache** next to the summaries, so that the next run starts warm — §8 step 4.
    ///
    /// One file, `<cache>/sharing.bin`. Its entries carry their own identity (the file's fingerprint and the projection
    /// of its conditions), so a stale file cannot answer a different question — it simply fails to hit.
    pub fn save_the_sharing(&self) -> std::io::Result<usize> {
        use crate::index::{walk_cache, walk_cache_codec};

        let entries = walk_cache::saved();
        let written = entries.len();
        std::fs::write(
            self.store.cache_directory().join(SHARING_FILE),
            walk_cache_codec::encode(&entries),
        )?;
        Ok(written)
    }

    /// **Read the sharing cache back**, if this run's cache directory holds one.
    ///
    /// A missing or unreadable file is a cold start, not an error: the cache is an accelerator and the walk is always
    /// correct without it. Say how many entries were restored, because "it loaded nothing" and "it did not run" are
    /// different facts and only one of them is interesting.
    pub fn load_the_sharing(&self) -> usize {
        use crate::index::{walk_cache, walk_cache_codec};

        let Ok(bytes) = std::fs::read(self.store.cache_directory().join(SHARING_FILE)) else {
            return 0;
        };
        let Some(entries) = walk_cache_codec::decode(&bytes) else {
            return 0;
        };
        let restored = entries.len();
        walk_cache::restore(entries);
        restored
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
        let file = self.vfs.held(&key)?;

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
            Some(rendered) => Some(FileView::parse_rendering(file, &rendered)),
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
        // Queue it exactly as `view` does, so that asking twice is a request rather than a wait.
        if let Ok(mut work) = self.macro_work.lock() {
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
        let macros = self
            .known_macros_of(&file.path, &file.text)
            .unwrap_or_default();
        Some(FileView::parse_with(&file, macros.as_ref()))
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
        Some(FileView::parse_rendering(&file, &rendered))
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

        if let Ok(mut known) = self.macro_environments.lock() {
            known.insert(
                (path.to_path_buf(), crate::cache::content_hash(text)),
                made.clone(),
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
                return Some((
                    at + indent,
                    at + line.trim_end().len(),
                    message.to_string(),
                ));
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
        crate::sema::check::run(crate::sema::check::CheckContext::new(
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
            self.config_for(&key).language_for(&key),
        ))
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
        let cache = crate::TranslationUnitCache::of_store(&self.store);
        let root_key = self.root_key_of(path);

        // Read out of the cache under a timer, then matched: a stage timer inside a `match` scrutinee is a block
        // clippy asks to hoist, and hoisting it is what the timer wants anyway — it should cover the read only.
        let cached = { cache.get(path, context, &self.files, root_key) };

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
                let read_started = std::time::Instant::now();
                let (closure, closure_by_key) = { self.closure_with_text(path) };
                let read_took = read_started.elapsed();
                let closure_bytes: usize = closure.iter().map(|(_, text)| text.len()).sum();
                let walk_started = std::time::Instant::now();

                // **The session's own definition cache**, not a fresh one: the walk reads back every `#define` a
                // file sees, and the same definition reaches hundreds of files — `<vector>` and `<string>` both see
                // `_STD_BEGIN`, whose text is in `yvals_core.h`. One cache per *walk* is one parse per definition
                // per file, which the census measured at 12.2 s of a 40 s run. Invalidated **per path** when a file
                // changes, since the keys are offsets in that file.
                let index = self.store.index();
                let root = index.summary(path)?;
                // **Held rather than taken**, for the reason the units table carries: this cache is what made the
                // unit walk need `&mut self`, and `&mut self` is what put the walk inside the writer. A lock of its
                // own is enough — the walk is the only thing that ever holds it for long.
                let mut definitions = self.held_definitions();

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
                    .and_then(|held| held.get(&identity).cloned());

                let (unit, provisional) = if let Some(shared) = shared {
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
                        crate::TranslationUnit::walk(
                            root,
                            |wanted| {
                                let (held, text) =
                                    closure.get(*closure_by_key.get(&queue_key(wanted))?)?;
                                Some((index.summary(held)?, text.as_str()))
                            },
                            index.macros(),
                            &mut definitions,
                            // **A/B switch, kept**: `CPPLS_NO_SHARE=1` turns the sharing off so that the same machine,
                            // the same day and the same entry file can be measured with and without it. Comparing
                            // against a wall clock from an older build is how a 103 s figure got quoted for a code
                            // base it no longer described.
                            !provisional && std::env::var_os("CPPLS_NO_SHARE").is_none(),
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
                        let _ = cache.put(path, context, &walked, &self.files, root_key);
                        put_took = put_started.elapsed();

                        // …and offered to the **rest of this one**, under the same condition: a provisional
                        // timeline is one the queue will make obsolete, and handing it to a second root would
                        // spread the smaller program rather than save a walk.
                        if let Ok(mut held) = self.walked_by_content.lock() {
                            held.insert(identity, std::sync::Arc::clone(&walked));
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
    /// The number to watch is `walked`: it is a closure read and a timeline built, and the design in
    /// `docs/status.md` says a keystroke in a file's **body** must leave it where it was.
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

    /// **What a unit rooted at `path` is keyed on** — the file's preamble, when its text is in reach.
    ///
    /// A file that cannot be read is [`RootKey::WholeText`], which is the rule that can only ever be less
    /// generous: it refuses the entry on any change at all, which is what happened for every file before this
    /// existed. The read is the same one the walk is about to make, and the scan is one lex of one file against a
    /// walk of a whole closure — see `crate::tu_cache::RootKey` for what the key is and why it is sound.
    fn root_key_of(&self, path: &Path) -> crate::tu_cache::RootKey {
        match self.files.read(path) {
            Some(text) => crate::tu_cache::RootKey::of(&text),
            None => crate::tu_cache::RootKey::WholeText,
        }
    }

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
    pub fn render_the_unit(&mut self, root: &Path) -> Option<crate::RenderedUnit> {
        let unit = self.translation_unit_of(root)?;

        // **The text of every file the walk entered**, which is what the cook reads.
        let mut sources: HashMap<PathBuf, String> = HashMap::new();
        for path in unit.files() {
            if let Some(text) = self.files.read(path) {
                sources.insert(path.to_path_buf(), text);
            }
        }

        // **What this render is a function of**: every file's path and content, folded into one number.
        //
        // Not the root path. A file the reader has open and has **not saved** comes from the overlay, so an edit
        // changes the program without changing its name, and a cache keyed on the name would answer with a stream
        // built from text the files no longer have. Folding the *content* in is what makes a hit mean "the same
        // input", which is the only thing that makes reusing the output sound.
        //
        // Sorted, so two runs that read the same files in a different order of *this loop* still agree; the include
        // order that matters is already fixed inside the stream by the walk.
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

        // **The same inputs, already rendered.** A reader asks for the same unit again and again 鈥?every keystroke
        // that lands on a name, every drain 鈥?and a render is a walk, a cook of every file in include order and a
        // stitched stream of megabytes. The plan's M4 is this call: the second answer must not cost what the first
        // did.
        if let Some((seen, cached)) = self.rendered_unit.as_ref()
            && *seen == key
        {
            return Some(cached.clone());
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

        self.rendered_unit = Some((key, stream.clone()));
        Some(stream)
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
    /// That is what would make the index hold "what the compiler saw" for files **nobody has opened**: today a
    /// header's cooked facts exist only for the files some request happened to name, so a cross-file answer depends
    /// on whether a reader had looked at the header yet.
    ///
    /// # Why it is **not** wired into the pump
    ///
    /// Because it makes a measured answer worse, and that is not understood yet. On the real workspace, calling it
    /// at the drain changes three readings the wrong way:
    ///
    /// ```text
    ///                                   without the unit read   with it
    /// declarations_in("std") after draining          2008        1959
    /// definition, resolved in a header                 50          46
    /// definition, the index has no such name            8          12   (`std::size_t` one of them)
    /// ```
    ///
    /// **The bisect is exact**: commenting out the one call at the drain restores all three, with everything else in
    /// this session's work in place — so the cause is this call and not the laziness policy, not the timeline the
    /// second pass now reads, and not the `want_cooked_reading` fix that came with it.
    ///
    /// The obvious explanation does not survive reading the code: `visible_declarations_upto` gets the raw facts
    /// **first** and skips a cooked fact whose `(name, kind)` the raw list already has, so adding cooked facts can
    /// only add candidates. Something else is going on, and the next measurement is the one that names it rather
    /// than guesses: `std_probe --cooked-index` on this workspace's closure prints the two readings' declaration
    /// counts and their difference **per file**, which is what tells "the cooked facts are wrong" apart from "the
    /// raw facts moved".
    ///
    /// `None` when the file has no summary or the walk cannot be built — nothing has read it yet, which is a state
    /// and not an error.
    pub fn read_the_unit(&mut self, root: &Path) -> Option<UnitReading> {
        // The index's own spelling again, for the same reason [`Session::cook`] takes it: the unit's frames are
        // keyed by the normalized path.
        let root = self.store.index().summary(root)?.path.clone();
        let stream = self.render_the_unit(&root)?;

        let key = SummaryKey::new(0, self.store.context_hash(&root));
        let program = crate::cache::content_hash(&stream.text);
        let indexed = match self.unit_index.as_ref() {
            // **The same program, already read.** The reading is a function of the stream's bytes, and a project's
            // sources share their headers, so this is the common case rather than a lucky one: measured, twenty
            // sources including `<future>` all render to the same 617 607 tokens, and this turns the second and
            // later of them from a parse-and-sweep of the whole program into a clone of the facts.
            Some((seen, cached)) if *seen == program => cached.clone(),
            _ => {
                let indexer = FileIndexer::new(&self.files, &self.config);
                let indexed = indexer.index_unit_rendering(&root, &stream, key);
                self.unit_index = Some((program, indexed.clone()));
                indexed
            }
        };

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
        for (path, cooked) in indexed.files {
            placed += 1;
            self.store.index_mut().insert_cooked(&path, cooked);
        }

        Some(UnitReading {
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
        })
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

    /// **A unit of one file a reader is looking at**, read as a program — one per drain.
    ///
    /// The policy is [`Session::cook`]'s: a reading is built when something looks at the file. What changed is the
    /// **unit** the reading is built for — not "this file and its direct includes", but the whole translation unit
    /// the file is read in, parsed once, with every declaration in it filed under the file it was written in.
    ///
    /// One per drain, like every other step here, and bounded by the number of *open* files rather than by the
    /// project: a project with two hundred translation units does not read two hundred programs because one file is
    /// open. [`Session::read_a_unit_of_the_project`] is the caller that wants all of them.
    pub fn read_a_looked_at_unit(&mut self) -> Option<UnitReading> {
        let root = self
            .documents
            .paths()
            .into_iter()
            .find(|path| !self.units_read.contains(&queue_key(path)))?;

        let reading = self.read_the_unit(&root);
        self.units_read.insert(queue_key(&root));
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

        for _ in 0..MAX_MODULES_READ_AT_ONCE {
            // The module names the index cannot point at a file for, from the file itself and from every file the
            // previous round brought in — a fresh walk each round, because a round is a handful of hashmap lookups
            // and the alternative is a worklist that has to be kept in step with the index.
            let wanted = self.modules_the_index_cannot_name(path);

            if wanted.is_empty() {
                break;
            }

            let found = self.module_interface_units_of(&wanted, path);
            if found.is_empty() {
                break;
            }

            read_in += found.len();
            self.add_project_files(found.clone());

            for unit in found {
                // Queued in the **open** half, and re-armed rather than added: a file that arrived days ago with the
                // project scan is sitting in the rest half behind everything else, and the point of this call is
                // that a reader is waiting for a name in it.
                self.queue.again(unit, Priority::Open, 0);
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

        // **The directory, not the file.** `ModuleScanner::resolve_module`'s naming convention looks for a module
        // "beside the importing file", which it does by joining the name onto the directory it is given — and this
        // call passed the importing *file*, so every candidate became `<dir>/main.cpp/math.cppm`, which is not a
        // path any filesystem has. The result was not an error but a silence: `read_the_modules_a_file_imports`
        // answered **0** for every file, `imports_a_module_that_is_not_read` stayed true for ever, and a request in
        // a module file therefore took the session's write lock on **every keystroke** to discover that there was
        // nothing to read. Measured on a C++23 modules project (`docs/status.md` §十三): `import math;` from
        // `src/main.cpp` with `src/math.cppm` beside it, reported as "no file in this project declares module
        // `math`". The other two callers of this resolution (`scan_imports`, `module_reading`) already pass a
        // directory, which is why the standard-module tests passed: their module is found through an include path's
        // sibling `modules/` directory, and the broken first entry was never the one that answered.
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
        Some(self.commit_a_cooked(rendered))
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
        let (unit, provisional) = self.unit_and_state_of(&path)?;
        if provisional {
            return None;
        }

        let text = self.files.read(&path)?;
        let seed = MacroTable::from_marked(self.store.index().macros());

        Some(CookingMaterials {
            path,
            key,
            unit,
            text,
            seed,
            already_declared,
        })
    }

    /// **Lex, render and parse one file's text** — the expensive half of cooking, and it takes `&self`.
    ///
    /// Everything it reads is owned by its argument, so a caller may hold it under a **read** lock while a query runs
    /// beside it. [`Session::commit_a_cooked`] is the other half: one insert, `&mut self`.
    ///
    /// The unit and the seed are moved in rather than borrowed because the caller has to let go of the session
    /// between the two halves — that is the whole point of the split — and a borrow of `&self` cannot survive it.
    pub fn render_a_cooked(
        &self,
        materials: &CookingMaterials,
    ) -> Option<(crate::IndexedRendering, CookedReading)> {
        let CookingMaterials {
            path,
            key,
            unit,
            text,
            seed,
            already_declared,
        } = materials;

        let (tokens, _) = { cpp_parser::lex(text, &cpp_parser::LexerConfig::default()) };
        let unit_definitions = unit.definitions();
        let macros = {
            crate::preprocess::cooked::FileMacros::new(
                unit.environment_of(path)?,
                &unit_definitions,
                Some(seed),
                true,
            )
        };
        let rendered = { crate::preprocess::cooked::cook_with(text, &tokens, &macros).render() };
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
        Some((indexed, reading))
    }

    /// **Put a rendered reading into the index** — one insert, and it is the whole of what needs `&mut self`.
    pub fn commit_a_cooked(
        &mut self,
        reading: (crate::IndexedRendering, CookedReading),
    ) -> CookedReading {
        let (indexed, reading) = reading;
        let path = indexed.summary.path.clone();
        // **The rendering is remembered, not dropped.** This is the half that was missing, and its absence is why
        // `Session::view` could answer `None` for a file whose rendering had *just* been built and parsed: the facts
        // went into the index and the tree went in the bin, so nothing could reconstruct the reading until some
        // other path happened to remember one. With this, "the file has been read the way a compiler reads it" and
        // "the reading is available to a query" are the same state — which is what the rest of this layer assumes.
        //
        // Keyed by content hash exactly as `commit_the_drain` keys it, so the two writers of this cache cannot
        // disagree about what a cache entry means.
        if let Some(text) = self.text(&path)
            && let Ok(mut known) = self.renderings.lock()
        {
            let key = (path.clone(), crate::cache::content_hash(&text));
            let before = known.len();
            known.insert(key, std::sync::Arc::clone(&indexed.rendered));
            if crate::probes::cooking() {
                eprintln!(
                    "commit_a_cooked: inserted {} | cache {} -> {}",
                    path.display(),
                    before,
                    known.len()
                );
            }
        } else if crate::probes::cooking() {
            // **The other way this can silently not happen**: `self.text` answers `None` (the file is no longer
            // readable) or the lock is poisoned, and both leave the cache exactly as it was while the facts go into
            // the index — "read" and "available" apart again, which is the state this whole change is about.
            eprintln!(
                "commit_a_cooked: NOT inserted {} | readable {} | lock {}",
                path.display(),
                self.text(&path).is_some(),
                self.renderings.lock().is_ok()
            );
        }
        {
            self.store.index_mut().insert_cooked(&path, indexed.into());
        }
        reading
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
    pub fn render_them(
        &self,
        materials: Vec<CookingMaterials>,
    ) -> Vec<(crate::IndexedRendering, CookedReading)> {
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
    pub fn commit_them(
        &mut self,
        rendered: Vec<(crate::IndexedRendering, CookedReading)>,
    ) -> Vec<CookedReading> {
        rendered
            .into_iter()
            .map(|reading| self.commit_a_cooked(reading))
            .collect()
    }

    /// **Everything a cook needs, gathered under the write lock and then used without it.**
    ///
    /// Owned rather than borrowed on purpose: the caller has to let go of the session between
    /// [`Session::cooking_materials`] and [`Session::render_a_cooked`], so nothing here may borrow it. The unit is an
    /// `Arc` the cache already holds, and the seed is a table built from the index's own macros.

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
        let mut out: Vec<(PathBuf, String)> = Vec::new();
        let mut by_key: HashMap<String, usize> = HashMap::new();

        for path in self.closure_paths(root) {
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
    fn closure_paths(&self, root: &Path) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        // **By the key every other queue in this crate compares by.** This used to ask `same_file` against every
        // file already listed, which normalizes two paths per comparison — and the closure of a standard-library
        // header is around a thousand files, so the walk was quadratic in it: measured as 100 s to cook a 108-file
        // corpus against 4.5 s for 255 files, because the closure here is the whole standard library and there it is
        // not. One hash per file is the same answer without the arithmetic.
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: std::collections::VecDeque<PathBuf> = std::collections::VecDeque::new();
        queue.push_back(root.to_path_buf());

        while let Some(path) = queue.pop_front() {
            // One entry per **file**: two spellings of it are one file to cook, and a closure that listed both would
            // cook it twice, once under a name the index does not use.
            if !seen.insert(queue_key(&path)) {
                continue;
            }
            out.push(path.clone());

            if let Some(summary) = self.store.index().summary(&path) {
                for include in &summary.includes {
                    if let Some(resolved) = &include.resolved {
                        queue.push_back(resolved.clone());
                    }
                }
            }
        }

        out
    }

    /// Which declaration the name at `offset` means, using this file's scopes and then the index.
    ///
    /// The *one* projection of [`Session::definitions`]: `Yes` only when the answer is a single declaration, so a
    /// consumer that can show only one location gets nothing rather than an arbitrary one. A client that can show a
    /// list — `textDocument/definition` — should ask that instead.
    pub fn definition(&self, view: &FileView, offset: ReadingOffset) -> Known<ProjectDefinition> {
        definition_across_files(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            offset.get(),
        )
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
        let Some(written) = view.written_text() else {
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
            });
        }

        crate::CompletionSet {
            scope: String::new(),
            prefix: written.to_string(),
            replace: range,
            truncated: false,
            items,
        }
    }

    pub fn macro_definition(&self, view: &FileView, offset: ReadingOffset) -> Known<ProjectMacro> {
        macro_across_files(self.store.index(), &view.root, &view.path, offset.get())
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
        if let Some(written) = view.written_text() {
            if crate::file::view::in_prose(written, offset.get()) {
                return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("")));
            }
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
        let mut names = crate::semantic::classified_names(self.store.index(), view);

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
                .map_or(start, |end| end);
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

    /// **Which of these declarations a call at this offset means** — an index into `parameter_lists`.
    ///
    /// The same resolution the argument checks and the signature popup use, asked over the declarations a caller already
    /// holds: a hover has the facts of the name under the cursor and needs to know which of them the call selects. The
    /// answer must not depend on the index having read the declaring file *as a source of parsed signatures* — the
    /// parameters are in the facts (`DeclFact::parameter_list`) — because a hover on a call whose header is not yet in
    /// the index is exactly the case that made the signature query answer nothing (`docs/overloads.md` §18).
    ///
    /// `None` when the offset is not on a call, when no candidate is viable, or when the choice is ambiguous — the three
    /// answers a reader should see nothing marked for.
    pub fn the_declaration_a_call_means(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        parameter_lists: &[Option<String>],
        template_parameters: &[Vec<String>],
    ) -> Option<usize> {
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
                // **A template candidate is ranked as what the call instantiates**, when the caller could read its
                // header: `take(T*)` and `take(T)` say nothing about an `int*`, and deduced they say `int*` and `int`.
                // The names are the caller's because they come from the declaring file's text, which a fact does not
                // carry — `docs/overloads.md` §21.
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

        match crate::sema::overload::choose(&types, &candidates) {
            crate::sema::overload::Choice::One(chosen) => Some(chosen),
            _ => None,
        }
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
        substituted_members(self.store.index(), &view.path, written_type, listed)
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
    /// # Two sources, and why the second one exists
    ///
    /// The index's summary when it has one, and **the buffer's own parse when it does not**. That second state is
    /// ordinary rather than exotic: a file's summary is dropped the moment it is edited and read again one drain
    /// later, and an outline is refreshed *as the user types* — so "the summary is gone" must not mean "the outline
    /// is empty". Between two keystrokes the panel would blink, and what it would blink to is a file whose
    /// declarations are still right there in the text in front of the reader.
    ///
    /// The two readings differ in exactly one way, and it is worth knowing which: a summary was built **with the
    /// closure's macro bodies**, so a namespace a macro opens (`BEGIN_NS` is `namespace one {`) is a scope in it and
    /// not in a buffer that has only this file's tokens. The fallback therefore nests what the file writes
    /// literally — the honest reading of the text on screen — and it lasts one drain.
    ///
    /// # Which reading of the *two* this is
    ///
    /// Neither: an outline is about the **file**, so it is built from the raw facts in both cases (see
    /// [`crate::FileSummary::outline`]) — a declaration in a branch nobody takes belongs in an outline, and a type
    /// a macro declares does not. That is the opposite choice from every other query here, and deliberately so.
    pub fn outline(&self, view: &FileView) -> Vec<OutlineSymbol> {
        if let Some(summary) = self.store.index().summary(&view.path) {
            return summary.outline();
        }

        let preprocessing = crate::preprocess(&view.source, view.tree.get_tokens());
        let errors: Vec<cpp_parser::SourceRange> = view
            .tree
            .get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range))
            .collect();
        let (facts, _guards) =
            crate::build_facts(&view.scopes, &preprocessing, &view.root, &errors);

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
/// holds the rest ([`crate::TranslationUnitCache`]) and a unit that falls out of here is one decode away. Sixteen is
/// enough for the units a reader is working in (the open files' roots and the headers they name) and small enough
/// that a project of a thousand sources does not hold a thousand timelines.
const MAX_UNITS: usize = 16;

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
}

impl UnitTable {
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

        if !self.held.contains_key(&key) && self.held.len() >= MAX_UNITS {
            let oldest = self
                .held
                .iter()
                .min_by_key(|(_, held)| held.used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.held.remove(&oldest);
            }
        }

        self.held.insert(
            key,
            HeldUnit {
                used: self.clock,
                provisional,
                unit,
            },
        );
    }

    /// **Forget one file's unit** — what an edit to that file invalidates, and nothing else.
    ///
    /// `clear` was the only removal for most of this table's life, and it is the wrong width: see
    /// [`Session::buffer_changed`]. `clear` stays for the one case that really is whole-table — a closed buffer,
    /// whose text every unit in the table may have been built from.
    fn remove(&mut self, key: &str) {
        self.held.remove(key);
    }

    fn clear(&mut self) {
        self.held.clear();
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
    /// [`Session::needs_catching_up`] decides whether to take the write lock with it. A second spelling of "is this
    /// file queued" is exactly the shape of defect `docs/status.md` §2 records twice.
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

/// The compile database, from where the project says it is or from the conventional place.
/// Fold a project's configuration into the filter a session reads through.
///
/// The exclusions are applied **on top of** whatever the caller passed, because the two lists are different
/// claims that are both true at once: the caller's is the editor's ("these files are not what I am working on"),
/// the project's is the file's ("these files are not source"). The cache directory goes in here too, so that the
/// rule keeping a watcher from re-indexing the cache's own writes follows the configuration rather than the
/// default — one place, and the store reads the same name from the same report.
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