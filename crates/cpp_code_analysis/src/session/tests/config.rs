//! **Config** — the project's own configuration file

use crate::file::paths::DiskFiles;
use crate::include::config::CompilerConfig;
use crate::index::watch::WatchFilter;
use crate::session::{OpenDocuments, Session, SessionFiles};

use super::*;

// -------------------------------------------------------------------------------------------
// The project's own configuration file
// -------------------------------------------------------------------------------------------

#[test]
pub(crate) fn a_project_configuration_narrows_the_scan_and_widens_the_source_list() {
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
pub(crate) fn the_projects_flags_win_over_the_databases_and_extra_args_add_to_the_winner() {
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

/// **The standard a database states is the project's own, so the seed carries it** — and a project whose entries
/// disagree is not one standard.
///
/// The seed is the macro table every cook starts from. Without a standard it has no `__cplusplus` and no
/// `_MSVC_LANG`, and MSVC's headers — which put everything inside `#if _HAS_CXX20` — render as their fallback stub.
/// Measured on a `<ranges>` fixture: the cook of the header was **12 bytes** (`#pragma once`, zero declarations) and
/// `std::views` did not exist, while the rendering with the standard present was **1.8 MB and 22 345 declarations**.
#[test]
pub(crate) fn the_database_states_the_projects_standard_and_the_seed_carries_it() {
    let project = Project::new("config-standard-seed");
    project.write("main.cpp", "int main() { return 0; }\n");
    let root = project.root.to_string_lossy().replace('\\', "/");
    // MSVC's spelling, which is what a CMake MSVC build writes.
    project.write(
        "compile_commands.json",
        &format!(
            "[\n  {{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"cl.exe\", \"/std:c++latest\", \"/c\", \"{root}/main.cpp\"]}}\n]\n"
        ),
    );

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let session = Session::open(&project.root, providers, WatchFilter::new(&project.root));
    assert_eq!(
        session.config().standard.as_deref(),
        Some("c++latest"),
        "the session's own configuration — the one the seed is built from — states the database's standard"
    );

    // And a header, which no entry names, is read with it too.
    assert_eq!(
        session
            .config_for(&project.root.join("some_header.hpp"))
            .standard
            .as_deref(),
        Some("c++latest"),
        "a file with no command inherits the project's one standard"
    );
}

#[test]
pub(crate) fn removing_and_adding_a_flag_is_one_pass_in_that_order() {
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

    assert_eq!(
        session
            .config_for(&project.root.join("main.cpp"))
            .standard
            .as_deref(),
        Some("c++20")
    );
    let per_file = session.config_for(&project.root.join("main.cpp"));
    let defines: Vec<&str> = per_file
        .defines
        .iter()
        .map(|define| define.name.as_ref())
        .collect();
    assert_eq!(defines, ["KEEP"], "a flag nobody removed still applies");
}

/// A database written somewhere no convention predicts, named by the project's own file.
#[test]
pub(crate) fn the_database_can_be_somewhere_no_convention_predicts() {
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
pub(crate) fn the_cache_goes_where_the_project_says_and_the_filter_follows_it() {
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

    // **The cache is one database file under the configured directory**, not a tree of summaries. The property this
    // test exists for is that the *name* has three agreeing readers — the store that writes, the filter that ignores
    // its events, and the report a user reads — and a directory is what all three join the name to.
    assert!(
        project.root.join(".cppls-cache").is_dir(),
        "the cache is under the configured directory, not the default: {}",
        project.root.join(".cppls-cache").display()
    );
    assert!(
        !project.root.join(crate::CACHE_DIRECTORY).exists(),
        "and the default directory was not created at all"
    );
}

#[test]
pub(crate) fn the_sections_a_language_server_reads_come_from_the_same_parse() {
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
pub(crate) fn a_configuration_file_with_a_typo_still_analyses_the_project() {
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
    assert!(
        session.project_config().problems[0]
            .message
            .contains("[index]")
    );
    assert_eq!(session.project_config().errors().count(), 1);
}

#[test]
pub(crate) fn a_database_in_a_build_directory_is_found_and_its_flags_reach_the_session() {
    // The whole point of the build-system half: a project that exports its database into `build/` must be
    // analysed with *those* flags and *that* file list, without anybody naming the path. This is the test that
    // fails if the discovery is written but not wired into the session.
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
    let per_file = session.config_for(&project.root.join("main.cpp"));
    let defines: Vec<&str> = per_file
        .defines
        .iter()
        .map(|define| define.name.as_ref())
        .collect();
    assert_eq!(defines, ["FROM_THE_BUILD_DIRECTORY"]);
    assert_eq!(
        session
            .config_for(&project.root.join("main.cpp"))
            .standard
            .as_deref(),
        Some("c++20")
    );
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
/// Restored after a scripted edit ate this header — the body below is unchanged.
pub(crate) fn a_project_without_a_database_is_analysed_against_the_compilers_defaults() {
    // No database and no `[compile].args`: the analysis is about the compiler's own defaults rather than about
    // the project's build. That *works*, and the problem list says what would make it better — which is the
    // whole reason the list exists.
    let project = Project::new("without-a-database");
    project.write("main.cpp", "int main() { return 0; }\n");

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let session = Session::open(&project.root, providers, WatchFilter::new(&project.root));

    assert!(session.compile_database().is_none());
    assert!(
        session
            .discovery()
            .problems
            .iter()
            .any(|problem| problem.message.contains("no compile database")),
        "the way to do better is spelled out: {:?}",
        session.discovery().problems
    );
}

#[test]
pub(crate) fn a_compile_database_names_the_files_that_are_here_and_the_flags_they_are_built_with() {
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
    let session = Session::open(
        &project.root,
        providers.clone(),
        WatchFilter::new(&project.root),
    );

    let database = session.compile_database().expect("the database is read");
    assert_eq!(database.len(), 2);

    let per_file = session.config_for(&project.root.join("main.cpp"));
    let defines: Vec<&str> = per_file
        .defines
        .iter()
        .map(|define| define.name.as_ref())
        .collect();
    assert_eq!(defines, ["FROM_THE_DATABASE"]);
    assert_eq!(
        session
            .config_for(&project.root.join("main.cpp"))
            .standard
            .as_deref(),
        Some("c++20")
    );

    assert_eq!(project.relative(session.project_files()), ["main.cpp"]);
    assert_eq!(
        session.pending(),
        1,
        "the entry for a file that is not here is not work"
    );
}

#[test]
/// Restored after a scripted edit ate this header — the body below is unchanged.
pub(crate) fn a_define_the_compile_database_carries_decides_a_condition_in_a_file() {
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
    let mut session = Session::open(
        &project.root,
        providers.clone(),
        WatchFilter::new(&project.root),
    );

    // **Both files, because the closure of `main.cpp` is what the query walks.**
    session.advance(64);

    let path = project.root.join("main.cpp");
    let _source = std::fs::read_to_string(&path).expect("the fixture reads");

    // **The asking side, which is how a project nobody has opened gets read.**
    //
    // `advance` reads files into the index and never cooks, and nothing here is open — so no file was queued for
    // cooking and the first ask has no rendering to answer from. That is the deferral, and this is what a real
    // caller does with it: the ask **queues** the file, the pump cooks it, the next ask has it. Written as the
    // two steps rather than one because the first answer is `None` by contract and a test that hid that would be
    // asserting a behaviour the product does not have.
    assert!(
        session.view(&path).is_none(),
        "nothing has read this file as a compiler reads it, and that is what the ask is for"
    );
    session.index_everything();

    let view = session
        .view(&path)
        .expect("the ask queued it and the pump cooked it");

    // **The rendering is the proof, and it is a stronger one than the reference list was.**
    //
    // The chain under test is *flags → configuration → environment → the branch an `#ifdef` takes*, and the
    // endpoint of that chain is the text a compiler would parse. `-DFROM_THE_DATABASE` decides
    // `#ifdef FROM_THE_DATABASE`, which decides whether `feature.h` is reached, which decides what
    // `FEATURE_ONLY` — defined there as `int` — expands to. So a rendering of exactly `int x;` says every link
    // held: the define came from the database, the include was taken, and the macro was substituted.
    assert_eq!(
        view.source.as_ref(),
        "int x;",
        "the -D took the branch, the branch included feature.h, and FEATURE_ONLY became int"
    );

    // **And what this test does NOT prove, said rather than implied.** It shows the chain holds *with* the
    // database's flag. It cannot show it *fails* without it, and the reason is the session's contract rather
    // than an oversight here: [`Session::open`] reads `compile_commands.json` whenever the project has one
    // (`session.rs:66`), so a second session over this same directory necessarily has the same `-D`. A negative
    // half was written and **measured to be worthless** — it rendered `int x;` as well, because the flag was
    // there in both sessions:
    //
    // ```text
    //   with no -D the `#ifdef` is false ...: got "int x;"
    // ```
    //
    // A one-sided assertion is the failure mode this test now names instead of hiding: without a session whose
    // flags differ, `int x;` is consistent with reading the include unconditionally. Building that session means
    // opening a project whose database has no flags — a fixture change, not a new assertion here.
    //
    // **And the reference half was removed rather than repaired.** It asked [`Session::macro_references`] about
    // `FEATURE_ONLY`, which needs a *name at an offset* and gets one from a tree; but a macro is **gone** from a
    // rendering — that is what a rendering is — so the question was a category error and answered
    // `Unknown(UnparsableName)`. A macro question wants the file's own tokens plus the closure's macro facts,
    // and `Session::tokens_of` is the first half of that. What is missing is the entry point, not the reading:
    // see the note on `Session::macro_references`.
}
