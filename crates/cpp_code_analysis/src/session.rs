//! The driver: one open project, with the four layers joined and a queue that notifications re-seed.
//!
//! Everything below this module is a *capability*: [`discover`](crate::discover) can find a toolchain,
//! [`SummaryStore`] can cache a file's facts, [`Worklist`](crate::Worklist) knows the order that answers a user's
//! question first, and the queries in [`crate::index`] answer about a name, a member or a cursor. Nothing joined
//! them, which is why `docs/index-design.md` calls "opening a project" the shortest piece of product work left:
//!
//! ```text
//! open a project     discover the toolchain, read compile_commands.json, list the sources
//! say what changed   didOpen / didChange / didClose / didSave, and the client's own file events
//! index some of it   one file per call, open files and what they include before the rest
//! answer a question  parse the buffer the cursor is in, then ask the index about it
//! ```
//!
//! # Why the caller owns the providers
//!
//! A [`Session`] reads through an [`OverlayFiles`] — the editor's open buffers in front of the filesystem — and
//! borrows it, because a [`SummaryStore`] borrows the provider it was built over. That makes the borrow part of
//! the API rather than an implementation detail, and the shape it forces is the right one:
//!
//! ```text
//! let documents = OpenDocuments::new();
//! let files = SessionFiles::new(documents.clone(), DiskFiles);
//! let mut session = Session::open(root, &files, WatchFilter::new(root));
//!
//! documents.open(path, buffer_text);   // ← works while the session is alive
//! ```
//!
//! The handle is the point: an edit has to reach the analysis **while the store holds the provider**, and
//! `OverlayFiles` is read through `&self` because its overlay has interior mutability. A session that owned its
//! providers would be self-referential — the store borrowing a field of the struct it sits in — which Rust has no
//! safe way to express and which is not worth an unsafe one.
//!
//! # What it deliberately is not
//!
//! * **No LSP types.** Positions here are byte offsets, and the answers are this crate's own. A server maps
//!   `Position` onto an offset (`cpp_parser::LineIndex::get_offset` is that mapping) and the answers onto protocol
//!   responses; keeping the protocol out means this layer is testable without a client, and `docs/index-design.md`
//!   records the mapping as the next piece of work rather than a thing to guess at here.
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
//! representable and simply not implemented. `docs/roadmap.md` carries it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use cpp_parser::{CppParseError, CppParser, CppSyntaxNode, CppSyntaxTree, Dialect, ParserConfig};

use crate::include::config::{CompileCommand, CompileCommands, CompilerConfig, parse_compile_commands};
use crate::include::paths::{DiskFiles, FileProvider, OverlayFiles, normalize_path};
use crate::include::graph::Marked;
use crate::include::toolchain::{self, DiskCommands, Environment, Toolchain};
use crate::index::project::{
    MemberCompletions, MemberList, NameCompletions, ProjectDefinition, ProjectIndex, ProjectMacro,
};
use crate::index::references::{MacroReferences, ReferenceBudget, macro_references};
use crate::index::store::{StoreStats, SummaryStore};
use crate::index::watch::{ChangeBatch, FileEvent, Response, WatchFilter};
use crate::index::worklist::{Priority, Step, outcome_of};
use crate::index::{
    definition_across_files, macro_across_files, member_completions_at, members_of, name_completions_at,
};
use crate::sema::scopes::build_scopes;
use crate::symbol::{Known, ScopeTree, UnknownReason};

/// The name a compile database is conventionally found under, relative to the project root.
const COMPILE_DATABASE: &str = "compile_commands.json";

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
/// normalized path, and readable through `&self` because the store holds that borrow for as long as it lives.
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
/// Because the alternative is a session that cannot be edited. The store borrows the provider chain for its whole
/// life, so a mutable borrow of the overlay could not be taken while it exists; interior mutability with a
/// read-mostly lock is what lets `did_change` run between two queries. The lock is never held across one.
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
}

/// One file, analysed from the text the cursor is in: the buffer if it is open, the disk otherwise.
///
/// The other half of a session's answer. A query about a *cursor* needs the file's scopes and its tree, and neither
/// is in the index — a summary holds facts about declarations, not the syntax a position has to be resolved
/// against. So the layer above asks for a view of the file the user is looking at, and asks the session about the
/// index from there.
///
/// The text is the **buffer** when there is one, which is what makes a query about an unsaved edit answer about
/// the edit. `open` says which of the two it was, because a consumer that shows the file's own diagnostics needs
/// to know whether the analysis saw what the user sees.
#[derive(Debug, Clone)]
pub struct FileView {
    pub path: PathBuf,
    /// The text analysed: the buffer when open, the file otherwise.
    pub source: String,
    /// The parse of [`FileView::source`], kept because a caller may want the diagnostics or the token list.
    pub tree: CppSyntaxTree,
    /// The tree's root. Exactly `tree.get_red_root()`, kept because every query wants it and re-deriving a red root
    /// per query would allocate one per query.
    pub root: CppSyntaxNode,
    /// The file's scopes, built from the same tree.
    pub scopes: ScopeTree,
    /// Did the text come from an open buffer rather than from the file?
    pub open: bool,
}

impl FileView {
    /// The parse diagnostics, as the parser reported them.
    ///
    /// A tolerant parser reports rather than fails, so this is empty for most files and non-empty for a file being
    /// typed at — which is not the same thing as a file that does not compile.
    pub fn errors(&self) -> &[CppParseError] {
        self.tree.get_errors()
    }

    /// A byte offset from a **line and column**, both counted from zero.
    ///
    /// The mapping a client's positions need, and the one place LSP's own rule is *not* implemented: the columns
    /// this counts are characters, while the protocol counts UTF-16 code units, and the two differ on a line with
    /// an emoji or any character outside the basic plane. That conversion is the protocol layer's — doing it here
    /// would put a rule about a wire format in the crate that has no wire format.
    ///
    /// The line index is built per call, which is a scan of the source: one call per request is what this is for,
    /// and a caller mapping thousands of positions should build one itself (`cpp_parser::LineIndex::parse`).
    pub fn offset_at(&self, line: usize, column: usize) -> Option<usize> {
        cpp_parser::LineIndex::parse(&self.source)
            .get_offset(line, column, &self.source)
            .map(usize::from)
    }
}

/// One open project: the configuration, the facts read so far, and the order the rest will be read in.
pub struct Session<'a, F: FileProvider = DiskFiles> {
    root: PathBuf,
    config: CompilerConfig,
    /// What `discover` found, or `None` when no compiler answered. Kept for a human reading a log line, and for
    /// `docs/std-library.md`'s measurements.
    toolchain: Option<Toolchain>,
    /// The compile database the configuration came from, when there was one. Kept because it is the project's own
    /// answer to "how is this built", which a caller diagnosing a wrong analysis wants to see.
    database: Option<CompileCommands>,
    /// The providers, borrowed. See the module documentation for why they are the caller's.
    files: &'a SessionFiles<F>,
    /// The buffer half of `files`, as a handle this session can write through.
    documents: OpenDocuments,
    store: SummaryStore<'a, SessionFiles<F>>,
    /// Which paths the client's events and the project scan are about. Kept because both questions — "is this
    /// event interesting" and "is this file part of the project" — have one answer, and the cache directory is the
    /// case that makes that concrete: it is not project source and it is not an event anybody wants.
    filter: WatchFilter,
    /// What the project scan found, so that a configuration change can re-seed from it.
    project: Vec<PathBuf>,
    queue: Work,
}

impl<'a> Session<'a, DiskFiles> {
    /// Open a project the way a language server does: ask the machine what it compiles with.
    ///
    /// The three steps, in the order their results depend on each other:
    ///
    /// ```text
    /// 1. compile_commands.json, if the project has one — the flags and the file list
    /// 2. discover(files, …)             — which compiler, and where its own headers are
    /// 3. the scan                       — the sources the project owns
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
        files: &'a SessionFiles<DiskFiles>,
        filter: WatchFilter,
    ) -> Session<'a, DiskFiles> {
        let root = root.into();
        let database = read_compile_database(files, &root);
        let base = base_config(database.as_ref());
        let for_file = first_compiled_file(database.as_ref()).unwrap_or_else(|| root.clone());

        let toolchain = toolchain::discover(
            files,
            &DiskCommands,
            database.as_ref(),
            &for_file,
            &Environment::current(),
        );

        let config = match &toolchain {
            Some(found) => found.config(&base),
            None => base,
        };

        // **Which compiler this is**, asked of the compiler rather than guessed from the flags: GCC and Clang
        // predefine `__GNUC__`, MSVC predefines `_MSC_VER`, and both arrive in the table the same `-dM -E` call
        // that gave the search paths (see `toolchain::discover`). It matters to the *parser*, which has to know
        // what `__int128` means to the compiler reading the file — see `CompilerConfig::dialect`.
        //
        // No toolchain, no answer: the configuration keeps its default, and a caller that knows the target
        // (`with_config`) sets it itself.
        let config = match toolchain
            .as_ref()
            .and_then(|found| Dialect::from_predefined_macros(found.macros()))
        {
            Some(dialect) => config.with_dialect(dialect),
            None => config,
        };

        Session::assemble(root, files, filter, config, toolchain, database)
    }
}

impl<'a, F: FileProvider> Session<'a, F> {
    /// A session with the configuration the caller already has: no compile database is read and no compiler is run.
    ///
    /// The path for a caller that knows how the project is built — a test, a build-system integration — and the
    /// only one that works over a provider that is not the disk.
    pub fn with_config(
        root: impl Into<PathBuf>,
        files: &'a SessionFiles<F>,
        filter: WatchFilter,
        config: CompilerConfig,
    ) -> Session<'a, F> {
        Session::assemble(root.into(), files, filter, config, None, None)
    }

    fn assemble(
        root: PathBuf,
        files: &'a SessionFiles<F>,
        filter: WatchFilter,
        config: CompilerConfig,
        toolchain: Option<Toolchain>,
        database: Option<CompileCommands>,
    ) -> Session<'a, F> {
        let documents = files.overlay.clone();
        let project = project_files(files, &filter, &root, database.as_ref());

        // **Not** `database.is_some()`, and the measurement is why: with the environment declared complete, the
        // standard-library closure decides 440 of its 486 conditional includes instead of 85 (`condition_reach`),
        // and two paths *lose* answers they used to give — `__attribute__` and `STDMETHODCALLTYPE` went from
        // 767/4 078 "maybe" to "not a use", because the walk visits a header once and the first visit was through
        // a *conditional* include (`minwindef.h` includes `winnt.h` before `windef.h` does, and the second, certain
        // visit is skipped). See `docs/roadmap.md` §3.5c: until a certain path can improve on an uncertain first
        // visit, a claim this strong would turn honest doubt into a wrong answer, which is the one thing this layer
        // must not do.
        let configured = false;
        let store = SummaryStore::with_provider(root.clone(), config.clone(), files).with_macros(
            compilation_environment(&config, toolchain.as_ref(), configured),
        );

        let mut session = Session {
            root,
            config,
            toolchain,
            database,
            files,
            documents,
            store,
            filter,
            project,
            queue: Work::default(),
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

    pub fn toolchain(&self) -> Option<&Toolchain> {
        self.toolchain.as_ref()
    }

    pub fn compile_database(&self) -> Option<&CompileCommands> {
        self.database.as_ref()
    }

    pub fn documents(&self) -> &OpenDocuments {
        &self.documents
    }

    /// Every summary loaded so far — what the session currently knows, as opposed to what it will.
    pub fn index(&self) -> &ProjectIndex {
        self.store.index()
    }

    pub fn stats(&self) -> StoreStats {
        self.store.stats()
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
        self.store.forget(path);
        self.queue.again(path.to_path_buf(), Priority::Rest, 0);
    }

    /// The client reported filesystem changes — its own watcher's events, not ours.
    ///
    /// The entry point `docs/index-design.md` fixes for the "no OS watcher" decision: a client that watches (or an
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
    fn buffer_changed(&mut self, path: &Path, text: &str) {
        self.documents.open(path, text);
        self.store.forget(path);
        self.queue.again(path.to_path_buf(), Priority::Open, 0);
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
    pub fn advance(&mut self, steps: usize) -> Vec<Step> {
        let mut done = Vec::new();

        for _ in 0..steps {
            let Some((path, priority, depth)) = self.queue.pop() else {
                break;
            };

            // Only the resolved includes are wanted, and they are cloned out before the queue is touched: `get`
            // borrows the store, and the queue is a field of the same struct.
            let before = self.store.stats();
            let includes: Vec<PathBuf> = self
                .store
                .get(&path)
                .map(|summary| {
                    summary
                        .includes
                        .iter()
                        .filter_map(|include| include.resolved.clone())
                        .collect()
                })
                .unwrap_or_default();
            let after = self.store.stats();

            // Everything this file includes joins the same half of the list the file came from, one level further
            // out — a header an open file includes is worth reading before the rest of the project, and a header
            // the project's tenth translation unit includes is not.
            for include in includes {
                self.queue.add(include, priority, depth + 1);
            }

            done.push(Step {
                path,
                priority,
                depth,
                outcome: outcome_of(before, after),
            });
        }

        done
    }

    /// Work until there is nothing left, and say how many files were read.
    ///
    /// Chunked rather than one `advance(usize::MAX)`: the steps of a whole project are a `Vec` nobody wants, and a
    /// caller that wants progress wants it per chunk. Terminates because every step removes one entry from the
    /// queue and adds only files it has not already worked.
    pub fn index_everything(&mut self) -> usize {
        let mut total = 0;

        while !self.is_idle() {
            total += self.advance(64).len();
        }

        total
    }

    /// How many files are queued and not yet worked.
    ///
    /// Counted by **file**, not by queue entry: a path that was discovered once and opened later sits in two places
    /// and is one file to read.
    pub fn pending(&self) -> usize {
        self.queue.pending()
    }

    /// Is everything the session knows about already read?
    pub fn is_idle(&self) -> bool {
        self.queue.pending() == 0
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
    pub fn view(&self, path: impl AsRef<Path>) -> Option<FileView> {
        let path = path.as_ref();
        let open = self.documents.is_open(path);
        let source = match self.documents.text(path) {
            Some(text) => text,
            None => self.files.read(path)?,
        };

        let tree = CppParser::parse(&source, ParserConfig::default());
        let root = tree.get_red_root();
        let scopes = build_scopes(&root);

        Some(FileView {
            path: path.to_path_buf(),
            source,
            tree,
            root,
            scopes,
            open,
        })
    }

    /// Which declaration the name at `offset` means, using this file's scopes and then the index.
    pub fn definition(&self, view: &FileView, offset: usize) -> Known<ProjectDefinition> {
        definition_across_files(
            self.store.index(),
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
            self.files,
            &name,
            ReferenceBudget::default(),
        )
    }

    /// What to offer after a member access at `offset`: the object's type, its members, and the edit.
    pub fn member_completions(&self, view: &FileView, offset: usize) -> Known<MemberCompletions> {
        member_completions_at(
            self.store.index(),
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
}

/// A path as the queue compares them — the same normalization the store and the index use.
fn queue_key(path: &Path) -> String {
    normalize_path(path, cfg!(windows))
}

/// The compile database under a project root, when there is one to read.
///
/// A database that parses to nothing usable is treated as absent rather than as an empty project: its whole
/// purpose is to say how files are compiled, and a project with a malformed one is better analysed the way a
/// project with none is — the toolchain's own search paths — than with no include paths at all.
fn read_compile_database(files: &impl FileProvider, root: &Path) -> Option<CompileCommands> {
    let json = files.read(&root.join(COMPILE_DATABASE))?;
    let database = parse_compile_commands(&json);

    (!database.is_empty()).then_some(database)
}

/// The project's flags, from the first entry of its compile database.
///
/// See the module documentation: one configuration for the project is an approximation, and this is where it is
/// chosen. The **first** entry rather than a merge, because merging two targets' `-D`s would describe a compilation
/// that no file is part of.
fn base_config(database: Option<&CompileCommands>) -> CompilerConfig {
    database
        .and_then(|database| database.commands.first())
        .map(CompileCommand::to_config)
        .unwrap_or_default()
}

/// The macros a compilation starts with: what the compiler predefines, then what the command line says.
///
/// The order is the compiler's own: a `-D` of a name the compiler also predefines is the one the translation unit
/// sees, so the built-ins go in first and the command line last. A `-U` — which is how a project removes a
/// compiler's built-in — is applied after both.
///
/// # `configured`: whether this environment is the whole of what the compilation defines
///
/// The flag is the difference between `Unknown` and "not defined" for every condition naming something no file
/// defines, and it is passed in rather than assumed because it is a statement about the caller's **inputs**:
/// [`Session::open`] sets it when it read the project's own `compile_commands.json`, which is the project saying
/// how its files are compiled — the `-D`s, the `-std=`, the include paths. Without one, the environment is what
/// the compiler predefines and nothing else, and a project built with flags nobody wrote down would be read as if
/// those names were undefined — which is why the unconfigured case stays
/// [`Marked::incomplete`](crate::Marked::incomplete) and answers `Unknown`.
///
/// What the flag is *not* is a promise about the files: a walk that runs into an `#include` that did not resolve,
/// or one nobody indexed, takes the claim back with [`Marked::mark_incomplete`] — see
/// [`crate::index::environment`], which is where the two meet.
fn compilation_environment(
    config: &CompilerConfig,
    toolchain: Option<&Toolchain>,
    configured: bool,
) -> Marked {
    let mut marked = Marked::default();

    if let Some(toolchain) = toolchain {
        for (name, value) in toolchain.macros() {
            marked.define_on_the_command_line(name, value);
        }
    }

    // **What the configuration itself decides** — the standard it was compiled with, the target it was compiled
    // for. A toolchain's `-dM` output answers for the *compiler's own default invocation*, which is a different
    // question from the one the project asked: `-std=c++11` in the compile database means `__cplusplus` is
    // `201103L` however the compiler would have been run by hand. Applied over the toolchain and under the
    // project's own `-D`s, which are the last word.
    for definition in crate::predefined_macros_of(config) {
        marked.define_on_the_command_line(&definition.name, definition.value.as_deref());
    }

    for definition in &config.defines {
        marked.define_on_the_command_line(&definition.name, definition.value.as_deref());
    }

    for name in &config.undefines {
        marked.undefine(name);
    }

    if configured {
        marked
    } else {
        marked.incomplete()
    }
}

/// The file the toolchain should be discovered for: one the database actually compiles.///
/// [`crate::discover`] asks the database for *this* file's compiler, so naming a file no entry mentions makes it
/// fall through to `$CXX` and `PATH` — which on a machine with two toolchains is the wrong compiler for the
/// project's own headers.
fn first_compiled_file(database: Option<&CompileCommands>) -> Option<PathBuf> {
    database
        .and_then(|database| database.commands.first())
        .map(|command| command.file.clone())
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
        scan(root, filter, &mut found);
    }

    // Sorted and deduplicated: the walk's order is the filesystem's, and an index whose contents depend on which
    // directory entry the OS returned first is one whose behaviour cannot be compared between two runs.
    found.sort();
    found.dedup();
    found
}

/// Every source under `root`, bounded and with symlinked directories left alone.
///
/// The bound is [`MAX_PROJECT_FILES`]. Symlinks are not followed **as directories**: a link back up the tree is an
/// infinite walk, and the cheap rule that rules it out — the entry's own type, which does not resolve the link — is
/// also the one that keeps a project from indexing the same files through two paths.
fn scan(root: &Path, filter: &WatchFilter, found: &mut Vec<PathBuf>) {
    let mut pending = vec![root.to_path_buf()];

    while let Some(directory) = pending.pop() {
        if found.len() >= MAX_PROJECT_FILES {
            return;
        }

        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };

        for entry in entries.flatten() {
            if found.len() >= MAX_PROJECT_FILES {
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
            } else if kind.is_file() && is_a_source_file(&path) {
                found.push(path);
            }
        }
    }
}

/// Does this path's name say it is a source? See [`SOURCE_EXTENSIONS`] for why a scan may ask this and an include
/// may not.
fn is_a_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let lower = extension.to_ascii_lowercase();
            SOURCE_EXTENSIONS.contains(&lower.as_str())
        })
}

#[cfg(test)]
mod tests {
    use super::{OpenDocuments, Session, SessionFiles};
    use crate::include::config::CompilerConfig;
    use crate::include::paths::{DiskFiles, FileProvider, MemoryFiles};
    use crate::index::watch::{FileEvent, WatchFilter};
    use crate::index::{Priority, StepOutcome};
    use crate::symbol::{Known, UnknownReason};
    use std::path::{Path, PathBuf};

    /// A project in memory: the files, the buffers in front of them, and the providers that join the two.
    ///
    /// A struct rather than a tuple because the providers have to *outlive* the session they are borrowed by —
    /// which is the same ownership rule the module documentation describes for a real caller, so the fixture is
    /// that rule made visible in a test.
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

        fn session(&self) -> Session<'_, MemoryFiles> {
            Session::with_config(
                &self.root,
                &self.providers,
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
        // The order `docs/index-design.md` fixes, read off the steps: what the user is looking at, then what it
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
            &providers,
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
        let session = Session::open(&project.root, &providers, WatchFilter::new(&project.root));

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
        let mut session = Session::open(&project.root, &providers, WatchFilter::new(&project.root));

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
}
