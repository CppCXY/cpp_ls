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
//! `docs/index-design.md` gives for `Unknown` being a first-class answer: a wrong entry is worse than a missing
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
    /// The project root. The cache lives under it, because `docs/index-design.md` puts it there on purpose: it
    /// travels with a checkout, so CI gets the same warm cache a developer has.
    root: PathBuf,
    /// Where the summaries are written: `<root>/.cppls`, or the directory `index.cache_dir` names.
    cache: PathBuf,
    config: CompilerConfig,
    files: F,
    index: ProjectIndex,
    stats: StoreStats,
}

/// What the store did, so that a caller can see the cache working.
///
/// A counter rather than a log line, because the four numbers `docs/index-design.md` says to measure before
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
    /// (`docs/std-library.md`), so 4096 is more than ten times the largest case anyone has measured — while a
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
        let source = self.files.read(path)?;
        let key = SummaryKey::new(content_hash(&source), self.context_hash(path));

        if let Ok(stored) = read_summary(&key.path_under(&self.cache))
            && stored.key == key
            && self.resolution_still_holds(path, &stored)
        {
            self.stats.reused += 1;
            // Filed under the path that asked, which is not necessarily the one recorded in the entry: the key
            // names the text, so two files with the same text share an entry. See `ProjectIndex::insert_at`.
            self.index.insert_at(path, stored);
            return self.index.summary(path);
        }

        self.stats.rebuilt += 1;
        let summary = FileIndexer::new(&self.files, &self.config).index(path, &source, key);

        // The one rule about the filesystem: a summary that records a *failed* search must not be stored, because
        // nothing in the key would notice the header appearing. See the module documentation.
        if has_unresolved_includes(&summary) {
            self.stats.unstored += 1;
        } else {
            // A failed write is not a failed lookup: the answer is in hand and in the index. Reporting it would
            // turn a read-only checkout — a perfectly ordinary way to work — into a broken editor.
            let _ = write_summary(&summary, &self.cache);
        }

        self.index.insert(summary);
        self.index.summary(path)
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
    ///   the `-D`s of the translation unit that includes it. `docs/std-library.md` calls that the expensive layer
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
        // See [`SummaryStore::re_read_what_a_body_changes`] for the filter and for what is deliberately not stored.
        let truncated = !outcome.not_indexed.is_empty();
        let indexed = outcome.indexed.clone();
        outcome.re_read = self.re_read_what_a_body_changes(&indexed, truncated);

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
    /// Returns how many files were re-read.
    fn re_read_what_a_body_changes(&mut self, indexed: &[PathBuf], truncated: bool) -> usize {
        // Every indexed file's text, read once: the closure walk slices each macro's body out of it, and a
        // candidate is re-parsed from it. One read per file per pass, rather than one per candidate include.
        let mut sources: std::collections::HashMap<PathBuf, String> = std::collections::HashMap::new();
        for path in indexed {
            if let Some(text) = self.files.read(path) {
                sources.insert(path.clone(), text);
            }
        }

        // The names whose body a **reading** uses, from the **macro facts** of the indexed closure — not from its
        // text, and not from the environment: this is the question "could any file's reading have changed", and
        // the answer has to be knowable without building an environment per file. `a_reading_uses_this` is the
        // vocabulary's own answer, so a shape added to the reader becomes a name added here rather than a silent
        // hole — see `cpp_parser::BodyShape`.
        let mut bodied: Vec<String> = Vec::new();
        for summary in self.index.summaries() {
            let Some(source) = sources.get(&summary.path) else {
                continue;
            };
            for fact in &summary.macros {
                let used = fact.kind.is_definition()
                    && fact.body_range.is_some_and(|range| {
                        source
                            .get(range.start_offset..range.start_offset + range.length)
                            .is_some_and(|body| {
                                cpp_parser::shape_of_a_body(body).a_reading_uses_this()
                            })
                    });
                if used && !bodied.contains(&fact.name) {
                    bodied.push(fact.name.clone());
                }
            }
        }

        if bodied.is_empty() {
            return 0;
        }

        // One cache of parsed `#define`s for the whole pass: a definition does not depend on which file is being
        // read, so the feed costs one parse per definition rather than one per definition per file (B95).
        let mut definitions = crate::summary::MacroDefinitions::default();
        let mut re_read = 0usize;

        for path in indexed {
            let Some(source) = sources.get(path) else {
                continue;
            };
            if !mentions_one_of(source, &bodied) {
                continue;
            }

            // The environment, built from the closure **as the walk found it** — the same call a query-time
            // consumer makes, so the reading and its check cannot disagree about what the evidence was.
            let environment = {
                let index = &self.index;
                let sources = &sources;
                let Some(summary) = index.summary(path) else {
                    continue;
                };

                let evidence = crate::summary::macros_from_the_closure_with_bodies(
                    summary,
                    |wanted| {
                        Some((
                            index.summary(wanted)?,
                            sources.get(wanted).map(String::as_str).unwrap_or(""),
                        ))
                    },
                    index.macros(),
                    &mut definitions,
                );

                cpp_parser::MacroEnvironment::from_included_macros(evidence.macros)
                    .with_bodies_in_force(evidence.conditional_bodies)
            };

            let key = SummaryKey::new(content_hash(source), self.context_hash(path));
            let rebuilt = FileIndexer::new(&self.files, &self.config)
                .with_macro_bodies(&environment)
                .index(path, source, key);

            self.stats.rebuilt += 1;
            re_read += 1;

            if truncated || has_unresolved_includes(&rebuilt) {
                self.stats.unstored += 1;
            } else {
                let _ = write_summary(&rebuilt, &self.cache);
            }

            self.index.insert(rebuilt);
        }

        re_read
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

        bytes.push(b'|');
        bytes.extend_from_slice(
            normalize_path(path.parent().unwrap_or(Path::new(".")), cfg!(windows)).as_bytes(),
        );

        fnv1a64(&bytes)
    }
}

/// Does this text mention any of `names` as a whole word?
///
/// The filter [`SummaryStore::re_read_what_a_body_changes`] uses, and it is deliberately a **text** scan rather
/// than a parse: a parse is the thing the filter exists to avoid, and the question it answers ("could this file's
/// reading change?") only ever needs a sound over-approximation. A name inside a comment or a string literal counts
/// as a mention, which costs one re-parse of a file whose summary comes out identical; a name that is *not* in the
/// text cannot be invoked, so no file that could change is skipped.
fn mentions_one_of(text: &str, names: &[String]) -> bool {
    text.split(|character: char| !(character.is_alphanumeric() || character == '_'))
        .any(|word| names.iter().any(|name| name == word))
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

    #[test]
    fn coming_back_to_earlier_text_finds_the_entry_again() {
        // The branch-switch property, in miniature: two texts, then back to the first. The third lookup is a hit
        // because the key names the *text* — the entry for the first one was never overwritten, and nothing had to
        // remember which texts have been seen. `docs/index-design.md` sets a hit rate of >90% for this case.
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
        // (see `docs/grammar-gaps.md` B61), and a cache that confused the two would answer with the wrong facts —
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
        // project cheap the second time, and it is the number `docs/std-library.md` measures at 11 ms for 185
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
        // that with room — see `docs/std-library.md`.
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

