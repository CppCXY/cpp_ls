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

use crate::include::config::{
    CompileCommands, CompilerConfig, project_config_from_flags,
};
use crate::file::paths::{DiskFiles, FileProvider, OverlayFiles, normalize_path};
use crate::include::toolchain::{self, DiskCommands, Environment, Toolchain};
use crate::file::view::FileView;
use crate::index::project::{
    MemberCompletions, MemberList, NameCompletions, ProjectDefinition, ProjectIndex, ProjectMacro,
};
use crate::index::references::{MacroReferences, ReferenceBudget, macro_references};
use crate::index::store::{StoreStats, SummaryStore};
use crate::index::worklist::StepOutcome;
use crate::index::watch::{ChangeBatch, FileEvent, Response, WatchFilter};
use crate::index::worklist::{Priority, Step, outcome_of};
use crate::index::{
    definition_across_files, macro_across_files, member_completions_at, members_of, name_completions_at,
};
use crate::project::{ConfigReport, ProjectDiscovery};
use crate::file::vfs::Vfs;
use crate::symbol::{Known, UnknownReason};

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
    /// `docs/std-library.md`'s measurements.
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
            discovery
                .database
                .as_ref()
                .map(|database| &database.commands),
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

        Session::assemble(
            root,
            files,
            filter,
            config,
            toolchain,
            discovery,
        )
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

        // **The project's own `compile_commands.json` is what makes the environment complete** (B131): it is the
        // project saying how its files are compiled — the `-D`s, the `-std=`, the include paths — so with one in
        // hand, a name nothing defines really is undefined rather than unknown, and conditions become decidable.
        //
        // This was `false` unconditionally before, and the reason was measured: with the environment declared
        // complete, the closure decides 440 of its 486 conditional includes instead of 85 (`condition_reach`), but
        // two names *lost* answers they used to give — `__attribute__` and `STDMETHODCALLTYPE` went from thousands
        // of "maybe" to "not a use" — because the walk visits a header once and the first visit was through a
        // *conditional* include (`minwindef.h` includes `winnt.h` before `windef.h` does, and the second, certain
        // visit was skipped). Declaring the environment complete turned honest doubt into a wrong answer, which is
        // the one thing this layer must not do.
        //
        // `ProjectIndex::macro_candidates` now asks the graph rather than the route ("is this file certainly part
        // of the translation unit", B131), so a certain path can no longer be skipped in favour of a conditional
        // one. What that did to the two names above is measured in `docs/roadmap.md` §3.5c and §7.
        let configured = database.is_some();
        let mut store = SummaryStore::with_provider(root.clone(), config.clone(), files.clone())
            .with_macros(crate::index::environment::compilation_environment(
                &config,
                toolchain.as_ref(),
                configured,
            ));

        if let Some(cache_dir) = &project_config.config.index.cache_dir {
            store = store.with_cache_directory(cache_dir);
        }

        let mut session = Session {
            root,
            config,
            toolchain,
            discovery,
            vfs: Vfs::new(files.clone()),
            files,
            documents,
            store,
            filter,
            project,
            queue: Work::default(),
            parsed_since_the_last_pass: Vec::new(),
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
        // The text the analysis was holding was the *buffer's*, and the disk may never have seen it. Dropping the
        // entry is what makes the next question read the file again — a closed buffer is not a text this session
        // knows any more, and a view built on it would answer about an edit nobody saved.
        self.vfs.close(path);
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
    ///
    /// The VFS is told **in the same breath** as the overlay, and that order matters: the schema is that a file's
    /// text and its line index are never out of step, so the entry takes the new text (and builds its index) before
    /// anything can ask a question about the file — and the summary is dropped with it, because the old one
    /// describes text that no longer exists.
    fn buffer_changed(&mut self, path: &Path, text: &str) {
        self.documents.open(path, text);
        self.vfs.insert(path, text, true);
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
            // The file is loaded into the VFS *before* it is read, so that everything the analysis reads it is
            // also holding: a hover that shows a declaration from a header nobody opened asks the VFS for it, and
            // a file that was indexed is a file whose text and line index are already here.
            self.vfs.load(&path);
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

        // **The second pass, at the moment the closure is in hand** (B131). `SummaryStore::get` reads one file with
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
            self.store.re_read_where_a_body_decides(&parsed);
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
    ///
    /// The view is **of the session's VFS**: the text and its line index come from the file the VFS is holding, and
    /// the view shares both rather than copying either. What is done here is the parse and the scopes, which are
    /// the two things a position needs and a summary cannot hold.
    pub fn view(&self, path: impl AsRef<Path>) -> Option<FileView> {
        Some(FileView::parse(self.vfs.held(path)?))
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
            &self.files,
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
    use crate::include::config::CompilerConfig;
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
        // The store's second pass, on the closure a **session** builds one file at a time (B131). MSVC's STL is
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
        // `docs/index-design.md` §"宏体推导出的事实：记证据，不进键".
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
}









