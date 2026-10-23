//! Deciding **when** to build a summary, and what to do with the one on disk.
//!
//! [`crate::index`] builds one summary from one file's text. This module is the layer that decides whether to ask
//! it to: it holds the project root, the compiler configuration and the provider, computes the key the file
//! currently has, and rebuilds when nothing is stored under it.
//!
//! # The three things it does, and the one it does not
//!
//! ```text
//! get(path)          the summary for the file as it is *now* — from disk, or freshly built and written
//! invalidate(path)   the work set when a file changes: it, and everything that includes it
//! stats()            how often the disk answered instead of the parser
//! ```
//!
//! What it does **not** do is watch the filesystem or schedule the work. A caller decides when to ask, which is
//! what keeps this type testable without a project on disk and free of any opinion about threads — the same
//! division [`crate::index::FileIndexer`] makes about parsing.
//!
//! # The lookup comes first
//!
//! Nothing here compares a file against a stored list of what it contained, and — the part that took a design
//! change to get right — nothing has to be *understood* before the disk is asked either:
//!
//! ```text
//! 1. read the text
//! 2. the key is (hash of that text, compilation context), and both are computable without parsing
//! 3. is a summary stored under it? then that summary is the answer, and nothing was parsed
//! 4. otherwise build it, file it under the key, and write it
//! ```
//!
//! Step 2 is only possible because the key has no macro-environment part, and [`crate::SummaryKey`] records why
//! leaving it out is sound rather than merely convenient. This is what makes a branch switch cheap: the files come
//! back with the contents the cache was built from, so their keys come back too, and the summaries are found
//! without being understood.
//!
//! The version before this one built the summary *first* and computed the key from it, because the environment
//! needed the file's own `#define`s. That made every lookup pay for a parse, so the cache saved writes and never
//! saved the thing it exists to save. [`StoreStats::rebuilt`] is the counter that says which of the two versions
//! is running: on a warm cache it stays at zero.
//!
//! # The one case that must not be cached
//!
//! A file that writes an `#include` whose target was not found says so — `resolved: None` — and *that* is a fact
//! about the filesystem rather than about the text. Storing it would produce an entry that looks valid and goes on
//! answering "unresolved" after the header appears, with nothing in the key to notice: which candidate paths exist
//! is deliberately not part of a key that has to stay computable from the text alone. So such a file is built,
//! returned, and not written; the caller is told through [`StoreStats::unstored`], and the reason is the same one
//! gives for `Unknown` being a first-class answer: a wrong entry is worse than a missing
//! one.
//!
//! The same reasoning covers the other direction — a header that appears *earlier* on a search path than the one
//! that resolved — and it is worth stating plainly, because no key of this shape can catch it: what a file's
//! includes resolve to depends on the filesystem, so **a change to which files exist is a change the key does not
//! see**. That is a job for the watcher, which knows a file was created or deleted and can invalidate the
//! directory's includers; until then, an unresolved include simply is never stored, so the common shape of the
//! problem heals on its own.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cpp_parser::Dialect;

use crate::IncludeForm;
use crate::cache::{SummaryKey, content_hash, fnv1a64};
use crate::file::paths::{DiskFiles, FileProvider, normalize_path};
use crate::include::config::CompilerConfig;
use crate::index::project::ProjectIndex;
use crate::index::FileIndexer;
use crate::summary::{FileSummary, IncludeFact};

/// A project's summaries on disk and in memory, and the rule for when each is rebuilt.
///
/// # Why the provider is owned
///
/// Because a store that borrowed its provider could only live inside the scope that declared the borrow, and the
/// caller that needs it most — a language server, which holds one store for as long as the editor is open — has no
/// such scope: the provider and the store are both created at startup and both outlive the function that made
/// them. Providers are *handles* rather than data ([`crate::OpenDocuments`] is a lock behind an `Arc`, `DiskFiles`
/// is a unit struct), so owning one costs a pointer and a clone shares the same buffers rather than copying them.
pub struct SummaryStore<F: FileProvider = DiskFiles> {
    /// The project root. The cache lives under it, because puts it there on purpose: it
    /// travels with a checkout, so CI gets the same warm cache a developer has.
    root: PathBuf,
    /// Where the summaries are written: `<root>/.cppls`, or the directory `index.cache_dir` names.
    cache: PathBuf,
    config: CompilerConfig,
    files: F,
    index: ProjectIndex,
    stats: StoreStats,
    /// See [`SummaryStore::revision`] — the clock `index::walk_cache` keys its contributions by.
    revision: std::sync::atomic::AtomicU64,
    /// **What the unit walk decided about each file's `#define`s**, kept here because two paths build a summary and
    /// only one of them has a unit in hand.
    ///
    /// The decision itself belongs to the walk, which evaluates every guarded fact against the unit's own state —
    /// the same evaluation the cook uses, and the only one that knows what the compilation defines. Handing it over
    /// through a parameter works for the re-read (`index_includes_from`, which has the units) and **not** for
    /// [`SummaryStore::prepare`], which is the path a query takes when it needs a file *before* the re-read reaches
    /// it. A header made only of `#define`s is exactly that file: the re-read skips it (`mentions_one_of`), so
    /// without this map its summary keeps every guarded `#define` forever and a consumer decides the regions again
    /// against a different environment.
    ///
    /// Measured on `vcruntime.h`, which writes `_STL_LANG` once per branch of `#ifdef __cplusplus`: the walk takes
    /// the `_MSVC_LANG` branch and skips the `#else`'s `0L`, and a lookup over the raw facts answered `0L` — for a
    /// file the compiler reads as C++20.
    ///
    /// # Why it is behind a lock now
    ///
    /// Because the pass that fills it ([`SummaryStore::prepare_the_re_read`]) takes `&self`: it is the expensive half
    /// of the second pass and it runs under the **read** lock, beside whatever a query is doing, so that the write
    /// lock a request waits for is held for the insert and nothing else. A `#define` decision has to be visible to
    /// every summary built after it — [`SummaryStore::prepare_telling`] consults this map — so the record is made
    /// where it was always made, at the start of the pass, and the lock is what makes that legal from `&self`.
    dead_macros: std::sync::Mutex<std::collections::HashMap<PathBuf, Vec<usize>>>,
    /// **The summaries in one database** — `CPPLS_CACHE_DB=1`, and `docs/refactor-cache-and-startup.md` M1.
    ///
    /// Opened on first use rather than in `with_provider`, because the cache directory is only final after
    /// `with_cache_directory` and a store that opened the wrong directory would be a store with two caches.
    summary_database: std::sync::OnceLock<Option<std::sync::Arc<crate::cache_db::SummaryCache>>>,
    /// The generation a write is recorded under — bumped when the store opens, so a collection can tell this run's
    /// rows from the previous one's.
    generation: u64,
    /// **One entry per distinct include search**, for the life of the store — see
    /// [`SummaryStore::resolution_still_holds`].
    ///
    /// The key is the question, not the file: `(form, is_next, spelling, directory)`. A quoted include is relative to
    /// the including file's directory and so keeps that in the key; an angled one is not, and gives the key an empty
    /// directory, because `<vector>` asked from four hundred headers is one search.
    ///
    /// **Bounded** ([`MAX_INCLUDE_SEARCHES`]): a project with a vendored tree asks a new question for every directory
    /// it searches, and the answer is one directory probe — cheap enough that the table is cleared rather than
    /// evicted when it fills.
    include_searches: std::sync::Mutex<
        std::collections::HashMap<(IncludeForm, bool, Box<str>, PathBuf), Option<PathBuf>>,
    >,
}

/// **How many include questions are remembered** — the cache inside [`SummaryStore`], cleared when it fills.
const MAX_INCLUDE_SEARCHES: usize = 8192;

/// **The second pass, planned but not applied** — what [`SummaryStore::prepare_the_re_read`] worked out.
///
/// The split exists for the reason every other prepare/commit pair in this crate does: the pass re-parses files and
/// walks their environments, which is 150 ms per file measured, and it used to do all of it while holding the write
/// lock a request waits for. What is left for [`SummaryStore::commit_the_re_read`] is an index insert per file.
pub struct ReRead {
    /// The files whose reading changed, and whether each may be written down.
    rebuilt: Vec<(FileSummary, bool)>,
}

impl ReRead {
    /// Is there nothing to apply? A caller that would take the write lock only for this can skip it.
    pub fn is_empty(&self) -> bool {
        self.rebuilt.is_empty()
    }
}

/// More threads than this stop paying: the parse is memory-bound long before it is core-bound.
const PARALLEL_WORKERS: usize = 8;

/// **The four stages of one prepared file**, accumulated only while `CPPLS_TRACE_COOKING` is on.
///
/// A warm start's cost is spread over eight workers and three different things — reading the text, decoding a stored
/// summary, and re-running the include search to see that the stored answer still holds — so a wall clock says
/// nothing about which one to work on. These are read as a delta around one [`SummaryStore::prepare_closure`], which
/// is one wave, and printed as one line.
///
/// `build` is the whole call for a file that was parsed (read, index, encode, write), because on that path there is
/// nothing to separate: the parse is what is being paid for.
static PREPARED: [std::sync::atomic::AtomicU64; 5] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Count one prepared file's stages — `(files, read, decode, verify, build)`, in nanoseconds.
fn note_a_prepared_file(
    read: std::time::Duration,
    decode: std::time::Duration,
    verify: std::time::Duration,
    build: std::time::Duration,
) {
    use std::sync::atomic::Ordering::Relaxed;

    let nanos =
        |duration: std::time::Duration| duration.as_nanos().min(u128::from(u64::MAX)) as u64;
    PREPARED[0].fetch_add(1, Relaxed);
    PREPARED[1].fetch_add(nanos(read), Relaxed);
    PREPARED[2].fetch_add(nanos(decode), Relaxed);
    PREPARED[3].fetch_add(nanos(verify), Relaxed);
    PREPARED[4].fetch_add(nanos(build), Relaxed);
}

/// [`note_a_prepared_file`]'s counters, as they stand — the delta between two reads is one wave's work.
fn prepared_stages() -> [u64; 5] {
    use std::sync::atomic::Ordering::Relaxed;

    [
        PREPARED[0].load(Relaxed),
        PREPARED[1].load(Relaxed),
        PREPARED[2].load(Relaxed),
        PREPARED[3].load(Relaxed),
        PREPARED[4].load(Relaxed),
    ]
}

/// The stack of an indexing worker. Larger than the default because the parser recurses on nesting depth and a
/// worker must not be the place a deeply nested header first overflows.
const WORKER_STACK: usize = 16 * 1024 * 1024;

/// `work` applied to every item, on as many threads as the machine has cores — the answers **in the order of the
/// items**, so a caller that applies them in that order gets exactly what a loop would have.
///
/// A single item (or a single core) runs on the calling thread: a thread is not worth starting for one parse. The
/// workers take the next unfinished item from a shared counter rather than a fixed share each, because the items are
/// wildly unequal (a 3 KB header beside a 400 KB one) and a fixed split leaves cores idle behind the biggest. Their
/// stacks are larger than the default, because the parser recurses on nesting depth and a worker must not be the
/// place a deeply nested header first overflows.
fn parallel_map<T, R>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R>
where
    T: Sync,
    R: Send,
{
    let workers = std::thread::available_parallelism()
        .map_or(1, |cores| cores.get())
        .min(PARALLEL_WORKERS)
        .min(items.len());

    if workers <= 1 {
        return items.iter().map(work).collect();
    }

    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<R>>> =
        items.iter().map(|_| std::sync::Mutex::new(None)).collect();

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let worker = || {
                loop {
                    let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(item) = items.get(at) else {
                        break;
                    };
                    let made = work(item);
                    *slots[at]
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(made);
                }
            };

            // A thread that cannot be started leaves its share to the others: the counter is shared, so the work
            // still gets done — by the calling thread below, if by nobody else.
            let _ = std::thread::Builder::new()
                .name("cppls-index".to_string())
                .stack_size(WORKER_STACK)
                .spawn_scoped(scope, worker);
        }
    });

    slots
        .into_iter()
        .zip(items)
        .map(|(slot, item)| {
            slot.into_inner()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .unwrap_or_else(|| work(item))
        })
        .collect()
}

/// A file read, looked up and — when the disk had nothing — parsed, waiting to be put in the index.
///
/// Opaque on purpose: the only things to do with one are hand it to [`SummaryStore::commit`], or drop it.
pub struct Prepared(PreparedOutcome);

enum PreparedOutcome {
    /// The file could not be read.
    Unreadable,
    /// The disk had the answer.
    Stored(FileSummary),
    /// The disk did not; this was parsed, and `stored` says whether it was written down.
    Built { summary: FileSummary, stored: bool },
}

/// What the store did, so that a caller can see the cache working.
///
/// A counter rather than a log line, because the four numbers says to measure before
/// building more — build time, resident memory, cache hit rate, and the proportion of unanswerable queries —
/// all start here, and a number nobody can read is how a cache quietly stops hitting.
///
/// The first two are about **parses**, not about calls: `reused` is a file the disk answered for, `rebuilt` is a
/// file that had to be parsed, and a call that could not read the file at all is in neither. That is the
/// distinction a reader of these numbers will get wrong, and it is the one worth having: `reused` counts exactly
/// the parses the cache saved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreStats {
    /// Summaries read from disk.
    pub reused: usize,
    /// Summaries built because nothing usable was stored.
    pub rebuilt: usize,
    /// Summaries built and deliberately **not** written, because they say something about the filesystem that a
    /// key computed from the text cannot check. See the module documentation.
    pub unstored: usize,
}

impl StoreStats {
    /// The fraction of files the disk answered for, or `None` when no file could be read.
    ///
    /// `None` rather than `0.0`: "nothing has been looked up yet" and "every lookup missed" are different states,
    /// and the second is the one worth a warning.
    pub fn hit_rate(&self) -> Option<f64> {
        let total = self.reused + self.rebuilt;
        (total > 0).then(|| self.reused as f64 / total as f64)
    }

    /// What happened between two readings — the cost of one call rather than of the session.
    ///
    /// A caller that wants to report "this call parsed 12 files and reused 173" has to subtract, and subtracting
    /// in every caller is how one of them ends up reporting the session's numbers as the call's. Saturating
    /// because the counters only grow: a `since` given a later reading is a caller's mistake, and answering `0`
    /// is a better failure than a panic in an editor.
    pub fn since(self, earlier: StoreStats) -> StoreStats {
        StoreStats {
            reused: self.reused.saturating_sub(earlier.reused),
            rebuilt: self.rebuilt.saturating_sub(earlier.rebuilt),
            unstored: self.unstored.saturating_sub(earlier.unstored),
        }
    }
}

/// How much of a project one walk may index.
///
/// Two limits rather than one, because they bound different failures: a **file count** bounds the total work, and
/// a **depth** bounds the recursion along a chain that is long but narrow (a generated include ladder, a cycle
/// that only its guards break). Neither is a policy about what is worth analysing — that is the caller's — they are
/// what keeps a pathological project from making one call take the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncludeBudget {
    /// How many files one call may open, the entry included.
    pub max_files: usize,
    /// How far the include chain is followed from the entry, which is at depth `0`.
    pub max_depth: usize,
}

impl Default for IncludeBudget {
    /// Roomy enough that a real closure is never truncated, bounded enough to be a limit.
    ///
    /// The measured worst case is `<bits/stdc++.h>` at 359 files and a normal translation unit's closure at 185
    /// so 4096 is more than ten times the largest case anyone has measured — while a
    /// project whose include graph is that large is one a caller wants to hear about anyway, which is what
    /// [`IncludeIndex::not_indexed`] is for.
    fn default() -> Self {
        IncludeBudget {
            max_files: 4096,
            max_depth: crate::MAX_INCLUDE_DEPTH,
        }
    }
}

/// What indexing one file's includes did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IncludeIndex {
    /// Every file whose summary is now in the index, in the order the walk reached them.
    ///
    /// The entry is first, and a file appears once however many includes name it.
    pub indexed: Vec<PathBuf>,
    /// The `#include`s that resolved to nothing, so a caller can say *which* header is missing rather than that
    /// something is.
    pub unresolved: Vec<UnresolvedEdge>,
    /// Files the walk reached and did not open, with why.
    ///
    /// The field that keeps a truncated index from reading as a complete one: `indexed` says what the analysis
    /// has, and this says where it stopped. Both are needed — "there is nothing more" and "there may be more" are
    /// the two answers a consumer has to be able to tell apart.
    pub not_indexed: Vec<NotIndexed>,
    /// How many files were read a **second** time because their scopes depend on a macro body the first pass could
    /// not see — see `SummaryStore::re_read_what_a_body_changes`, which is private because the caller's question
    /// is `index_includes_from`.
    ///
    /// Reported rather than folded into `stats.rebuilt`, because it is the number that says whether this pass is
    /// cheap (33 files in MSVC's STL closure) or has run away (every file, which would mean the filter stopped
    /// filtering). The parses are counted in `stats.rebuilt` as well: they were spent.
    pub re_read: usize,
    /// What this call cost: parses the disk saved, parses that were spent, summaries deliberately not written.
    pub stats: StoreStats,
}

/// An `#include` that resolved to nothing, and where it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedEdge {
    /// The file that writes the directive.
    pub from: PathBuf,
    /// The name between the delimiters, as written.
    pub spelling: String,
    /// The directive's span, so a caller can point at the line.
    pub range: cpp_parser::SourceRange,
}

/// A file the walk reached but did not index, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotIndexed {
    pub path: PathBuf,
    pub reason: NotIndexedReason,
}

/// Why a reached file was not opened.
///
/// Three reasons because they have three different fixes: a budget is raised, a depth limit says the include
/// chain is longer than the walk follows, and unreadable says the filesystem changed under the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotIndexedReason {
    /// [`IncludeBudget::max_files`] ran out.
    Budget,
    /// The chain is deeper than [`IncludeBudget::max_depth`].
    Depth,
    /// The file could not be read: it resolved a moment ago, and it is not there now.
    Unreadable,
}

impl SummaryStore<DiskFiles> {
    /// A store over a project on disk.
    pub fn open(root: impl Into<PathBuf>, config: CompilerConfig) -> SummaryStore<DiskFiles> {
        SummaryStore::with_provider(root, config, DiskFiles)
    }
}

impl<F: FileProvider> SummaryStore<F> {
    /// A store over any provider, which is what makes the whole layer testable without a filesystem.
    ///
    /// By value, and a caller that wants to keep talking to the same provider clones it first: every provider in
    /// this crate is a handle, so the clone reads the same buffers and the same disk.
    pub fn with_provider(
        root: impl Into<PathBuf>,
        config: CompilerConfig,
        files: F,
    ) -> SummaryStore<F> {
        let root = root.into();

        SummaryStore {
            cache: root.join(crate::CACHE_DIRECTORY),
            root,
            config,
            files,
            index: ProjectIndex::new(),
            stats: StoreStats::default(),
            revision: std::sync::atomic::AtomicU64::new(0),
            dead_macros: std::sync::Mutex::new(std::collections::HashMap::new()),
            summary_database: std::sync::OnceLock::new(),
            generation: 0,
            include_searches: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Put the cache under a directory with another **name** (`index.cache_dir` in `.cppls.toml`).
    ///
    /// A name, not a path: the cache is the project's, it travels with the checkout, and a caller that wants it
    /// somewhere else entirely is asking for a different feature. An empty name is ignored, because "the cache
    /// directory is the empty string" is not a directory.
    pub fn with_cache_directory(mut self, name: &str) -> Self {
        if !name.is_empty() {
            self.cache = self.root.join(name);
        }
        self
    }
    /// Where the summaries are written, which is what a caller reporting on the cache wants to print.
    pub fn cache_directory(&self) -> &Path {
        &self.cache
    }

    /// **The summary database, opened on first use and used always** — see [`SummaryStore::summary_database`].
    fn summary_database(&self) -> Option<std::sync::Arc<crate::cache_db::SummaryCache>> {
        self.summary_database
            .get_or_init(|| crate::cache_db::shared(&self.cache, crate::READING_FINGERPRINT))
            .clone()
    }

    /// **Start a generation**, so a collection can tell this run's rows from the last one's.
    pub fn begin_generation(&mut self) {
        if let Some(database) = self.summary_database() {
            if let Ok(generation) = database.begin_generation() {
                self.generation = generation;
            }
        }
    }

    /// **A unit rendering the database already has**, by the closure key `Session::render_the_unit` computes.
    ///
    /// `None` is "not cached" — never an error: a record from another version decodes to nothing and the caller
    /// cooks the unit again, which is the answer for a cache that is not authoritative.
    pub fn cached_unit(&self, key: u64) -> Option<crate::RenderedUnit> {
        self.summary_database()?.unit(key).ok().flatten()
    }

    /// **A unit's facts**, by the hash of the rendering they were read from — see [`SummaryStore::cached_unit`].
    pub fn cached_indexed_unit(&self, program: u64) -> Option<crate::IndexedUnit> {
        self.summary_database()?.indexed_unit(program).ok().flatten()
    }

    /// Remember a unit's facts — see [`SummaryStore::cached_indexed_unit`].
    pub fn remember_indexed_unit(&self, program: u64, unit: &crate::IndexedUnit) {
        if let Some(database) = self.summary_database() {
            let _ = database.put_indexed_unit(program, unit);
        }
    }

    /// Remember a unit rendering — see [`SummaryStore::cached_unit`].
    pub fn remember_unit(&self, key: u64, unit: &crate::RenderedUnit) {
        if let Some(database) = self.summary_database() {
            let _ = database.put_unit(key, unit);
        }
    }

    /// **Collect the rows nothing references** — the idle step, never a query.
    ///
    /// A write transaction like any other, so it runs beside a reader and needs no exclusive handle. **Compaction
    /// is not here**, and the reason is worth keeping: it rewrites the file and takes `&mut`, while the handle this
    /// store holds comes from [`crate::cache_db::shared`] and is shared with every other store in the process. A
    /// collection is what stops the database growing with rows nothing references; compaction is a separate job for
    /// a moment when nobody else holds the handle.
    pub fn collect_the_cache(&mut self) -> Option<crate::cache_db::Collected> {
        self.summary_database().and_then(|database| database.collect())
    }

    /// Commit whatever the database is holding — the end of a slice.
    pub fn flush_summaries(&self) {
        if let Some(database) = self.summary_database() {
            database.flush();
        }
    }

    /// Rows and bytes of the summary database, when it is in use.
    pub fn summary_database_sizes(&self) -> Option<crate::cache_db::DatabaseSizes> {
        self.summary_database().and_then(|database| database.sizes())
    }

    pub fn stats(&self) -> StoreStats {
        self.stats
    }

    /// Tell the index what the **compilation** defines, so that the conditions stored in every summary can be
    /// evaluated: the compiler's predefined names (see `Toolchain::macros`) and the command line's `-D`s.
    ///
    /// Not part of the store's key, and that is the point: a summary records the *question* each `#if` asks, and
    /// the answer belongs to the compilation. The same summary is served to a session configured with `-DX` and to
    /// one that is not, and they get different answers to "was this code compiled" without either of them being
    /// re-indexed — which is what keeps `-DX` from invalidating a project's cache. See
    /// [`crate::index::environment`].
    pub fn with_macros(mut self, macros: crate::include::graph::Marked) -> Self {
        self.index = std::mem::take(&mut self.index).with_macros(macros);
        self
    }

    /// Every summary loaded so far — the project as the store currently understands it.
    pub fn index(&self) -> &ProjectIndex {
        &self.index
    }

    /// **What the file directs, without parsing it** — see [`FileIndexer::directive_summary`](crate::index::FileIndexer::directive_summary).
    ///
    /// `None` when the file cannot be read. The indexer is built the way every summary this store builds is: the
    /// configuration, the seed the index has collected, and nothing else — a directive reading has no use for macro
    /// bodies or for a scanned include list.
    pub fn directive_summary(&self, path: &Path) -> Option<crate::FileSummary> {
        let source = self.files.read(path)?;
        Some(
            crate::index::FileIndexer::new(&self.files, &self.config)
                .with_seed(self.index.macros())
                .directive_summary(path, &source),
        )
    }

    /// **Parse one file now, from its own text** — the *raw* summary, whatever the index happens to hold.
    ///
    /// Not the same question as [`ProjectIndex::summary`](crate::ProjectIndex::summary): the index holds what was
    /// read there and then, and once a file has been cooked it holds the **rendering's** reading (which is the right
    /// answer for "what does the compiler see" and the wrong one for "what does this file say"). This builds the
    /// raw reading from the text as it stands, with the same seed every other build in this store uses — which is
    /// what makes it comparable with [`SummaryStore::directive_summary`] field by field.
    ///
    /// Nothing is stored; a parse paid for by a caller that wants to look at it and nothing else.
    pub fn parsed_summary(&self, path: &Path) -> Option<crate::FileSummary> {
        let source = self.files.read(path)?;
        let key = crate::SummaryKey::new(crate::content_hash(&source), self.context_hash(path));
        Some(
            crate::index::FileIndexer::new(&self.files, &self.config)
                .with_seed(self.index.macros())
                .index(path, &source, key),
        )
    }

    /// The index, for a caller that **read a file and has something to add to it** — the cooked reading
    /// ([`crate::Session::cook`]).
    ///
    /// Narrow on purpose, and the store is still the owner: what a caller may do here is say what one file was
    /// read *as*, not replace the index or re-key the summaries. Everything the store derives from the summaries
    /// (the include graph, the visibility memo) is recomputed by the index's own `insert`, so a caller cannot get
    /// that half out of step by forgetting it.
    pub fn index_mut(&mut self) -> &mut ProjectIndex {
        &mut self.index
    }

    /// The configuration every summary in this store is keyed against.
    ///
    /// Exposed because a caller cannot always reconstruct it: the watcher asks which files would search a given
    /// directory ([`crate::index::watch`]), and only the configuration knows. Read-only, and it has to stay that
    /// way — changing it under a store would leave summaries keyed against a context that is no longer the one
    /// being asked about, which is the state [`crate::SummaryKey`] exists to make impossible.
    pub fn config(&self) -> &CompilerConfig {
        &self.config
    }

    /// The summary for `path` **as it is now**, from disk when there is one.
    ///
    /// `None` when the file cannot be read: a deleted file, a path that is not a file. That is not an error a cache
    /// should report — the caller asked about a file that is not there, and the answer is that it has nothing to
    /// say about it. A file that reads but does not parse is a different matter, and gets a summary like any other:
    /// the parser is tolerant, and a summary of a file with errors is still what a partially working editor needs.
    ///
    /// The lookup is a single `read` of the entry named by [`SummaryStore::context_hash`] and the text's hash, and
    /// it is done before anything is parsed. On a hit the file itself is read, and the stored summary is
    /// **re-checked** against the filesystem — see `resolution_still_holds` below — which is the one thing about a
    /// summary that its key cannot name. What a hit does not do is parse, search for a *new* include, or write.
    pub fn get(&mut self, path: &Path) -> Option<&FileSummary> {
        let prepared = self.prepare(path);
        self.commit(path, prepared)
    }

    /// **Everything [`SummaryStore::get`] does that does not change the store**: read the file, hash it, ask the
    /// disk, and — when the disk has nothing — parse it and write the answer down.
    ///
    /// Split out because it is the expensive half and it is a **pure function of the file**: a summary is made from
    /// the text, the configuration and the filesystem, with no reference to what the index already holds (the
    /// macro environment left the key for that reason — see `crate::cache`). So many files can be prepared at once,
    /// and [`SummaryStore::commit`] — the half that needs `&mut self` — applied to them in whatever order the caller
    /// wants the index to see them.
    pub fn prepare(&self, path: &Path) -> Prepared {
        // **And no early include scan**, which is a whole lex of the file that nothing here reads: the scan exists
        // so that a *wave* can put a file's includes on a shared frontier while this one is still being parsed
        // ([`SummaryStore::prepare_closure`]), and a caller preparing one file has no frontier to feed. The
        // includes come out of the parse either way — `summary.includes` is the same answer — so the only thing
        // this gives up is being told about them a moment sooner, and what it saves is measured per keystroke,
        // because a rebuilt summary is exactly what an edit produces. See `docs/status.md` §5.2.
        self.prepare_inner(path, None, None)
    }

    /// **Prepare a file the caller already has the macro environment for** — the one parse an edit needs.
    ///
    /// # What this is for
    ///
    /// A file's scopes can come out of a macro *body* written in another file: MSVC's `<vector>` writes
    /// `_STD_BEGIN` and `namespace std {` is in `yvals_core.h`, so a summary built from the file's own text alone
    /// files every one of those declarations at file scope. The second pass
    /// ([`SummaryStore::prepare_the_re_read`]) exists to repair that afterwards, and it re-parses the file to do
    /// it — which is a parse the *caller* can make unnecessary when it already holds the environment, because
    /// [`FileIndexer::with_macro_bodies`] builds the right summary the first time.
    ///
    /// `bodies` and `facts` are the **same value** passed twice; see
    /// [`FileIndexer::with_macro_body_readers`] for why the type has to be erased here.
    ///
    /// # What it does not change
    ///
    /// The disk lookup: a summary is keyed by the text and the configuration, and the environment deliberately
    /// left that key (see `crate::cache`), so an entry written by a caller *without* an environment is served to
    /// one *with* it. That is the existing rule for [`SummaryStore::get`] and this does not alter it — it means a
    /// warm cache is as cheap here as anywhere else, and a cold one is where the saving is.
    ///
    /// A caller that sets `Step::with_the_environment` from this is claiming the second pass has nothing to do for
    /// that file. It is the same claim the flag's documentation makes, and it is only true because the parse below
    /// was given the environment.
    pub fn prepare_with_the_environment(
        &self,
        path: &Path,
        bodies: &dyn cpp_parser::MacroBodies,
        facts: &dyn cpp_parser::MacroFacts,
    ) -> Prepared {
        self.prepare_inner(path, None, Some((bodies, facts)))
    }

    /// [`SummaryStore::prepare`] that says what the file includes **as soon as it knows** — before the parse.
    ///
    /// A file the disk has an answer for names its includes in that answer; a file that has to be parsed has its
    /// `#include` lines scanned first ([`FileIndexer::includes_of`]), which costs a fraction of the parse and is the
    /// same answer the parse will give. Either way `includes` is called once, with the resolved targets, before the
    /// expensive part starts — which is what lets [`SummaryStore::prepare_closure`] have other cores reading those
    /// files while this one is still busy with this one.
    fn prepare_telling(&self, path: &Path, includes: &mut dyn FnMut(&[PathBuf])) -> Prepared {
        self.prepare_inner(path, Some(includes), None)
    }

    /// The one body, with the early include notification and the macro environment **both optional** — see
    /// [`SummaryStore::prepare`] for what the callers want from the first and
    /// [`SummaryStore::prepare_with_the_environment`] for the second.
    fn prepare_inner(
        &self,
        path: &Path,
        mut includes: Option<&mut dyn FnMut(&[PathBuf])>,
        environment: Option<(&dyn cpp_parser::MacroBodies, &dyn cpp_parser::MacroFacts)>,
    ) -> Prepared {
        let tracing = crate::probes::cooking();
        let entered = std::time::Instant::now();
        let Some(source) = ({ self.files.read(path) }) else {
            return Prepared(PreparedOutcome::Unreadable);
        };
        let read = entered.elapsed();
        // **The key knows which environment the reading is under** — see [`SummaryKey::env_hash`]. Without it the
        // second pass would write over the first pass's entry and a later reader could get either one.
        let mut key = SummaryKey::new(content_hash(&source), self.context_hash(path));
        if let Some((_, facts)) = environment {
            key = key.with_environment(environment_hash(facts));
        }

        {
            let reading = std::time::Instant::now();
            // (the decode is the database's, so the two timers it fed are zero — see `note_a_prepared_file`)
            // **The database, and only the database.**
            let stored = self
                .summary_database()
                .and_then(|database| database.summary(key))
                .map_or(Err(()), Ok);
            if let Ok(stored) = stored
                && stored.key == key
            {
                // **The second half, timed apart from the decode**: re-running the include search for every
                // `#include` of a stored summary is what makes a hit trustworthy, and it is a filesystem search per
                // include — the one part of a warm start that looks like nothing in the code and can cost seconds.
                let verifying = std::time::Instant::now();
                let still_holds = self.resolution_still_holds(path, &stored);
                if tracing {
                    note_a_prepared_file(
                        read,
                        reading.elapsed(),
                        verifying.elapsed(),
                        std::time::Duration::ZERO,
                    );
                }
                if still_holds {
                    if let Some(tell) = includes.as_mut() {
                        let targets: Vec<PathBuf> = stored
                            .includes
                            .iter()
                            .filter_map(|include| include.resolved.clone())
                            .collect();
                        tell(&targets);
                    }
                    return Prepared(PreparedOutcome::Stored(stored));
                }
            } else if tracing {
                note_a_prepared_file(
                    read,
                        reading.elapsed(),
                    std::time::Duration::ZERO,
                    std::time::Duration::ZERO,
                );
            }
        }

        // The scan is what tells a *wave* that there is work to hand out; a caller with no wave does not pay it,
        // and its own parse resolves the same `#include` lines a moment later.
        let scanned = match includes.is_some() {
            true => {
                let scanned = {
                    FileIndexer::new(&self.files, &self.config)
                        .with_seed(self.index.macros())
                        .scan_includes(path, &source)
                };
                if let Some(tell) = includes.as_mut() {
                    tell(&scanned.targets());
                }
                Some(scanned)
            }
            false => None,
        };

        let indexer = FileIndexer::new(&self.files, &self.config)
            .with_seed(self.index.macros())
            // Without a scan the sweep finds the `#include`s itself. With one it is handed them, so that the
            // search is not run twice for the same file.
            .with_scanned_includes_opt(scanned.as_ref())
            // **The environment, when the caller has one** — the difference between a summary whose scopes are
            // right and one the second pass has to come back for. See
            // [`SummaryStore::prepare_with_the_environment`].
            .with_a_macro_environment(environment)
            // **Whatever the unit walk already decided about this file's `#define`s.** `prepare` is the path a file
            // takes when a query needs it *before* the unit's re-read reaches it — and a header made only of
            // `#define`s is exactly the file the re-read skips (`mentions_one_of`), so without this its summary
            // keeps every guarded `#define` and a consumer decides the regions again, with a different environment.
            // See [`SummaryStore::dead_macros`].
            .without_these_macros(&self.dead_macros_for(path));
        let summary = indexer.index(path, &source, key);

        // The one rule about the filesystem: a summary that records a *failed* search must not be stored, because
        // nothing in the key would notice the header appearing. See the module documentation.
        let stored = !has_unresolved_includes(&summary);
        if stored {
            // **The database, and only the database** — the per-directory `summaries/*.bin` files are gone with
            // `cache.redb` taking their place (see `docs/refactor-cache-and-startup.md` M1/M5).
            //
            // A failed write is not a failed lookup: the answer is in hand and in the index. Reporting it would turn
            // a read-only checkout — a perfectly ordinary way to work — into a broken editor.
            if let Some(database) = self.summary_database() {
                database.put(key, &summary);
                // **And what the summary belongs to.** A summary no `files` row references is what a collection
                // deletes, so the pair is written together (`SummaryCache` batches both into one transaction).
                database.note_file(path, key.content_hash, key.context_hash, 0, self.generation);
            }
        }

        if tracing {
            note_a_prepared_file(
                read,
                std::time::Duration::ZERO,
                std::time::Duration::ZERO,
                entered.elapsed(),
            );
        }

        Prepared(PreparedOutcome::Built { summary, stored })
    }

    /// Put what [`SummaryStore::prepare`] made into the index, and count it.
    ///
    /// `path` is the file it was prepared for. The answer is what [`SummaryStore::get`] answers: the summary, or
    /// `None` for a file that could not be read.
    pub fn commit(&mut self, path: &Path, prepared: Prepared) -> Option<&FileSummary> {
        match prepared.0 {
            PreparedOutcome::Unreadable => None,
            PreparedOutcome::Stored(stored) => {
                self.stats.reused += 1;
                // Filed under the path that asked, which is not necessarily the one recorded in the entry: the key
                // names the text, so two files with the same text share an entry. See `ProjectIndex::insert_at`.
                self.index.insert_at(path, stored);
                self.index.summary(path)
            }
            PreparedOutcome::Built { summary, stored } => {
                self.stats.rebuilt += 1;
                if !stored {
                    self.stats.unstored += 1;
                }

                self.index.insert(summary);
                self.index.summary(path)
            }
        }
    }

    /// **Prepare `roots` and — while they are being prepared — the files they include, and theirs**, on every core.
    ///
    /// # What this buys
    ///
    /// [`SummaryStore::prepare_many`] can only be as wide as the list it is given, and the list a session has is
    /// short at exactly the moment it matters: a project is one `.cpp` file whose closure is a hundred and fifty
    /// headers, and the headers are not on any list until the file naming them has been parsed. So a cold start reads
    /// the closure a level at a time, and a level is often three files wide.
    ///
    /// This does not wait for the parse. A worker that takes a file **scans its `#include` lines first** and puts the
    /// targets on the shared frontier, *then* parses — so the moment the first file has been lexed the whole graph
    /// below it is being walked by every other worker, breadth first, and the width of the work is the width of the
    /// include graph rather than of the level.
    ///
    /// # What it will not do
    ///
    /// * It reads only files `wanted` accepts — a session says "not one that is already in the index" — and only as
    ///   many as `budget` allows in total, so a call has a bounded cost whatever the closure is.
    /// * It changes nothing in the store: the answers are [`Prepared`] values, and what the caller does with them
    ///   (commit them, keep them for later, drop them) is its own decision. A file prepared and never committed
    ///   cost a parse and left a cache entry, which is where it would have been anyway.
    /// * It names no order. The caller commits in the order *it* wants the index to see files in.
    ///
    /// A panic in a worker is carried out of the call and raised on the calling thread, after every worker has
    /// stopped: a worker that died holding "one file in flight" would otherwise leave the others waiting for a
    /// result that is never coming.
    pub fn prepare_closure(
        &self,
        roots: &[PathBuf],
        wanted: impl Fn(&Path) -> bool + Sync,
        budget: usize,
    ) -> Vec<(PathBuf, Prepared)> {
        use std::sync::{Condvar, Mutex};

        let tracing = crate::probes::cooking();
        let before = prepared_stages();
        let started = std::time::Instant::now();

        struct Frontier {
            waiting: std::collections::VecDeque<PathBuf>,
            seen: HashSet<String>,
            in_flight: usize,
            done: Vec<(PathBuf, Prepared)>,
            panic: Option<Box<dyn std::any::Any + Send>>,
        }

        let mut frontier = Frontier {
            waiting: std::collections::VecDeque::new(),
            seen: HashSet::new(),
            in_flight: 0,
            done: Vec::new(),
            panic: None,
        };
        for root in roots {
            if frontier.seen.insert(normalize_path(root, cfg!(windows))) {
                frontier.waiting.push_back(root.clone());
            }
        }
        let budget = budget.max(frontier.waiting.len());

        let shared = (Mutex::new(frontier), Condvar::new());
        let lock = || {
            shared
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        };

        let work = || {
            loop {
                let path = {
                    let mut frontier = lock();
                    loop {
                        if frontier.panic.is_some() {
                            break None;
                        }
                        if let Some(path) = frontier.waiting.pop_front() {
                            frontier.in_flight += 1;
                            break Some(path);
                        }
                        if frontier.in_flight == 0 {
                            break None;
                        }
                        frontier = shared
                            .1
                            .wait(frontier)
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                    }
                };
                let Some(path) = path else {
                    shared.1.notify_all();
                    return;
                };

                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.prepare_telling(&path, &mut |targets| {
                        let mut frontier = lock();
                        let mut added = false;
                        for target in targets {
                            if frontier.seen.len() >= budget {
                                break;
                            }
                            if !wanted(target)
                                || !frontier.seen.insert(normalize_path(target, cfg!(windows)))
                            {
                                continue;
                            }
                            frontier.waiting.push_back(target.clone());
                            added = true;
                        }
                        drop(frontier);
                        if added {
                            shared.1.notify_all();
                        }
                    })
                }));

                let mut frontier = lock();
                frontier.in_flight -= 1;
                match outcome {
                    Ok(prepared) => frontier.done.push((path, prepared)),
                    Err(payload) => frontier.panic = Some(payload),
                }
                drop(frontier);
                shared.1.notify_all();
            }
        };

        let workers = std::thread::available_parallelism()
            .map_or(1, |cores| cores.get())
            .min(PARALLEL_WORKERS);

        if workers <= 1 {
            work();
        } else {
            std::thread::scope(|scope| {
                for _ in 0..workers {
                    let _ = std::thread::Builder::new()
                        .name("cppls-index".to_string())
                        .stack_size(WORKER_STACK)
                        .spawn_scoped(scope, work);
                }
                // The calling thread is a worker too — and the one that finishes the job if no thread could start.
                work();
            });
        }

        let mut frontier = shared
            .0
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(payload) = frontier.panic.take() {
            std::panic::resume_unwind(payload);
        }

        if tracing {
            let after = prepared_stages();
            let millis = |slot: usize| (after[slot] - before[slot]) as f64 / 1e6;
            let took = started.elapsed();
            // **Only the calls that are worth reading.** A wave of eight files is a few milliseconds; printing all
            // 232 of them buries the handful that take a hundred, which is where a warm start still goes.
            if took.as_millis() >= 40 {
                eprintln!(
                    "prepare: {} file(s) in {} ms | text {:.0} ms, decode {:.0} ms, verify {:.0} ms, build+encode \
                     {:.0} ms (summed over workers)",
                    after[0] - before[0],
                    took.as_millis(),
                    millis(1),
                    millis(2),
                    millis(3),
                    millis(4)
                );
            }
        }

        frontier.done
    }

    /// [`SummaryStore::prepare`] for many files at once, on as many threads as the machine has cores.
    ///
    /// The answers are in the order of `paths`, so a caller that commits them in that order gets exactly the index
    /// it would have got one file at a time. A single file (or a single core) is prepared on the calling thread:
    /// a thread is not worth starting for one parse.
    ///
    /// The workers take the next unprepared file from a shared counter rather than a fixed share each, because the
    /// files are wildly unequal (a 3 KB header beside a 400 KB one) and a fixed split leaves cores idle behind the
    /// biggest. Their stacks are larger than the default, because the parser recurses on nesting depth and a
    /// worker must not be the place a deeply nested header first overflows.
    pub fn prepare_many(&self, paths: &[PathBuf]) -> Vec<Prepared> {
        parallel_map(paths, |path| self.prepare(path))
    }

    /// Index `entry` **and everything it includes**, so that the declarations the file can see are in the index.
    ///
    /// The one call that makes the store's cache worth having on a real project. [`SummaryStore::get`] answers for
    /// one file and records where that file's `#include`s resolved; this follows those edges until it runs out of
    /// them, asking the cache at every step. That is what turns `#include <vector>` from a line the analysis
    /// cannot follow into 185 summarised files that the next session reads in milliseconds.
    ///
    /// # It follows the *summaries*, not the syntax
    ///
    /// The traversal needs no second parse and no separate graph walk, because a summary already records the path
    /// each of its includes resolved to — see [`IncludeFact::resolved`]. So the loop is: ask the store for a file,
    /// read the resolved targets out of the answer, repeat. Every level is a cache lookup, which is also why this
    /// is cheap on the second call: the files come back from disk and no parse happens at all.
    ///
    /// The alternative — parse the translation unit twice, once to build a graph and once to index — would pay the
    /// most expensive step in the crate twice for information the first pass already had.
    ///
    /// # What it does not do
    ///
    /// * **No macro environment.** Each file is indexed on its own, so a header's conditions are decided without
    ///   the `-D`s of the translation unit that includes it. That is the expensive layer
    ///   (P3) and keeps it separate on purpose: feeding the macro environment in means the key must name it, which
    ///   invalidates every stored summary in every project.
    /// * **No scheduling and no watching.** The caller says when. A file that changes afterwards is
    ///   [`SummaryStore::invalidate`]'s question, not this one's.
    /// * **No completeness claim.** [`IncludeIndex::not_indexed`] names every file the walk reached and did not
    ///   open, with why — a budget that truncated silently would make a partial index look like a whole one, which
    ///   is the failure `Known` exists to prevent one layer up.
    ///
    /// # The budget
    ///
    /// A file count, a depth, and both are reported rather than enforced quietly. The measured worst case is
    /// `<bits/stdc++.h>` at 359 files, so the default leaves room for a project far larger than that while still
    /// bounding what one call can cost; see [`IncludeBudget::default`].
    pub fn index_includes_from(&mut self, entry: &Path, budget: IncludeBudget) -> IncludeIndex {
        let before = self.stats;
        let mut outcome = IncludeIndex::default();

        // A file reached twice is one file: `#include <vector>` written in twenty headers is twenty edges and one
        // node, and the second visit would either re-ask the cache or re-parse. The root is also the only seed.
        let mut seen: HashSet<String> = HashSet::new();
        let mut pending: Vec<(PathBuf, usize)> = vec![(entry.to_path_buf(), 0)];

        while let Some((path, depth)) = pending.pop() {
            if !seen.insert(normalize_path(&path, cfg!(windows))) {
                continue;
            }

            if outcome.indexed.len() >= budget.max_files {
                outcome.not_indexed.push(NotIndexed {
                    path,
                    reason: NotIndexedReason::Budget,
                });
                continue;
            }

            if depth > budget.max_depth {
                outcome.not_indexed.push(NotIndexed {
                    path,
                    reason: NotIndexedReason::Depth,
                });
                continue;
            }

            // Unreadable is a real case and not an error: the target resolved a moment ago, and the walk is not
            // the thing that gets to complain about the file having moved since.
            let Some(summary) = self.get(&path) else {
                outcome.not_indexed.push(NotIndexed {
                    path,
                    reason: NotIndexedReason::Unreadable,
                });
                continue;
            };

            let includes: Vec<IncludeFact> = summary.includes.clone();
            outcome.indexed.push(path.clone());

            // Reversed onto the stack so that they come off it in the order the file writes them. A stack is the
            // whole reason this walk needs no recursion, but it turns "the includes, in order" into "the includes,
            // backwards" unless the push is reversed — and the order is worth keeping: it is the order a compiler
            // reads them in, and the order a caller showing what was indexed expects to see.
            for include in includes.into_iter().rev() {
                match include.resolved {
                    Some(target) => pending.push((target, depth + 1)),
                    None => outcome.unresolved.push(UnresolvedEdge {
                        from: path.clone(),
                        spelling: include.spelling,
                        range: include.range,
                    }),
                }
            }
        }

        // **The second pass**, and the reason it is here rather than inside the loop: a file whose scopes come out
        // of a macro body needs the files it includes to be indexed first, and the walk above cannot have that.
        // See [`SummaryStore::prepare_the_re_read`] for the filter and for what is deliberately not stored.
        let truncated = !outcome.not_indexed.is_empty();
        let indexed = outcome.indexed.clone();
        let plan = self.prepare_the_re_read(&indexed, &[], truncated);
        outcome.re_read = self.commit_the_re_read(plan);

        outcome.stats = self.stats.since(before);
        outcome
    }

    /// Re-read the files whose **reading depends on a macro body** the walk could not see the first time.
    ///
    /// # Why a second pass exists at all
    ///
    /// A file's scopes can depend on the replacement list of a macro it invokes, and that list is usually in a file
    /// the file *includes*: MSVC's `<vector>` writes `_STD_BEGIN` and `namespace std {` is in `yvals_core.h`, so
    /// every one of the 165 declarations that header contributes is scoped by another file's text. Building that
    /// environment needs the included file's summary, and the walk above reads a file **before** the files it
    /// includes — it cannot be otherwise, because a file's own `#include`s are a product of parsing it. So the
    /// first pass reads each file with whatever evidence the index had (usually none), and this pass re-reads the
    /// ones whose answer could have changed now that the whole closure is in hand.
    ///
    /// # Which files those are, and why that is a *sound* filter rather than a guess
    ///
    /// The candidate set is: a file whose text mentions a name the closure defines with a **structural** body
    /// (`namespace X {` or `}`, see [`cpp_parser::shape_of_a_body`]). The filter can only ever *add* work — a file
    /// that does not mention such a name cannot have a reading that depends on one, because the reading is asked at
    /// an invocation of that name — so a mention inside a comment or a string costs one re-parse and never a wrong
    /// summary. Measured on MSVC's STL: 33 of the 109 files in the closure of `<vector>`, `<string>` and `<map>`.
    ///
    /// # What is *not* written to disk
    ///
    /// A reading is only as good as the closure behind it, so a summary read under a **truncated** one is kept in
    /// memory and not stored: the walk's budget and depth limits are the caller's, they are not part of the key, and
    /// an entry written under a partial closure would be served to the next run as if the evidence had been
    /// complete. The same rule as a failed `#include`, for the same reason — see [`SummaryStore::get`] — and it is
    /// counted rather than silent.
    ///
    /// The second pass of [`SummaryStore::index_includes_from`], for a caller that indexes **file by file**.
    ///
    /// [`SummaryStore::get`] reads one file with whatever evidence the index has at that moment, and for a file
    /// whose scopes come out of a macro body the evidence is usually not there yet: MSVC's `<vector>` writes
    /// `_STD_BEGIN` and `namespace std {` is in `yvals_core.h`, which `<vector>` *includes* — and a file is read
    /// before the files it includes, because its own `#include`s are a product of parsing it. A caller that walks a
    /// closure one file at a time ([`crate::Session`]) therefore has to ask for this pass once the closure is in
    /// hand, or every reading it holds is the one from before the evidence arrived.
    ///
    /// Measured: without this, the driver answers **0/9** on MSVC's STL where the same index built by
    /// [`SummaryStore::index_includes_from`] answers 9/9 — `std::basic_string` is spelled `basic_string` at file
    /// scope when `_STD_BEGIN`'s body is not read, so the name the query asks about is not in the file.
    ///
    /// `files` are the ones this caller has just **parsed** — a summary read from the disk cache carries whatever
    /// reading it was stored with, and a file whose text did not change cannot have a different one. Returns how
    /// many were re-read.
    pub fn re_read_where_a_body_decides(
        &mut self,
        files: &[PathBuf],
        units: &[std::sync::Arc<crate::TranslationUnit>],
    ) -> usize {
        let plan = self.prepare_the_re_read(files, units, false);
        self.commit_the_re_read(plan)
    }

    /// **Put what [`SummaryStore::prepare_the_re_read`] worked out into the index.**
    ///
    /// The cheap half: one `ProjectIndex::insert` per file the pass re-read, and nothing read, parsed or walked. The
    /// pair exists because of what the write lock costs — measured on this project, a slice of four files held it for
    /// **572 ms**, and the whole of that was the pass above.
    ///
    /// # What it refuses to overwrite
    ///
    /// A reading is only applied when the file's summary **is still the one the pass re-read**: the key names the
    /// text, so a key that moved means something read that file again in between — a request's `catch_up`, which
    /// builds a summary from text this plan never saw — and the newer reading is the right one. The check costs a
    /// lookup and it is what makes the split safe rather than merely fast; the file is simply left for the next pass,
    /// which will re-read it from the text it now has.
    pub fn commit_the_re_read(&mut self, plan: ReRead) -> usize {
        let mut re_read = 0;
        for (rebuilt, stored) in plan.rebuilt {
            let current = self.index.summary(&rebuilt.path).map(|summary| summary.key);
            if current != Some(rebuilt.key) {
                continue;
            }
            self.commit_reread(rebuilt, stored);
            re_read += 1;
        }
        re_read
    }

    /// **The second pass, planned but not applied** — everything that does not change the store.
    ///
    /// This is [`SummaryStore::re_read_what_a_body_changes`] with the two writes taken out of it: the `#define`
    /// decisions it records (which stay here, because a summary built later in this same pass has to see them) and
    /// the index inserts (which are [`SummaryStore::commit_the_re_read`]'s). Everything else — reading the texts,
    /// building the environments, re-parsing the candidates, writing the summaries to disk — is a pure function of
    /// the store as it stands, which is what lets it run under the **read** lock a query already holds.
    pub fn prepare_the_re_read(
        &self,
        indexed: &[PathBuf],
        units: &[std::sync::Arc<crate::TranslationUnit>],
        truncated: bool,
    ) -> ReRead {
        // **The shape of this call, on `CPPLS_TRACE_COOKING=1`.** The pass has two forms — one rooted at a slice
        // with a timeline in hand, and an "eager" one for a caller with no timeline at all that copies every indexed
        // file's text and asks a question about every `#define` in the project — and from outside they are the same
        // call. Measured on a real project: four files took 315 ms with a timeline and 18 000 ms without, so the
        // first question about a slow start-up here is *which form ran*, and these counts answer it.
        let tracing = crate::probes::cooking();
        let started = std::time::Instant::now();
        // Every indexed file's text, read once: the walk slices each macro's body out of it, and a candidate is
        // re-parsed from it. One read per file per pass, rather than one per candidate include.
        //
        // **Every** indexed file, not only the candidates — and that is not laziness, it is what the two consumers
        // of this map need. Which names are bodied is a fact about the files that *define* macros, which need not
        // be the files being re-read; and the walk that builds one candidate's evidence reaches whatever that
        // candidate includes, handing out `&str` for each (see `summary::macros_from_the_closure_with_bodies`).
        // Restricting the map to the candidates is what left the driver at **0/9** on MSVC's STL while the same
        // index built by [`SummaryStore::index_includes_from`] answered 9/9.
        //
        // Keyed by the **normalized** spelling, which is the spelling the walk asks with: a resolved `#include` is
        // normalized (`c:/users/…`) while a summary filed by a session is keyed by the path the caller spelled
        // (`C:\Users\…`). The index looks its own paths up through the same normalization, so a map keyed by the raw
        // spelling answers `None` for a file that is right there — measured as `open.h` arriving with an empty text
        // and every macro body in it silently unscoped, which is exactly the bug this pass exists to prevent.
        //
        // **Every** file only for a caller that has no timelines. With units the pass reads the files it *re-parses*
        // and, once it knows which names they mention, the files that define one of those names — the two things it
        // slices text out of. On a project of a hundred thousand files the edit that parsed one of them used to read
        // **What the walk decided is remembered here**, once, before any summary is built — so that both paths that
        // build one (`prepare`, which has no unit in hand, and the re-read below, which does) agree about which
        // `#define`s a compiler would have read. See [`SummaryStore::dead_macros`].
        //
        // Recorded from `&self` through the map's own lock, and recorded **here** rather than at the commit below:
        // a summary built by anything at all in between — a request's `catch_up` is the one that happens — has to
        // see the same decision, and the position this loop has always had is what says so.
        if let Ok(mut dead) = self.dead_macros.lock() {
            for unit in units {
                for (path, offsets) in &unit.dead_macros {
                    dead.insert(path.clone(), offsets.clone());
                }
            }
        }

        // and copy all of them, on every drain.
        let mut sources: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let eager = units.is_empty();
        {
            if eager {
                for summary in self.index.summaries() {
                    self.read_into(&mut sources, &summary.path);
                }
            } else {
                for path in indexed {
                    self.read_into(&mut sources, path);
                }
            }
        }

        // **One unit, not a closure walk per file.** The words inside a macro body are decided by the environment
        // of the file that *defines* it, and that environment is a **position in the walk of the translation unit
        // that reads it** — the same walk for every file in the unit. Building it per file is what this pass used
        // to do, in both of its loops, and it was 7.4 s of a 9.3 s cold index (`stages`: `bodied-env` 4 040 ms +
        // `re-env` 3 363 ms). A unit is cached — on disk by the content of its whole closure, in memory by
        // `Session::translation_unit_of` — so the pass pays for one walk and then asks positions in it.
        let seed = crate::macros::MacroTable::from_marked(self.index.macros());
        // **Built once per timeline, not once per slice.** definitions() walks every event and allocates an
        // Arc per definition; the pass used to pay that again for every slice that reused the same unit —
        // measured at ~2.6 ms per unit, ~8 units per slice, 81 slices, **1.7 s of a 5.8 s pass**.
        let unit_definitions: Vec<std::sync::Arc<crate::UnitDefinitions>> = units
            .iter()
            .map(|unit| unit.definitions_cached())
            .collect();
        let environments: Vec<(
            &std::sync::Arc<crate::TranslationUnit>,
            std::sync::Arc<crate::UnitDefinitions>,
        )> = units.iter().zip(unit_definitions).collect();

        // **The names the files being re-read could possibly need** — the filter's own filter, and the one that
        // decides whether this pass costs anything on a warm start.
        //
        // The scan below asks a question about every `#define` in the project, and that question is expensive
        // whenever the plain shape does not answer it: it needs the *defining* file's closure environment, which is
        // a walk of that file's includes. Measured on the 138-file project — plain shapes 9.3 ms, environments
        // **4 127 ms** — the environments are this pass. But the answer is only ever *used* for names the files
        // being re-read mention (`mentions_one_of` below), and that test is a **whole-word** test on their text. So
        // a name that appears in none of their texts cannot change any decision, and the files that define it do
        // not have to be asked about at all.
        //
        // On a warm start this is the whole bill: one file was re-parsed and it mentions a few hundred words, so
        // only the handful of files defining those words are asked — 4.1 s becomes nothing. On a **cold** start
        // every file was parsed, so every one of their words is a candidate and the narrowing does nothing; that
        // case is honest work about a real question, and the unit reading (a later step) is what changes its shape:
        // there the environment is a *position in one timeline* rather than a closure walk per file.
        //
        // It is not an approximation: `bodied` only ever reaches `mentions_one_of(text, &bodied)`, and a name
        // dropped here is a name no parsed file's text contains.
        let mentioned = words_mentioned_by(&sources, indexed);
        let wanted = |name: &str| mentioned.contains(name);

        // **The files the scan below can be about at all**: those that define a word the re-read files mention. With
        // an index of names that is a lookup per word; without timelines it is every file, as it always was.
        let mut candidates: Vec<&FileSummary> = if eager {
            self.index.summaries().collect()
        } else {
            self.index
                .files_defining_any_macro(mentioned.iter().map(String::as_str))
        };
        let uncovered_candidates = candidates
            .iter()
            .filter(|summary| bodies_of(&environments, &seed, &summary.path).is_none())
            .count();

        let uncovered = uncovered_candidates > 0;
        if !eager {
            // **A file the timelines do not cover cannot be about the reading being repaired.** The units are the
            // re-read files' own closures, so a name whose only definers are outside them is not a macro the re-read
            // files can expand — its body cannot change their reading. Dropping those candidates therefore decides
            // nothing; what it removes is the fallback they used to trigger: a closure walk per candidate, in the
            // environment that has no timeline, measured at **17 747 ms for one drain of four files**.
            //
            // The candidates come from the *whole index* (`files_defining_any_macro`), which is why an ordinary
            // project has them: `EmmyLuaCodeStyle` mentions `MAX` and `MIN`, which a dozen unrelated headers define.
            // Measured there: five to forty-one candidates per drain, of which **none** was in the closure.
            candidates.retain(|summary| bodies_of(&environments, &seed, &summary.path).is_some());

            // Then their text, which is what the scan slices bodies out of — and, for a file being re-read that no
            // timeline covers, its own closure's text (the re-read falls back to walking it). This used to read
            // **every indexed file** the moment one candidate was uncovered: 1761 texts per drain, 40 MB of copying,
            // twelve times in one start-up. `read_into` skips what is already there, so a closure is paid once.
            for summary in &candidates {
                self.read_into(&mut sources, &summary.path);
            }
            for path in indexed {
                if units.iter().any(|unit| unit.environment_of(path).is_some()) {
                    continue;
                }
                self.read_into(&mut sources, path);
                for reached in self.includes_reached_from(path) {
                    self.read_into(&mut sources, &reached);
                }
            }
        }

        let uncovered = uncovered || uncovered_candidates > 0;

        // The names whose body a **reading** uses, from the **macro facts** of the indexed closure — not from its
        // text, and not from the environment: this is the question "could any file's reading have changed", and
        // the answer has to be knowable without building an environment per file. `a_reading_uses_this` is the
        // vocabulary's own answer, so a shape added to the reader becomes a name added here rather than a silent
        // hole — see `cpp_parser::BodyShape`.
        //
        // **Asked of the shape with the definer's own environment**, because a body can name a word that expands
        // to nothing and the shape is only readable through it: measured on MSVC 14.51's `yvals_core.h`,
        // `_STD_BEGIN` is `_EXTERN_CXX_WORKAROUND namespace std {` and `_EXTERN_CXX_WORKAROUND` is empty in the arm
        // in force. Read without that environment the shape is `Other` — a body whose first token is a word nobody
        // could resolve — and a name the reader would not use is a file this pass does not re-read.
        //
        // # What that costs, and the caches that bound it
        //
        // The environment is a **closure walk**, and a closure holds thousands of macros whose bodies mostly cannot
        // be placed by the plain reader — so asking per *macro* is a closure walk per *macro*. Measured, with the
        // environment built inside the inner loop and its `MacroDefinitions` created there too: indexing the 138
        // files of one real project took **362 s**, against **16.5 s** for the same index before this question was
        // asked. Three things bound it, and all three are the same shape — build the expensive thing once, or not
        // at all:
        //
        // * **one `MacroDefinitions` for the whole pass**: reading a definition out of a file's text does not depend
        //   on which body is asking, and this is the cache that exists to say so (it was being defeated by being
        //   created per macro);
        // * **one environment per file, built lazily**: the words inside a body are decided by the include order of
        //   the file that *defines* the macro, so the environment is a fact about that file and not about the body —
        //   and a file with no unplaceable body never builds one at all;
        // * **`mentioned` above**: a file that defines none of the words the files being re-read mention is not
        //   asked about, so its environment is never built (4 127 ms of a 4 186 ms pass, on a cold start's corpus,
        //   and all of it on a warm one).
        let mut bodied: Vec<String> = Vec::new();
        let mut is_bodied: std::collections::HashSet<&str> = std::collections::HashSet::new();
        // The cache the **timeline-less** caller's fallback reads definitions through — one parse per definition
        // rather than one per definition per file. Unused when the caller passes units, which is the session.
        let mut definitions = crate::summary::MacroDefinitions::default();

        // Scoped to **this loop** and stopped before the pass continues: what follows re-parses files through
        // `FileIndexer::index`, which is already timed as `Parse` + `Sweep`, and a timer that enclosed it would
        // count that work twice (measured: an enclosing version reported 106% of the wall clock).
        for summary in candidates.iter().copied() {
            let key = normalize_path(&summary.path, cfg!(windows));
            let Some(source) = sources.get(&key) else {
                continue;
            };

            // A file that defines none of the words the files being re-read mention can contribute nothing — see
            // `mentioned` above. Asked before the environment is built, which is the whole point.
            if !summary
                .macros
                .iter()
                .any(|fact| fact.kind.is_definition() && wanted(&fact.name))
            {
                continue;
            }

            let mut environment: Option<cpp_parser::MacroEnvironment> = None;
            let bodies = bodies_of(&environments, &seed, &summary.path);

            for fact in &summary.macros {
                if !fact.kind.is_definition() || is_bodied.contains(fact.name.as_str()) {
                    continue;
                }
                let Some(range) = fact.body_range else {
                    continue;
                };
                let Some(body) = source.get(range.start_offset..range.start_offset + range.length)
                else {
                    continue;
                };

                // The plain shape first — it is free (9.7 ms for every body in the project) and it settles most
                // bodies; an environment is only asked for the ones it cannot read.
                let plain = { cpp_parser::shape_of_a_body(body).a_reading_uses_this() };
                let used = if plain {
                    true
                } else {
                    match &bodies {
                        // **The unit's own timeline**: the body is read in the environment of the file that
                        // defines it, which is a position in the walk — no closure walked here at all.
                        Some(bodies) => {
                            cpp_parser::shape_of_a_body_at(body, bodies, range.start_offset)
                                .a_reading_uses_this()
                        }
                        // A caller with **no timeline at all** — a probe indexing a closure from an entry point, a
                        // test — keeps the reading this pass has always had: the defining file's own closure,
                        // walked once per file that needs it. Slower, and the same answer for a file that *is* its
                        // own unit root; the session passes units and never comes here.
                        None => {
                            let environment = environment.get_or_insert_with(|| {
                                let evidence = crate::summary::macros_from_the_closure_with_bodies(
                                    summary,
                                    |wanted| {
                                        let key = normalize_path(wanted, cfg!(windows));
                                        Some((
                                            self.index.summary(std::path::Path::new(&key))?,
                                            sources.get(&key)?.as_str(),
                                        ))
                                    },
                                    self.index.macros(),
                                    &mut definitions,
                                );

                                cpp_parser::MacroEnvironment::from_included_macros(evidence.macros)
                                    .with_bodies_in_force(evidence.conditional_bodies)
                            });

                            cpp_parser::shape_of_a_body_at(body, environment, range.start_offset)
                                .a_reading_uses_this()
                        }
                    }
                };

                if used {
                    is_bodied.insert(&fact.name);
                    bodied.push(fact.name.clone());
                }
            }
        }

        if bodied.is_empty() {
            if tracing {
                eprintln!(
                    "re-read: eager {eager}, {} source(s), {} word(s) mentioned, {} candidate file(s), \
                     uncovered {uncovered}, nothing bodied — {} ms",
                    sources.len(),
                    mentioned.len(),
                    candidates.len(),
                    started.elapsed().as_millis()
                );
            }
            return ReRead {
                rebuilt: Vec::new(),
            };
        }

        // **The names as a set, not as a list.** `mentions_one_of` walks every word of a file and asks whether it
        // is one of these; against a `Vec` that is a linear scan *per word* — tens of thousands of words per file,
        // over a list of hundreds — and it was the last unnamed third of a cold index: 9.45 s of wall clock with
        // 5.70 s of stages, and this line was the difference. A `HashSet` makes the same question one hash.
        let bodied_names: std::collections::HashSet<&str> =
            bodied.iter().map(String::as_str).collect();

        // The fallback's cache of parsed `#define`s — one parse per definition rather than one per definition per
        // file. Read only by a caller that has no timeline to hand this pass (see the `None` arm below).
        let mut definitions = crate::summary::MacroDefinitions::default();
        // **The re-reads themselves, waiting to be applied.** Everything below builds a summary; putting one in the
        // index is the one thing that is not a pure function of the store, and it is what
        // [`SummaryStore::commit_the_re_read`] is for — so the plan is a list here and a loop there.
        let mut rebuilt: Vec<(FileSummary, bool)> = Vec::new();

        // **Two kinds of re-read, and only one of them waits for the other.** A file a unit's timeline covers is
        // re-read through that timeline — a pure function of the file, the configuration and the walk the pass
        // already paid for — so those are done together, on every core. A file no unit covers falls back to walking
        // its own closure, which needs the pass's shared definition cache and the index as it stands; those are done
        // here, one at a time, exactly as they always were.
        let mut through_a_timeline: Vec<(&PathBuf, &String)> = Vec::new();

        for path in indexed {
            let Some(source) = sources.get(&normalize_path(path, cfg!(windows))) else {
                continue;
            };
            let mentioned = { mentions_one_of(source, &bodied_names) };
            // **And a file whose `#define`s the walk decided about, whether or not it mentions a bodied macro.**
            //
            // `mentions_one_of` is a filter about *macro bodies*: parsing a file that expands nothing cannot change
            // what a body reads as, so it is skipped. A file whose guarded `#define`s the walk judged — one that is
            // all conditionals and no declarations, which a compiler's own `vcruntime.h` is — mentions no body and
            // is exactly the file whose summary has to change: it was built during indexing, **before** the unit was
            // walked, with no decision to apply.
            //
            // Measured: `cppls-build: vcruntime.h built with 0 dead offset(s)` during indexing, and nothing built it
            // again afterwards — so its summary kept every guarded `#define` and a lookup answered `_STL_LANG = 0L`
            // for a file the compiler reads as C++20. See [`SummaryStore::dead_macros`].
            let decided_about = !self.dead_macros_for(path).is_empty();
            if !mentioned && !decided_about {
                continue;
            }

            if units.iter().any(|unit| unit.environment_of(path).is_some()) {
                through_a_timeline.push((path, source));
                continue;
            }

            let key = SummaryKey::new(content_hash(source), self.context_hash(path));
            let indexer =
                FileIndexer::new(&self.files, &self.config).with_seed(self.index.macros());
            // **A caller with no timeline** — a probe indexing a closure from an entry point, a test — keeps
            // the reading this pass has always had: the file's own closure, walked once for it. The session
            // passes units and never comes here; dropping this arm would silently unscope every declaration
            // behind a namespace-opening macro for those callers (`tests/scopes.rs` is one).
            let Some(environment) = self.closure_environment(path, &sources, &mut definitions)
            else {
                continue;
            };
            let made = indexer
                .with_macro_bodies(&environment)
                .index(path, source, key);

            let stored = !truncated && !has_unresolved_includes(&made);
            if stored {
                if let Some(database) = self.summary_database() {
                    database.put(made.key, &made);
                    database.note_file(&made.path, made.key.content_hash, made.key.context_hash, made.key.env_hash, self.generation);
                }
            }
            rebuilt.push((made, stored));
        }

        // **The unit's timeline again, and this is the half that used to walk a closure per file** (3 363 ms,
        // `re-env`, of a 5.46 s cold index). The file was read once before its includes were in the index and
        // is read here as the program reads it: `MacroView` is a *position* in the walk the pass already paid
        // for, and it answers the parser's questions — `kind_of`, `body_text_of`, the in-force bodies — out of
        // that walk rather than out of a map materialised for this file alone.
        let rebuilt_through_a_timeline = parallel_map(&through_a_timeline, |(path, source)| {
            let view = units.iter().find_map(|unit| unit.environment_of(path))?;

            let key = SummaryKey::new(content_hash(source), self.context_hash(path));
            // **And what the walk decided is not compiled.** The unit has already evaluated every guarded `#define`
            // against its own state — the same evaluation the cook uses — and a fact it skipped is one a compiler
            // would not have read. The summary is built from the raw reading and would keep it, which is how a
            // consumer comes to decide the same region a second time and answer differently. See
            // [`crate::TranslationUnit::dead_macros`].
            // **What the walk decided, by the spelling both sides agree on.** The unit keys its map by the path the
            // *include graph* spells (`c:/users/…`) and this holds the one the session was given (`C:\Users\…`).
            // See [`SummaryStore::dead_macros_for`].
            let dead = self.dead_macros_for(path);
            let rebuilt = FileIndexer::new(&self.files, &self.config)
                .with_seed(self.index.macros())
                .with_macro_bodies(&view)
                .without_these_macros(&dead)
                .index(path, source, key);

            let stored = !truncated && !has_unresolved_includes(&rebuilt);
            if stored {
                if let Some(database) = self.summary_database() {
                    database.put(rebuilt.key, &rebuilt);
                    database.note_file(&rebuilt.path, rebuilt.key.content_hash, rebuilt.key.context_hash, rebuilt.key.env_hash, self.generation);
                }
            }
            Some((rebuilt, stored))
        });

        rebuilt.extend(rebuilt_through_a_timeline.into_iter().flatten());

        if tracing {
            eprintln!(
                "re-read: eager {eager}, {} source(s), {} word(s) mentioned, {} candidate file(s), \
                 uncovered {uncovered}, {} bodied name(s) — {} ms",
                sources.len(),
                mentioned.len(),
                candidates.len(),
                bodied.len(),
                started.elapsed().as_millis()
            );
        }

        ReRead { rebuilt }
    }

    /// Count a re-read summary and put it in the index.
    fn commit_reread(&mut self, rebuilt: FileSummary, stored: bool) {
        self.stats.rebuilt += 1;
        if !stored {
            self.stats.unstored += 1;
        }
        self.index.insert(rebuilt);
    }

    /// **What the unit walk decided about this file's `#define`s**, for a caller building its summary without a unit
    /// in hand — see [`SummaryStore::dead_macros`].
    ///
    /// **Normalized on both sides**, and that is not tidiness: the walk keys its map by the path the *include graph*
    /// spells (`c:/users/…`), while a caller here holds the one the session was given (`C:\Users\…`). Compared raw,
    /// the two never match — measured as `vcruntime.h` being absent from the re-read log entirely, so its summary
    /// kept every guarded `#define` and a lookup answered `_STL_LANG = 0L`.
    fn dead_macros_for(&self, path: &Path) -> Vec<usize> {
        let wanted = normalize_path(path, cfg!(windows));
        let Ok(dead) = self.dead_macros.lock() else {
            return Vec::new();
        };
        dead.iter()
            .find(|(held, _)| normalize_path(held, cfg!(windows)) == wanted)
            .map(|(_, offsets)| offsets.clone())
            .unwrap_or_default()
    }

    /// **How many guarded `#define`s the unit walks have decided about** — `(files, offsets)`.
    ///
    /// The instrument behind the question the second pass's cost hinges on: a timeline walk records which
    /// `#define`s under a conditional a compiler would not have read ([`SummaryStore::dead_macros`]), and the only
    /// reason the pass cannot skip its walks is that recording. If a project has no such decisions, the recording is
    /// a no-op and the walk is paying for something it never produces — which is what this answers, rather than
    /// assumes.
    pub fn dead_macro_decisions(&self) -> (usize, usize) {
        let Ok(dead) = self.dead_macros.lock() else {
            return (0, 0);
        };
        (
            dead.len(),
            dead.values().map(|offsets| offsets.len()).sum(),
        )
    }

    fn read_into(&self, sources: &mut std::collections::HashMap<String, String>, path: &Path) {
        let key = normalize_path(path, cfg!(windows));
        if sources.contains_key(&key) {
            return;
        }
        if let Some(text) = self.files.read(path) {
            sources.insert(key, text);
        }
    }

    /// **Every file `root` includes, transitively** — the set whose text a closure walk from `root` can slice a body
    /// out of, and no more.
    ///
    /// The bounded form of "read the whole index", which is what this pass used to do the moment one candidate was
    /// not covered by a timeline: on a 1761-file project that is every source text copied **per drain** to answer a
    /// question about four files. A file outside the closure cannot be reached by the walk, so reading it was never
    /// an answer.
    fn includes_reached_from(&self, root: &Path) -> Vec<PathBuf> {
        let mut reached: Vec<PathBuf> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];

        while let Some(path) = pending.pop() {
            if !seen.insert(normalize_path(&path, cfg!(windows))) {
                continue;
            }
            let Some(summary) = self.index.summary(&path) else {
                continue;
            };
            for include in &summary.includes {
                if let Some(resolved) = &include.resolved {
                    reached.push(resolved.clone());
                    pending.push(resolved.clone());
                }
            }
        }

        reached
    }

    /// **One file's own closure, as an environment** — the reading this pass had before a session could hand it a
    /// unit's timeline.
    ///
    /// The fallback for a caller with no units at all: `SummaryStore::index_includes_from` walks a closure from an
    /// entry point and has no session behind it, and `FileMacros`/`MacroView` need a walk to be a position in. The
    /// cost is a closure walk per file that needs one, which is why the session passes units instead.
    fn closure_environment(
        &self,
        path: &Path,
        sources: &std::collections::HashMap<String, String>,
        definitions: &mut crate::summary::MacroDefinitions,
    ) -> Option<cpp_parser::MacroEnvironment> {
        let summary = self.index.summary(path)?;

        let evidence = crate::summary::macros_from_the_closure_with_bodies(
            summary,
            |wanted| {
                Some((
                    self.index.summary(wanted)?,
                    sources
                        .get(&normalize_path(wanted, cfg!(windows)))
                        .map(String::as_str)
                        .unwrap_or(""),
                ))
            },
            self.index.macros(),
            definitions,
        );

        Some(
            cpp_parser::MacroEnvironment::from_included_macros(evidence.macros)
                .with_bodies_in_force(evidence.conditional_bodies),
        )
    }

    /// Forget everything the store holds about `path`, because the file is gone.    ///
    /// The disk entry is **not** removed: it is keyed by the text, so if the file comes back with the text it had,
    /// the entry is exactly the summary it needs, and deleting it would throw away a hit for no reason. What this
    /// drops is the in-memory summary, so that a query stops finding declarations in a file that no longer exists.
    ///
    /// Returns whether there was anything to forget.
    /// **How many times a summary has been dropped or invalidated** — the clock `index::walk_cache` keys its
    /// contributions by.
    ///
    /// A contribution depends on the contents of every file in its subtree, and there is no cheap way to hash that
    /// whole set. What is cheap is a counter that only moves when a summary is dropped: one edit anywhere retires every
    /// contribution, which is conservative in the only safe direction, and ordinary commits do not move it — so a load
    /// keeps its contributions from one file to the next, which is the whole point.
    pub fn revision(&self) -> u64 {
        self.revision.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn a_summary_was_dropped(&self) {
        self.revision
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::index::walk_cache::clear();
    }

    pub fn forget(&mut self, path: &Path) -> bool {
        self.a_summary_was_dropped();
        self.index.forget(path)
    }

    /// Does a stored summary still describe what the filesystem says *now*?
    ///
    /// The one question its key cannot answer. Everything else a summary depends on is in the key — the text, the
    /// configuration, the directory — but `#include "x.h"` resolving to `/p/x.h` is a fact about **which candidate
    /// paths exist**, and a key that had to name that could not be computed from the text, which is what the whole
    /// lookup-before-parse design rests on.
    ///
    /// So a stored summary is a *candidate*, and this is the check that makes it an answer: every include is
    /// resolved again, and the summary is used only if each one resolves exactly where it did before. The
    /// alternative — trusting the entry and letting the watcher repair things — would make correctness depend on
    /// never missing a filesystem event, and a watcher can always miss one: a queue overflows, a network filesystem
    /// reports nothing, an editor writes in a way nobody anticipated. This way the watcher is an optimisation (it
    /// refreshes things *promptly*) rather than a load-bearing part of the design.
    ///
    /// The cost is a handful of `exists` calls per include on a hit, against a parse. `examples/measure.rs` is
    /// where that trade is measured rather than assumed.
    fn resolution_still_holds(&self, path: &Path, summary: &FileSummary) -> bool {
        // A summary with no includes has nothing that could have moved, which is the common case for a `.cpp` and
        // costs nothing to recognise.
        if summary.includes.is_empty() {
            return true;
        }

        let directory = path.parent().unwrap_or(Path::new("."));

        summary
            .includes
            .iter()
            .all(|fact| self.where_it_resolves_now(fact, directory) == fact.resolved)
    }

    /// **Where an `#include` resolves *now*** — the search, remembered for the life of the store.
    ///
    /// # Why it is remembered, measured
    ///
    /// [`SummaryStore::resolution_still_holds`] re-runs the search for every include of every stored summary, because
    /// a summary records where an include resolved and nothing in its key would notice a header appearing earlier on
    /// the path. That is the right rule, and it is what makes a warm start almost entirely this: on
    /// `EmmyLuaCodeStyle` a warm start's `prepare_a_wave` was **9.3 s**, of which the decode of 1714 stored summaries
    /// was **0.4 s** and these searches were the rest (`CPPLS_TRACE_COOKING=1` prints the split per wave). Each
    /// search walks the include paths with a filesystem probe per candidate.
    ///
    /// # Why it may be remembered
    ///
    /// Because the answer is a function of the **question** — the spelling, the form, and (for a quoted include) the
    /// directory it is asked from — and not of the file that asks it. `#include <vector>` is one search asked by four
    /// hundred headers; `#include "config.h"` is one search per directory. The store is built once per session and
    /// its index already answers from summaries read at one moment, so remembering the searches of that moment cannot
    /// be less current than what is already in memory — and it is *more* consistent than the loop it replaces, where
    /// two files verified a millisecond apart could disagree if a header appeared between them.
    fn where_it_resolves_now(&self, fact: &IncludeFact, directory: &Path) -> Option<PathBuf> {
        // An angled include's search does not look at the including file's directory, so it is not part of the
        // question — and leaving it out is what turns four hundred `<vector>`s into one search.
        let key = (
            fact.form,
            fact.is_next,
            Box::from(fact.spelling.as_str()),
            match fact.form {
                IncludeForm::Angle => PathBuf::new(),
                IncludeForm::Quote | IncludeForm::Macro => directory.to_path_buf(),
            },
        );

        if let Ok(searches) = self.include_searches.lock()
            && let Some(known) = searches.get(&key)
        {
            return known.clone();
        }

        let resolver = crate::include::IncludeResolver::new(&self.files, &self.config);
        let mut interner = crate::file::paths::PathInterner::new(cfg!(windows));
        let resolved = resolver
            .resolve(&fact.as_include(), directory, None, &mut interner)
            .resolved()
            .map(|found| found.path.clone());

        if let Ok(mut searches) = self.include_searches.lock() {
            if searches.len() >= MAX_INCLUDE_SEARCHES {
                searches.clear();
            }
            searches.insert(key, resolved.clone());
        }

        resolved
    }

    /// The work set when `path` changes: the file, and everything that transitively includes it.
    ///
    /// The reverse include edges are the whole point of having them. A change to a header can change the meaning
    /// of every file below it, and a change to a `.cpp` changes nothing else — because nothing includes it — so
    /// the two cases differ by a graph walk rather than by a policy.
    ///
    /// The order is the walk's, outermost last, which is the order a caller should rebuild in if it wants the
    /// files that depend on others to be rebuilt after them — though the key makes the order irrelevant to
    /// *correctness*, since each file is judged against its own contents.
    ///
    /// Note what this does **not** say: that the returned files need rebuilding. Their keys are computed from
    /// their own text, so a file whose text did not change is a cache hit, and asking is cheaper than deciding.
    /// That is also why the *watcher* ([`crate::index::watch`]) does not call this for an ordinary edit: a file's
    /// summary does not depend on the contents of what it includes, so an edited header is one file to re-read,
    /// not fifty. What does invalidate a summary without its text changing is a file **appearing or
    /// disappearing**, because a summary records where its includes *resolved* — and that is what this walk is
    /// for, since the files that resolved to the one that moved are exactly its includers.
    pub fn invalidate(&self, path: &Path) -> Vec<PathBuf> {
        self.a_summary_was_dropped();
        let mut work = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut pending = vec![path.to_path_buf()];

        while let Some(current) = pending.pop() {
            let key = normalize_path(&current, cfg!(windows));
            if !visited.insert(key) {
                continue;
            }

            work.push(current.clone());

            for includer in self.index.includers_of(&current) {
                pending.push(includer);
            }
        }

        work
    }

    /// The compilation context of `path`, as the cache keys on it: the configuration **and** the directory.
    ///
    /// Every field, in order, spelled out rather than hashed through a derived implementation — a new field on
    /// [`CompilerConfig`] has to be added here deliberately. A configuration field that is missed makes two
    /// different configurations share a key, which is a wrong summary rather than a cache miss, and a derived
    /// hash would make that the default outcome of forgetting.
    ///
    /// The directory is here for the same reason the include paths are: it is where the compiler *starts looking*.
    /// `#include "widget.h"` in `/p/a.cpp` and in `/p/sub/b.cpp` is the same text describing two different
    /// compilations, and leaving the directory out let the two share one entry — so the second file was handed the
    /// first one's resolved includes, and a jump into a header it does not include. See [`SummaryKey`].
    ///
    /// The include paths are hashed **as the resolver uses them** — resolved against the working directory — so
    /// the working directory is accounted for exactly to the extent that it changes what is found, and a caller
    /// that runs the compiler from somewhere else without changing any `-I` keeps the cache.
    ///
    /// The standard, the target, `-D` and `-U` are here although nothing in a summary depends on them yet: they
    /// are the rest of what the compile database says a file was compiled with, they cost one hash of a few
    /// strings, and the first feature that reads them — a parser that is told `-std=c++20`, a condition evaluated
    /// against a command-line macro — must not also have to remember to come back here.
    pub fn context_hash(&self, path: &Path) -> u64 {
        let mut bytes = Vec::new();

        for include_path in &self.config.include_paths {
            let directory = self
                .config
                .resolve_against_working_directory(&include_path.directory);
            bytes.extend_from_slice(normalize_path(&directory, cfg!(windows)).as_bytes());
            bytes.push(u8::from(include_path.is_system));
            bytes.push(0);
        }
        bytes.push(b'|');

        for define in &self.config.defines {
            bytes.extend_from_slice(define.name.as_bytes());
            bytes.push(b'=');
            if let Some(value) = &define.value {
                bytes.extend_from_slice(value.as_bytes());
            }
            bytes.push(0);
        }
        bytes.push(b'|');

        for undefine in &self.config.undefines {
            bytes.extend_from_slice(undefine.as_bytes());
            bytes.push(0);
        }
        bytes.push(b'|');

        for text in [&self.config.standard, &self.config.target] {
            if let Some(text) = text {
                bytes.extend_from_slice(text.as_bytes());
            }
            bytes.push(0);
        }
        bytes.push(b'|');

        // **Which compiler** the configuration is for, because it changes the *reading*: `__int128` is a type to
        // g++ and a name to cl.exe, so the same text yields two different summaries and a cache that confused them
        // would serve one target's facts to the other. See `CompilerConfig::dialect`.
        bytes.push(match self.config.dialect() {
            Dialect::Gnu => b'g',
            Dialect::Msvc => b'm',
        });
        bytes.push(0);

        // **Whether an include nothing could place may be looked for in the tree**, because it changes which file a
        // summary is *about*: the same text resolves `#include "Session/IOSession.h"` to a header under the root with
        // this on and to nothing with it off, and a cache keyed without it would serve one project's facts to the
        // other. One byte, and it is the difference between an answer and a wrong one.
        bytes.push(u8::from(self.config.fuzzy_includes));
        bytes.push(0);

        // **And whether the macro environment is the whole of what the compilation defines**, which is the same
        // kind of fact as the dialect: it changes the *reading* rather than the configuration. A summary built
        // without the claim cannot put a macro body behind a conditional in force — `_STD_BEGIN`'s
        // `namespace std {` is written under `#if _STL_COMPILER_PREPROCESSOR` in `yvals_core.h` — so every
        // declaration MSVC's headers contribute is filed at file scope, and with the claim made the same text under
        // the same flags gives the scoped reading. Measured on one workspace, one file, one configuration:
        // `<xstring>` came out with 129 facts, 38 of them scoped, and 1631 facts, 524 of them scoped. Two readings
        // that far apart must not share a key, and the flag is not in `CompilerConfig` because it is not a flag the
        // project was compiled with — it is what the analysis was able to find out. See
        // [`crate::index::environment::compilation_environment`].
        bytes.push(b'c');
        bytes.push(u8::from(self.index.macros().is_incomplete()));
        bytes.push(0);

        bytes.push(b'|');
        bytes.extend_from_slice(
            normalize_path(path.parent().unwrap_or(Path::new(".")), cfg!(windows)).as_bytes(),
        );

        fnv1a64(&bytes)
    }
}

/// **The macro bodies the shape reader asks about, out of the unit that reads this file.**
///
/// The one place the pass's environments are built, for both of its loops: a file has one environment — the
/// position its frame occupies in the timeline of the unit that reads it — and asking twice would be asking the
/// same question twice.
///
/// `None` when no unit in `environments` reads the file, which the caller answers with the plain shape: a file
/// outside every unit is one nobody compiled, and there is no environment to read its bodies in.
fn bodies_of<'pass>(
    environments: &'pass [(
        &std::sync::Arc<crate::TranslationUnit>,
        std::sync::Arc<crate::UnitDefinitions>,
    )],
    seed: &'pass crate::macros::MacroTable,
    path: &Path,
) -> Option<crate::preprocess::cooked::FileMacros<'pass>> {
    environments.iter().find_map(|(unit, definitions)| {
        let view = unit.environment_of(path)?;
        Some(crate::preprocess::cooked::FileMacros::new(
            view,
            definitions,
            Some(seed),
            true,
        ))
    })
}

/// Does this text mention any of `names` as a whole word?
///
/// The filter [`SummaryStore::re_read_what_a_body_changes`] uses, and it is deliberately a **text** scan rather
/// than a parse: a parse is the thing the filter exists to avoid, and the question it answers ("could this file's
/// reading change?") only ever needs a sound over-approximation. A name inside a comment or a string literal counts
/// as a mention, which costs one re-parse of a file whose summary comes out identical; a name that is *not* in the
/// text cannot be invoked, so no file that could change is skipped.
///
/// `names` is a **set** and not a slice: this runs once per word of every file being re-read, and asked against a
/// list it is a linear scan per word (see the call site — that was 3.5 s of a 9.4 s index).
fn mentions_one_of(text: &str, names: &std::collections::HashSet<&str>) -> bool {
    words_of(text).any(|word| names.contains(word))
}

/// The words of a text, by the rule [`mentions_one_of`] compares with — **one rule, two readers**, because the
/// scan that decides which macro bodies to ask about ([`SummaryStore::re_read_what_a_body_changes`]) narrows itself
/// by "which words do the files being re-read mention", and a second, slightly different notion of a word there
/// would be a filter that silently skips a file this one would have re-read.
fn words_of(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !(character.is_alphanumeric() || character == '_'))
        .filter(|word| !word.is_empty())
}

/// Every whole word the files in `files` mention, as a set — the narrowing [`SummaryStore::re_read_what_a_body_changes`]
/// applies, and a **superset** of the names `mentions_one_of` could match in any of them.
fn words_mentioned_by(
    sources: &std::collections::HashMap<String, String>,
    files: &[PathBuf],
) -> std::collections::HashSet<String> {
    let mut mentioned: std::collections::HashSet<String> = std::collections::HashSet::new();

    for path in files {
        let Some(text) = sources.get(&normalize_path(path, cfg!(windows))) else {
            continue;
        };
        for word in words_of(text) {
            if !mentioned.contains(word) {
                mentioned.insert(word.to_string());
            }
        }
    }

    mentioned
}

/// Does this summary write an `#include` the compiler **certainly looks for** whose target was never found?
///
/// The one question that has to be asked before a summary is stored, because the answer is a fact about the
/// filesystem and the key cannot see it. See the module documentation.
///
/// # Only the unconditional ones, and the measurement that says why
///
/// An `#include` written inside a conditional is one the compilation **may not take**, and refusing to write a whole
/// summary because such a target is missing is refusing it for a line the compiler never reads. MSVC's own
/// `xmmintrin.h` is that case, and it is not exotic — `<intrin.h>` reaches it, and much of the Windows SDK includes
/// that:
///
/// ```c
/// #ifdef __ICL                  /* Intel's compiler, and only it */
/// #ifdef _MM_FUNCTIONALITY
/// #include "xmm_func.h"         /* absent from the whole MSVC tree, and never looked for */
/// #endif
/// ```
///
/// Measured before this rule was narrowed: a 126-file standard-library closure reported **1 file not stored**, and
/// `E:\EmmyLuaCodeStyle` reported **235 of 1729** — every file that reaches that header, re-parsed on every run, for
/// a line no compiler reads.
///
/// # Why narrowing does not weaken the guarantee
///
/// The hazard is "the file appears and the key does not notice", and it is covered on the **read** side by
/// [`SummaryStore::resolution_still_holds`], which re-runs the search for *every* include — conditional or not — and
/// refuses an entry whose answer has changed. A stored `None` that has become a `Some` is exactly that mismatch, so
/// the entry is dropped the moment the header appears, whether or not this rule was willing to write it.
///
/// `FactGuard::Unconditional` is the guard of an include with no `#if` around it **and of one inside nothing but the
/// file's own include guard**, which [`crate::index`] clears on purpose (`deguard_the_files_own_guard`): a guarded
/// header's body is read by every inclusion that does anything. So a missing unconditional include — the case this
/// rule has always been about, and the case its tests write — is still refused, and only a genuinely conditional one
/// is let through.
fn has_unresolved_includes(summary: &FileSummary) -> bool {
    summary.includes.iter().any(|include| {
        include.resolved.is_none()
            && matches!(include.guard, crate::summary::FactGuard::Unconditional)
    })
}

#[cfg(test)]
mod tests;
/// **What a reading was taken under**, as one number — the `(name, body)` pairs in force, sorted and hashed.
///
/// Sorted first, so two environments that differ only in the order a walk met the definitions hash the same: the
/// whole point of the number is that a header included by two roots shares one summary when it sees the same macros,
/// and only then. See [`SummaryKey::env_hash`].
fn environment_hash(facts: &dyn cpp_parser::MacroFacts) -> u64 {
    let mut pairs: Vec<(String, String)> = Vec::new();
    facts.for_each_body_in_force(&mut |body| {
        pairs.push((body.name.to_string(), body.body.to_string()));
    });
    pairs.sort();
    let mut bytes: Vec<u8> = Vec::new();
    for (name, body) in &pairs {
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(body.as_bytes());
        bytes.push(0);
    }
    crate::cache::fnv1a64(&bytes)
}