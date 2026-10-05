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

use crate::include::config::{
    CompileCommands, CompilerConfig, project_config_from_flags,
};
use crate::file::paths::{DiskFiles, FileProvider, OverlayFiles, normalize_path};
use crate::include::toolchain::{self, DiskCommands, Environment, Toolchain};
use crate::file::view::FileView;
use crate::index::project::{
    MemberCompletions, MemberList, NameCompletions, ProjectDefinition, ProjectDefinitions,
    ProjectIndex, ProjectMacro,
};
use crate::index::references::{MacroReferences, ReferenceBudget, macro_references};
use crate::inlay::ParameterHint;
use crate::index::store::{StoreStats, SummaryStore};
use crate::index::worklist::StepOutcome;
use crate::summary::{OutlineSymbol, UnitReading};
use crate::index::watch::{ChangeBatch, FileEvent, Response, WatchFilter};
use crate::index::worklist::{Priority, Step, outcome_of};
use crate::index::{
    FileIndexer, definition_across_files, definitions_across_files, macro_across_files,
    member_completions_at, members_of,
    name_completions_at,
};
use crate::macros::MacroTable;
use crate::cache::SummaryKey;
use crate::project::{ConfigReport, ProjectDiscovery};
use crate::file::vfs::Vfs;
use crate::sema::modules::{ImportOutcome, ModuleScanner};
use crate::symbol::{Known, UnknownReason};
use crate::PathInterner;

/// How many files one drain **cooks**.
///
/// The same shape as the indexing slice, and for the same reason: cooking a file is a unit walk, a parse of the
/// rendering and an index of it, so a slice keeps the writer lock short enough that the server answers between two
/// of them. The rest stays in the list and the next drain takes the next few — see `Session::pending`, which is
/// what tells a caller there is work left.
const COOK_SLICE: usize = 4;
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
    "c", "cc", "cpp", "cxx", "c++", "h", "hh", "hpp", "hxx", "h++", "ipp", "inl", "tpp", "tcc", "cppm", "ixx",
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

/// One open project: the configuration, the facts read so far, and the order the rest will be read in.
///
/// The provider chain is owned rather than borrowed, which is what lets a long-lived caller — a language server —
/// hold a session in a field. See the module documentation: the caller keeps a handle to the same chain by cloning
/// it before the session takes it, and a clone of a provider is a second handle on the same files.
pub struct Session<F: FileProvider = DiskFiles> {
    root: PathBuf,
    config: CompilerConfig,
    /// What `discover` found, or `None` when no compiler answered. Kept for a human reading a log line, and for
    /// 's measurements.
    toolchain: Option<Toolchain>,
    /// **What this analysis thinks the project is**: the configuration file, the compile database and where it was
    /// found, CMake's cache, and every problem. Kept whole rather than reduced to the parts the analysis uses,
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
    definitions: crate::MacroDefinitions,
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
    directive_signatures: std::collections::HashMap<String, crate::preprocess::directive::DirectiveSignature>,
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
    units: UnitTable,
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
        std::collections::HashMap<
            (PathBuf, u64),
            std::sync::Arc<cpp_parser::MacroEnvironment>,
        >,
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
        // **What the project is** — its own file, its build system, CMake's cache — worked out before anything is
        // asked of a compiler, because the first half decides *which* compiler gets asked (`named_compilers`) and
        // what flags it is asked about.
        let discovery = crate::project::discover(&files, &root, config_file);
        let project_config = discovery.config.clone();
        let filter = apply_project_config(filter, &project_config);

        let for_file = discovery
            .database
            .as_ref()
            .and_then(|database| database.commands.commands.first())
            .map(|command| command.file.clone())
            .unwrap_or_else(|| root.clone());

        // **Which flags won, and then what the project does to them.** `[compile].args` is the compilation when a
        // person wrote it, the database's first entry when the project's build says so, and CMake's cache when
        // there is no database at all. Either way the result goes through one pass — `remove_args` drops,
        // `extra_args` appends — so that the three keys have one implementation and one order, whichever list they
        // start from.
        let base = project_config_from_flags(
            &discovery.flags(),
            discovery.working_directory(),
            &project_config.config.compile,
        );

        // CMake states the language standard as a **number**, not as a flag (`CMAKE_CXX_STANDARD=20`), so it is
        // applied here rather than parsed out of the argument list — and only when nothing above it stated one: a
        // database's `-std=` or a project's own `args` are closer to the truth than the configuration a build tree
        // was generated with.
        let base = match (base.standard.clone(), discovery.standard()) {
            (None, Some(standard)) => base.with_standard(standard),
            _ => base,
        };

        // The compilers the *project* named, in their order (`discovery.named_compilers`): a project that says
        // `clang++` gets clang's headers, and a project whose CMake cache names a compiler gets that one — before
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
            // CMake's `CMAKE_CXX_STANDARD`). The compiler has to be asked about *that* standard: its
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

        let session = Session::assemble(
            root,
            files,
            filter,
            config,
            toolchain,
            discovery,
        );
        session.sweep_the_cache_in_the_background();
        session
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
            cmake: None,
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
        let database = discovery.database.as_ref().map(|database| &database.commands);
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
        // what `discovery` reads (`.cppls.toml`, `compile_commands.json`, CMake's cache) and what
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
            definitions: crate::MacroDefinitions::default(),
            units: UnitTable::default(),
            units_read: std::collections::HashSet::new(),
            unit_index: None,
            rendered_unit: None,
            directive_signatures: std::collections::HashMap::new(),
            prefetched: HashMap::new(),
            headers,
            macro_environments: std::sync::Mutex::new(std::collections::HashMap::new()),
            macro_work: std::sync::Mutex::new(Cooking::default()),
            renderings: std::sync::Mutex::new(std::collections::HashMap::new()),
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

    /// **What this analysis thinks the project is** — its configuration file, its build system, CMake's cache, and
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
        if added > 0 && let Ok(mut headers) = self.headers.write() {
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
        self.units.clear();
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
            self.definitions.forget(path);
            // …and the **translation units**: a unit is a reading of its whole closure, so a file whose *directives*
            // moved anywhere in it makes every unit built over it a reading of a timeline that is no longer there.
            // Cleared whole, for the reason `Session::translation_unit_of` gives: the honest short answer to "did
            // something inside it move" is "assume so", and the disk entry checks file by file when it is asked
            // again. A change that left every directive where it was leaves the timeline as it was.
            if moved.layout {
                self.units.clear();
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
            && let Some(held) = self.vfs.held(path) {
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
        self.definitions.forget(path);
        // The **translation units** too — every one of them was a walk over a closure that contains this file — but
        // only when a directive moved: a unit is a timeline of directives, so an edit that leaves every one of them
        // where and what it was (typing in a function body, which is nearly every keystroke) leaves it as it was. This
        // is the rule the "no incremental AST" designs use for their preamble: the part above the first thing that
        // can change the environment is reused, and the part below is re-read.
        if moved.layout {
            self.units.clear();
            self.units_read.clear();
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
            return DirectivesMoved { environment: true, layout: true };
        };

        let now = crate::preprocess::directive::directive_signature(text);
        match self.directive_signatures.insert(key, now) {
            Some(before) => DirectivesMoved {
                environment: before.environment != now.environment,
                layout: before.layout != now.layout,
            },
            None => DirectivesMoved { environment: true, layout: true },
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
        let already_read = |session: &Self, path: &Path| {
            session.store.index().cooked_declarations(path).is_some()
        };

        let direct = self.direct_includes_of(root);
        for included in &direct {
            if !already_read(self, included) {
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
        for included in &direct {
            for nested in self.direct_includes_of(included) {
                if budget == 0 {
                    break;
                }
                if already_read(self, &nested) {
                    continue;
                }
                self.cooking.want(&nested);
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
    pub fn want_cooked_reading(&mut self, path: &Path) {
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
            .filter(|summary| self.store.index().cooked_declarations(&summary.path).is_none())
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
            if left > 1 && !self.prefetched.contains_key(&key) {
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

            let ready = self.prefetched.remove(&key);
            done.push(self.index_one(path, priority, depth, ready));
        }

        // Nothing left to read means nothing left to read *ahead*: what is still here was prepared for a queue that
        // has since been emptied by something else, and would be stale by the next time it is looked at.
        if self.queue.pending() == 0 {
            self.prefetched.clear();
        }

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
        self.parsed_since_the_last_pass.extend(
            done.iter()
                .filter(|step| matches!(step.outcome, StepOutcome::Built | StepOutcome::Unstored))
                .map(|step| step.path.clone()),
        );

        if self.is_idle() && !self.parsed_since_the_last_pass.is_empty() {
            let parsed = std::mem::take(&mut self.parsed_since_the_last_pass);
            // **The timelines the pass reads its environments out of**, built by the session because a unit is a
            // session's fact rather than a store's: `translation_unit_of` walks a closure, caches on disk by the
            // content of that closure, and holds the result in `units`. The pass used to walk a closure *per file*
            // inside itself; this is one walk for the whole pass, and on the 138-file project it is **one unit**.
            let units = self.units_for_the_pass(&parsed);
            self.store.re_read_where_a_body_decides(&parsed, &units);
        }

        // **And the files whose cooked reading is out of date**, now that their environments are complete: a file's
        // macros are the ones its includes brought in, and the index queue emptying is the first moment that is
        // true. This is the product's only caller of the cooked reading — every declaration query picks it up from
        // the index, and the files are the ones a reader has open, or has open and just edited.
        //
        // A **slice**, like the indexing steps: cooking a file is a unit walk, a parse and an index, and a session
        // that did a whole closure in one call would hold the writer for seconds while the user types.
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

            for path in self.cooking.take(COOK_SLICE) {
                // The queue is drained **in the order it was filled**: the open file first, then the includes it
                // names, then whatever a request asked about — so the file a reader is looking at is ready first.
                self.cook(&path);
            }

            // **…and the macro environments a query asked for**, once each per pass.
            //
            // One per drain rather than four, and the difference from `COOK_SLICE` is measured rather than chosen:
            // cooking a file is a render and a parse of **that file**, while building an environment walks the
            // file's whole closure and copies every file's text — 103.7 ms against a cook's fraction of that on the
            // same headers. This is the budget that keeps a pass responsive while still getting there: the query
            // that asked read the plain view, and the next one about the same text finds the environment.
            for _ in 0..MACRO_SLICE {
                let Some(path) = self
                    .macro_work
                    .lock()
                    .ok()
                    .and_then(|mut work| work.take(1).into_iter().next())
                else {
                    break;
                };
                if let Some(rendered) = self.rendering_of(&path) {
                    // **Remembered under the text it was read from**, which is what the next `view` looks up. The
                    // text is taken from the VFS rather than from the rendering: the key is the *file's* content,
                    // because that is what a client's buffer changes.
                    if let Some(text) = self.text(&path) {
                        if let Ok(mut known) = self.renderings.lock() {
                            known.insert(
                                (path.clone(), crate::cache::content_hash(&text)),
                                std::sync::Arc::new(rendered),
                            );
                        }
                    }
                }
            }
        }

        done
    }

    /// **The translation units a pass over these files reads its environments out of** — one walk each, and cached.
    ///
    /// The project's own translation units are tried first, because one of them covers the most: a project file is
    /// a root whose closure is the program, while a header's own closure is "this header compiled standalone".
    /// A candidate that a unit already built reads is skipped — the question is "which timeline holds this file's
    /// environment", and the first unit that answers `environment_of` is that timeline. On the 138-file project
    /// this returns **one** unit, where the second pass used to walk a closure per file.
    fn units_for_the_pass(
        &mut self,
        files: &[PathBuf],
    ) -> Vec<std::sync::Arc<crate::TranslationUnit>> {
        // Asked by the **index's own spelling** of each path: a unit's frames are keyed by the normalized form, so
        // asking with `C:\…` about a frame spelled `c:/…` answers `None` — which would build a unit per file and
        // put the whole cost back where it was.
        let mut candidates: Vec<PathBuf> = Vec::new();
        let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        for path in self.project.iter().chain(files.iter()) {
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

        let mut units: Vec<std::sync::Arc<crate::TranslationUnit>> = Vec::new();
        for candidate in candidates {
            if units
                .iter()
                .any(|unit| unit.environment_of(&candidate).is_some())
            {
                continue;
            }
            if let Some(unit) = self.translation_unit_of(&candidate) {
                units.push(unit);
            }
        }

        units
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
            let _load = crate::stages::StageTimer::new(crate::stages::Stage::Load);
            self.vfs.load(&path);
        }
        let before = self.store.stats();
        let summary = match ready {
            Some(ready) => self.store.commit(&path, ready),
            None => self.store.get(&path),
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
            outcome: outcome_of(before, after),
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

    /// Work until there is nothing left, and say how many files were read.
    ///
    /// Chunked rather than one `advance(usize::MAX)`: the steps of a whole project are a `Vec` nobody wants, and a
    /// caller that wants progress wants it per chunk. Terminates because every step removes one entry from the
    /// queue and adds only files it has not already worked — and because the cooking backlog only shrinks during a
    /// drain (`cook` never marks a file for cooking).
    ///
    /// **Both kinds of work**, which is [`Session::pending_work`]'s count rather than [`Session::is_idle`]'s: a caller
    /// asking for "everything" wants the cooked readings too, and a test that read one and not the other would be
    /// asserting about the parts it happened to wait for.
    pub fn index_everything(&mut self) -> usize {
        let mut total = 0;

        while self.pending_work() > 0 {
            total += self.advance(64).len();
        }

        total
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

    /// **Everything the session has left to do**: the index queue plus the cooking backlog.
    ///
    /// What a pump loops on. A caller looping on [`Session::pending`] alone would stop while the files it has open
    /// still have no cooked reading — which is the state the closure marking exists to leave, not the state a
    /// finished session is in.
    pub fn pending_work(&self) -> usize {
        self.queue.pending() + self.cooking.len()
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

    /// Is this file's summary in the index — the question [`Session::pending`] answers for the whole project.
    pub fn is_indexed(&self, path: impl AsRef<Path>) -> bool {
        self.store.index().summary(path.as_ref()).is_some()
    }

    // ---------------------------------------------------------------------------------------------
    // The questions
    // ---------------------------------------------------------------------------------------------

    /// The file as the cursor is in it: the buffer when it is open, the disk otherwise.
    ///
    /// `None` when there is neither — a path that is not open and cannot be read. Everything below takes a view,
    /// so one parse per request serves every question about that request's file.
    ///
    /// The view is **of the session's VFS**: the text and its line index come from the file the VFS is holding, and
    /// the view shares both rather than copying either. What is done here is the parse and the scopes, which are
    /// the two things a position needs and a summary cannot hold.
    pub fn view(&self, path: impl AsRef<Path>) -> Option<FileView> {
        let file = self.vfs.held(path)?;

        // **The rendering if it is already read, and a request for it if it is not.**
        //
        // A rendering is what a compiler's parser is handed: the file with its macros replaced, so `_STD` is
        // `::std::` and `_STD_BEGIN` is `namespace std {`, and nothing in the text is an invocation any more. That
        // is the reading this view is **of**, and it is why `Session::view` no longer hands the grammar a file full
        // of macros and a family of rules for guessing which identifier is one.
        //
        // Nothing here builds it. A rendering is a unit walk, a preprocess and a render — measured at 65–190 ms for
        // a standard-library header on an indexed project, and far more on a cold one — and a query must not pay
        // that. So a file with no rendering yet is answered from **its own tokens**, put in the work loop's hands,
        // and the next query about it finds the rendering built. The same "not now, next time" the diagnostics
        // channel states through `isIncomplete`.
        match self.known_rendering_of(&file.path, &file.text) {
            Some(rendered) => Some(FileView::parse_rendering(file, &rendered)),
            None => {
                if let Ok(mut work) = self.macro_work.lock() {
                    work.want(&file.path);
                }
                Some(FileView::parse(file))
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
    pub fn view_of_the_file(&self, path: impl AsRef<Path>) -> Option<FileView> {
        Some(FileView::parse(self.vfs.held(path)?))
    }

    /// **The view of what the file itself writes, with the macros its includes define.**
    ///
    /// [`Session::view_of_the_file`] plus the closure's macro bodies, which is what makes a *namespace-opening*
    /// macro readable without a full render: `_STD_BEGIN` becomes `namespace std {` in the scope tree, so a
    /// declaration is filed where a compiler files it. Cheaper than a rendering and less complete — the tokens are
    /// still the file's own — and it is the fallback for a caller that asked for a rendering before one was built.
    pub fn view_of_the_file_with_macros(&self, path: impl AsRef<Path>) -> Option<FileView> {
        let file = self.vfs.held(path)?;
        match self.known_rendering_of(&file.path, &file.text) {
            Some(rendered) => Some(FileView::parse_rendering(file, &rendered)),
            None => match self.known_macros_of(&file.path, &file.text) {
                Some(macros) => Some(FileView::parse_with(file, &macros)),
                None => Some(FileView::parse(file)),
            },
        }
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
    pub fn view_with_macros(&self, path: impl AsRef<Path>) -> Option<FileView> {
        let file = self.vfs.held(path)?;
        match self.the_macros_of(&file.path, &file.text) {
            Some(macros) => Some(FileView::parse_with(file, &macros)),
            None => Some(FileView::parse(file)),
        }
    }

    /// The environment for `text`, **if it is already read** — a hash and a lookup, and nothing else.
    fn known_macros_of(
        &self,
        path: &Path,
        text: &str,
    ) -> Option<std::sync::Arc<cpp_parser::MacroEnvironment>> {
        let key = (path.to_path_buf(), crate::cache::content_hash(text));
        self.macro_environments
            .lock()
            .ok()?
            .get(&key)
            .cloned()
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
            known.insert((path.to_path_buf(), crate::cache::content_hash(text)), made.clone());
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
            let range = (declaration.range.start_offset, declaration.range.end_offset());

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
            let view = self.view(path);
            if let Some(view) = &view {
                notes = self.notes_about_the_modules(view);
            }
            let checks = match &view {
                Some(view) => self.checks_about(&key, view),
                None => Vec::new(),
            };

            return Some(FileDiagnostics {
                reading: DiagnosticReading::Cooked,
                unplaced: cooked.unplaced,
                errors: cooked
                    .diagnostics
                    .iter()
                    .map(|error| FileDiagnostic {
                        start: error.range.start_offset,
                        end: error.range.end_offset(),
                        message: error.message.clone(),
                    })
                    .collect(),
                notes,
                checks,
            });
        }

        let view = self.view(path)?;
        let notes = self.notes_about_the_modules(&view);

        Some(FileDiagnostics {
            reading: DiagnosticReading::Raw,
            // Every error of the raw reading is about text in this file, by construction: it parsed this file.
            unplaced: 0,
            errors: view
                .errors()
                .iter()
                .map(|error| {
                    let (start, end) = error.offsets();
                    FileDiagnostic {
                        start,
                        end,
                        message: error.message.clone(),
                    }
                })
                .collect(),
            notes,
            checks: self.checks_about(&key, &view),
        })
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

        crate::sema::check::Checks {
            path: key,
            summary,
            index,
            source: &view.source,
            tree: &view.root,
        }
        .run()
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
    /// Any change to any file: the map is cleared whole. A unit is a reading of *its closure*, and the honest
    /// short answer to "did something in it move" is "assume so" — the disk entry is the one that can afford to
    /// check file by file (it hashes the closure), and it is consulted fresh each time this map misses.
    fn translation_unit_of(&mut self, path: &Path) -> Option<std::sync::Arc<crate::TranslationUnit>> {
        let key = queue_key(path);

        if let Some(unit) = self.units.get(&key) {
            return Some(unit);
        }

        let context = self.store.context_hash(path);
        let cache = crate::TranslationUnitCache::of_store(&self.store);

        // Read out of the cache under a timer, then matched: a stage timer inside a `match` scrutinee is a block
        // clippy asks to hoist, and hoisting it is what the timer wants anyway — it should cover the read only.
        let cached = {
            let _get = crate::stages::StageTimer::new(crate::stages::Stage::UnitGet);
            cache.get(path, context, &self.files)
        };

        let unit = match cached {
            Some(unit) => unit,
            None => {
                let (closure, closure_by_key) = {
                    let _closure = crate::stages::StageTimer::new(crate::stages::Stage::Closure);
                    self.closure_with_text(path)
                };

                // **The session's own definition cache**, not a fresh one: the walk reads back every `#define` a
                // file sees, and the same definition reaches hundreds of files — `<vector>` and `<string>` both see
                // `_STD_BEGIN`, whose text is in `yvals_core.h`. One cache per *walk* is one parse per definition
                // per file, which the census measured at 12.2 s of a 40 s run. Invalidated **per path** when a file
                // changes, since the keys are offsets in that file.
                let index = self.store.index();
                let root = index.summary(path)?;
                let mut definitions = std::mem::take(&mut self.definitions);
                let unit = {
                    let _walk = crate::stages::StageTimer::new(crate::stages::Stage::Walk);
                    crate::TranslationUnit::walk(
                        root,
                        |wanted| {
                            let (held, text) = closure.get(*closure_by_key.get(&queue_key(wanted))?)?;
                            Some((index.summary(held)?, text.as_str()))
                        },
                        index.macros(),
                        &mut definitions,
                    )
                };
                self.definitions = definitions;

                // Written for the **next run**, and the write is the caller's business to fail: a read-only
                // checkout is an ordinary way to work, and a cache that cannot be written is not a wrong answer.
                {
                    let _put = crate::stages::StageTimer::new(crate::stages::Stage::UnitPut);
                    let _ = cache.put(path, context, &unit, &self.files);
                }
                unit
            }
        };

        let unit = std::sync::Arc::new(unit);
        self.units.insert(key, unit.clone());
        Some(unit)
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
        let stream = {
            let _render = crate::stages::StageTimer::new(crate::stages::Stage::UnitRender);
            unit.cook_the_unit(&sources, &definitions, Some(&seed), true, Some(&search))
        };

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
            let wanted: Vec<String> = self
                .modules_in_view_of(path)
                .into_iter()
                .filter(|module| self.store.index().interface_unit_of(module).is_none())
                .collect();

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

    /// **Which file each of these module names is declared in** — resolved the way `scan_imports` resolves them.
    ///
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

        for module in wanted {
            if let ImportOutcome::Resolved(unit) = scanner.resolve_module(module, importing, &mut interner)
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
        // **The index's own spelling of the path**, taken once and used from here on. Two spellings of one file are
        // ordinary on Windows — a client sends `C:\…`, the include resolver produces `c:/…` — and the index, the
        // frames of a walk and the declarations all key on the *normalized* form. Comparing the raw spellings reads
        // as two files: the reading is built under a path nothing else names, every query goes on finding nothing,
        // and nothing reports a problem.
        let path = self.store.index().summary(path.as_ref())?.path.clone();
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

        // **The closure, with its text**, from the index and the buffers: the walk reads a file's includes, so it
        // needs the same edge set the visibility walk uses, and one read per file rather than one per edge. The map
        // beside it is what makes answering the walk's questions a lookup rather than a scan — see
        // [`Session::closure_with_text`].
        //
        // …and it is read **only on a miss**: the unit itself is what is expensive, and it is asked for first.
        let unit = self.translation_unit_of(&path)?;
        let text = self.files.read(&path)?;

        let seed = MacroTable::from_marked(self.store.index().macros());
        let (tokens, _) = {
            let _lex = crate::stages::StageTimer::new(crate::stages::Stage::Lex);
            cpp_parser::lex(&text, &cpp_parser::LexerConfig::default())
        };
        let unit_definitions = unit.definitions();
        let macros = {
            let _macros = crate::stages::StageTimer::new(crate::stages::Stage::Macros);
            crate::preprocess::cooked::FileMacros::new(
                unit.environment_of(&path)?,
                &unit_definitions,
                Some(&seed),
                true,
            )
        };
        let rendered = {
            let _render = crate::stages::StageTimer::new(crate::stages::Stage::Render);
            crate::preprocess::cooked::cook_with(&text, &tokens, &macros).render()
        };
        let indexer = FileIndexer::new(&self.files, &self.config);
        let indexed = indexer.index_rendering(&path, &rendered, key);
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

        {
            let _insert = crate::stages::StageTimer::new(crate::stages::Stage::Insert);
            self.store.index_mut().insert_cooked(&path, indexed.into());
        }
        Some(reading)
    }

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
    pub fn rendered_text_of(&mut self, path: impl AsRef<Path>) -> Option<String> {
        Some(self.rendering_of(path)?.text)
    }

    /// **The rendering of one file, with its spans** — the text and the map that says where each token of it was
    /// written.
    ///
    /// The same artifact as [`Session::rendered_text_of`] with the half that makes it usable for a **view**:
    /// [`crate::FileView::parse_rendering`] needs the spans, because a rendering's offsets are its own and a client
    /// speaks the file's.
    pub fn rendering_of(
        &mut self,
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
    pub fn definition(&self, view: &FileView, offset: usize) -> Known<ProjectDefinition> {
        definition_across_files(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            offset,
        )
    }

    /// **Every declaration the name at `offset` refers to** — an overload set, a redeclaration, or one entity.
    ///
    /// See [`crate::ProjectIndex::definitions`] for what the list means and why one namespace collapses to one
    /// entry. This is the query behind `textDocument/definition`: measured on one real file, **46** identifiers
    /// answered `Ambiguous` through the single-answer form, and every one of them had a usable list behind it.
    pub fn definitions(&self, view: &FileView, offset: usize) -> Known<ProjectDefinitions> {
        definitions_across_files(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            offset,
        )
    }

    /// Which `#define` or `#undef` settles the macro name at `offset`.
    pub fn macro_definition(&self, view: &FileView, offset: usize) -> Known<ProjectMacro> {
        macro_across_files(self.store.index(), &view.root, &view.path, offset)
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
    pub fn header_at(&self, view: &FileView, offset: usize) -> Known<crate::HeaderTarget> {
        crate::header_at(self.store.index(), &view.root, &view.path, offset)
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
    pub fn macro_references(&self, view: &FileView, offset: usize) -> Known<MacroReferences> {
        // `name_at_including_directives` rather than `name_at`: the place a user asks this question is the name
        // itself, and half the time that name is written in a `#define` — which is tokens in a directive, not a
        // name node the scope walker sees.
        let Some((name, _)) = crate::sema::resolve::name_at_including_directives(&view.root, offset) else {
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
    pub fn member_completions(&self, view: &FileView, offset: usize) -> Known<MemberCompletions> {
        member_completions_at(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            offset,
        )
    }

    /// What to offer at a cursor with no member access: the scope's names, then the index's.
    pub fn name_completions(&self, view: &FileView, offset: usize) -> Known<NameCompletions> {
        name_completions_at(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            offset,
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
    pub fn completions(&self, view: &FileView, offset: usize) -> crate::CompletionSet {
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
            offset,
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
            return crate::file::view::documentation_at(&view.root, offset);
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
    pub fn type_at(&self, view: &FileView, offset: usize) -> Known<ExpressionType> {
        let Some(expression) = crate::sema::resolve::expression_at(&view.root, offset) else {
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
    pub fn inlay_hints(&self, view: &FileView, range: cpp_parser::SourceRange) -> Vec<ParameterHint> {
        crate::inlay::parameter_hints(self.store.index(), view, range, |path| {
            self.view_of_the_file(path)
        })
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
        crate::semantic::classified_names(self.store.index(), view)
    }

    /// **The file's foldable regions** — see [`crate::folding`].
    ///
    /// Read from the **buffer's own tokens and directives**, which is the same source the outline uses and for the
    /// same reason: folding is about the file in front of the reader, so an unsaved edit is folded as it is typed,
    /// and a declaration in a branch nobody takes is a region like any other. Nothing here consults the index, the
    /// cooked reading, or any other file — a fold is a fact about this text.
    pub fn folding_ranges(&self, view: &FileView) -> Vec<crate::folding::Fold> {
        crate::folding::folding_ranges(&view.source, view.tree.get_tokens())
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
        offset: usize,
    ) -> Vec<crate::signature::CallSignature> {
        let mut signatures =
            crate::signature::signatures_at(self.store.index(), view, offset, |path| self.view(path));

        for signature in &mut signatures {
            signature.documentation =
                self.documentation(view, &signature.declared_in, signature.declared_at);
        }

        signatures
    }

    /// The members of a type, as a file's own scopes and the index together know them.
    ///
    /// `written_type` is a spelling — `Widget`, `ns::Widget`, `const Widget&` — and following it to a class is the
    /// same work [`Session::member_completions`] does for an object expression.
    pub fn members_of(&self, view: &FileView, written_type: &str) -> Known<MemberList> {
        members_of(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            written_type,
        )
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
        if let Some(scope) = in_scope.filter(|_| !class.starts_with("::") && !class.contains("::")) {
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

        let mut found = list.members.iter().filter(|found| found.fact.name == member);

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
        let (facts, _guards) = crate::build_facts(&view.scopes, &preprocessing, &view.root, &errors);

        crate::outline_of(&facts)
    }
}

/// Which reading answered a file's diagnostics — see [`Session::diagnostics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticReading {
    /// The file's own bytes, **unexpanded**: what a reader editing the file sees, macros and untaken branches
    /// included.
    Raw,
    /// The text a **compiler** parses: the file's bytes with its translation unit's macros expanded and the branches
    /// nobody takes left out.
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
    held: std::collections::HashMap<String, (u64, std::sync::Arc<crate::TranslationUnit>)>,
    clock: u64,
}

impl UnitTable {
    fn get(&mut self, key: &str) -> Option<std::sync::Arc<crate::TranslationUnit>> {
        self.clock += 1;
        let (used, unit) = self.held.get_mut(key)?;
        *used = self.clock;
        Some(unit.clone())
    }

    fn insert(&mut self, key: String, unit: std::sync::Arc<crate::TranslationUnit>) {
        self.clock += 1;

        if !self.held.contains_key(&key) && self.held.len() >= MAX_UNITS {
            let oldest = self
                .held
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.held.remove(&oldest);
            }
        }

        self.held.insert(key, (self.clock, unit));
    }

    fn clear(&mut self) {
        self.held.clear();
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
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
        let key = queue_key(path);

        if matches!(self.standing.get(&key), Some(Standing::Queued(_))) {
            self.standing.insert(key, Standing::Worked);
            self.queued -= 1;
            return true;
        }

        false
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
mod tests {
    use super::{OpenDocuments, Session, SessionFiles};
    use crate::include::config::{CommandLineMacro, CompilerConfig};
    use crate::include::toolchain::{Toolchain, ToolchainSource};
    use crate::file::paths::{DiskFiles, FileProvider, MemoryFiles};
    use crate::index::watch::{FileEvent, WatchFilter};
    use crate::index::{Priority, StepOutcome};
    use crate::symbol::{Known, UnknownReason};
    use std::path::{Path, PathBuf};

    /// A project in memory: the files, the buffers in front of them, and the providers that join the two.
    ///
    /// The fixture keeps the providers and hands each session a **clone**, which is what a caller outside a test
    /// does too — a provider is a handle, so the fixture's copy and the session's copy read the same files, and a
    /// test can go on looking at what the analysis read while the session owns its own chain.
    struct Memory<'a> {
        files: &'a MemoryFiles,
        documents: OpenDocuments,
        providers: SessionFiles<MemoryFiles>,
        root: PathBuf,
    }

    impl<'a> Memory<'a> {
        fn new(name: &str, files: &'a MemoryFiles) -> Self {
            let root = std::env::temp_dir().join("cppls-session-tests").join(name);
            let _ = std::fs::remove_dir_all(&root);

            let documents = OpenDocuments::new();
            let providers = SessionFiles::new(documents.clone(), files.clone());

            Memory {
                files,
                documents,
                providers,
                root,
            }
        }

        fn session(&self) -> Session<MemoryFiles> {
            Session::with_config(
                &self.root,
                self.providers.clone(),
                WatchFilter::new(&self.root),
                CompilerConfig::default(),
            )
        }

        /// The text the analysis would read for a path — the buffer if there is one.
        fn text(&self, path: &str) -> Option<String> {
            self.documents
                .text(path)
                .or_else(|| self.files.read(Path::new(path)))
        }
    }

    impl Drop for Memory<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The paths of a run of steps, for an assertion about the order rather than about the work.
    fn paths(steps: &[crate::index::Step]) -> Vec<String> {
        steps
            .iter()
            .map(|step| step.path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    // -------------------------------------------------------------------------------------------
    // The buffer is the text
    // -------------------------------------------------------------------------------------------

    /// **A declaration only a macro makes becomes one this session answers for**, once its queue drains.
    ///
    /// `DECLARE_HANDLE(HWND)` declares `HWND__` and `HWND` to a compiler and *nothing* to a reader of the file's own
    /// text: the declaration is inside the macro's replacement list, and the invocation is not a declaration. So the
    /// index has no `HWND__` before the file is cooked and has one after — which is the whole reason the reading
    /// exists.
    ///
    /// (The other half of what cooking buys — the *scope* a namespace-opening macro puts a declaration in — is
    /// already handled by the raw reading when the indexer is given the closure's macro bodies: `BEGIN_NS` is
    /// `namespace ns {` to the parser and a declaration inside it is scoped correctly. That is the two-hop reading of
    /// a macro-shaped scope and of a body written in another file, and it is why this test is about a declaration
    /// the file does not write at all rather than about one it writes.)
    #[test]
    fn a_declaration_only_a_macro_makes_is_found_once_the_session_has_cooked_the_file() {
        let handle = "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                      typedef struct name##__ *name\n";
        let source = "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n";
        let files = MemoryFiles::new()
            .with_file("/p/handle.h", handle)
            .with_file("/p/api.h", source);
        let fixture = Memory::new("cooked-reading-is-indexed", &files);
        let mut session = fixture.session();

        session.did_open("/p/api.h", source);

        // **One step**: `api.h` is read and its include is queued, so the queue is *not* empty — and a file whose
        // includes have not been read has an environment that is not complete, which is why nothing is cooked yet.
        session.advance(1);
        let before = session.index().definition("HWND__", Path::new("/p/api.h"));
        assert!(
            matches!(before, Known::Unknown(_)),
            "the file's own text does not declare what the macro declares: {before:?}"
        );
        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/api.h"))
                .is_none(),
            "and nothing has cooked it yet"
        );

        // **Drain**: the queue emptying is the moment a file's environment is complete, and that is where the
        // session cooks the files a reader is looking at. No caller asked for this — it is the indexing loop's own
        // last step.
        session.index_everything();

        let after = session.index().definition("HWND__", Path::new("/p/api.h"));
        let Known::Yes(found) = after else {
            panic!("the cooked reading declares it: {after:?}");
        };
        assert_eq!(found.file, Path::new("/p/api.h"));
        let at = found.fact.range;
        let written = &source[at.start_offset..at.end_offset()];
        assert!(
            written.contains("DECLARE_HANDLE"),
            "the declaration is reported at the invocation: {written:?}"
        );

        // What the cook reported about itself, asked for explicitly so the numbers are part of the test: the file
        // was already cooked once by the drain, and cooking it again reads the same thing.
        let cooked = session.cook_the_open_files();
        assert_eq!(cooked.len(), 1, "api.h is the open file");
        let (path, reading) = &cooked[0];
        assert_eq!(path, Path::new("/p/api.h"));
        assert!(
            reading.declarations >= 2,
            "the struct, its member and the typedef are declarations here: {reading:?}"
        );
        assert!(
            reading.only_after_expansion >= 2,
            "`HWND__` and `HWND` exist only after expansion: {reading:?}"
        );
        assert_eq!(reading.mapped.dropped, 0, "every range landed in the file");
    }

    /// **Opening a file reads the files it includes the way a compiler reads them too.**
    ///
    /// A name used in the file the user is editing is declared in a header — often one they never open — and the
    /// answer comes from **that** file's reading. So the closure is what gets cooked, not the one file: here
    /// `main.cpp` is open, `api.h` is not, and the declaration only exists because `api.h` was read as a compiler
    /// reads it. Without the closure marking the index would have the raw reading of `api.h` — which cannot see
    /// what `DECLARE_HANDLE` declares at all.
    ///
    /// **The bound is one level of includes, and a unit read moves it** — see
    /// [`a_unit_read_reads_the_whole_program_once`] for what the session does when a caller asks for the program
    /// rather than for one file, and why that is not wired into the pump yet.
    #[test]
    fn a_header_the_user_never_opened_is_cooked_when_a_request_names_it() {
        let handle = "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                      typedef struct name##__ *name\n";
        let api = "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n";
        let main = "#include \"api.h\"\nHWND h;\n";
        let files = MemoryFiles::new()
            .with_file("/p/handle.h", handle)
            .with_file("/p/api.h", api)
            .with_file("/p/main.cpp", main);
        let fixture = Memory::new("a-named-file-is-cooked", &files);
        let mut session = fixture.session();

        session.did_open("/p/main.cpp", main);
        session.index_everything();

        // **The open file, and the files its own text names.** `api.h` is one of them, so the declaration the macro
        // writes in it is `api.h`'s declaration and a question about `HWND` is answered with a jump into it — the
        // capability that the closure used to buy, bought for the price of one direct include instead of 138 files.
        let after = session.index().definition("HWND__", Path::new("/p/main.cpp"));
        let Known::Yes(found) = after else {
            panic!("`api.h`'s cooked reading declares it: {after:?}");
        };
        assert_eq!(
            found.file,
            Path::new("/p/api.h"),
            "the file that invoked the macro, which nobody opened"
        );

        // **And not one level further**: `handle.h` is nobody's direct include here, so it has no reading until
        // something looks at it. That is the difference between a bounded set and the closure — measured on a real
        // project: the closure was 138 files and **11.7 s of every startup**, the direct includes are four.
        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/handle.h"))
                .is_none(),
            "a transitive include is not cooked for free"
        );

        // A request naming it — the shell's `prepare` does this for every file a request is about — is what makes it.
        session.want_cooked_reading(Path::new("/p/handle.h"));
        session.index_everything();
        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/handle.h"))
                .is_some(),
            "and then it has one"
        );
    }

    /// **A file whose text does not balance its braces is named, and its declarations are filed anyway.**
    ///
    /// The hazard this pins was found by measurement, not by reading: on a real project
    /// `CodeAnalysis/sourceannotations.h` — the Windows SDK's `/analyze` header, the one file a census has never
    /// read cleanly — opens `namespace vc_attributes {` whose close is not in the text we rendered. Everything
    /// spliced after it read as `vc_attributes::std`: `<cstdio>`'s declarations, `<string>`'s, the lot. A reader
    /// asking about `std::size_t` was told the index had no such name while the file in front of them declared it.
    ///
    /// # What this used to assert, and why it was the wrong shape
    ///
    /// It used to assert that such a file's text was **left out of the stream** — the plan's gate ①, one of the two
    /// all-or-nothing gates §2 names. That throws away every declaration in the file to protect the ones after it,
    /// which is §4 rule 1's *"任何一层都不允许因为'不确定'而放弃已经确定的答案"* broken in the same way the
    /// `crossings == 0` gate broke it one layer up.
    ///
    /// The text now goes in, and the crossed pairing it creates is repaired like every other crossed pairing: the
    /// `{` this file opened is paired with a `}` in another file, **that pair** is neutralised, and both files are
    /// read on their own as well. So the assertions here are the two that matter and they pull in opposite
    /// directions, which is what makes them worth having: the file after the unbalanced one is still scoped by its
    /// own namespace, **and** the unbalanced file's own declaration is in the index.
    #[test]
    fn a_file_that_does_not_balance_its_braces_is_named_and_still_read() {
        let files = MemoryFiles::new()
            // Opens a namespace and never closes it: what a `/analyze` header does, and what swallows a program.
            .with_file(
                "/p/broken.h",
                "namespace vc_attributes {\nstruct NotAClosure { int x; };\n",
            )
            .with_file(
                "/p/good.h",
                "namespace good {\nstruct Inside { int y; };\n}\n",
            )
            .with_file(
                "/p/main.cpp",
                "#include \"broken.h\"\n#include \"good.h\"\nInside i;\n",
            );
        let fixture = Memory::new("a-unit-that-does-not-balance", &files);
        let mut session = fixture.session();

        session.did_open("/p/main.cpp", "#include \"broken.h\"\n#include \"good.h\"\nInside i;\n");
        session.index_everything();

        let reading = session
            .read_the_unit(Path::new("/p/main.cpp"))
            .expect("the unit reads");

        assert_eq!(
            reading.unbalanced.len(),
            1,
            "one file does not balance: {reading:?}"
        );
        assert!(
            reading.unbalanced[0].ends_with("broken.h"),
            "and it is the one that opens a brace it never closes: {reading:?}"
        );

        // **And the file after it is not nested inside the scope `broken.h` left open.** `broken.h`'s unclosed
        // namespace ends at the end of `broken.h`, so `good.h`'s own namespace holds `Inside` and nothing else.
        let inside = session
            .index()
            .cooked_declarations(Path::new("/p/good.h"))
            .expect("good.h is part of the program");
        let inside = inside
            .iter()
            .find(|fact| fact.name == "Inside")
            .expect("the class is declared there");
        assert_eq!(
            inside.scope.as_deref(),
            Some("good"),
            "a scope does not cross a file boundary, so `good.h` is scoped by its own namespace"
        );

        // **And the unbalanced file did not lose its own declarations.** This is the half gate ① could not give: the
        // file's struct is declared in it, and it is filed under the namespace the file itself opens.
        let broken = session
            .index()
            .cooked_declarations(Path::new("/p/broken.h"))
            .expect("the unbalanced file's own reading is filed too");
        let struct_fact = broken
            .iter()
            .find(|fact| fact.name == "NotAClosure")
            .expect("the struct is declared in the file that does not balance");
        assert_eq!(
            struct_fact.scope.as_deref(),
            Some("vc_attributes"),
            "and at the scope its own text gives it: {broken:?}"
        );
    }

    /// **MSVC's single-bracket attribute no longer leaks, and the fence is not what saves it.**
    ///
    /// This test used to assert that `broken.h` was *quarantined*: its text balances its braces, the parser still
    /// got it wrong — `REPEATABLE [source_annotation_attribute(1)] struct X { … };` read as an expression whose
    /// lambda body never closes — and the fence took the file out of the program so that `good.h` could still be
    /// read as itself. The reading was right and the file was not.
    ///
    /// The parser now reads that shape (a decl-specifier sequence accepts a single-bracket attribute, see
    /// [`cpp_parser`]'s `at_a_single_bracket_attribute`), so the assertion has to be the *stronger* one: nothing
    /// is quarantined, because nothing leaked — and the declarations of the file that used to poison the program
    /// are in the index under their own names.
    ///
    /// The fence itself is still tested, by [`a_file_that_really_leaks_is_repaired_and_still_filed`] with a file that
    /// still does.
    #[test]
    fn a_single_bracket_attribute_no_longer_props_the_program_open() {
        let leaky = "REPEATABLE\n[source_annotation_attribute( 1 )]\nstruct Pre\n{\n int Deref;\n};\n";
        let main = "#include \"broken.h\"\n#include \"good.h\"\nInside i;\n";
        let files = MemoryFiles::new()
            .with_file("/p/broken.h", leaky)
            .with_file("/p/good.h", "namespace good {\nstruct Inside { int y; };\n}\n")
            .with_file("/p/main.cpp", main);
        let fixture = Memory::new("a-scope-that-used-to-leak", &files);
        let mut session = fixture.session();

        session.did_open("/p/main.cpp", main);
        session.index_everything();
        let reading = session
            .read_the_unit(Path::new("/p/main.cpp"))
            .expect("the unit reads");

        assert!(
            reading.unbalanced.is_empty(),
            "the text balances, and now the parse does too: {reading:?}"
        );
        assert!(
            reading.unbalanced.is_empty(),
            "nothing leaked and nothing is out of balance: {reading:?}"
        );

        // **The declarations of the file that used to poison the program are filed, under their own names.**
        let pre = session
            .index()
            .cooked_declarations(Path::new("/p/broken.h"))
            .expect("the reading was filed")
            .iter()
            .find(|fact| fact.name == "Pre")
            .expect("the struct the attribute precedes is declared there");
        assert_eq!(pre.scope, None, "and it is at file scope, not inside anything");

        let inside = session
            .index()
            .cooked_declarations(Path::new("/p/good.h"))
            .expect("the reading was filed")
            .iter()
            .find(|fact| fact.name == "Inside")
            .expect("the class is declared there")
            .scope
            .clone();
        assert_eq!(inside.as_deref(), Some("good"), "the file after it is read as itself");
    }

    /// **A file that really leaks no longer costs anything, because there is no fence to invoke.**
    ///
    /// `good.h` opens a namespace and never closes it, so a parse of the program sees `other.h` inside it. That
    /// used to be found (a brace paired across two files) and repaired (the pair neutralised, both files read on
    /// their own). **Both halves are gone**, and the reason is the plan's §3.0: a crossing needs the parser to pair
    /// a brace in one file with one in another, which it can only do when the text it is given has a macro invocation
    /// in place of the braces — and this stream is the *cooked* one, where every macro is already replaced.
    ///
    /// So what is asserted here is what remains true and matters: nothing is refused, the file after the leak is
    /// scoped by its own namespace, and the leaking file keeps its own declaration.
    #[test]
    fn a_file_that_does_not_balance_costs_nothing_but_a_name() {
        // `good.h` opens a namespace and never closes it. `other.h` is the file after it.
        let good = "namespace good {\nstruct Good { int x; };\n";
        let other = "struct Other { int y; };\n";
        let main = "#include \"good.h\"\n#include \"other.h\"\nOther o;\n";
        let files = MemoryFiles::new()
            .with_file("/p/good.h", good)
            .with_file("/p/other.h", other)
            .with_file("/p/main.cpp", main);
        let fixture = Memory::new("a-scope-that-really-leaks", &files);
        let mut session = fixture.session();

        session.did_open("/p/main.cpp", main);
        session.index_everything();
        let reading = session
            .read_the_unit(Path::new("/p/main.cpp"))
            .expect("the unit reads");

        // **The imbalance is named, and that is all that happens to it.** No file is quarantined, nothing is
        // repaired, and the reading is filed — the plan's §4 rule 1.
        assert_eq!(
            reading.unbalanced.len(),
            1,
            "one file does not balance, and it is reported: {reading:?}"
        );

        // **The file after it is read as itself, at file scope.**
        //
        // This is what the fence used to buy, and it is now bought **in the parser**: a scope may not cross a file
        // boundary, so the `{` that `good.h` never closed is closed at the end of `good.h` rather than paired with a
        // `}` in `other.h`. See `ParserConfig::file_boundaries` for the rule and `balance_events` for where it is
        // applied.
        //
        // The distinction matters for who owns the fix: deleting the fence exposed this leak, and the answer was not
        // to put the fence back — it was a defect **in the parser**, and the parser is where it is now fixed.
        let other_fact = session
            .index()
            .cooked_declarations(Path::new("/p/other.h"))
            .expect("other.h is part of the program")
            .iter()
            .find(|fact| fact.name == "Other")
            .map(|fact| fact.scope.clone())
            .expect("the struct is declared there");
        assert_eq!(
            other_fact, None,
            "the file after the unbalanced one is at file scope, not inside `good::`: {reading:?}"
        );

        // **And the unbalanced file did not lose its own declarations.**
        let good_facts = session
            .index()
            .cooked_declarations(Path::new("/p/good.h"))
            .expect("the leaking file's own reading is filed too");
        assert!(
            good_facts.iter().any(|fact| fact.name == "Good"),
            "the struct is declared in the file that does not balance: {good_facts:?}"
        );
    }

    /// **A program in which no file leaks is read once** — the ordinary case has no quarantine and no second parse.
    #[test]
    fn a_program_whose_files_keep_their_scopes_quarantines_nothing() {
        let main = "#include \"a.h\"\n#include \"b.h\"\nint main_variable;\n";
        let files = MemoryFiles::new()
            .with_file("/p/a.h", "namespace a {\nstruct A { int x; };\n}\n")
            .with_file("/p/b.h", "namespace b {\n#include \"c.h\"\n}\n")
            .with_file("/p/c.h", "struct C { int z; };\n")
            .with_file("/p/main.cpp", main);
        let fixture = Memory::new("a-program-that-does-not-leak", &files);
        let mut session = fixture.session();

        session.did_open("/p/main.cpp", main);
        session.index_everything();
        let reading = session.read_the_unit(Path::new("/p/main.cpp")).expect("the unit reads");

        assert!(
            reading.unbalanced.is_empty(),
            "a scope that closes around an `#include` is in balance: {reading:?}"
        );
        let c = session
            .index()
            .cooked_declarations(Path::new("/p/c.h"))
            .expect("filed")
            .iter()
            .find(|fact| fact.name == "C")
            .expect("declared")
            .scope
            .clone();
        assert_eq!(c.as_deref(), Some("b"), "the namespace `b.h` opens around its include still holds `C`");
    }

    /// **A unit read puts the whole program in the index, once** — [`Session::read_the_unit`].
    ///
    /// The reading this session is built to reach and has not reached: one walk, one stream, one parse, and every
    /// declaration filed under the file it was written in — so a header has the program's reading whether or not
    /// anybody opened it. Here the same fixture as above, asked for the **program** rather than for one file:
    /// `handle.h` is read (it is what the program is made of), and a file the program does not read is not.
    ///
    /// It is a test of a call the pump does **not** make yet, and that is deliberate: wiring it changes answers in a
    /// way that is not explained (the note on [`Session::read_the_unit`] has the three readings and the bisect), and
    /// an unexplained change in answers is not shipped to buy a capability.
    #[test]
    fn a_unit_read_reads_the_whole_program_once() {
        let handle = "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                      typedef struct name##__ *name\n";
        let api = "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n";
        let main = "#include \"api.h\"\nHWND h;\n";
        let files = MemoryFiles::new()
            .with_file("/p/handle.h", handle)
            .with_file("/p/api.h", api)
            .with_file("/p/main.cpp", main)
            .with_file("/p/other.cpp", "#include \"other.h\"\nvoid g() { }\n")
            .with_file("/p/other.h", "struct Unrelated { int y; };\n");
        let fixture = Memory::new("a-unit-read", &files);
        let mut session = fixture.session();

        session.did_open("/p/main.cpp", main);
        session.index_everything();

        // Nothing has read `handle.h` as a program yet: it is a transitive include, and the per-file policy stops
        // at the direct ones.
        assert!(session.index().cooked_declarations(Path::new("/p/handle.h")).is_none());

        let reading = session
            .read_the_unit(Path::new("/p/main.cpp"))
            .expect("the unit reads");
        assert_eq!(reading.root, Path::new("/p/main.cpp"));
        // **`files` is the walk's frames; `files_with_tokens` is smaller, and that is not a defect.** `handle.h`
        // holds one `#define` and nothing else, and a directive contributes no token to the rendering — the same
        // distinction the census records as "a clean file whose rendering is empty was not read at all" (§7). Both
        // numbers are asserted so that the difference stays visible rather than being smoothed over.
        assert!(
            reading.files >= 3,
            "the program is three files: {reading:?}"
        );
        assert_eq!(
            reading.files_with_tokens, 2,
            "two of them write tokens; a file of directives writes none: {reading:?}"
        );
        assert_eq!(reading.missing, 0, "every file had text: {reading:?}");

        // **The program's files**, including the header nobody opened and the macro's declaration in the file that
        // invoked it.
        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/handle.h"))
                .is_some(),
            "a file the program is made of is read with it"
        );
        let after = session.index().definition("HWND__", Path::new("/p/main.cpp"));
        let Known::Yes(found) = after else {
            panic!("`api.h`'s cooked reading declares it: {after:?}");
        };
        assert_eq!(found.file, Path::new("/p/api.h"));

        // **And a file outside the program is not touched**: nothing compiles `other.cpp`, nobody opens it.
        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/other.cpp"))
                .is_none(),
            "a file no program reads and nobody opens has no reading"
        );
    }

    /// **The diagnostics of a file whose error is in a branch nobody takes.**
    ///
    /// The two readings disagree here, and that is the whole reason [`Session::diagnostics`] exists: the file's own
    /// text has an unclosed class inside `#if OFF`, so the raw reading reports it (correctly — the *text* says so),
    /// while a compiler never sees that branch at all. Publishing the raw answer would put a red squiggle on code
    /// that is not compiled, which is the false positive the cooked reading removes.
    #[test]
    fn a_branch_nobody_takes_reports_nothing_once_the_file_is_cooked() {
        let main = "#include \"cfg.h\"\n#if OFF\nstruct Unclosed { int x;\n#endif\nint ok;\n";
        let files = MemoryFiles::new()
            .with_file("/p/cfg.h", "#define OFF 0\n")
            .with_file("/p/main.cpp", main);
        let fixture = Memory::new("a-branch-nobody-takes", &files);
        let mut session = fixture.session();

        // Before the file is cooked the answer is the file's own text — and it is not empty, because the text really
        // does have an unclosed class in it. The file is *held* rather than opened: the diagnostics channel is asked
        // about files the VFS has read, and holding the text is what makes the raw reading answerable.
        session.load("/p/main.cpp");
        let raw = session.diagnostics("/p/main.cpp").expect("the file reads");
        assert_eq!(raw.reading, super::DiagnosticReading::Raw);
        assert_eq!(raw.errors.len(), 1, "the unclosed class is the error: {raw:?}");

        session.did_open("/p/main.cpp", main);
        session.index_everything();

        let cooked = session.diagnostics("/p/main.cpp").expect("the file reads");
        assert_eq!(
            cooked.reading,
            super::DiagnosticReading::Cooked,
            "the index holds a reading of this file now"
        );
        assert!(
            cooked.errors.is_empty(),
            "and the branch it complained about is not in it: {cooked:?}"
        );
        assert_eq!(cooked.unplaced, 0, "nothing was hidden from the reader either");
    }

    /// **A cooked error's offsets are offsets in the file**, not in the rendering it was found in.
    ///
    /// The two coordinate systems are different here, which is what makes the claim checkable: the file writes
    /// `DECLARE_HANDLE(HWND);` and the rendering has the struct and the typedef the macro expands to, so an offset
    /// taken from the rendering lands past the end of the file. The evidence that the expansion really happened is
    /// in the same test — the declaration the file never wrote (`HWND__`) — because a fixture where the macro stayed
    /// unexpanded would make a rendering offset and a file offset the same number and the assertion worthless.
    #[test]
    fn a_cooked_error_is_placed_in_the_file_not_in_the_rendering() {
        let handle = "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                      typedef struct name##__ *name\n";
        let main = "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\nstruct Unclosed { int x;\n";
        let files = MemoryFiles::new()
            .with_file("/p/handle.h", handle)
            .with_file("/p/main.cpp", main);
        let fixture = Memory::new("cooked-errors-are-file-offsets", &files);
        let mut session = fixture.session();

        session.did_open("/p/main.cpp", main);
        session.index_everything();

        let declarations = session
            .index()
            .cooked_declarations(Path::new("/p/main.cpp"))
            .expect("the file was cooked");
        assert!(
            declarations.iter().any(|fact| fact.name == "HWND__"),
            "the rendering is not the file's text: the macro declared a struct this file never wrote: {:?}",
            declarations.iter().map(|fact| &fact.name).collect::<Vec<_>>()
        );

        let found = session.diagnostics("/p/main.cpp").expect("the file reads");
        assert_eq!(found.reading, super::DiagnosticReading::Cooked);
        assert_eq!(found.errors.len(), 1, "{found:?}");

        let error = &found.errors[0];
        let unclosed = main.find("struct Unclosed").expect("the fixture writes it");
        assert!(
            error.start >= unclosed,
            "the error is about the line that is still unclosed: {error:?}"
        );
        assert!(
            error.end <= main.len(),
            "and it is a position in the file — a rendering offset would be past its end: {error:?}"
        );
        assert_eq!(found.unplaced, 0, "nothing had to be hidden: {found:?}");
    }

    /// **What is cooked is what is looked at**: the open files and the files their own text names, plus any file a
    /// request is about — not a closure, and not a project.
    ///
    /// The reader who needed the old behaviour was the diagnostic pass, which publishes an answer for every indexed
    /// file — but "every indexed file" is the **closure** of an open file, and on a real project that was 138 files:
    /// measured at **11.7 s** of the startup, every time, cache or no cache, for the difference between 999 and 1000
    /// declarations. A file nobody looks at is answered from its own text, and the diagnostic layer says so
    /// ([`super::DiagnosticReading::Raw`]) rather than pretending otherwise.
    #[test]
    fn a_file_nobody_looked_at_is_not_cooked_and_one_a_request_names_is() {
        let main = "#include \"a.h\"\n#include \"b.h\"\n#include \"c.h\"\nA a; B b; C c;\n";
        let files = MemoryFiles::new()
            .with_file("/p/a.h", "struct A { int x; };\n")
            .with_file("/p/b.h", "struct B { int x; };\n")
            .with_file("/p/c.h", "struct C { int x; };\n")
            .with_file("/p/main.cpp", main)
            .with_file("/p/other.cpp", "#include \"a.h\"\nvoid g() { }\n");
        let fixture = Memory::new("only-what-is-looked-at-is-cooked", &files);
        let mut session = fixture.session();

        // A file the caller knows about, in the rest half of the queue. Nothing opens it, nothing looks at it.
        session.add_project_files([PathBuf::from("/p/other.cpp")]);
        session.did_open("/p/main.cpp", main);

        session.index_everything();
        assert_eq!(session.pending_work(), 0, "both kinds of work are done");

        for looked_at in ["/p/main.cpp", "/p/a.h", "/p/b.h", "/p/c.h"] {
            assert!(
                session
                    .index()
                    .cooked_declarations(Path::new(looked_at))
                    .is_some(),
                "{looked_at} is what the reader is looking at"
            );
        }
        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/other.cpp"))
                .is_none(),
            "and a file nobody opened, nobody included and no request named has no reading"
        );

        // A request names it: that is what the shell does before every query, and the reading follows.
        session.want_cooked_reading(Path::new("/p/other.cpp"));
        session.index_everything();
        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/other.cpp"))
                .is_some(),
            "the file a request is about is read the way a compiler reads it"
        );
    }

    /// **Editing a header invalidates the reading of every file that had one**, and those come back.
    ///
    /// The set is every dependent ([`crate::ProjectIndex::dependents_of`]), because a stale reading is a wrong answer
    /// rather than a missing one — and it is *only* the dependents that had a reading, because a file nobody has
    /// looked at has nothing to invalidate and asking for a new reading would cook a file nobody asked about.
    #[test]
    fn editing_a_header_invalidates_the_readings_that_depend_on_it() {
        let ns = "#define BEGIN_NS namespace one {\n#define END_NS }\n";
        let api = "#include \"ns.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n";
        let other = "#include \"api.h\"\nWidget w;\n";
        let files = MemoryFiles::new()
            .with_file("/p/ns.h", ns)
            .with_file("/p/api.h", api)
            .with_file("/p/other.cpp", other);
        let fixture = Memory::new("a-cooked-file-goes-stale", &files);
        let mut session = fixture.session();

        session.add_project_files([PathBuf::from("/p/other.cpp")]);
        session.did_open("/p/api.h", api);
        // A request names `other.cpp`, so it has a reading to lose — which is what this test is about.
        session.want_cooked_reading(Path::new("/p/other.cpp"));
        session.index_everything();
        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/other.cpp"))
                .is_some(),
            "the fixture starts with the dependent cooked"
        );

        // The header the macro is written in opens the *other* namespace.
        let changed = "#define BEGIN_NS namespace two {\n#define END_NS }\n";
        session.did_open("/p/ns.h", changed);

        for stale in ["/p/other.cpp", "/p/api.h"] {
            assert!(
                session
                    .index()
                    .cooked_declarations(Path::new(stale))
                    .is_none(),
                "{stale} was read as the namespace the macro used to open"
            );
        }

        session.index_everything();
        assert!(
            matches!(
                session.index().definition("two::Widget", Path::new("/p/other.cpp")),
                Known::Yes(_)
            ),
            "and it is read again, as the new macro says: {:?}",
            session.index().definition("two::Widget", Path::new("/p/other.cpp"))
        );
    }

    /// **A file that defines no macros invalidates nothing**, which is what keeps the common edit cheap.
    ///
    /// The environment is macros: a header that only declares things cannot change what any file that includes it
    /// expands to, so its dependents' readings are still readings of the truth. Without this gate, an edit anywhere
    /// in a project would drop the readings of everything that includes the file — and in a standard library that is
    /// most of the project, on every keystroke.
    #[test]
    fn editing_a_file_that_defines_no_macros_keeps_its_dependents_readings() {
        let api = "struct Widget { int size; };\n";
        let other = "#include \"api.h\"\nWidget w;\n";
        let files = MemoryFiles::new()
            .with_file("/p/api.h", api)
            .with_file("/p/other.cpp", other);
        let fixture = Memory::new("a-declaration-is-not-an-environment", &files);
        let mut session = fixture.session();

        session.add_project_files([PathBuf::from("/p/other.cpp")]);
        session.did_open("/p/api.h", api);
        session.want_cooked_reading(Path::new("/p/other.cpp"));
        session.index_everything();

        // The same declarations with one more member: the file changed, its macros did not.
        session.did_open("/p/api.h", "struct Widget { int size; int weight; };\n");

        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/other.cpp"))
                .is_some(),
            "nothing in this file is part of anyone's environment"
        );
        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/api.h"))
                .is_none(),
            "…while the file itself is re-read, which is not a question about macros"
        );
    }

    /// **The first `#define` a header gets reaches the files that include it.**
    ///
    /// The gate this replaced asked whether the header defined macros *before* the edit — so the edit that added the
    /// header's first macro was the one edit it could not see, and every includer went on answering from a reading
    /// built when the macro did not exist.
    #[test]
    fn the_first_macro_a_header_defines_invalidates_the_readings_that_include_it() {
        let api = "#include \"ns.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n";
        let other = "#include \"api.h\"\nWidget w;\n";
        let files = MemoryFiles::new()
            .with_file("/p/ns.h", "// nothing to say yet\n")
            .with_file("/p/api.h", api)
            .with_file("/p/other.cpp", other);
        let fixture = Memory::new("the-first-macro-of-a-header", &files);
        let mut session = fixture.session();

        session.add_project_files([PathBuf::from("/p/other.cpp")]);
        session.want_cooked_reading(Path::new("/p/other.cpp"));
        session.index_everything();
        assert!(
            session.index().cooked_declarations(Path::new("/p/other.cpp")).is_some(),
            "the fixture starts with the dependent cooked"
        );

        session.did_open("/p/ns.h", "#define BEGIN_NS namespace one {\n#define END_NS }\n");

        assert!(
            session.index().cooked_declarations(Path::new("/p/other.cpp")).is_none(),
            "the reading was built when `BEGIN_NS` meant nothing"
        );
    }

    /// **An `#include` added to a header that defines nothing is still a change to its includers' environment.**
    #[test]
    fn an_include_added_to_a_header_invalidates_the_readings_that_include_it() {
        let files = MemoryFiles::new()
            .with_file("/p/macros.h", "#define ANSWER 42\n")
            .with_file("/p/api.h", "int declared;\n")
            .with_file("/p/other.cpp", "#include \"api.h\"\nint x = ANSWER;\n");
        let fixture = Memory::new("an-include-is-an-environment", &files);
        let mut session = fixture.session();

        session.add_project_files([PathBuf::from("/p/other.cpp")]);
        session.want_cooked_reading(Path::new("/p/other.cpp"));
        session.index_everything();
        assert!(session.index().cooked_declarations(Path::new("/p/other.cpp")).is_some());

        session.did_open("/p/api.h", "#include \"macros.h\"\nint declared;\n");

        assert!(
            session.index().cooked_declarations(Path::new("/p/other.cpp")).is_none(),
            "`ANSWER` is now in force where it was not"
        );
    }

    /// **Typing below the last directive keeps the translation units; touching a directive drops them.**
    ///
    /// A unit is a timeline of directives, so what decides whether an edit can have moved it is whether any directive
    /// did — nearly every keystroke is in a body, and re-walking the closure for each of them was most of what a
    /// keystroke cost.
    #[test]
    fn a_unit_survives_typing_in_a_body_and_not_a_change_to_a_directive() {
        let ns = "#define BEGIN_NS namespace one {\n#define END_NS }\n";
        let api = "#include \"ns.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n";
        let files = MemoryFiles::new().with_file("/p/ns.h", ns).with_file("/p/api.h", api);
        let fixture = Memory::new("a-unit-survives-a-body", &files);
        let mut session = fixture.session();

        session.did_open("/p/api.h", api);
        session.index_everything();
        assert!(!session.units.is_empty(), "the fixture cooked a file, which walked a unit");

        let typed = format!("{api}void body() {{ int inside; }}\n");
        session.did_change("/p/api.h", &typed);
        assert!(!session.units.is_empty(), "a body was typed below the last directive");

        let typed_more = format!("{api}void body() {{ int inside; int more; }}\n");
        session.did_change("/p/api.h", &typed_more);
        assert!(!session.units.is_empty(), "and again");

        session.did_change("/p/api.h", &format!("#include \"ns.h\"\n#define EXTRA 1\n{typed_more}"));
        assert!(session.units.is_empty(), "a directive appeared, so the timeline is out of date");
    }

    /// **Editing a header invalidates the reading of the open file that includes it.**
    ///
    /// A file's cooked reading is a reading of its *environment* as well as of its text: `BEGIN_NS` opening a
    /// namespace is a fact about `ns.h`, and a reading of `api.h` built before `ns.h` changed would keep answering
    /// the old way — a jump to a scope that no longer exists, which is a wrong answer rather than a missing one.
    #[test]
    fn editing_a_header_stales_the_cooked_reading_of_a_file_that_includes_it() {
        let files = MemoryFiles::new()
            .with_file("/p/ns.h", "#define BEGIN_NS namespace one {\n#define END_NS }\n")
            .with_file(
                "/p/api.h",
                "#include \"ns.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n",
            );
        let fixture = Memory::new("a-header-edit-stales-readings", &files);
        let mut session = fixture.session();

        session.did_open("/p/api.h", "#include \"ns.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n");
        session.index_everything();
        assert!(
            matches!(
                session.index().definition("one::Widget", Path::new("/p/api.h")),
                Known::Yes(_)
            ),
            "the fixture reads as the header says: {:?}",
            session.index().definition("one::Widget", Path::new("/p/api.h"))
        );

        // The header changes the namespace it opens. `api.h`'s text does not change at all — what changes is what
        // its environment says, which is exactly the dependency a per-file invalidation would miss.
        session.did_change("/p/ns.h", "#define BEGIN_NS namespace two {\n#define END_NS }\n");
        assert!(
            session.pending_cooking() > 0,
            "the open file that includes it is marked for cooking again"
        );
        session.index_everything();

        assert!(
            matches!(
                session.index().definition("two::Widget", Path::new("/p/api.h")),
                Known::Yes(_)
            ),
            "and the new reading is the one in the index: {:?}",
            session.index().definition("two::Widget", Path::new("/p/api.h"))
        );
        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/api.h"))
                .is_some_and(|facts| facts
                    .iter()
                    .any(|fact| fact.qualified_name() == "two::Widget")),
            "the stale reading was replaced, not kept beside it"
        );
    }

    #[test]
    fn a_buffer_is_the_text_the_analysis_reads() {
        // The whole reason an editor needs an overlay: the file on disk says `old`, the user is typing `fresh`, and
        // an analysis that answered about the disk would answer about code that no longer exists.
        let files = MemoryFiles::new()
            .with_file("/p/widget.h", "struct Widget { int old; };\n")
            .with_file("/p/main.cpp", "#include \"widget.h\"\nvoid f() { Widget w; }\n");
        let fixture = Memory::new("buffer-is-the-text", &files);
        let mut session = fixture.session();

        session.did_open("/p/widget.h", "struct Widget { int fresh; };\n");
        session.did_open("/p/main.cpp", "#include \"widget.h\"\nvoid f() { Widget w; }\n");
        session.index_everything();

        let view = session.view("/p/main.cpp").expect("the file reads");
        let at = view.source.find("Widget w").expect("the use is in the text");

        match session.definition(&view, at) {
            Known::Yes(found) => {
                assert_eq!(found.file, Path::new("/p/widget.h"));
                assert_eq!(found.fact.name, "Widget");
            }
            other => panic!("the buffer's declaration should be found, got {other:?}"),
        }

        // And the members are the buffer's, which is the half a definition jump cannot show.
        match session.members_of(&view, "Widget") {
            Known::Yes(members) => {
                let names: Vec<&str> = members.own().map(|member| member.fact.name.as_str()).collect();
                assert_eq!(names, ["fresh"], "the disk says `old` and the disk is not the text");
            }
            other => panic!("the buffer's class should have members, got {other:?}"),
        }

        assert_eq!(
            fixture.text("/p/widget.h").as_deref(),
            Some("struct Widget { int fresh; };\n")
        );
    }

    #[test]
    fn a_buffer_that_has_never_been_saved_is_a_file_like_any_other() {
        // A file the user has created and not saved has no path on disk at all. The overlay answers for it, the
        // include resolves to it, and its summary is built from text no filesystem has seen.
        let files = MemoryFiles::new().with_file(
            "/p/main.cpp",
            "#include \"widget.h\"\nvoid f() { Widget w; }\n",
        );
        let fixture = Memory::new("unsaved", &files);
        let mut session = fixture.session();

        session.did_open("/p/widget.h", "struct Widget { int size; };\n");
        session.did_open(
            "/p/main.cpp",
            "#include \"widget.h\"\nvoid f() { Widget w; }\n",
        );
        assert!(session.index_everything() >= 2, "both the buffer and its includer");

        let view = session.view("/p/main.cpp").expect("the file reads");
        let at = view.source.find("Widget w").expect("the use is in the text");
        assert!(
            matches!(session.definition(&view, at), Known::Yes(found) if found.file == Path::new("/p/widget.h")),
            "a buffer no file has answers as the declaration's file"
        );
    }

    #[test]
    fn an_edit_replaces_what_the_index_knew_and_the_gap_is_visible() {
        // Two claims in one test because they are one decision: the summary built from the old text is dropped —
        // so a query about the file answers "not read yet" rather than the edit's predecessor — and the file is
        // queued again, so the answer comes back a step later.
        let files = MemoryFiles::new().with_file("/p/widget.h", "struct Widget { int before; };\n");
        let fixture = Memory::new("edit", &files);
        let mut session = fixture.session();

        session.add_project_files([PathBuf::from("/p/widget.h")]);
        session.index_everything();
        assert!(session.is_indexed("/p/widget.h"));
        assert!(session.is_idle());

        session.did_change("/p/widget.h", "struct Widget { int after; };\n");

        assert!(
            !session.is_indexed("/p/widget.h"),
            "the summary describes text the user has replaced"
        );
        assert_eq!(session.pending(), 1, "and the file is queued again");

        let steps = session.advance(4);
        assert_eq!(paths(&steps), ["/p/widget.h"]);
        assert_eq!(
            steps[0].outcome,
            StepOutcome::Built,
            "the summary was dropped, so this is a parse and not a cache hit"
        );

        let view = session.view("/p/widget.h").expect("the file reads");
        match session.members_of(&view, "Widget") {
            Known::Yes(members) => {
                let names: Vec<&str> = members.own().map(|member| member.fact.name.as_str()).collect();
                assert_eq!(names, ["after"]);
            }
            other => panic!("the edited buffer's class should have members, got {other:?}"),
        }
    }

    #[test]
    fn closing_a_document_goes_back_to_the_file() {
        // An unsaved buffer defines a name the disk does not have. Closing is the client saying "this path is the
        // filesystem's again", and an analysis that kept the buffer would report a declaration that exists nowhere.
        let files = MemoryFiles::new().with_file("/p/widget.h", "struct Widget { int on_disk; };\n");
        let fixture = Memory::new("close", &files);
        let mut session = fixture.session();

        session.did_open("/p/widget.h", "struct Widget { int in_the_buffer; };\n");
        session.index_everything();
        session.did_close("/p/widget.h");

        assert!(!fixture.documents.is_open("/p/widget.h"));
        assert!(!session.is_indexed("/p/widget.h"));

        let steps = session.advance(4);
        assert_eq!(paths(&steps), ["/p/widget.h"]);
        assert_eq!(
            steps[0].priority,
            Priority::Rest,
            "the user has stopped looking at it, so it is not urgent"
        );

        let view = session.view("/p/widget.h").expect("the file reads");
        match session.members_of(&view, "Widget") {
            Known::Yes(members) => {
                let names: Vec<&str> = members.own().map(|member| member.fact.name.as_str()).collect();
                assert_eq!(names, ["on_disk"]);
            }
            other => panic!("the file's own class should have members, got {other:?}"),
        }
    }

    // -------------------------------------------------------------------------------------------
    // The order
    // -------------------------------------------------------------------------------------------

    #[test]
    fn the_open_file_is_worked_first_and_its_includes_next() {
        // The order fixes, read off the steps: what the user is looking at, then what it
        // includes, and only then the project. A session that worked the project first would answer the first
        // question after ten thousand files.
        let files = MemoryFiles::new()
            .with_file("/p/open.cpp", "#include \"deep.h\"\nvoid f() { }\n")
            .with_file("/p/deep.h", "int deep;\n")
            .with_file("/p/one.cpp", "int one;\n")
            .with_file("/p/two.cpp", "int two;\n");
        let fixture = Memory::new("order", &files);
        let mut session = fixture.session();

        session.add_project_files([PathBuf::from("/p/one.cpp"), PathBuf::from("/p/two.cpp")]);
        session.did_open("/p/open.cpp", "#include \"deep.h\"\nvoid f() { }\n");

        let steps = session.advance(8);
        assert_eq!(
            paths(&steps),
            ["/p/open.cpp", "/p/deep.h", "/p/one.cpp", "/p/two.cpp"]
        );

        assert_eq!(steps[0].priority, Priority::Open);
        assert_eq!(steps[0].depth, 0);
        assert_eq!(
            (steps[1].priority, steps[1].depth),
            (Priority::Open, 1),
            "a header an open file includes is worth reading before the project"
        );
        assert_eq!((steps[2].priority, steps[2].depth), (Priority::Rest, 0));
    }

    #[test]
    fn a_project_file_that_is_opened_moves_to_the_open_half() {
        // The case the two halves exist for: the scan lists every file at open time, and then the user opens one of
        // them. Without the upgrade the file the user is looking at would sit behind everything else in the
        // project, and the queue would look right in every test that never opens anything.
        let files = MemoryFiles::new()
            .with_file("/p/widget.h", "struct Widget { int size; };\n")
            .with_file("/p/one.cpp", "int one;\n");
        let fixture = Memory::new("upgrade", &files);
        let mut session = fixture.session();

        session.add_project_files([PathBuf::from("/p/one.cpp"), PathBuf::from("/p/widget.h")]);
        session.did_open("/p/widget.h", "struct Widget { int size; };\n");

        let steps = session.advance(1);
        assert_eq!(paths(&steps), ["/p/widget.h"]);
        assert_eq!(steps[0].priority, Priority::Open);

        // And the entry it left in the rest half is not a second read of the same file.
        let rest = session.advance(4);
        assert_eq!(paths(&rest), ["/p/one.cpp"]);
        assert!(session.is_idle());
        assert_eq!(session.pending(), 0);
    }

    #[test]
    fn a_file_that_was_worked_is_queued_again_when_its_answer_changes() {
        // The other half of the queue's memory: `add` refuses a path it has already worked, which is what keeps a
        // header included forty times one step — and a change has to get past exactly that rule.
        let files = MemoryFiles::new().with_file("/p/widget.h", "struct Widget { int a; };\n");
        let fixture = Memory::new("again", &files);
        let mut session = fixture.session();

        session.did_open("/p/widget.h", "struct Widget { int a; };\n");
        session.index_everything();
        assert!(session.is_idle());

        let before = session.stats();
        session.did_change("/p/widget.h", "struct Widget { int b; };\n");
        assert_eq!(session.pending(), 1);

        let steps = session.advance(4);
        assert_eq!(paths(&steps), ["/p/widget.h"]);
        assert_eq!(session.stats().since(before).rebuilt, 1);
    }

    // -------------------------------------------------------------------------------------------
    // What the client says changed
    // -------------------------------------------------------------------------------------------

    #[test]
    fn a_reported_change_is_read_again_and_a_reported_removal_is_forgotten() {
        // The "no OS watcher" entry point: the client reports events, the session turns them into work. A modified
        // header is one file to re-read — its includers' summaries do not depend on its contents — and a removed
        // one is forgotten together with the files whose includes resolved to it.
        let files = MemoryFiles::new()
            .with_file("/p/widget.h", "struct Widget { int size; };\n")
            .with_file("/p/main.cpp", "#include \"widget.h\"\nvoid f() { Widget w; }\n");
        let fixture = Memory::new("client-events", &files);
        let mut session = fixture.session();

        session.add_project_files([PathBuf::from("/p/main.cpp")]);
        session.index_everything();
        assert!(session.is_indexed("/p/widget.h"), "the closure was followed");

        let modified = session.changed([FileEvent::modified("/p/widget.h")]);
        assert_eq!(modified.reindex, [PathBuf::from("/p/widget.h")]);
        assert!(modified.forgotten.is_empty());
        assert!(!modified.everything);
        assert_eq!(session.pending(), 1, "the reported change is queued");

        let steps = session.advance(4);
        assert_eq!(paths(&steps), ["/p/widget.h"]);
        assert_eq!(
            steps[0].outcome,
            StepOutcome::Reused,
            "the text did not change, so the key still names the stored summary"
        );

        let removed = session.changed([FileEvent::removed("/p/widget.h")]);
        assert_eq!(removed.forgotten, [PathBuf::from("/p/widget.h")]);
        assert_eq!(
            removed.reindex,
            [PathBuf::from("/p/main.cpp")],
            "the file that resolved to it records a search that would now fail"
        );
        assert!(
            !session.is_indexed("/p/widget.h"),
            "and the declarations are out of the index before anything is re-read"
        );
    }

    #[test]
    fn a_change_the_client_does_not_care_about_is_no_work() {
        // The filter is the session's, so the cache directory and `.git` are dropped once, where the events arrive,
        // rather than in every query. The compile database is the one path that is *reported* rather than dropped.
        let files = MemoryFiles::new().with_file("/p/a.cpp", "int a;\n");
        let fixture = Memory::new("ignored-events", &files);
        let mut session = fixture.session();
        session.add_project_files([PathBuf::from("/p/a.cpp")]);
        session.index_everything();

        let root = fixture.root.clone();
        let quiet = session.changed([
            FileEvent::modified(root.join(".git/index.lock")),
            FileEvent::modified(root.join(".cppls/summaries/ab/cdef.bin")),
            FileEvent::modified(root.join("notes.md")),
        ]);

        assert!(quiet.is_empty(), "{quiet:?}");
        assert!(session.is_idle());

        let configuration = session.changed([FileEvent::modified(root.join("compile_commands.json"))]);
        assert!(configuration.everything);
    }

    #[test]
    fn a_configuration_change_re_seeds_the_project_and_the_open_buffers() {
        // The configuration is part of every summary's key, so one `-I` added to the database makes every stored
        // summary a miss — and the work is the project list again. What the user has open is queued first, because
        // that is the file they are waiting for.
        let files = MemoryFiles::new()
            .with_file("/p/a.cpp", "int a;\n")
            .with_file("/p/b.cpp", "int b;\n")
            .with_file("/p/open.cpp", "int open;\n");
        let fixture = Memory::new("configuration", &files);
        let mut session = fixture.session();

        session.add_project_files([PathBuf::from("/p/a.cpp"), PathBuf::from("/p/b.cpp")]);
        session.did_open("/p/open.cpp", "int open;\n");
        session.index_everything();
        assert!(session.is_idle());

        let response = session.changed([FileEvent::modified(
            fixture.root.join("compile_commands.json"),
        )]);
        assert!(response.everything);

        assert_eq!(
            session.pending(),
            3,
            "the project list and the buffer are all work again"
        );

        let steps = session.advance(8);
        assert_eq!(paths(&steps), ["/p/open.cpp", "/p/a.cpp", "/p/b.cpp"]);
    }

    #[test]
    fn a_change_to_a_file_that_was_already_worked_is_queued_and_then_reused() {
        // A configuration change re-arms files the queue had already finished, which is the one thing `add` refuses
        // to do. The work is still cheap: the key is computed from the text, so a file whose text did not change is
        // a hit, and "ask again" costs a read rather than a parse.
        let files = MemoryFiles::new().with_file("/p/a.cpp", "int a;\n");
        let fixture = Memory::new("rearm", &files);
        let mut session = fixture.session();

        session.add_project_files([PathBuf::from("/p/a.cpp")]);
        session.index_everything();

        let before = session.stats();
        session.changed([FileEvent::modified(
            fixture.root.join("compile_commands.json"),
        )]);

        let steps = session.advance(4);
        assert_eq!(paths(&steps), ["/p/a.cpp"]);
        assert_eq!(steps[0].outcome, StepOutcome::Reused);
        assert_eq!(
            session.stats().since(before).rebuilt,
            0,
            "a configuration change costs reads, not parses"
        );
    }

    // -------------------------------------------------------------------------------------------
    // What the answers are, and what they are before the work is done
    // -------------------------------------------------------------------------------------------

    #[test]
    fn a_name_in_a_file_that_has_not_been_read_is_unknown_and_the_queue_says_so() {
        // The honest boundary of lazy indexing, asserted rather than described: the query answers
        // `NotDeclaredHere` — which is what a fully indexed project says about a name that is not there — and the
        // only thing that tells the two apart is whether the queue is empty. A consumer that reports an unresolved
        // name as an error has to ask.
        let files = MemoryFiles::new()
            .with_file("/p/widget.h", "struct Widget { int size; };\n")
            .with_file("/p/main.cpp", "#include \"widget.h\"\nvoid f() { Widget w; }\n");
        let fixture = Memory::new("not-read-yet", &files);
        let mut session = fixture.session();

        session.add_project_files([PathBuf::from("/p/main.cpp")]);

        // The file is loaded but **not indexed**: a view is of the text the session holds, while the *index* is what
        // this test is about — reading it would take the queue step it goes on to assert.
        session.load("/p/main.cpp");

        let view = session.view("/p/main.cpp").expect("the file reads");
        let at = view.source.find("Widget w").expect("the use is in the text");

        assert_eq!(
            session.definition(&view, at),
            Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("Widget")))
        );
        assert!(!session.is_indexed("/p/widget.h"));
        assert_eq!(session.pending(), 1, "and the queue is why the answer is what it is");

        session.index_everything();

        assert!(session.is_idle());
        assert!(
            matches!(session.definition(&view, at), Known::Yes(found) if found.file == Path::new("/p/widget.h")),
            "the same question, answered once the file has been read"
        );
    }

    #[test]
    fn a_view_is_the_buffer_parsed_and_maps_a_position_onto_an_offset() {
        // What a request needs from a session before it can ask anything: the file's own scopes, from the buffer,
        // and a mapping from the line and column a client sends to the byte offset a query takes.
        let files = MemoryFiles::new().with_file("/p/a.cpp", "int on_disk;\nvoid f() { on_disk = 1; }\n");
        let fixture = Memory::new("view", &files);
        let mut session = fixture.session();

        session.did_open("/p/a.cpp", "int in_buffer;\nvoid f() { in_buffer = 1; }\n");

        let view = session.view("/p/a.cpp").expect("the buffer is the text");
        assert!(view.open, "the view says which of the two texts it read");
        assert!(view.errors().is_empty());

        let at = view.source.find("in_buffer = 1").expect("the use is in the text");
        assert_eq!(
            view.offset_at(1, 11),
            Some(at),
            "line 1, column 11 is where `in_buffer` is written on the second line"
        );
        assert_eq!(
            view.offset_at(99, 0),
            None,
            "a line past the end is not an offset"
        );

        // And the file's own scope answers the query, with no index involved at all.
        assert!(
            matches!(session.definition(&view, at), Known::Yes(found) if found.file == Path::new("/p/a.cpp")),
            "a local declaration is resolved by the file's scopes"
        );

        assert!(session.view("/p/not_there.cpp").is_none());
    }

    #[test]
    fn a_position_maps_back_onto_the_offset_it_came_from() {
        // The direction a diagnostic needs: the parser reports byte offsets and a client wants a position. The two
        // mappings have to agree, and the case that makes that worth a test is a line that is not ASCII — the
        // column a client sends counts **characters**, so a one-byte-per-column round trip is the wrong answer the
        // moment a line contains `é`.
        let files = MemoryFiles::new()
            .with_file("/p/a.cpp", "// é comment\nint x = 1;\n")
            .with_file("/p/crlf.cpp", "int a;\r\nint b;\r\n");
        let fixture = Memory::new("position", &files);
        let mut session = fixture.session();
        // A view is of a file the session is **holding** (`file::vfs`): nothing has read these yet, so they are
        // loaded explicitly — the way a notification or an indexing step loads one.
        session.load("/p/a.cpp");
        session.load("/p/crlf.cpp");

        let view = session.view("/p/a.cpp").expect("the file reads");
        let at = view.source.find("x = 1").expect("the statement is in the text");
        assert_eq!(
            view.position_at(at),
            Some((1, 4)),
            "the offset of `x` is line 1, column 4"
        );

        // The round trip in both directions, on the line with the two-byte character.
        let (line, column) = view.position_at(at).expect("the offset is in the file");
        assert_eq!(view.offset_at(line, column), Some(at));

        let comment = view.source.find('é').expect("the character is in the text");
        assert_eq!(
            view.position_at(comment),
            Some((0, 3)),
            "a character counts as one column, whatever it costs in bytes"
        );

        // And a CRLF file is one line break, not two: the `\r` is part of line 0, so line 1 starts after it.
        let crlf = session.view("/p/crlf.cpp").expect("the file reads");
        let second = crlf.source.find("int b").expect("the second line is in the text");
        assert_eq!(crlf.position_at(second), Some((1, 0)));
        assert_eq!(
            crlf.position_at(crlf.source.len()),
            Some((2, 0)),
            "the end of the text is a position: it is where a range at EOF sits"
        );

        assert_eq!(
            crlf.position_at(crlf.source.len() + 1),
            None,
            "an offset past the end is not a position"
        );
    }

    // -------------------------------------------------------------------------------------------
    // Opening a project on disk
    // -------------------------------------------------------------------------------------------

    /// A project directory, removed when the test ends — including when it fails.
    struct Project {
        root: PathBuf,
    }

    impl Project {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join("cppls-session-disk").join(name);
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("the fixture directory");

            Project { root }
        }

        fn write(&self, name: &str, text: &str) -> PathBuf {
            let path = self.root.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the fixture directory");
            }
            std::fs::write(&path, text).expect("the fixture writes");
            path
        }

        /// The project's own files, as paths relative to the root with `/` separators.
        fn relative(&self, paths: &[PathBuf]) -> Vec<String> {
            paths
                .iter()
                .filter_map(|path| path.strip_prefix(&self.root).ok())
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .collect()
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn the_scan_finds_the_projects_sources_and_leaves_the_rest() {
        // What a scan is for, and what it must not do: a build tree and `.git` are not project source, a `.md` is
        // not a source at all, and a file with no extension is reached through an include rather than guessed at.
        let project = Project::new("scan");
        project.write("main.cpp", "int main() { return 0; }\n");
        project.write("include/widget.hpp", "struct Widget { int size; };\n");
        project.write("notes.md", "# notes\n");
        project.write("build/generated.cpp", "int generated;\n");
        project.write(".git/hooks/thing.cpp", "int hook;\n");
        project.write("extensionless", "int nothing;\n");

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, DiskFiles);
        let filter = WatchFilter::new(&project.root).ignore(project.root.join("build"));

        let session = Session::with_config(
            &project.root,
            providers.clone(),
            filter,
            CompilerConfig::default(),
        );

        assert_eq!(
            project.relative(session.project_files()),
            ["include/widget.hpp", "main.cpp"]
        );
        assert_eq!(
            session.pending(),
            2,
            "and the list is the queue's seed, so the work is ready before the first query"
        );
    }

    #[test]
    fn a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session() {
        // The store's second pass, on the closure a **session** builds one file at a time. MSVC's STL is
        // written this way: `<vector>` opens `std` with `_STD_BEGIN`, whose replacement list is in `yvals_core.h`,
        // and it *includes* that file — while a file is read before the files it includes, because its own
        // `#include`s are a product of parsing it. So the first reading of every such file puts its declarations at
        // file scope. Measured on the whole STL through the driver: **0/9** without this pass, **9/9** with it,
        // against 9/9 for the same index built by `SummaryStore::index_includes_from` (which always had one).
        //
        // No compiler and no compile database take part: the body is written in a file of the project, which is
        // what makes this test the same on every machine.
        let project = Project::new("body-pass");
        project.write(
            "open.h",
            "#pragma once\n#define _STD_BEGIN namespace std {\n#define _STD_END }\n",
        );
        let widget = project.write(
            "widget.h",
            "#include \"open.h\"\n_STD_BEGIN\nstruct Widget { int size; };\n_STD_END\n",
        );
        let main = project.write("main.cpp", "#include \"widget.h\"\n");

        let mut session = Session::with_config(
            &project.root,
            SessionFiles::new(OpenDocuments::new(), DiskFiles),
            WatchFilter::new(&project.root),
            CompilerConfig::default(),
        );
        session.index_everything();

        let found = session.index().definition("std::Widget", &main);
        let Known::Yes(found) = found else {
            panic!("`Widget` is declared behind a macro body written in `open.h`: {found:?}");
        };
        assert_eq!(found.file, widget);
        assert_eq!(found.fact.scope.as_deref(), Some("std"));

        // And the reading is recorded as **evidence** rather than as a conclusion: the summary says which body
        // opened the scope, which is what lets a consumer re-read it when the macro changes. See
        // A fact derived from a macro body is evidence, not part of the key.
        let summary = session.index().summary(&widget).expect("indexed");
        assert!(
            !summary.macro_readings.is_empty(),
            "the scope came from a body, and the summary has to say so"
        );
    }

    // -------------------------------------------------------------------------------------------
    // The project's own configuration file
    // -------------------------------------------------------------------------------------------

    #[test]
    fn a_project_configuration_narrows_the_scan_and_widens_the_source_list() {
        // The two things `[workspace]` is for: a tree the analysis should not read, and an extension the engine
        // does not know about. Both are read from the project's own file, on disk, by a session the caller
        // configured — which is the path a test can take without running a compiler.
        let project = Project::new("config-scan");
        project.write("main.cpp", "int main() { return 0; }\n");
        project.write("vendor/third_party.cpp", "int vendored;\n");
        project.write("kernels/vector.cuh", "struct FromCuda { int x; };\n");
        project.write(
            ".cppls.toml",
            "[workspace]\nexclude = [\"vendor/**\"]\nsource_extensions = [\"cuh\"]\n",
        );

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, DiskFiles);
        let session = Session::with_config(
            &project.root,
            providers,
            WatchFilter::new(&project.root),
            CompilerConfig::default(),
        );

        assert_eq!(
            project.relative(session.project_files()),
            ["kernels/vector.cuh", "main.cpp"],
            "the excluded tree is not scanned, and the project's own extension is"
        );
        assert!(
            session.project_config().found(),
            "the file was read: {:?}",
            session.project_config()
        );
        assert!(session.project_config().problems.is_empty());
    }

    #[test]
    fn the_projects_flags_win_over_the_databases_and_extra_args_add_to_the_winner() {
        // The decision `CompileSection::args` records, pinned: `args` wins outright, `extra_args` adds to whatever
        // won, and `remove_args` drops a flag from it. One test because the three keys are one pass over one list —
        // splitting them would let the order they are documented in stop being the order they happen in.
        let project = Project::new("config-flags");
        project.write("main.cpp", "int main() { return 0; }\n");
        let root = project.root.to_string_lossy().replace('\\', "/");

        project.write(
            "compile_commands.json",
            &format!(
                "[\n  {{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"g++\", \"-DFROM_THE_DATABASE\", \"-std=c++17\", \"-Werror\", \
                 \"-c\", \"{root}/main.cpp\"]}}\n]\n"
            ),
        );
        project.write(
            ".cppls.toml",
            "[compile]\nargs = [\"-DFROM_THE_CONFIG\", \"-std=c++20\"]\nextra_args = [\"-DEXTRA\"]\n",
        );

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, DiskFiles);
        let session = Session::open(&project.root, providers, WatchFilter::new(&project.root));

        let defines: Vec<&str> = session
            .config()
            .defines
            .iter()
            .map(|define| define.name.as_ref())
            .collect();
        assert_eq!(
            defines,
            ["FROM_THE_CONFIG", "EXTRA"],
            "the file's flags are the compilation, and `extra_args` joins them"
        );
        assert_eq!(
            session.config().standard.as_deref(),
            Some("c++20"),
            "the database's `-std=c++17` is not in effect"
        );
    }

    #[test]
    fn removing_and_adding_a_flag_is_one_pass_in_that_order() {
        // `remove_args` drops from the list that won, and `extra_args` appends to it — removing *first* is what
        // makes "drop the database's `-std=c++17`, add `-std=c++20`" mean c++20 rather than c++17.
        let project = Project::new("config-remove");
        project.write("main.cpp", "int main() { return 0; }\n");
        let root = project.root.to_string_lossy().replace('\\', "/");

        project.write(
            "compile_commands.json",
            &format!(
                "[\n  {{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"g++\", \"-std=c++17\", \"-DKEEP\", \"-Werror\", \"-c\", \"{root}/main.cpp\"]}}\n]\n"
            ),
        );
        project.write(
            ".cppls.toml",
            "[compile]\nremove_args = [\"-Werror\", \"-std=c++17\"]\nextra_args = [\"-std=c++20\"]\n",
        );

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, DiskFiles);
        let session = Session::open(&project.root, providers, WatchFilter::new(&project.root));

        assert_eq!(session.config().standard.as_deref(), Some("c++20"));
        let defines: Vec<&str> = session
            .config()
            .defines
            .iter()
            .map(|define| define.name.as_ref())
            .collect();
        assert_eq!(defines, ["KEEP"], "a flag nobody removed still applies");
    }

    #[test]
    fn the_database_can_be_somewhere_no_convention_predicts() {
        let project = Project::new("config-database");
        project.write("main.cpp", "int main() { return 0; }\n");
        let root = project.root.to_string_lossy().replace('\\', "/");
        project.write(
            "out/compile_commands.json",
            &format!(
                "[\n  {{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"g++\", \"-DFROM_ELSEWHERE\", \"-c\", \"{root}/main.cpp\"]}}\n]\n"
            ),
        );
        project.write(
            ".cppls.toml",
            "[compile]\ndatabase = \"out/compile_commands.json\"\n",
        );

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, DiskFiles);
        let session = Session::open(&project.root, providers, WatchFilter::new(&project.root));

        assert_eq!(
            session.compile_database().map(crate::CompileCommands::len),
            Some(1)
        );
        let defines: Vec<&str> = session
            .config()
            .defines
            .iter()
            .map(|define| define.name.as_ref())
            .collect();
        assert_eq!(defines, ["FROM_ELSEWHERE"]);
        assert_eq!(project.relative(session.project_files()), ["main.cpp"]);
    }

    #[test]
    fn the_cache_goes_where_the_project_says_and_the_filter_follows_it() {
        // The cache directory is one name with three readers — the store that writes under it, the filter that
        // ignores its events, and the report a user reads — and this is the property that keeps them one answer.
        let project = Project::new("config-cache");
        project.write("main.cpp", "int main() { return 0; }\n");
        project.write(".cppls.toml", "[index]\ncache_dir = \".cppls-cache\"\n");

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, DiskFiles);
        let mut session = Session::with_config(
            &project.root,
            providers,
            WatchFilter::new(&project.root),
            CompilerConfig::default(),
        );

        session.index_everything();
        assert_eq!(session.stats().rebuilt, 1, "the file was read");

        let summaries = project.root.join(".cppls-cache").join("summaries");
        assert!(
            summaries.is_dir(),
            "the summaries are under the configured directory, not the default: {}",
            summaries.display()
        );
        assert!(
            !project.root.join(crate::CACHE_DIRECTORY).exists(),
            "and the default directory was not created at all"
        );
    }

    #[test]
    fn the_sections_a_language_server_reads_come_from_the_same_parse() {
        // One file, one parser: the server's keys are read from the session's report rather than by a second
        // TOML reader in the other crate, which is what keeps the two from disagreeing about what it says.
        let project = Project::new("config-server-keys");
        project.write("main.cpp", "int main() { return 0; }\n");
        project.write(
            ".cppls.toml",
            "[diagnostics]\non_change_ms = 250\n\n[hover]\nenable = false\n",
        );

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, DiskFiles);
        let session = Session::with_config(
            &project.root,
            providers,
            WatchFilter::new(&project.root),
            CompilerConfig::default(),
        );

        let config = &session.project_config().config;
        assert_eq!(config.diagnostics.on_change_ms, Some(250));
        assert_eq!(config.hover.enable, Some(false));
    }

    #[test]
    fn a_configuration_file_with_a_typo_still_analyses_the_project() {
        // The failure mode the section-by-section parse exists for: one bad key must not cost the project its
        // exclusions, and the problem must be visible to whoever asks the session what it thought.
        let project = Project::new("config-typo");
        project.write("main.cpp", "int main() { return 0; }\n");
        project.write("vendor/third_party.cpp", "int vendored;\n");
        project.write(
            ".cppls.toml",
            "[workspace]\nexclude = [\"vendor/**\"]\n\n[index]\ncache_dir = 5\n",
        );

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, DiskFiles);
        let session = Session::with_config(
            &project.root,
            providers,
            WatchFilter::new(&project.root),
            CompilerConfig::default(),
        );

        assert_eq!(
            project.relative(session.project_files()),
            ["main.cpp"],
            "the exclusions are in effect"
        );
        assert_eq!(session.project_config().problems.len(), 1);
        assert!(session.project_config().problems[0].message.contains("[index]"));
        assert_eq!(session.project_config().errors().count(), 1);
    }

    #[test]
    fn a_database_in_a_build_directory_is_found_and_its_flags_reach_the_session() {
        // The whole point of the build-system half: a CMake project that exports its database into `build/` — which
        // is where CMake puts it — must be analysed with *those* flags and *that* file list, without anybody
        // naming the path. This is the test that fails if the discovery is written but not wired into the session.
        let project = Project::new("build-directory-database");
        project.write("main.cpp", "int main() { return 0; }\n");
        let root = project.root.to_string_lossy().replace('\\', "/");

        project.write(
            "build/compile_commands.json",
            &format!(
                "[\n  {{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"g++\", \"-DFROM_THE_BUILD_DIRECTORY\", \"-std=c++20\", \
                 \"-c\", \"{root}/main.cpp\"]}}\n]\n"
            ),
        );

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, DiskFiles);
        let session = Session::open(&project.root, providers, WatchFilter::new(&project.root));

        assert_eq!(
            session.compile_database().map(crate::CompileCommands::len),
            Some(1),
            "the database in the build directory was read"
        );
        assert_eq!(
            session
                .discovery()
                .database
                .as_ref()
                .map(|found| found.origin),
            Some(crate::DatabaseOrigin::BuildDirectory),
            "and the report says where it was found"
        );
        let defines: Vec<&str> = session
            .config()
            .defines
            .iter()
            .map(|define| define.name.as_ref())
            .collect();
        assert_eq!(defines, ["FROM_THE_BUILD_DIRECTORY"]);
        assert_eq!(session.config().standard.as_deref(), Some("c++20"));
        assert!(
            session
                .discovery()
                .problems
                .iter()
                .any(|problem| problem.message.contains("build directory")),
            "and a user reading the report is told: {:?}",
            session.discovery().problems
        );
    }

    #[test]
    fn a_cmake_project_without_a_database_is_analysed_with_what_cmake_knows() {
        // `CMAKE_EXPORT_COMPILE_COMMANDS=OFF` is the shape that produces this: no database at all, and CMake's
        // cache as the only statement about the compilation. It is a *worse* answer than a database — whatever a
        // target adds is missing — and it is much better than nothing, which is why it is worth the code and why
        // the problem list says how to do better.
        let project = Project::new("cmake-without-database");
        project.write("main.cpp", "int main() { return 0; }\n");
        project.write(
            "CMakeCache.txt",
            &format!(
                "CMAKE_HOME_DIRECTORY:INTERNAL={}\n\
                 CMAKE_CXX_COMPILER:FILEPATH=g++\n\
                 CMAKE_CXX_FLAGS:STRING=-DFROM_CMAKE_CACHE\n\
                 CMAKE_CXX_STANDARD:STRING=20\n\
                 CMAKE_EXPORT_COMPILE_COMMANDS:BOOL=OFF\n",
                project.root.to_string_lossy().replace('\\', "/")
            ),
        );

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, DiskFiles);
        let session = Session::open(&project.root, providers, WatchFilter::new(&project.root));

        assert!(session.compile_database().is_none());
        assert!(session.discovery().cmake.is_some(), "the cache was read");
        let defines: Vec<&str> = session
            .config()
            .defines
            .iter()
            .map(|define| define.name.as_ref())
            .collect();
        assert_eq!(
            defines,
            ["FROM_CMAKE_CACHE"],
            "the project-wide flags came from CMake"
        );
        assert_eq!(session.config().standard.as_deref(), Some("c++20"));
        assert!(
            session
                .discovery()
                .problems
                .iter()
                .any(|problem| problem.message.contains("CMAKE_EXPORT_COMPILE_COMMANDS=OFF")),
            "and the one command that would make this better is spelled out: {:?}",
            session.discovery().problems
        );
    }

    #[test]
    fn a_compile_database_names_the_files_that_are_here_and_the_flags_they_are_built_with() {
        // The two things a database is read for: which files are compiled — the list is better than a scan, because
        // it is the project's own statement — and the flags, which are the whole reason the configuration is not
        // guessed. An entry naming a file this checkout does not have is dropped rather than queued, because a
        // checked-in database writes the absolute paths of the machine that produced it.
        let project = Project::new("database");
        project.write("main.cpp", "int main() { return 0; }\n");
        let root = project.root.to_string_lossy().replace('\\', "/");

        project.write(
            "compile_commands.json",
            &format!(
                "[\n  {{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"g++\", \"-DFROM_THE_DATABASE\", \"-I{root}/include\", \"-std=c++20\", \
                 \"-c\", \"{root}/main.cpp\"]}},\n  \
                 {{\"directory\": \"{root}\", \"file\": \"{root}/elsewhere.cpp\", \
                 \"arguments\": [\"g++\", \"-c\", \"{root}/elsewhere.cpp\"]}}\n]\n"
            ),
        );

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, DiskFiles);
        let session = Session::open(&project.root, providers.clone(), WatchFilter::new(&project.root));

        let database = session.compile_database().expect("the database is read");
        assert_eq!(database.len(), 2);

        let defines: Vec<&str> = session
            .config()
            .defines
            .iter()
            .map(|define| define.name.as_ref())
            .collect();
        assert_eq!(defines, ["FROM_THE_DATABASE"]);
        assert_eq!(session.config().standard.as_deref(), Some("c++20"));

        assert_eq!(project.relative(session.project_files()), ["main.cpp"]);
        assert_eq!(
            session.pending(),
            1,
            "the entry for a file that is not here is not work"
        );
    }

    #[test]
    fn a_define_the_compile_database_carries_decides_a_condition_in_a_file() {
        // The whole way a `-D` travels: the database's flags become the configuration, the configuration becomes
        // the macro environment, and the environment decides the `#ifdef` around an `#include`. The *answer*
        // therefore depends on how the project is built while the *summary* does not — which is why a summary
        // stores the question rather than the answer, and why changing `-D` does not throw the cache away.
        let project = Project::new("conditional-include");
        let root = project.root.to_string_lossy().replace('\\', "/");

        project.write("feature.h", "#define FEATURE_ONLY int\n");
        project.write(
            "main.cpp",
            "#ifdef FROM_THE_DATABASE\n#include \"feature.h\"\n#endif\nFEATURE_ONLY x;\n",
        );
        project.write(
            "compile_commands.json",
            &format!(
                "[{{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"g++\", \"-DFROM_THE_DATABASE\", \"-c\", \"{root}/main.cpp\"]}}]\n"
            ),
        );

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, DiskFiles);
        let mut session = Session::open(&project.root, providers.clone(), WatchFilter::new(&project.root));

        // Both files, because the closure of `main.cpp` is what the query walks.
        session.advance(64);

        let path = project.root.join("main.cpp");
        let source = std::fs::read_to_string(&path).expect("the fixture reads");
        let cursor = source.find("FEATURE_ONLY x").expect("the use");

        let view = session.view(&path).expect("the file was read");
        let references = session.macro_references(&view, cursor);

        let Known::Yes(found) = references else {
            panic!("the macro is defined in the closure, got {references:?}");
        };

        assert_eq!(
            found.uncertain(),
            0,
            "the `#ifdef` is decided by the database's `-D`, so the use is a use: {:#?}",
            found.files
        );
    }

    #[test]
    fn the_compilation_is_known_only_when_the_compiler_answered_with_its_macros() {
        // The licence for one claim — *a name none of the witnesses mentions is not defined* — pinned on the three
        // shapes a session can be in. The middle one is why this is a function rather than a field read at the call
        // site: a toolchain **exists** on a machine whose headers were found by a convention and whose compiler
        // could not be asked, and its macro table is empty. Claiming completeness there would read all five hundred
        // of the compiler's own names as "not defined" — `#ifdef _MSC_VER` coming out false *on MSVC* — which is a
        // wrong answer rather than a missing one, the direction this layer must never fail in.
        assert!(
            !super::the_compilation_is_known(None),
            "no compiler answered, so nothing witnesses the built-ins"
        );

        let unasked = Toolchain {
            compiler: None,
            version: None,
            system_include_paths: vec![PathBuf::from("/usr/include")],
            builtin_macros: Vec::new(),
            dialect: None,
            standard: None,
            source: ToolchainSource::SystemHeaders,
            note: Some("no compiler could be asked".to_string()),
        };
        assert!(
            !super::the_compilation_is_known(Some(&unasked)),
            "directories without a macro table are half a toolchain, and the missing half is the built-ins"
        );

        let asked = Toolchain {
            compiler: Some(PathBuf::from("cl")),
            builtin_macros: vec![CommandLineMacro::defined("_MSC_VER")],
            ..unasked
        };
        assert!(
            super::the_compilation_is_known(Some(&asked)),
            "a compiler that answered is the only witness there is to the names no file defines"
        );
    }
}




