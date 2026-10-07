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

use crate::cache::{SummaryKey, content_hash, fnv1a64};
use crate::include::config::CompilerConfig;
use crate::file::paths::{DiskFiles, FileProvider, normalize_path};
use crate::index::project::ProjectIndex;
use crate::index::{FileIndexer, read_summary, write_summary};
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
}

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
    let slots: Vec<std::sync::Mutex<Option<R>>> = items.iter().map(|_| std::sync::Mutex::new(None)).collect();

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let worker = || {
                loop {
                    let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(item) = items.get(at) else {
                        break;
                    };
                    let made = work(item);
                    *slots[at].lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(made);
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
            dead_macros: std::sync::Mutex::new(std::collections::HashMap::new()),
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
        self.prepare_telling(path, &mut |_| {})
    }

    /// [`SummaryStore::prepare`] that says what the file includes **as soon as it knows** — before the parse.
    ///
    /// A file the disk has an answer for names its includes in that answer; a file that has to be parsed has its
    /// `#include` lines scanned first ([`FileIndexer::includes_of`]), which costs a fraction of the parse and is the
    /// same answer the parse will give. Either way `includes` is called once, with the resolved targets, before the
    /// expensive part starts — which is what lets [`SummaryStore::prepare_closure`] have other cores reading those
    /// files while this one is still busy with this one.
    fn prepare_telling(&self, path: &Path, includes: &mut dyn FnMut(&[PathBuf])) -> Prepared {
        let Some(source) = ({
            let _read = crate::stages::StageTimer::new(crate::stages::Stage::Read);
            self.files.read(path)
        }) else {
            return Prepared(PreparedOutcome::Unreadable);
        };
        let key = {
            let _hash = crate::stages::StageTimer::new(crate::stages::Stage::Hash);
            SummaryKey::new(content_hash(&source), self.context_hash(path))
        };

        {
            let _lookup = crate::stages::StageTimer::new(crate::stages::Stage::Lookup);
            if let Ok(stored) = read_summary(&key.path_under(&self.cache))
                && stored.key == key
                && self.resolution_still_holds(path, &stored)
            {
                let targets: Vec<PathBuf> = stored
                    .includes
                    .iter()
                    .filter_map(|include| include.resolved.clone())
                    .collect();
                includes(&targets);
                return Prepared(PreparedOutcome::Stored(stored));
            }
        }

        let scanned = {
            let _scan = crate::stages::StageTimer::new(crate::stages::Stage::IncludeScan);
            let scanned = FileIndexer::new(&self.files, &self.config)
            .with_seed(self.index.macros())
            .scan_includes(path, &source);
            includes(&scanned.targets());
            scanned
        };
        let summary = FileIndexer::new(&self.files, &self.config)
            .with_seed(self.index.macros())
            .with_scanned_includes(&scanned)
            // **Whatever the unit walk already decided about this file's `#define`s.** `prepare` is the path a file
            // takes when a query needs it *before* the unit's re-read reaches it — and a header made only of
            // `#define`s is exactly the file the re-read skips (`mentions_one_of`), so without this its summary
            // keeps every guarded `#define` and a consumer decides the regions again, with a different environment.
            // See [`SummaryStore::dead_macros`].
            .without_these_macros(&self.dead_macros_for(path))
            .index(path, &source, key);

        // The one rule about the filesystem: a summary that records a *failed* search must not be stored, because
        // nothing in the key would notice the header appearing. See the module documentation.
        let stored = !has_unresolved_includes(&summary);
        if stored {
            let _encode = crate::stages::StageTimer::new(crate::stages::Stage::Encode);
            // A failed write is not a failed lookup: the answer is in hand and in the index. Reporting it would
            // turn a read-only checkout — a perfectly ordinary way to work — into a broken editor.
            let _ = write_summary(&summary, &self.cache);
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

                let _insert = crate::stages::StageTimer::new(crate::stages::Stage::IndexInsert);
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
        let lock = || shared.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

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
                        frontier = shared.1.wait(frontier).unwrap_or_else(|poisoned| poisoned.into_inner());
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
                            if !wanted(target) || !frontier.seen.insert(normalize_path(target, cfg!(windows))) {
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

        let mut frontier = shared.0.into_inner().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(payload) = frontier.panic.take() {
            std::panic::resume_unwind(payload);
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
        let mut sources: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let eager = units.is_empty();
        {
            let _read = crate::stages::StageTimer::new(crate::stages::Stage::Read);
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
        let unit_definitions: Vec<crate::UnitDefinitions> =
            units.iter().map(|unit| unit.definitions()).collect();
        let environments: Vec<(&std::sync::Arc<crate::TranslationUnit>, crate::UnitDefinitions)> =
            units.iter().zip(unit_definitions).collect();

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
        let candidates: Vec<&FileSummary> = if eager {
            self.index.summaries().collect()
        } else {
            self.index
                .files_defining_any_macro(mentioned.iter().map(String::as_str))
        };

        if !eager {
            let _read = crate::stages::StageTimer::new(crate::stages::Stage::Read);

            // A file no unit covers has no timeline to be read through, so its reading falls back to walking its own
            // closure — and that walk asks for the text of every file in it. Rare (a project file outside every
            // unit), and the answer is the old one: read everything, once.
            let uncovered = candidates
                .iter()
                .any(|summary| bodies_of(&environments, &seed, &summary.path).is_none())
                || indexed
                    .iter()
                    .any(|path| !units.iter().any(|unit| unit.environment_of(path).is_some()));

            if uncovered {
                for summary in self.index.summaries() {
                    self.read_into(&mut sources, &summary.path);
                }
            } else {
                for summary in &candidates {
                    self.read_into(&mut sources, &summary.path);
                }
            }
        }

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
        let scan = crate::stages::StageTimer::new(crate::stages::Stage::BodiedScan);
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
                let plain = {
                    let _plain = crate::stages::StageTimer::new(crate::stages::Stage::BodiedPlain);
                    cpp_parser::shape_of_a_body(body).a_reading_uses_this()
                };
                let used = if plain {
                    true
                } else {
                    let _inconclusive = crate::stages::StageTimer::new(crate::stages::Stage::BodiedEnv);
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
        scan.stop();

        if bodied.is_empty() {
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
            let mentioned = {
                let _filter = crate::stages::StageTimer::new(crate::stages::Stage::ReFilter);
                mentions_one_of(source, &bodied_names)
            };
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
            let indexer = FileIndexer::new(&self.files, &self.config).with_seed(self.index.macros());
            // **A caller with no timeline** — a probe indexing a closure from an entry point, a test — keeps
            // the reading this pass has always had: the file's own closure, walked once for it. The session
            // passes units and never comes here; dropping this arm would silently unscope every declaration
            // behind a namespace-opening macro for those callers (`tests/scopes.rs` is one).
            let Some(environment) = self.closure_environment(path, &sources, &mut definitions) else {
                continue;
            };
            let made = indexer.with_macro_bodies(&environment).index(path, source, key);

            let stored = !truncated && !has_unresolved_includes(&made);
            if stored {
                let _encode = crate::stages::StageTimer::new(crate::stages::Stage::Encode);
                let _ = write_summary(&made, &self.cache);
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
                let _encode = crate::stages::StageTimer::new(crate::stages::Stage::Encode);
                let _ = write_summary(&rebuilt, &self.cache);
            }
            Some((rebuilt, stored))
        });

        rebuilt.extend(rebuilt_through_a_timeline.into_iter().flatten());

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


    fn read_into(&self, sources: &mut std::collections::HashMap<String, String>, path: &Path) {
        let key = normalize_path(path, cfg!(windows));
        if sources.contains_key(&key) {
            return;
        }
        if let Some(text) = self.files.read(path) {
            sources.insert(key, text);
        }
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
    pub fn forget(&mut self, path: &Path) -> bool {
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
        let resolver = crate::include::IncludeResolver::new(&self.files, &self.config);
        let mut interner = crate::file::paths::PathInterner::new(cfg!(windows));

        summary.includes.iter().all(|fact| {
            let now = resolver.resolve(&fact.as_include(), directory, None, &mut interner);
            now.resolved().map(|found| found.path.as_path()) == fact.resolved.as_deref()
        })
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
    environments: &'pass [(&std::sync::Arc<crate::TranslationUnit>, crate::UnitDefinitions)],
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

/// Does this summary write an `#include` whose target was never found?
///
/// The one question that has to be asked before a summary is stored, because the answer is a fact about the
/// filesystem and the key cannot see it. See the module documentation.
fn has_unresolved_includes(summary: &FileSummary) -> bool {
    summary.includes.iter().any(|include| include.resolved.is_none())
}

#[cfg(test)]
mod tests {
    use super::SummaryStore;
    use crate::include::config::CompilerConfig;
    use crate::file::paths::MemoryFiles;
    use cpp_parser::Dialect;
    use std::path::Path;

    /// A store over a few files in memory, with a cache directory of its own.
    ///
    /// A fresh directory per test, because the cache is keyed by *content* rather than by path: two tests with
    /// the same fixture text would otherwise share an entry, and a test would pass because of another test's
    /// leftovers.
    fn store(name: &str, files: &MemoryFiles) -> (SummaryStore<MemoryFiles>, std::path::PathBuf) {
        let root = std::env::temp_dir().join("cppls-store-tests").join(name);
        let _ = std::fs::remove_dir_all(&root);

        let store = SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
        (store, root)
    }

    #[test]
    fn a_summary_is_built_once_and_then_read_from_disk() {
        let files = MemoryFiles::new().with_file("/p/widget.h", "struct Widget { int size; };\n");
        let (mut store, root) = store("reuse", &files);

        // Cloned out of the store rather than held: `get` hands back a borrow of the index, and the stats below
        // need the store again. A `FileSummary` is a plain value, so copying one in a test costs nothing.
        let first = store
            .get(Path::new("/p/widget.h"))
            .expect("the file reads")
            .clone();
        assert_eq!(first.declarations.len(), 2, "the class and its member");
        assert_eq!(store.stats().rebuilt, 1);
        assert_eq!(store.stats().reused, 0);

        // A second store over the same root and the same text: the summary is on disk, so nothing is rebuilt.
        let mut reopened = SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
        let second = reopened.get(Path::new("/p/widget.h")).expect("the file reads");

        assert_eq!(second.declarations, first.declarations);
        assert_eq!(
            reopened.stats().rebuilt,
            0,
            "the parser was not asked: the disk had the answer"
        );
        assert_eq!(reopened.stats().reused, 1);
        assert_eq!(reopened.stats().hit_rate(), Some(1.0));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// **Preparing files together and committing them in order is reading them one at a time.**
    ///
    /// The parallel half of the pump rests on this: the same summaries, the same counters, and cache entries that
    /// read back whole — including for files that share a text (so share a cache key, and were written by two
    /// workers at once), a file that includes something that is not there (parsed, not stored) and a path that
    /// cannot be read.
    #[test]
    fn preparing_files_together_gives_the_index_reading_them_one_by_one_gives() {
        let mut files = MemoryFiles::new();
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        for number in 0..48 {
            let text = match number % 8 {
                // Six files with one text between them: one cache key, several concurrent writers.
                0 => "struct Same { int v; };\n".to_string(),
                // A search that fails, so the answer is not stored.
                1 => format!("#include \"missing{number}.h\"\nint one{number};\n"),
                _ => format!("#include \"f{}.h\"\nstruct S{number} {{ int v; }};\n", number.max(2) - 1),
            };
            let path = format!("/p/f{number}.h");
            files.insert(&path, &text);
            paths.push(path.into());
        }
        paths.push("/p/nowhere.h".into());

        let (mut one_by_one, root_one) = store("prepared-sequential", &files);
        for path in &paths {
            let _ = one_by_one.get(path);
        }

        let (mut together, root_together) = store("prepared-together", &files);
        for (path, prepared) in paths.iter().zip(together.prepare_many(&paths)) {
            let _ = together.commit(path, prepared);
        }

        // Files that share a text may be parsed twice when their workers reach them together (neither has written
        // the entry yet), so the split between `reused` and `rebuilt` can differ; the totals and the answers cannot.
        let (a, b) = (together.stats(), one_by_one.stats());
        assert_eq!((a.reused + a.rebuilt, a.unstored), (b.reused + b.rebuilt, b.unstored));
        assert_eq!(together.index().len(), one_by_one.index().len());
        for summary in one_by_one.index().summaries() {
            let other = together.index().summary(&summary.path).expect("the same files are indexed");
            assert_eq!(other.declarations, summary.declarations, "{:?}", summary.path);
            assert_eq!(other.includes, summary.includes, "{:?}", summary.path);
        }

        // What the workers wrote is what a later run reads back: every stored entry decodes, and none is torn.
        let mut reopened = SummaryStore::with_provider(&root_together, CompilerConfig::default(), files.clone());
        for path in &paths {
            let _ = reopened.get(path);
        }
        assert_eq!(
            reopened.stats().reused + reopened.stats().rebuilt,
            paths.len() - 1,
            "every readable file was read"
        );
        assert_eq!(
            reopened.stats().rebuilt,
            together.stats().unstored,
            "only the answers that were deliberately not stored are parsed again: {:?}",
            reopened.stats()
        );

        let _ = std::fs::remove_dir_all(&root_one);
        let _ = std::fs::remove_dir_all(&root_together);
    }

    /// **The include scan says what the parse will say.** It is only useful to act on if it is the same answer.
    #[test]
    fn scanning_the_include_lines_gives_the_includes_the_parse_records() {
        let files = MemoryFiles::new()
            .with_file("/p/a.h", "#pragma once\n#include \"b.h\"\n#if 0\n#include \"c.h\"\n#endif\n#include \"gone.h\"\nint a;\n")
            .with_file("/p/b.h", "int b;\n")
            .with_file("/p/c.h", "int c;\n");
        let (mut store, root) = store("include-scan", &files);
        let config = CompilerConfig::default();
        let source = crate::FileProvider::read(&files, Path::new("/p/a.h")).expect("the fixture is there");

        let scanned = crate::FileIndexer::new(&files, &config).includes_of(Path::new("/p/a.h"), &source);
        let recorded: Vec<std::path::PathBuf> = store
            .get(Path::new("/p/a.h"))
            .expect("reads")
            .includes
            .iter()
            .filter_map(|include| include.resolved.clone())
            .collect();

        assert_eq!(scanned, recorded);
        assert_eq!(scanned.len(), 2, "b.h and the one behind `#if 0`; the missing one is not resolved: {scanned:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **Reading a closure together reads the closure** — every file once, none the caller did not want, and never
    /// more than the budget — and what it prepared commits to the index a one-by-one walk builds.
    #[test]
    fn preparing_a_closure_reaches_every_file_it_includes_within_its_limits() {
        let mut files = MemoryFiles::new().with_file("/p/main.cpp", "#include \"h0.h\"\n#include \"h1.h\"\nint main;\n");
        for number in 0..30 {
            // A diamond-ridden graph with a cycle (h29 includes h0): each header includes the next two.
            files.insert(
                format!("/p/h{number}.h"),
                format!(
                    "#include \"h{}.h\"\n#include \"h{}.h\"\nstruct H{number} {{ int v; }};\n",
                    (number + 1) % 30,
                    (number + 2) % 30
                ),
            );
        }
        let (store, root) = store("prepare-closure", &files);
        let main = std::path::PathBuf::from("/p/main.cpp");

        let everything = store.prepare_closure(std::slice::from_ref(&main), |_| true, 1000);
        let mut reached: Vec<String> = everything.iter().map(|(path, _)| path.to_string_lossy().into_owned()).collect();
        reached.sort();
        reached.dedup();
        assert_eq!(reached.len(), 31, "main and thirty headers, each once: {}", everything.len());
        assert_eq!(everything.len(), 31, "and no file was prepared twice");

        let limited = store.prepare_closure(std::slice::from_ref(&main), |_| true, 10);
        assert!(limited.len() <= 10, "the budget bounds the call: {}", limited.len());
        assert!(limited.iter().any(|(path, _)| path == &main), "the root is always prepared");

        let refusing = store.prepare_closure(std::slice::from_ref(&main), |path| !path.ends_with("h1.h"), 1000);
        assert!(refusing.iter().all(|(path, _)| !path.ends_with("h1.h") || path == &main));

        // Committing what was prepared, in any order, gives the index of reading the same files one at a time.
        let (mut together, root_together) = store_named("prepare-closure-commit", &files);
        for (path, prepared) in everything {
            let _ = together.commit(&path, prepared);
        }
        let (mut one_by_one, root_one) = store_named("prepare-closure-one", &files);
        for path in &reached {
            let _ = one_by_one.get(Path::new(path));
        }
        assert_eq!(together.index().len(), one_by_one.index().len());
        for summary in one_by_one.index().summaries() {
            let other = together.index().summary(&summary.path).expect("indexed");
            assert_eq!(other.declarations, summary.declarations);
            assert_eq!(other.includes, summary.includes);
        }

        for directory in [root, root_together, root_one] {
            let _ = std::fs::remove_dir_all(&directory);
        }
    }

    fn store_named(name: &str, files: &MemoryFiles) -> (SummaryStore<MemoryFiles>, std::path::PathBuf) {
        store(name, files)
    }

    #[test]
    fn coming_back_to_earlier_text_finds_the_entry_again() {
        // The branch-switch property, in miniature: two texts, then back to the first. The third lookup is a hit
        // because the key names the *text* — the entry for the first one was never overwritten, and nothing had to
        // remember which texts have been seen. sets a hit rate of >90% for this case.
        let root = std::env::temp_dir().join("cppls-store-tests").join("branch-switch");
        let _ = std::fs::remove_dir_all(&root);

        let one = MemoryFiles::new().with_file("/p/a.cpp", "int x;\n");
        let two = MemoryFiles::new().with_file("/p/a.cpp", "int y;\n");

        let mut store = SummaryStore::with_provider(&root, CompilerConfig::default(), one.clone());
        store.get(Path::new("/p/a.cpp")).expect("the file reads");

        let mut other = SummaryStore::with_provider(&root, CompilerConfig::default(), two.clone());
        other.get(Path::new("/p/a.cpp")).expect("the file reads");
        assert_eq!(other.stats().rebuilt, 1, "the second text is new");

        let mut back = SummaryStore::with_provider(&root, CompilerConfig::default(), one.clone());
        back.get(Path::new("/p/a.cpp")).expect("the file reads");
        assert_eq!(
            back.stats().reused,
            1,
            "and the first text's entry is still there"
        );
        assert_eq!(back.stats().rebuilt, 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_edited_file_is_rebuilt_rather_than_reused() {
        let files = MemoryFiles::new().with_file("/p/widget.h", "struct Widget { int size; };\n");
        let (mut store, root) = store("edited", &files);
        store.get(Path::new("/p/widget.h")).expect("the file reads");

        // The same path with different text. The key changes, so the stored entry no longer names it and there is
        // nothing to reuse.
        let edited = MemoryFiles::new().with_file("/p/widget.h", "struct Widget { int a; int b; };\n");
        let mut second = SummaryStore::with_provider(&root, CompilerConfig::default(), edited.clone());
        let summary = second
            .get(Path::new("/p/widget.h"))
            .expect("the file reads")
            .clone();

        assert_eq!(
            summary.declarations.len(),
            3,
            "the class and both of its members — the edited text, not the stored one"
        );
        assert_eq!(second.stats().reused, 0, "the stored entry names other text");
        assert_eq!(second.stats().rebuilt, 1);
        assert_eq!(second.stats().unstored, 0, "nothing was declined");
        assert_eq!(
            summary.key.content_hash,
            crate::cache::content_hash("struct Widget { int a; int b; };\n"),
            "and the rebuilt summary is keyed by the text it was built from"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_file_gets_a_cache_entry_of_its_own() {
        // The bug this pins, found by a failing restart test rather than by reading: `FileIndexer` used to store
        // the caller's key verbatim, and `get` passed a *placeholder* — it computed the key after building, and
        // the build's own macros were an input to it. So every summary in every project recorded content hash `0`,
        // all of them were written to one entry, and the cache appeared to work: one file stored, one file reused.
        // It was answering with the wrong file.
        //
        // The placeholder is gone by construction now — the key is computed from the text before the build — so
        // this test guards the property rather than the mechanism.
        let files = MemoryFiles::new()
            .with_file("/p/one.h", "struct One { int a; };\n")
            .with_file("/p/two.h", "struct Two { int b; };\n");
        let (mut store, root) = store("distinct-entries", &files);

        let one = store
            .get(Path::new("/p/one.h"))
            .expect("the file reads")
            .clone();
        let two = store
            .get(Path::new("/p/two.h"))
            .expect("the file reads")
            .clone();

        assert_eq!(
            one.key.content_hash,
            crate::cache::content_hash("struct One { int a; };\n"),
            "the summary is keyed by its own text"
        );
        assert_eq!(
            two.key.content_hash,
            crate::cache::content_hash("struct Two { int b; };\n")
        );
        assert_ne!(
            one.key.path_under(store.cache_directory()),
            two.key.path_under(store.cache_directory()),
            "two files, two entries"
        );

        // And both are really on disk, each holding the facts of the file that wrote it — which is what a
        // collision would have made impossible to notice from the outside.
        let stored_one =
            super::read_summary(&one.key.path_under(store.cache_directory())).expect("written");
        let stored_two =
            super::read_summary(&two.key.path_under(store.cache_directory())).expect("written");
        assert_eq!(stored_one.path, std::path::Path::new("/p/one.h"));
        assert_eq!(stored_two.path, std::path::Path::new("/p/two.h"));
        assert!(
            stored_one
                .declarations
                .iter()
                .any(|fact| fact.name == "One")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_different_configuration_is_a_different_key() {
        let files = MemoryFiles::new().with_file("/p/a.cpp", "int x;\n");
        let (mut store, root) = store("config", &files);
        store.get(Path::new("/p/a.cpp")).expect("the file reads");

        let configured = CompilerConfig::default().with_standard("c++20");
        let mut second = SummaryStore::with_provider(&root, configured, files.clone());
        second.get(Path::new("/p/a.cpp")).expect("the file reads");

        assert_eq!(
            second.stats().reused,
            0,
            "the same text under a different configuration is a different summary"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_identical_configuration_gives_an_identical_key() {
        // The other half: the hash has to be stable, or the cache would never hit at all.
        let files = MemoryFiles::new();
        let one = SummaryStore::with_provider("r", CompilerConfig::default(), files.clone());
        let two = SummaryStore::with_provider("r", CompilerConfig::default(), files.clone());

        assert_eq!(
            one.context_hash(Path::new("/p/a.cpp")),
            two.context_hash(Path::new("/p/a.cpp"))
        );

        let different = SummaryStore::with_provider(
            "r",
            CompilerConfig::default().with_define(crate::CommandLineMacro::defined("A")),
            files.clone(),
        );
        assert_ne!(
            one.context_hash(Path::new("/p/a.cpp")),
            different.context_hash(Path::new("/p/a.cpp"))
        );
    }

    #[test]
    fn the_key_knows_which_compiler_the_file_is_read_for() {
        // The dialect is not a detail of the flags: it changes what the *text* means, so two targets are two
        // summaries of the same file. `unsigned __int128 x;` declares `x` under GNU and something else under MSVC
        //, and a cache that confused the two would answer with the wrong facts
        // the one failure mode a key must not have.
        let files = MemoryFiles::new();
        let gnu = SummaryStore::with_provider(
            "r",
            CompilerConfig::default().with_dialect(Dialect::Gnu),
            files.clone(),
        );
        let msvc = SummaryStore::with_provider(
            "r",
            CompilerConfig::default().with_dialect(Dialect::Msvc),
            files.clone(),
        );

        assert_ne!(
            gnu.context_hash(Path::new("/p/a.cpp")),
            msvc.context_hash(Path::new("/p/a.cpp")),
            "the same file read for two compilers is two different summaries"
        );
    }

    #[test]
    fn the_working_directory_counts_exactly_where_it_changes_what_is_found() {
        // A relative `-I` is relative to where the compiler ran, so the same configuration string means two
        // different sets of search directories in two working directories — and the same set when nothing is
        // relative. Hashing the *resolved* directories is what makes both halves true; hashing the working
        // directory itself would make a caller's shell a cache key, and hashing only the raw `-I` would let two
        // genuinely different searches share one entry.
        let files = MemoryFiles::new();

        let relative = |directory: &str| {
            SummaryStore::with_provider(
                "r",
                CompilerConfig::default()
                    .with_include_path("inc")
                    .with_working_directory(directory),
                files.clone(),
            )
        };
        assert_ne!(
            relative("build-one").context_hash(Path::new("/p/a.cpp")),
            relative("build-two").context_hash(Path::new("/p/a.cpp")),
            "a relative -I is a different directory in a different working directory"
        );

        let absolute = |directory: &str| {
            SummaryStore::with_provider(
                "r",
                CompilerConfig::default()
                    .with_include_path("/opt/inc")
                    .with_working_directory(directory),
                files.clone(),
            )
        };
        assert_eq!(
            absolute("build-one").context_hash(Path::new("/p/a.cpp")),
            absolute("build-two").context_hash(Path::new("/p/a.cpp")),
            "an absolute -I does not care where the compiler ran"
        );
    }

    #[test]
    fn a_file_whose_include_was_not_found_is_not_stored() {
        // The one thing a key computed from the text cannot check is which files exist, so a summary that records
        // a *failed* search must not be cached: it would go on saying "unresolved" after the header appeared.
        let files = MemoryFiles::new().with_file("/p/main.cpp", "#include \"missing.h\"\nint x;\n");
        let (mut store, root) = store("unresolved", &files);

        let summary = store.get(Path::new("/p/main.cpp")).expect("the file reads");
        assert_eq!(summary.declarations.len(), 1, "the summary is still built");
        assert_eq!(store.stats().unstored, 1);
        assert_eq!(store.stats().reused, 0);
        assert_eq!(store.stats().rebuilt, 1);
        assert!(
            !root.join(crate::CACHE_DIRECTORY).exists(),
            "nothing under the root that a summary could have been written to"
        );

        // A later store builds again rather than reading an entry that recorded a search which has since been
        // answered: the header is there now, and the answer has to change.
        let with_header = MemoryFiles::new()
            .with_file("/p/main.cpp", "#include \"missing.h\"\nint x;\n")
            .with_file("/p/missing.h", "struct Now { int here; };\n");
        let mut second = SummaryStore::with_provider(&root, CompilerConfig::default(), with_header.clone());
        let rebuilt = second
            .get(Path::new("/p/main.cpp"))
            .expect("the file reads")
            .clone();
        assert_eq!(second.stats().reused, 0);
        assert_eq!(second.stats().rebuilt, 1);
        assert_eq!(second.stats().unstored, 0, "now the search is answered");
        assert_eq!(
            rebuilt.includes[0].resolved.as_deref(),
            Some(Path::new("/p/missing.h")),
            "and the entry records where it was found"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_whose_include_was_never_indexed_is_still_stored() {
        // The counterpart of the rule above, and the difference is the whole reason the rule is about *resolving*
        // rather than about indexing: `widget.h` is on disk and was found, so the entry records a search that was
        // answered. That `widget.h` has no summary yet is a fact about the index, not about this file's text, and
        // it heals the moment something asks for `widget.h` — so there is nothing to prevent caching.
        let files = MemoryFiles::new()
            .with_file("/p/widget.h", "struct Widget { int size; };\n")
            .with_file("/p/main.cpp", "#include \"widget.h\"\nvoid f() { Widget w; }\n");
        let (mut store, root) = store("unindexed-include", &files);

        store.get(Path::new("/p/main.cpp")).expect("the file reads");
        assert_eq!(store.stats().unstored, 0);

        let mut reopened = SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
        reopened.get(Path::new("/p/main.cpp")).expect("the file reads");
        assert_eq!(reopened.stats().reused, 1, "the entry is usable");
        assert_eq!(reopened.stats().rebuilt, 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_key_is_the_same_for_the_same_contents_in_the_same_directory() {
        // What makes a branch switch cheap: a file that comes back to a content the cache has seen is found
        // without being parsed. `a.cpp` and `b.cpp` here are two files, and they share one entry — the directory
        // is the same, so the text determines everything the summary says.
        let files = MemoryFiles::new()
            .with_file("/p/a.cpp", "int x;\n")
            .with_file("/p/b.cpp", "int x;\n");
        let (mut store, root) = store("content-addressed", &files);

        let one = store
            .get(Path::new("/p/a.cpp"))
            .expect("the file reads")
            .clone();
        assert_eq!(one.path, Path::new("/p/a.cpp"));

        let two = store
            .get(Path::new("/p/b.cpp"))
            .expect("the file reads")
            .clone();
        assert_eq!(two.key, one.key, "the same text is the same entry");
        assert_eq!(store.stats().reused, 1, "so the second one is a hit");
        assert_eq!(store.stats().rebuilt, 1);

        // And each path keeps its own name: a reused summary records the path of whoever wrote the entry, so it
        // has to be refiled under the path that asked — otherwise every fact in it points at the wrong file.
        assert_eq!(two.path, Path::new("/p/b.cpp"));
        for path in ["/p/a.cpp", "/p/b.cpp"] {
            assert_eq!(
                store
                    .index()
                    .summary(Path::new(path))
                    .map(|held| held.path.clone()),
                Some(std::path::PathBuf::from(path))
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_same_text_in_two_directories_does_not_share_an_entry() {
        // The bug this pins: a summary holds not only what the text *says* but where its includes *resolved to*,
        // and `#include "shared.h"` means the one beside me. With the directory left out of the key these two
        // files were one entry, so `sub/b.h` was handed `a.h`'s resolution of `shared.h` and would have offered a
        // jump into a header it does not include.
        let files = MemoryFiles::new()
            .with_file("/p/shared.h", "struct Outside { int x; };\n")
            .with_file("/p/sub/shared.h", "struct Inside { int y; };\n")
            .with_file("/p/a.h", "#include \"shared.h\"\n")
            .with_file("/p/sub/b.h", "#include \"shared.h\"\n");

        let (mut store, root) = store("directory-context", &files);

        for path in ["/p/shared.h", "/p/sub/shared.h", "/p/a.h", "/p/sub/b.h"] {
            assert!(store.get(Path::new(path)).is_some(), "the file reads: {path}");
        }

        let outside = store
            .index()
            .summary(Path::new("/p/a.h"))
            .expect("held")
            .clone();
        let inside = store
            .index()
            .summary(Path::new("/p/sub/b.h"))
            .expect("held")
            .clone();

        assert_eq!(
            outside.key.content_hash, inside.key.content_hash,
            "the two files really do say the same thing"
        );
        assert_ne!(
            outside.key, inside.key,
            "the same text compiled one directory down is a different compilation"
        );

        // The resolutions, which is what the collision would have got wrong: each file found the header beside
        // itself, and the class it declares is visible from that file and not from the other.
        let resolution = |summary: &crate::FileSummary| {
            summary.includes[0]
                .resolved
                .as_ref()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
        };
        assert_eq!(resolution(&outside).as_deref(), Some("/p/shared.h"));
        assert_eq!(resolution(&inside).as_deref(), Some("/p/sub/shared.h"));

        assert!(
            matches!(
                store.index().definition("Outside", Path::new("/p/a.h")),
                crate::Known::Yes(_)
            ),
            "`a.h` sees the header beside it"
        );
        assert!(
            matches!(
                store.index().definition("Outside", Path::new("/p/sub/b.h")),
                crate::Known::Unknown(_)
            ),
            "and `sub/b.h` does not"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_hit_re_checks_the_includes_without_parsing_them() {
        // "The disk saves the parse" is this module's claim, and `StoreStats` is the module's own accounting of it
        // — a counter that could be moved. This checks the behaviour instead, by counting what the provider was
        // asked: a hit reads the file once, to hash it, and asks about each **stored** include exactly once, to
        // check that it still resolves where it did. Nothing is read but the file itself, and no include is
        // *searched for* beyond the ones the summary already lists.
        let files = MemoryFiles::new()
            .with_file("/p/widget.h", "struct Widget { int size; };\n")
            .with_file(
                "/p/main.cpp",
                "#include \"widget.h\"\nvoid f() { Widget w; }\n",
            );
        let (mut store, root) = store("no-search-on-hit", &files);

        store.get(Path::new("/p/main.cpp")).expect("the file reads");
        let after_build = files.exists_of("/p/widget.h");
        assert_eq!(after_build, 1, "one candidate, probed once while resolving");

        let mut reopened = SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
        reopened.get(Path::new("/p/main.cpp")).expect("the file reads");

        assert_eq!(reopened.stats().reused, 1);
        assert_eq!(reopened.stats().rebuilt, 0, "the parser was not asked");
        assert_eq!(
            files.reads_of("/p/main.cpp"),
            2,
            "the file was read once per get, and nothing else was read at all"
        );
        assert_eq!(
            files.exists_of("/p/widget.h"),
            after_build + 1,
            "and the header was probed once more — the re-check, not a search"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stored_summary_whose_include_no_longer_resolves_is_rebuilt() {
        // The one fact about a summary that its key cannot name: which candidate paths *exist*. A stored
        // `resolved` is a claim about the filesystem, so it is checked before it is believed — and the check is
        // what keeps correctness from depending on a watcher never missing an event.
        let files = MemoryFiles::new()
            .with_file("/p/widget.h", "struct Widget { int size; };\n")
            .with_file(
                "/p/main.cpp",
                "#include \"widget.h\"\nvoid f() { Widget w; }\n",
            );
        let (mut store, root) = store("stale-resolution", &files);
        let built = store
            .get(Path::new("/p/main.cpp"))
            .expect("the file reads")
            .clone();
        assert_eq!(built.includes[0].resolved.as_deref(), Some(Path::new("/p/widget.h")));

        // The header is gone, and nothing tells the store so: no event, no invalidation, no forget. The entry is
        // still there, under a key that has not moved.
        let without = MemoryFiles::new().with_file(
            "/p/main.cpp",
            "#include \"widget.h\"\nvoid f() { Widget w; }\n",
        );
        let mut reopened = SummaryStore::with_provider(&root, CompilerConfig::default(), without.clone());
        let rebuilt = reopened
            .get(Path::new("/p/main.cpp"))
            .expect("the file reads")
            .clone();

        assert_eq!(
            reopened.stats().reused,
            0,
            "the entry named a file that is not there, so it was not an answer"
        );
        assert_eq!(reopened.stats().rebuilt, 1);
        assert_eq!(
            rebuilt.includes[0].resolved, None,
            "and the rebuilt summary records the search that now fails"
        );
        assert_eq!(
            reopened.stats().unstored,
            1,
            "so it is not stored, which is what makes the change recoverable"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_that_includes_another_is_indexed_after_it() {
        // The narrowest case of an include graph: one header that includes another. Nothing about the order
        // matters any more — each file's key is its own text — but the graph is what the definition query walks,
        // so both files have to end up in the index with their edge intact.
        let files = MemoryFiles::new()
            .with_file("/includes/widget.h", "struct Widget { int size; };\n")
            .with_file("/includes/middle.h", "#include \"widget.h\"\n");

        let (mut store, root) = store("include-after", &files);

        assert!(
            store.get(Path::new("/includes/widget.h")).is_some(),
            "the included header is indexed first"
        );
        assert!(
            store.get(Path::new("/includes/middle.h")).is_some(),
            "and the file that includes it is indexed after, in either order"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_changed_header_invalidates_the_files_that_include_it() {
        let files = MemoryFiles::new()
            .with_file("/invalidate-a/widget.h", "struct Widget { int size; };\n")
            .with_file("/invalidate-a/middle.h", "#include \"widget.h\"\n")
            .with_file(
                "/invalidate-a/main.cpp",
                "#include \"middle.h\"\nvoid f() { Widget w; }\n",
            )
            .with_file("/invalidate-a/unrelated.cpp", "int x;\n");

        let (mut store, root) = store("invalidate", &files);

        for path in [
            "/invalidate-a/widget.h",
            "/invalidate-a/middle.h",
            "/invalidate-a/main.cpp",
            "/invalidate-a/unrelated.cpp",
        ] {
            assert!(store.get(Path::new(path)).is_some(), "the file reads: {path}");
        }

        let mut work = store.invalidate(Path::new("/invalidate-a/widget.h"));
        work.sort();
        assert_eq!(
            work,
            [
                Path::new("/invalidate-a/main.cpp"),
                Path::new("/invalidate-a/middle.h"),
                Path::new("/invalidate-a/widget.h"),
            ],
            "the header and everything below it, transitively, and nothing else"
        );

        // A file nothing includes invalidates only itself, which is the case that makes this worth a graph walk
        // rather than a rule.
        let mut alone = store.invalidate(Path::new("/invalidate-a/unrelated.cpp"));
        alone.sort();
        assert_eq!(alone, [Path::new("/invalidate-a/unrelated.cpp")]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalidation_terminates_on_a_cycle_of_includes() {
        let files = MemoryFiles::new()
            .with_file("/p/a.h", "#include \"b.h\"\nstruct A { int x; };\n")
            .with_file("/p/b.h", "#include \"a.h\"\nstruct B { int y; };\n");

        let (mut store, root) = store("cycle", &files);
        store.get(Path::new("/p/a.h")).expect("the file reads");
        store.get(Path::new("/p/b.h")).expect("the file reads");

        let mut work = store.invalidate(Path::new("/p/a.h"));
        work.sort();
        assert_eq!(work, [Path::new("/p/a.h"), Path::new("/p/b.h")]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_that_cannot_be_read_is_not_an_error() {
        let files = MemoryFiles::new();
        let (mut store, root) = store("missing", &files);

        assert!(store.get(Path::new("/p/nothing.cpp")).is_none());
        assert_eq!(store.stats(), super::StoreStats::default());
        assert_eq!(
            store.stats().hit_rate(),
            None,
            "no lookups is not a zero hit rate"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_whole_store_survives_a_restart_with_its_facts_intact() {
        // The end-to-end property: build a small project, throw the store away, build it again over the same
        // directory, and get the same answers **without parsing anything**. `rebuilt == 0` is that claim — the
        // counter is incremented exactly where a file is parsed — and it is the difference between a cache that
        // saves writes and a cache that saves work.
        let files = MemoryFiles::new()
            .with_file("/p/config.h", "#define FEATURE 1\n")
            .with_file("/p/widget.h", "struct Widget { int size; };\n")
            .with_file(
                "/p/main.cpp",
                "#include \"widget.h\"\n#include \"config.h\"\nvoid f() { Widget w; }\n",
            );

        let (mut store, root) = store("restart", &files);
        for path in ["/p/config.h", "/p/widget.h", "/p/main.cpp"] {
            store.get(Path::new(path)).expect("the file reads");
        }
        assert_eq!(store.stats().rebuilt, 3);

        let found = store.index().definition("Widget", Path::new("/p/main.cpp"));
        assert!(
            matches!(found, crate::Known::Yes(_)),
            "the class is visible through the include: {found:?}"
        );

        let mut reopened = SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
        for path in ["/p/config.h", "/p/widget.h", "/p/main.cpp"] {
            reopened.get(Path::new(path)).expect("the file reads");
        }

        assert_eq!(reopened.stats().rebuilt, 0, "nothing needed parsing");
        assert_eq!(reopened.stats().reused, 3);

        let after = reopened
            .index()
            .definition("Widget", Path::new("/p/main.cpp"));
        assert!(
            matches!(after, crate::Known::Yes(_)),
            "and the answer survives the restart: {after:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_summary_stored_by_another_build_is_not_trusted() {
        // The entry's own key is checked against the one just computed, so a file that happens to be sitting at the
        // right name — a hash collision, or a directory copied from a build with a different idea of the key — is a
        // miss rather than an answer. Written by hand rather than produced, because the point is a *disagreement*
        // between the name and the contents.
        let files = MemoryFiles::new().with_file("/p/a.cpp", "int x;\n");
        let (mut store, root) = store("mismatched-entry", &files);

        let key = crate::SummaryKey::new(
            crate::cache::content_hash("int x;\n"),
            store.context_hash(Path::new("/p/a.cpp")),
        );
        let elsewhere = crate::SummaryKey::new(crate::cache::content_hash("int x;\n"), 999);
        let impostor = crate::index::summarize(Path::new("/p/a.cpp"), "int x;\n", elsewhere);
        super::write_summary(&impostor, &root).expect("the write must succeed");

        store.get(Path::new("/p/a.cpp")).expect("the file reads");
        assert_eq!(
            store.stats().reused,
            0,
            "an entry whose key disagrees with its name is not an answer"
        );
        assert_eq!(store.stats().rebuilt, 1);
        assert_eq!(
            store.index().summary(Path::new("/p/a.cpp")).map(|held| held.key),
            Some(key),
            "and the entry is replaced by one that does agree"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // -------------------------------------------------------------------------------------------
    // Indexing a file's includes
    //
    // The fixtures are shaped like a real project: a `.cpp` that includes a header, which includes another, with
    // a standard-library-shaped target that resolves into a search path. What each test is about is what the walk
    // *reached* and what it said about where it stopped.
    // -------------------------------------------------------------------------------------------

    use super::{
        IncludeBudget, NotIndexedReason, StoreStats,
    };

    /// A translation unit with a two-level include chain, and nothing missing.
    fn chain() -> MemoryFiles {
        MemoryFiles::new()
            .with_case_insensitive(false)
            .with_file("/p/main.cpp", "#include \"middle.h\"\nint main() { return 0; }\n")
            .with_file("/p/middle.h", "#include \"deep.h\"\nstruct Middle { Deep d; };\n")
            .with_file("/p/deep.h", "struct Deep { int x; };\n")
    }

    /// The paths a walk indexed, as strings, so an assertion can read as the shape of the graph.
    fn indexed(index: &super::IncludeIndex) -> Vec<String> {
        index
            .indexed
            .iter()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn indexing_a_file_indexes_everything_it_includes() {
        // What the call is for: after it, the declarations the file can *see* are in the index, which is the
        // difference between answering about `Deep` and reporting it as not declared here.
        let files = chain();
        let (mut store, root) = store("closure", &files);

        let index = store.index_includes_from(Path::new("/p/main.cpp"), IncludeBudget::default());

        assert_eq!(
            indexed(&index),
            ["/p/main.cpp", "/p/middle.h", "/p/deep.h"],
            "the entry first, then outwards"
        );
        assert!(index.not_indexed.is_empty(), "{:?}", index.not_indexed);
        assert!(index.unresolved.is_empty());
        assert_eq!(index.stats.rebuilt, 3, "three files, three parses");
        assert_eq!(index.stats.reused, 0);

        let found = store.index().definition("Deep", Path::new("/p/main.cpp"));
        assert!(
            matches!(found, crate::Known::Yes(_)),
            "and a name two headers down is now visible from the file that includes them: {found:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_header_reached_twice_is_indexed_once() {
        // `#include <vector>` written in twenty headers is twenty edges and one node. Without the visited set the
        // second visit would re-ask the cache — cheap, but it would also put the file in `indexed` twice, and a
        // caller counting files would report a project larger than it is.
        let files = MemoryFiles::new()
            .with_case_insensitive(false)
            .with_file("/p/main.cpp", "#include \"a.h\"\n#include \"b.h\"\n")
            .with_file("/p/a.h", "#include \"shared.h\"\nstruct A { int x; };\n")
            .with_file("/p/b.h", "#include \"shared.h\"\nstruct B { int y; };\n")
            .with_file("/p/shared.h", "struct Shared { int x; };\n");
        let (mut store, root) = store("diamond", &files);

        let index = store.index_includes_from(Path::new("/p/main.cpp"), IncludeBudget::default());

        assert_eq!(indexed(&index).len(), 4, "{:?}", indexed(&index));
        assert_eq!(
            indexed(&index).iter().filter(|path| *path == "/p/shared.h").count(),
            1
        );
        assert_eq!(
            index.stats.rebuilt, 4,
            "four files, four parses — the fifth visit is the one the visited set stopped, and it is why this \
             number is not five"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_files_with_the_same_text_side_by_side_share_one_parse() {
        // The content-key property turning up inside a closure walk, and it is worth a test precisely because it
        // is surprising: the key names a file's **text and directory**, not its path, so two headers that happen
        // to be identical share an entry. Both are still in `indexed` — they are two files — while only one of
        // them was parsed.
        let files = MemoryFiles::new()
            .with_case_insensitive(false)
            .with_file("/p/main.cpp", "#include \"one.h\"\n#include \"two.h\"\n")
            .with_file("/p/one.h", "struct Same { int x; };\n")
            .with_file("/p/two.h", "struct Same { int x; };\n");
        let (mut store, root) = store("twin-headers", &files);

        let index = store.index_includes_from(Path::new("/p/main.cpp"), IncludeBudget::default());

        assert_eq!(indexed(&index).len(), 3, "three files");
        assert_eq!(index.stats.rebuilt, 2, "and two parses, because two of them are the same text");
        assert_eq!(index.stats.reused, 1, "the twin was answered by the entry the first one wrote");

        // And the one entry is filed under **each** path that asked for it — see `ProjectIndex::insert_at` — so a
        // query about either header finds what it declares. Asked as a list rather than as a definition because
        // this fixture declares `Same` twice: two headers defining one class is an ODR violation, and the honest
        // answer to "which one" is that nothing chooses.
        let declared_in: Vec<String> = store
            .index()
            .files_declaring("Same", Path::new("/p/main.cpp"))
            .iter()
            .map(|found| found.file.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(declared_in, ["/p/one.h", "/p/two.h"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_cycle_of_includes_terminates() {
        // Two headers that include each other with no guards, which is a mistake a compiler would report and an
        // editor still has to survive. The visited set is what ends this; without it the walk does not fail, it
        // does not return.
        let files = MemoryFiles::new()
            .with_case_insensitive(false)
            .with_file("/p/main.cpp", "#include \"a.h\"\n")
            .with_file("/p/a.h", "#include \"b.h\"\nstruct A { int x; };\n")
            .with_file("/p/b.h", "#include \"a.h\"\nstruct B { int y; };\n");
        let (mut store, root) = store("cycle", &files);

        let index = store.index_includes_from(Path::new("/p/main.cpp"), IncludeBudget::default());

        assert_eq!(indexed(&index).len(), 3, "{:?}", indexed(&index));
        assert!(index.not_indexed.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_include_that_resolved_to_nothing_is_reported_rather_than_dropped() {
        // The walk continues past it — a missing header is one file's problem, not the walk's — and says which
        // directive it was, because "something is missing" is not actionable and "`missing.h` in `middle.h`, line
        // 2" is.
        let files = MemoryFiles::new()
            .with_case_insensitive(false)
            .with_file(
                "/p/main.cpp",
                "#include \"middle.h\"\n#include \"gone.h\"\nint main() { return 0; }\n",
            )
            .with_file("/p/middle.h", "#include \"deep.h\"\n")
            .with_file("/p/deep.h", "struct Deep { int x; };\n");
        let (mut store, root) = store("missing", &files);

        let index = store.index_includes_from(Path::new("/p/main.cpp"), IncludeBudget::default());

        assert_eq!(indexed(&index).len(), 3, "the rest of the closure was still indexed");
        assert_eq!(index.unresolved.len(), 1, "{:?}", index.unresolved);
        assert_eq!(index.unresolved[0].from, Path::new("/p/main.cpp"));
        assert_eq!(index.unresolved[0].spelling, "gone.h");
        assert!(
            index.unresolved[0].range.start_offset > 0,
            "and the range points at the directive rather than at the file"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_file_budget_stops_the_walk_and_says_so() {
        // A budget that truncated silently would make a partial index look like a whole one — the same failure
        // `Known` exists to prevent one layer up, one layer down.
        let files = chain();
        let (mut store, root) = store("budget", &files);

        let index = store.index_includes_from(
            Path::new("/p/main.cpp"),
            IncludeBudget {
                max_files: 2,
                ..IncludeBudget::default()
            },
        );

        assert_eq!(indexed(&index).len(), 2);
        assert_eq!(index.not_indexed.len(), 1, "{:?}", index.not_indexed);
        assert_eq!(
            index.not_indexed[0].reason,
            NotIndexedReason::Budget,
            "and the reason names the budget rather than the filesystem"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_depth_limit_stops_a_long_chain_and_says_so() {
        // The other limit, and the other failure: a chain that is long rather than wide.
        let files = MemoryFiles::new()
            .with_case_insensitive(false)
            .with_file("/p/l0.h", "#include \"l1.h\"\n")
            .with_file("/p/l1.h", "#include \"l2.h\"\n")
            .with_file("/p/l2.h", "struct Deep { int x; };\n");
        let (mut store, root) = store("depth", &files);

        let index = store.index_includes_from(
            Path::new("/p/l0.h"),
            IncludeBudget {
                max_depth: 1,
                ..IncludeBudget::default()
            },
        );

        assert_eq!(indexed(&index), ["/p/l0.h", "/p/l1.h"]);
        assert_eq!(
            index
                .not_indexed
                .iter()
                .map(|stopped| (stopped.reason, stopped.path.to_string_lossy().to_string()))
                .collect::<Vec<_>>(),
            [(NotIndexedReason::Depth, "/p/l2.h".to_string())]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_that_cannot_be_read_is_reported_rather_than_being_an_error() {
        // The entry itself is missing. A caller asking about a file that is not there is an ordinary thing to do
        // — an editor restoring a session, a watcher reporting a deletion — and the answer is that the index has
        // nothing to say, not a panic.
        let files = chain();
        let (mut store, root) = store("missing-entry", &files);

        let index = store.index_includes_from(Path::new("/p/nope.cpp"), IncludeBudget::default());

        assert!(index.indexed.is_empty());
        assert_eq!(
            index.not_indexed,
            [super::NotIndexed {
                path: Path::new("/p/nope.cpp").to_path_buf(),
                reason: NotIndexedReason::Unreadable,
            }]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_second_call_reads_the_whole_closure_from_disk() {
        // The point of the whole exercise. A second session — a new store, nothing in memory, only what the first
        // one wrote — must reach the same index without parsing anything: that is what makes opening a real
        // project cheap the second time, and it is the number measures at 11 ms for 185
        // files against a second and a half of parsing.
        let files = chain();
        let root = std::env::temp_dir().join("cppls-store-tests").join("closure-warm");
        let _ = std::fs::remove_dir_all(&root);

        let cold = {
            let mut store = SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
            store.index_includes_from(Path::new("/p/main.cpp"), IncludeBudget::default())
        };
        assert_eq!(cold.stats.rebuilt, 3);

        let mut reopened = SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
        let warm = reopened.index_includes_from(Path::new("/p/main.cpp"), IncludeBudget::default());

        assert_eq!(indexed(&warm), indexed(&cold), "the same files, in the same order");
        assert_eq!(warm.stats.rebuilt, 0, "the parser was not asked");
        assert_eq!(warm.stats.reused, 3);
        assert_eq!(warm.stats.hit_rate(), Some(1.0));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_stats_of_one_call_are_a_delta_and_not_the_session() {
        // A caller reporting "this call parsed 3 files" must not report the session's numbers, and the way that
        // goes wrong is every caller subtracting for itself until one of them forgets.
        let files = chain();
        let (mut store, root) = store("stats-delta", &files);

        let first = store.index_includes_from(Path::new("/p/main.cpp"), IncludeBudget::default());
        let second = store.index_includes_from(Path::new("/p/main.cpp"), IncludeBudget::default());

        assert_eq!(first.stats.rebuilt, 3);
        assert_eq!(
            second.stats.rebuilt, 0,
            "the second call found its own work on disk"
        );
        assert_eq!(second.stats.reused, 3);
        assert_eq!(
            store.stats().rebuilt,
            3,
            "while the session's total is still the whole story"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_default_budget_does_not_truncate_a_measured_real_closure() {
        // The default is a limit, and a limit that cuts into something real is a bug rather than a policy. The
        // worst case measured on a real toolchain is `<bits/stdc++.h>` at 359 files, so the default has to clear
        // that with room
        let budget = IncludeBudget::default();

        assert!(
            budget.max_files > 359,
            "the default must not truncate the largest real closure anyone has measured: {budget:?}"
        );
        assert_eq!(budget.max_depth, crate::MAX_INCLUDE_DEPTH);
    }

    #[test]
    fn a_delta_of_stats_saturates_rather_than_panicking() {
        let later = StoreStats {
            reused: 1,
            rebuilt: 0,
            unstored: 0,
        };
        let earlier = StoreStats {
            reused: 0,
            rebuilt: 5,
            unstored: 2,
        };

        assert_eq!(
            later.since(earlier),
            StoreStats {
                reused: 1,
                rebuilt: 0,
                unstored: 0,
            },
            "a reading given out of order is a caller's mistake, and zero is a better failure than a panic"
        );
    }
}



