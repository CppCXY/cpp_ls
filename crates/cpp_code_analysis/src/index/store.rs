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

    /// The summary for `path` **as it is now**, from disk when there is one.
    ///
    /// `None` when the file cannot be read: a deleted file, a path that is not a file. That is not an error a cache
    /// should report — the caller asked about a file that is not there, and the answer is that it has nothing to
    /// say about it. A file that reads but does not parse is a different matter, and gets a summary like any other:
    /// the parser is tolerant, and a summary of a file with errors is still what a partially working editor needs.
    ///
    /// The lookup is a single `read` of the entry named by [`SummaryStore::context_hash`] and the text's hash, and
    /// it is done before anything is parsed. On a hit the file itself is read and nothing else happens: no parse,
    /// no include search, no write.
    pub fn get(&mut self, path: &Path) -> Option<&FileSummary> {
        let source = self.files.read(path)?;
        let key = SummaryKey::new(content_hash(&source), self.context_hash(path));

        if let Ok(stored) = read_summary(&key.path_under(&self.root))
            && stored.key == key
        {
            self.stats.reused += 1;
            // Filed under the path that asked, which is not necessarily the one recorded in the entry: the key
            // names the text, so two files with the same text share an entry. See `ProjectIndex::insert_at`.
            self.index.insert_at(path, stored);
            return self.index.summary(path);
        }

        self.stats.rebuilt += 1;
        let summary = FileIndexer::new(self.files, &self.config).index(path, &source, key);

        // The one rule about the filesystem: a summary that records a *failed* search must not be stored, because
        // nothing in the key would notice the header appearing. See the module documentation.
        if has_unresolved_includes(&summary) {
            self.stats.unstored += 1;
        } else {
            // A failed write is not a failed lookup: the answer is in hand and in the index. Reporting it would
            // turn a read-only checkout — a perfectly ordinary way to work — into a broken editor.
            let _ = write_summary(&summary, &self.root);
        }

        self.index.insert(summary);
        self.index.summary(path)
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
        bytes.extend_from_slice(
            normalize_path(path.parent().unwrap_or(Path::new(".")), cfg!(windows)).as_bytes(),
        );

        fnv1a64(&bytes)
    }
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

        let mut store = SummaryStore::with_provider(&root, CompilerConfig::default(), &one);
        store.get(Path::new("/p/a.cpp")).expect("the file reads");

        let mut other = SummaryStore::with_provider(&root, CompilerConfig::default(), &two);
        other.get(Path::new("/p/a.cpp")).expect("the file reads");
        assert_eq!(other.stats().rebuilt, 1, "the second text is new");

        let mut back = SummaryStore::with_provider(&root, CompilerConfig::default(), &one);
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
        let mut second = SummaryStore::with_provider(&root, CompilerConfig::default(), &with_header);
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

        let mut reopened = SummaryStore::with_provider(&root, CompilerConfig::default(), &files);
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
    fn a_hit_does_not_even_look_for_the_includes() {
        // "The disk saves the parse" is this module's claim, and `StoreStats` is the module's own accounting of
        // it — a counter that could be moved. This checks the behaviour instead: resolving an `#include` asks the
        // provider whether each candidate exists, so a second `get` that asks nothing is a `get` that never built
        // a summary, because `FileIndexer` is the only thing in the crate that resolves an include.
        let files = MemoryFiles::new()
            .with_file("/p/widget.h", "struct Widget { int size; };\n")
            .with_file(
                "/p/main.cpp",
                "#include \"widget.h\"\nvoid f() { Widget w; }\n",
            );
        let (mut store, root) = store("no-search-on-hit", &files);

        store.get(Path::new("/p/main.cpp")).expect("the file reads");
        let after_first = files.exists_of("/p/widget.h");
        assert!(after_first > 0, "the first get searched for the header");

        let mut reopened = SummaryStore::with_provider(&root, CompilerConfig::default(), &files);
        reopened.get(Path::new("/p/main.cpp")).expect("the file reads");

        assert_eq!(reopened.stats().reused, 1);
        assert_eq!(
            files.exists_of("/p/widget.h"),
            after_first,
            "and the second did not search at all: the entry was the whole answer"
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

        let mut reopened = SummaryStore::with_provider(&root, CompilerConfig::default(), &files);
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
}
