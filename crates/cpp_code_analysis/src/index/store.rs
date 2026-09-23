//! Deciding **when** to build a summary, and what to do with the one on disk.
//!
//! [`crate::index`] builds one summary from one file's text. This module is the layer that decides whether to ask
//! it to: it holds the project root, the compiler configuration and the provider, looks for an existing summary
//! under the key the file currently has, and rebuilds when there is none.
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
//! # Why the key is the whole answer
//!
//! Nothing here compares a file against a stored list of what it contained. The key *is* the comparison: a
//! summary is stored under a hash of the text, the compilation context and the macro environment, so "is the
//! stored one still right?" is "does it have the name this file's contents compute to?". That is what makes a
//! branch switch cheap — the files come back with the contents the cache was built from, so their keys come back
//! too, and the summaries are found without being understood.
//!
//! The two parts of that key which need a decision are [`SummaryStore::context_hash`] (the configuration, and the
//! directory, because `#include "widget.h"` means the one beside me) and [`SummaryStore::macro_environment`] (the
//! files that define macros on the way in, seed included).
//!
//! # The one case that must not be cached
//!
//! A file whose macro environment is **incomplete** — because something it includes was never indexed — has a key
//! that does not account for macros nobody has seen. Writing a summary under it would produce an entry that looks
//! valid and is reused in a context where it is wrong. So it is built, returned, and not written; the caller is
//! told through [`StoreStats::unkeyed`], and the reason is the same one `docs/index-design.md` gives for
//! `Unknown` being a first-class answer: a wrong entry is worse than a missing one.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::cache::{SummaryKey, content_hash, fnv1a64};
use crate::include::config::CompilerConfig;
use crate::include::paths::{DiskFiles, FileProvider, normalize_path};
use crate::index::project::ProjectIndex;
use crate::index::{FileIndexer, read_summary, write_summary};
use crate::summary::FileSummary;

/// A project's summaries on disk and in memory, and the rule for when each is rebuilt.
pub struct SummaryStore<'a, F: FileProvider = DiskFiles> {
    /// The project root. The cache lives under it, because `docs/index-design.md` puts it there on purpose: it
    /// travels with a checkout, so CI gets the same warm cache a developer has.
    root: PathBuf,
    config: CompilerConfig,
    files: &'a F,
    index: ProjectIndex,
    /// Every path the store has read, and the content hash of the text it read.
    ///
    /// Held for two reasons, and the second is the one that matters:
    ///
    /// * a file that includes *itself*, or that is reached again through a cycle, is answered from what was just
    ///   read rather than treated as unknown;
    /// * a file's macro environment has to say whether the file itself defines macros, and the file being read has
    ///   not been summarised yet when that question is asked. What *is* known is its text — and a text that was
    ///   just parsed and found to contain no `#define` defines nothing.
    ///
    /// The order the two are used in is why this is a content hash rather than a flag. Without it the first key of
    /// every file would be incomplete, nothing would ever be written, and the cache would never hit — which is
    /// what the first version did.
    ///
    /// Never removed: a file that is deleted cannot have its summary reused, and the entry is one hash.
    content: HashMap<String, u64>,
    stats: StoreStats,
}

/// What the store did, so that a caller can see the cache working.
///
/// A counter rather than a log line, because the four numbers `docs/index-design.md` says to measure before
/// building more — build time, resident memory, cache hit rate, and the proportion of unanswerable queries —
/// all start here, and a number nobody can read is how a cache quietly stops hitting.
///
/// The counters are about **the disk**, not about calls: a `get` that needed no lookup at all is in neither
/// `reused` nor `rebuilt`, and `hit_rate` is the fraction of lookups the disk answered. That distinction is here
/// rather than in a comment on `get` because it is the thing a reader of these numbers will get wrong.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreStats {
    /// Summaries read from disk.
    pub reused: usize,
    /// Summaries built because nothing usable was stored.
    pub rebuilt: usize,
    /// Summaries built and deliberately **not** written, because their macro environment was incomplete.
    pub unkeyed: usize,
}

impl StoreStats {
    /// The fraction of lookups that were served from disk, or `None` when nothing was looked up.
    ///
    /// `None` rather than `0.0`: "no lookups yet" and "every lookup missed" are different states, and the second
    /// is the one worth a warning.
    pub fn hit_rate(&self) -> Option<f64> {
        let total = self.reused + self.rebuilt;
        (total > 0).then(|| self.reused as f64 / total as f64)
    }
}

impl<'a> SummaryStore<'a, DiskFiles> {
    /// A store over a project on disk.
    pub fn open(root: impl Into<PathBuf>, config: CompilerConfig) -> SummaryStore<'static, DiskFiles> {
        // `DiskFiles` is a unit struct, so a `'static` borrow of one is free and the default type parameter is
        // the ordinary case.
        static DISK: DiskFiles = DiskFiles;

        SummaryStore {
            root: root.into(),
            config,
            files: &DISK,
            index: ProjectIndex::new(),
            content: HashMap::new(),
            stats: StoreStats::default(),
        }
    }
}

impl<'a, F: FileProvider> SummaryStore<'a, F> {
    /// A store over any provider, which is what makes the whole layer testable without a filesystem.
    pub fn with_provider(
        root: impl Into<PathBuf>,
        config: CompilerConfig,
        files: &'a F,
    ) -> SummaryStore<'a, F> {
        SummaryStore {
            root: root.into(),
            config,
            files,
            index: ProjectIndex::new(),
            content: HashMap::new(),
            stats: StoreStats::default(),
        }
    }

    pub fn stats(&self) -> StoreStats {
        self.stats
    }

    /// Every summary loaded so far — the project as the store currently understands it.
    pub fn index(&self) -> &ProjectIndex {
        &self.index
    }

    /// The summary for `path` **as it is now**, from disk when it is still right.
    ///
    /// `None` when the file cannot be read: an include that did not resolve, a deleted file, a path that is not a
    /// file. That is not an error a cache should report — the caller asked about a file that is not there, and
    /// the answer is that it has nothing to say about it.
    ///
    /// # Why the file is built before its key is known
    ///
    /// A file's macro environment includes **its own** `#define`s, and those are facts about its text — so the
    /// question "what key does this file have?" cannot be answered before the file has been read and summarised.
    /// The order is therefore:
    ///
    /// ```text
    /// 1. read the text, and record what text this path now holds
    /// 2. build the summary — which also says whether the file defines any macros at all
    /// 3. complete the environment with that, and compute the key
    /// 4. look for a stored summary under that key; if there is none, file this one under it and write it
    /// ```
    ///
    /// Step 2 cannot be skipped by hoping for a cache hit, because the key *is* its output. What pays for it is
    /// step 4: a hit means the file's *dependants* are not rebuilt — the expensive part in a project where one
    /// header has fifty includers.
    ///
    /// The build in step 2 is the only one. Its key comes back with a placeholder for the two parts that describe
    /// the *compilation* — which is what the caller knows and the builder cannot — so those are filled in on the
    /// summary that was just built rather than by building it a second time. The first version did build it twice,
    /// which made every cold file cost two parses: the one number `docs/index-design.md` says to measure first.
    pub fn get(&mut self, path: &Path) -> Option<&FileSummary> {
        let source = self.files.read(path)?;

        // The path's current text is recorded before anything else, so that a file which includes *itself* — or
        // which is reached again through a cycle — is answered from what was just read rather than treated as
        // unknown.
        self.note(path, &source);

        let mut summary =
            FileIndexer::new(self.files, &self.config).index(path, &source, SummaryKey::new(0, 0, 0));

        match self.environment_of(path, &summary) {
            Environment::Complete(environment) => {
                let key = SummaryKey::new(
                    summary.key.content_hash,
                    self.context_hash(path),
                    environment.hash(),
                );

                if let Ok(stored) = read_summary(&key.path_under(&self.root))
                    && stored.key == key
                {
                    self.stats.reused += 1;
                    // Filed under the path that asked, which is not necessarily the one in the entry — see
                    // `ProjectIndex::insert_at`.
                    self.index.insert_at(path, stored);
                    return self.index.summary(path);
                }

                self.stats.rebuilt += 1;
                summary.key = key;
                self.index.insert(summary.clone());

                // A failed write is not a failed lookup: the answer is in hand and in the index. Reporting it
                // would turn a read-only checkout — a perfectly ordinary way to work — into a broken editor.
                let _ = write_summary(&summary, &self.root);
            }
            // An incomplete macro environment: the summary is handed back and deliberately **not** written. See
            // the module documentation — an entry under a key that omits unseen macros is one that would be
            // reused where it is wrong. It keeps the placeholder key, which is why `unkeyed` is counted: the key
            // it holds is not a name anything can be stored under.
            Environment::Incomplete => {
                self.stats.unkeyed += 1;
                self.index.insert(summary);
            }
        }

        self.index.summary(path)
    }

    /// The macro environment of `path` plus everything it includes, completed with `summary`'s own macros.
    ///
    /// This is the second phase of [`SummaryStore::get`]: [`SummaryStore::macro_environment`] walks the includes
    /// and knows nothing about the file itself, and this adds what the freshly built summary says about it.
    fn environment_of(&self, path: &Path, summary: &FileSummary) -> Environment {
        let (environment, complete) = self.macro_environment(path, summary);

        if complete {
            Environment::Complete(environment)
        } else {
            Environment::Incomplete
        }
    }

    /// The macro environment of `path` **and everything it includes**, and whether all of them were known.
    ///
    /// The **seed is part of it**, which is not decoration: a file's own `#define`s are macros its includers see,
    /// and — the reason this was found by a test rather than by thinking — two files that include the same header
    /// and define nothing have *the same environment* unless they are in it themselves. Leaving the seed out made
    /// `middle.h` and `widget.h` share a cache key whenever their own bodies contributed nothing to it, and the
    /// second file read the first file's summary: a wrong answer, silently, for a key collision that no
    /// content hash was distinguishing because the content hashes of two files are different but neither was in
    /// the key.
    ///
    /// `the_file` is the summary just built for `path`, which is what lets the seed be included before it has
    /// been indexed.
    ///
    /// A path that has been read but not summarised counts as **accounted for**: its content is in the environment
    /// set. Whether it defines a macro is a fact its summary would carry, and the caller is building exactly that
    /// summary — so reporting `Incomplete` here would make the first key of every file unanswerable and the cache
    /// would never hit.
    ///
    /// What does **not** count as accounted for is an include the resolver could not find. That is a different
    /// fact from "not read": nobody knows what is in that file, not even this store, so the environment is
    /// genuinely incomplete and the summary must not be cached under it.
    fn macro_environment(&self, path: &Path, the_file: &FileSummary) -> (crate::MacroEnvironment, bool) {
        let mut environment = crate::MacroEnvironment::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut complete = true;
        let mut pending = vec![normalize_path(path, cfg!(windows))];

        while let Some(current) = pending.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }

            // The seed is answered from the summary in hand rather than from the index, because it has not been
            // indexed yet — it *is* what is being keyed.
            let summary = if current == normalize_path(path, cfg!(windows)) {
                Some(the_file)
            } else {
                self.summary_of(Path::new(&current), &current)
            };

            match summary {
                Some(summary) => {
                    complete &= !has_unresolved_includes(summary);
                    environment.add(summary.key.content_hash, !summary.macros.is_empty());
                    pending.extend(include_paths(summary));
                }
                // Read at some point, so its contents are part of the environment whatever else is unknown —
                // but nothing further is known about it, so the walk cannot go on through it.
                None => {
                    if let Some(read_hash) = self.content.get(&current).copied() {
                        environment.add(read_hash, true);
                    }
                    complete = false;
                }
            }
        }

        (environment, complete)
    }

    /// The held summary for a path, when it describes the text the store last read for it.
    ///
    /// The content check is what keeps a stale summary from answering: a file edited since it was indexed has a
    /// different hash, and its old facts describe a text that is no longer there.
    fn summary_of(&self, path: &Path, normalized: &str) -> Option<&FileSummary> {
        let read_hash = self.content.get(normalized)?;
        let summary = self.index.summary(path)?;

        (summary.key.content_hash == *read_hash).then_some(summary)
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

    /// Note that a path's includes are known.
    ///
    /// Called from every path that produces or loads a summary, which is the definition of "seen": the file has
    /// been read, so the macro environment of anything that includes it can account for it.
    fn note(&mut self, path: &Path, source: &str) {
        self.content
            .insert(normalize_path(path, cfg!(windows)), content_hash(source));
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
    /// first one's resolved includes, and a jump into a header it does not include. See [`crate::SummaryKey`].
    ///
    /// The include paths are hashed **as the resolver uses them** — resolved against the working directory — so
    /// the working directory is accounted for exactly to the extent that it changes what is found, and a caller
    /// that runs the compiler from somewhere else without changing any `-I` keeps the cache.
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

        // The standard and the target: switching to `c++23` recompiles the file, but it does not change which
        // files it finds, and putting them here keeps the whole context one number.
        for text in [&self.config.standard, &self.config.target] {
            if let Some(text) = text {
                bytes.extend_from_slice(text.as_bytes());
            }
            bytes.push(0);
        }

        bytes.push(b'|');
        bytes.extend_from_slice(
            normalize_path(path.parent().unwrap_or(Path::new(".")), cfg!(windows)).as_bytes(),
        );

        fnv1a64(&bytes)
    }
}

/// The normalized paths a summary's includes resolved to.
fn include_paths(summary: &FileSummary) -> Vec<String> {
    summary
        .includes
        .iter()
        .filter_map(|include| include.resolved.as_ref())
        .map(|resolved| normalize_path(resolved, cfg!(windows)))
        .collect()
}

/// Does this summary write an `#include` whose target was never found?
///
/// The question the environment walk asks about a file it *has* read: its own text is accounted for, and what it
/// pulls in is not.
fn has_unresolved_includes(summary: &FileSummary) -> bool {
    summary.includes.iter().any(|include| include.resolved.is_none())
}

/// A file's macro environment, once the file itself has been accounted for.
///
/// Private because it is an internal step rather than an answer: a caller asks [`SummaryStore::get`] for a summary,
/// and this is the phase in between — the environment walk with the seed's own macros folded in.
enum Environment {
    Complete(crate::MacroEnvironment),
    /// Something the file reaches was never read, so its macros are not accounted for.
    Incomplete,
}

#[cfg(test)]
mod tests {
    use super::SummaryStore;
    use crate::include::config::CompilerConfig;
    use crate::include::paths::MemoryFiles;
    use std::path::Path;

    /// A store over a few files in memory, with a cache directory of its own.
    ///
    /// A fresh directory per test, because the cache is keyed by *content* rather than by path: two tests with
    /// the same fixture text would otherwise share an entry, and a test would pass because of another test's
    /// leftovers.
    fn store<'a>(
        name: &str,
        files: &'a MemoryFiles,
    ) -> (SummaryStore<'a, MemoryFiles>, std::path::PathBuf) {
        let root = std::env::temp_dir().join("cppls-store-tests").join(name);
        let _ = std::fs::remove_dir_all(&root);

        let store = SummaryStore::with_provider(&root, CompilerConfig::default(), files);
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
        let mut reopened = SummaryStore::with_provider(&root, CompilerConfig::default(), &files);
        let second = reopened.get(Path::new("/p/widget.h")).expect("the file reads");

        assert_eq!(second.declarations, first.declarations);
        assert_eq!(reopened.stats().reused, 1);
        assert_eq!(reopened.stats().rebuilt, 0);
        assert_eq!(reopened.stats().hit_rate(), Some(1.0));

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
        let mut second = SummaryStore::with_provider(&root, CompilerConfig::default(), &edited);
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
        assert_eq!(second.stats().unkeyed, 0, "the environment was complete");
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
        // the caller's key verbatim, and `get` has to pass a *placeholder* — it cannot know a file's macro
        // environment before the file has been built. So every summary in every project recorded content hash
        // `0`, all of them were written to one entry, and each file's environment hashed a set of zeroes. The
        // cache appeared to work: one file was stored, one file was reused. It was answering with the wrong file.
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
            one.key.path_under(&root),
            two.key.path_under(&root),
            "two files, two entries"
        );

        // And both are really on disk, each holding the facts of the file that wrote it — which is what a
        // collision would have made impossible to notice from the outside.
        let stored_one = super::read_summary(&one.key.path_under(&root)).expect("written");
        let stored_two = super::read_summary(&two.key.path_under(&root)).expect("written");
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
        let mut second = SummaryStore::with_provider(&root, configured, &files);
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
        let one = SummaryStore::with_provider("r", CompilerConfig::default(), &files);
        let two = SummaryStore::with_provider("r", CompilerConfig::default(), &files);

        assert_eq!(
            one.context_hash(Path::new("/p/a.cpp")),
            two.context_hash(Path::new("/p/a.cpp"))
        );

        let different = SummaryStore::with_provider(
            "r",
            CompilerConfig::default().with_define(crate::CommandLineMacro::defined("A")),
            &files,
        );
        assert_ne!(
            one.context_hash(Path::new("/p/a.cpp")),
            different.context_hash(Path::new("/p/a.cpp"))
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
                &files,
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
                &files,
            )
        };
        assert_eq!(
            absolute("build-one").context_hash(Path::new("/p/a.cpp")),
            absolute("build-two").context_hash(Path::new("/p/a.cpp")),
            "an absolute -I does not care where the compiler ran"
        );
    }

    #[test]
    fn a_file_whose_includes_are_unindexed_gets_no_key() {
        let files = MemoryFiles::new().with_file("/p/main.cpp", "#include \"missing.h\"\nint x;\n");
        let (mut store, root) = store("incomplete", &files);

        let summary = store.get(Path::new("/p/main.cpp")).expect("the file reads");
        assert_eq!(summary.declarations.len(), 1, "the summary is still built");
        assert_eq!(store.stats().unkeyed, 1);
        assert_eq!(store.stats().reused, 0);
        assert_eq!(store.stats().rebuilt, 0);

        // And nothing was written: a later store has nothing to reuse, so it builds again rather than reading an
        // entry keyed on an environment that omitted whatever `missing.h` would have defined.
        let mut second = SummaryStore::with_provider(&root, CompilerConfig::default(), &files);
        second.get(Path::new("/p/main.cpp")).expect("the file reads");
        assert_eq!(second.stats().reused, 0);
        assert_eq!(second.stats().unkeyed, 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_key_is_the_same_for_the_same_contents_in_the_same_directory() {
        // What makes a branch switch cheap: a file that comes back to a content the cache has seen is found
        // without being parsed. `a.cpp` and `b.cpp` here are two files, and they share one entry — the dir is the
        // same, so the text determines everything the summary says.
        let files = MemoryFiles::new()
            .with_file("/p/a.cpp", "int x;\n")
            .with_file("/p/b.cpp", "int x;\n");
        let (mut store, root) = store("content-addressed", &files);

        let one = store.get(Path::new("/p/a.cpp")).expect("the file reads").clone();
        assert_eq!(one.path, Path::new("/p/a.cpp"));

        let two = store.get(Path::new("/p/b.cpp")).expect("the file reads").clone();
        assert_eq!(two.key, one.key, "the same text is the same entry");
        assert_eq!(store.stats().reused, 1, "so the second one is a hit");
        assert_eq!(store.stats().rebuilt, 1);

        // And each path keeps its own name: a reused summary records the path of whoever wrote it, so the entry
        // has to be refiled under the path that asked — otherwise every fact in it points at the wrong file.
        assert_eq!(two.path, Path::new("/p/b.cpp"));
        for path in ["/p/a.cpp", "/p/b.cpp"] {
            assert_eq!(
                store.index().summary(Path::new(path)).map(|held| held.path.clone()),
                Some(std::path::PathBuf::from(path))
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_same_text_in_two_directories_does_not_share_an_entry() {
        // The bug this pins, found by asking what a second, apparently harmless entry point would have to know: a
        // summary holds not only what the text *says* but where its includes *resolved to*, and
        // `#include "shared.h"` means the one beside me. With the directory left out of the key these two files
        // were one entry, so `sub/b.h` was handed `a.h`'s resolution of `shared.h` and would have offered a jump
        // into a header it does not include.
        let files = MemoryFiles::new()
            .with_file("/p/shared.h", "struct Outside { int x; };\n")
            .with_file("/p/sub/shared.h", "struct Inside { int y; };\n")
            .with_file("/p/a.h", "#include \"shared.h\"\n")
            .with_file("/p/sub/b.h", "#include \"shared.h\"\n");

        let (mut store, root) = store("directory-context", &files);

        // The headers first, so that both includers have a complete macro environment and are actually keyed —
        // an unkeyed summary is not compared by its key at all, and the test would pass for the wrong reason.
        for path in ["/p/shared.h", "/p/sub/shared.h", "/p/a.h", "/p/sub/b.h"] {
            assert!(store.get(Path::new(path)).is_some(), "the file reads: {path}");
        }

        let outside = store.index().summary(Path::new("/p/a.h")).expect("held").clone();
        let inside = store.index().summary(Path::new("/p/sub/b.h")).expect("held").clone();

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
    fn a_file_that_includes_another_is_indexed_after_it() {
        // The narrowest case of the invalidation fixture: one header that includes another. It is here because
        // that fixture failed for this reason and no other, and a two-file test makes the state machine visible.
        let files = MemoryFiles::new()
            .with_file("/invalidate-b/widget.h", "struct Widget { int size; };\n")
            .with_file("/invalidate-b/middle.h", "#include \"widget.h\"\n");

        let (mut store, root) = store("include-after", &files);

        assert!(
            store.get(Path::new("/invalidate-b/widget.h")).is_some(),
            "the included header is indexed first"
        );
        assert!(
            store.get(Path::new("/invalidate-b/middle.h")).is_some(),
            "and the file that includes it is indexed after"
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

        // Index them in an order that lets the environment be complete: a header before its includers.
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
        // directory, and get the same answers without rebuilding anything.
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

        let found = store
            .index()
            .definition("Widget", Path::new("/p/main.cpp"));
        assert!(
            matches!(found, crate::Known::Yes(_)),
            "the class is visible through the include: {found:?}"
        );

        let mut reopened = SummaryStore::with_provider(&root, CompilerConfig::default(), &files);
        for path in ["/p/config.h", "/p/widget.h", "/p/main.cpp"] {
            reopened.get(Path::new(path)).expect("the file reads");
        }

        assert_eq!(reopened.stats().rebuilt, 0, "nothing needed rebuilding");
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
    fn an_unkeyed_file_is_reported_rather_than_quietly_stored() {
        // The contract is that a file whose environment is incomplete is *counted* rather than stored silently,
        // and that the count is the only evidence a caller gets — there is no entry on disk to inspect.
        let files = MemoryFiles::new().with_file("/p/main.cpp", "#include \"nope.h\"\n");
        let (mut store, root) = store("outcome", &files);

        assert!(store.get(Path::new("/p/main.cpp")).is_some());
        assert_eq!(store.stats().unkeyed, 1);
        assert_eq!(
            store.stats().hit_rate(),
            None,
            "an unkeyed file is not a cache miss: the cache was never asked"
        );

        // Nothing under the root that a summary could have been written to: the directory the write would have
        // created is not there.
        assert!(
            !root.join(crate::CACHE_DIRECTORY).exists(),
            "an unkeyed summary must not reach the disk"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
