    use super::SummaryStore;
    use crate::file::paths::MemoryFiles;
    use crate::include::config::CompilerConfig;
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

        // **The file cache, on purpose**: these tests are about the shape of the directory, and the database has
        // its own tests (`cache_db::tests`) and its own measurement in `docs/refactor-cache-and-startup.md`.
        let store = SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone())
            .without_the_cache_database();
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
        let mut reopened =
            SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
        let second = reopened
            .get(Path::new("/p/widget.h"))
            .expect("the file reads");

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
                _ => format!(
                    "#include \"f{}.h\"\nstruct S{number} {{ int v; }};\n",
                    number.max(2) - 1
                ),
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
        assert_eq!(
            (a.reused + a.rebuilt, a.unstored),
            (b.reused + b.rebuilt, b.unstored)
        );
        assert_eq!(together.index().len(), one_by_one.index().len());
        for summary in one_by_one.index().summaries() {
            let other = together
                .index()
                .summary(&summary.path)
                .expect("the same files are indexed");
            assert_eq!(
                other.declarations, summary.declarations,
                "{:?}",
                summary.path
            );
            assert_eq!(other.includes, summary.includes, "{:?}", summary.path);
        }

        // What the workers wrote is what a later run reads back: every stored entry decodes, and none is torn.
        let mut reopened =
            SummaryStore::with_provider(&root_together, CompilerConfig::default(), files.clone());
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
        let source =
            crate::FileProvider::read(&files, Path::new("/p/a.h")).expect("the fixture is there");

        let scanned =
            crate::FileIndexer::new(&files, &config).includes_of(Path::new("/p/a.h"), &source);
        let recorded: Vec<std::path::PathBuf> = store
            .get(Path::new("/p/a.h"))
            .expect("reads")
            .includes
            .iter()
            .filter_map(|include| include.resolved.clone())
            .collect();

        assert_eq!(scanned, recorded);
        assert_eq!(
            scanned.len(),
            2,
            "b.h and the one behind `#if 0`; the missing one is not resolved: {scanned:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **Reading a closure together reads the closure** — every file once, none the caller did not want, and never
    /// more than the budget — and what it prepared commits to the index a one-by-one walk builds.
    #[test]
    fn preparing_a_closure_reaches_every_file_it_includes_within_its_limits() {
        let mut files = MemoryFiles::new().with_file(
            "/p/main.cpp",
            "#include \"h0.h\"\n#include \"h1.h\"\nint main;\n",
        );
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
        let mut reached: Vec<String> = everything
            .iter()
            .map(|(path, _)| path.to_string_lossy().into_owned())
            .collect();
        reached.sort();
        reached.dedup();
        assert_eq!(
            reached.len(),
            31,
            "main and thirty headers, each once: {}",
            everything.len()
        );
        assert_eq!(everything.len(), 31, "and no file was prepared twice");

        let limited = store.prepare_closure(std::slice::from_ref(&main), |_| true, 10);
        assert!(
            limited.len() <= 10,
            "the budget bounds the call: {}",
            limited.len()
        );
        assert!(
            limited.iter().any(|(path, _)| path == &main),
            "the root is always prepared"
        );

        let refusing = store.prepare_closure(
            std::slice::from_ref(&main),
            |path| !path.ends_with("h1.h"),
            1000,
        );
        assert!(
            refusing
                .iter()
                .all(|(path, _)| !path.ends_with("h1.h") || path == &main)
        );

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

    fn store_named(
        name: &str,
        files: &MemoryFiles,
    ) -> (SummaryStore<MemoryFiles>, std::path::PathBuf) {
        store(name, files)
    }

    #[test]
    fn coming_back_to_earlier_text_finds_the_entry_again() {
        // The branch-switch property, in miniature: two texts, then back to the first. The third lookup is a hit
        // because the key names the *text* — the entry for the first one was never overwritten, and nothing had to
        // remember which texts have been seen. sets a hit rate of >90% for this case.
        let root = std::env::temp_dir()
            .join("cppls-store-tests")
            .join("branch-switch");
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
        let edited =
            MemoryFiles::new().with_file("/p/widget.h", "struct Widget { int a; int b; };\n");
        let mut second =
            SummaryStore::with_provider(&root, CompilerConfig::default(), edited.clone());
        let summary = second
            .get(Path::new("/p/widget.h"))
            .expect("the file reads")
            .clone();

        assert_eq!(
            summary.declarations.len(),
            3,
            "the class and both of its members — the edited text, not the stored one"
        );
        assert_eq!(
            second.stats().reused,
            0,
            "the stored entry names other text"
        );
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
        let mut second =
            SummaryStore::with_provider(&root, CompilerConfig::default(), with_header.clone());
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
            .with_file(
                "/p/main.cpp",
                "#include \"widget.h\"\nvoid f() { Widget w; }\n",
            );
        let (mut store, root) = store("unindexed-include", &files);

        store.get(Path::new("/p/main.cpp")).expect("the file reads");
        assert_eq!(store.stats().unstored, 0);

        let mut reopened =
            SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
        reopened
            .get(Path::new("/p/main.cpp"))
            .expect("the file reads");
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
            assert!(
                store.get(Path::new(path)).is_some(),
                "the file reads: {path}"
            );
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

        let mut reopened =
            SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
        reopened
            .get(Path::new("/p/main.cpp"))
            .expect("the file reads");

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
        assert_eq!(
            built.includes[0].resolved.as_deref(),
            Some(Path::new("/p/widget.h"))
        );

        // The header is gone, and nothing tells the store so: no event, no invalidation, no forget. The entry is
        // still there, under a key that has not moved.
        let without = MemoryFiles::new().with_file(
            "/p/main.cpp",
            "#include \"widget.h\"\nvoid f() { Widget w; }\n",
        );
        let mut reopened =
            SummaryStore::with_provider(&root, CompilerConfig::default(), without.clone());
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
            assert!(
                store.get(Path::new(path)).is_some(),
                "the file reads: {path}"
            );
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

        let mut reopened =
            SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
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
            store
                .index()
                .summary(Path::new("/p/a.cpp"))
                .map(|held| held.key),
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

    use super::{IncludeBudget, NotIndexedReason, StoreStats};

    /// A translation unit with a two-level include chain, and nothing missing.
    fn chain() -> MemoryFiles {
        MemoryFiles::new()
            .with_case_insensitive(false)
            .with_file(
                "/p/main.cpp",
                "#include \"middle.h\"\nint main() { return 0; }\n",
            )
            .with_file(
                "/p/middle.h",
                "#include \"deep.h\"\nstruct Middle { Deep d; };\n",
            )
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
            indexed(&index)
                .iter()
                .filter(|path| *path == "/p/shared.h")
                .count(),
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
        assert_eq!(
            index.stats.rebuilt, 2,
            "and two parses, because two of them are the same text"
        );
        assert_eq!(
            index.stats.reused, 1,
            "the twin was answered by the entry the first one wrote"
        );

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

        assert_eq!(
            indexed(&index).len(),
            3,
            "the rest of the closure was still indexed"
        );
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
        let root = std::env::temp_dir()
            .join("cppls-store-tests")
            .join("closure-warm");
        let _ = std::fs::remove_dir_all(&root);

        let cold = {
            let mut store =
                SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
            store.index_includes_from(Path::new("/p/main.cpp"), IncludeBudget::default())
        };
        assert_eq!(cold.stats.rebuilt, 3);

        let mut reopened =
            SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
        let warm = reopened.index_includes_from(Path::new("/p/main.cpp"), IncludeBudget::default());

        assert_eq!(
            indexed(&warm),
            indexed(&cold),
            "the same files, in the same order"
        );
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
