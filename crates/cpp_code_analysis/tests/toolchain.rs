//! Toolchain discovery against the machine it is running on.
//!
//! The unit tests in [`cpp_code_analysis::toolchain`] run with no compiler at all — that is what the injectable
//! `CommandRunner` and `Environment` are for. What they *cannot* answer is whether the shape of a real compiler's
//! answer is the shape the parser was written for: a fixture is written by whoever wrote the parser, and a
//! compiler is not. That is what this file is for, and it is the only test in the crate that runs a program.
//!
//! It **skips** rather than fails on a machine with no compiler. A test suite that cannot run without `g++` is a
//! test suite that fails for a reason unrelated to the change — and the discovery returning `None` is itself the
//! designed answer for "no compiler here", which the unit tests already pin.

use std::path::Path;

use cpp_code_analysis::{
    CompilerConfig, DiskCommands, DiskFiles, Environment, FileProvider, Include, IncludeForm,
    IncludeResolver, PathInterner, Resolution, SummaryStore, discover,
};

/// The discovery on this machine, or a printed reason and `None`.
fn toolchain_here() -> Option<cpp_code_analysis::Toolchain> {
    let files = DiskFiles;
    let found = discover(
        &files,
        &DiskCommands,
        None,
        Path::new("probe.cpp"),
        &Environment::current(),
    );

    if found.is_none() {
        println!(
            "no compiler was found on this machine, so the discovery has nothing to check — \
             the unit tests cover that answer"
        );
    }

    found
}

/// Resolve one `#include <…>` against a configuration, the way the analysis does.
fn resolve(name: &str, config: &CompilerConfig) -> Resolution {
    let files = DiskFiles;
    let resolver = IncludeResolver::new(&files, config);
    let mut interner = PathInterner::new(cfg!(windows));

    resolver.resolve(
        &Include {
            form: IncludeForm::Angle,
            target: name.into(),
            is_next: false,
        },
        Path::new("."),
        None,
        &mut interner,
    )
}

#[test]
fn a_real_compiler_is_found_and_answers_with_its_own_directories() {
    let Some(toolchain) = toolchain_here() else {
        return;
    };

    assert!(
        !toolchain.system_include_paths.is_empty(),
        "a compiler that answered must name at least one directory: {toolchain:?}"
    );
    assert!(
        toolchain.version.is_some(),
        "both compilers this was written against identify themselves, and a version is what explains a wrong \
         analysis: {toolchain:?}"
    );
    assert!(
        toolchain.include_paths().iter().all(|path| path.is_system),
        "a toolchain's own directories are what `-isystem` would name"
    );
}

#[test]
fn a_standard_header_resolves_once_the_toolchain_has_been_asked() {
    // The claim P0 exists to make good on, and the reason it is worth making on its own: `index::store` refuses to
    // cache a summary whose includes did not resolve, so before this, **a file that includes any standard header
    // was never cached at all** — which is to say no real project's files were.
    let Some(toolchain) = toolchain_here() else {
        return;
    };

    let config = toolchain.config(&CompilerConfig::new());
    let resolution = resolve("stddef.h", &config);

    assert!(
        resolution.is_resolved(),
        "`<stddef.h>` is in every C and C++ toolchain's own directories, so failing to find it means the \
         discovery produced directories that are not the compiler's: {resolution:?}"
    );
}

#[test]
fn the_cpp_standard_library_resolves_when_this_toolchain_has_one() {
    // `<vector>` is the motivating case — it is the first `#include` in most real C++ files — and it is asserted
    // whenever the discovered directories **actually hold it**, which is a fact about this toolchain rather than
    // a guess: a compiler installed without its C++ standard library is a real configuration, and reporting that
    // as a failure would make this test about the machine.
    let Some(toolchain) = toolchain_here() else {
        return;
    };

    let files = DiskFiles;
    let holds_vector = toolchain
        .system_include_paths
        .iter()
        .any(|directory| files.exists(&directory.join("vector")));

    if !holds_vector {
        println!(
            "this toolchain has no C++ standard library in its search list, so there is nothing for `<vector>` \
             to resolve to: {:?}",
            toolchain.system_include_paths
        );
        return;
    }

    let config = toolchain.config(&CompilerConfig::new());
    let resolution = resolve("vector", &config);

    assert!(
        resolution.is_resolved(),
        "`<vector>` is in one of the directories the compiler named, so the resolver must find it there: \
         {resolution:?}"
    );
}

#[test]
fn a_file_that_includes_a_standard_header_becomes_cacheable() {
    // P0's actual claim, and the reason it is worth doing on its own: `index::store` refuses to store a summary
    // whose includes did not resolve, so before the toolchain was asked, every real `.cpp` — all of which include
    // a standard header — was rebuilt on every query and written nowhere. No cache at all, for any real project.
    //
    // The evidence is not "a summary exists": one exists either way. It is the **second session finding it on
    // disk**, which is the only thing the cache is for.
    //
    // Both halves were checked against the pre-P0 state — the same test with an empty configuration instead of a
    // discovered one. `unstored` came back `1`: the summary was built, deliberately not written, and the second
    // session parsed the file again. That is what "no real project's files were ever cached" means as a number.
    let Some(toolchain) = toolchain_here() else {
        return;
    };

    let root = std::env::temp_dir().join("cppls-toolchain-cache");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");

    let source = root.join("main.cpp");
    std::fs::write(&source, "#include <vector>\nint main() { return 0; }\n").expect("the fixture writes");

    let config = toolchain.config(&CompilerConfig::new());

    {
        let mut store = SummaryStore::open(&root, config.clone());
        assert!(store.get(&source).is_some(), "the file is indexed");
        assert_eq!(
            store.stats().unstored,
            0,
            "and stored — a summary is not written when one of its includes failed to resolve, which is what \
             `#include <vector>` used to do"
        );
    }

    // A second session over the same project: a new store, nothing in memory, only the disk.
    let mut store = SummaryStore::open(&root, config);
    assert!(store.get(&source).is_some());
    assert_eq!(
        store.stats().reused,
        1,
        "the second session read the stored summary instead of parsing again — which is the whole point"
    );

    let _ = std::fs::remove_dir_all(&root);
}
