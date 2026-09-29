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
    IncludeResolver, Known, OpenDocuments, PathInterner, Resolution, Session, SessionFiles,
    SummaryStore, WatchFilter, discover,
};

/// The discovery on this machine, or a printed reason and `None`.
fn toolchain_here() -> Option<cpp_code_analysis::Toolchain> {
    let files = DiskFiles;
    let found = discover(
        &files,
        &DiskCommands,
        cpp_code_analysis::BuildStatement::default(),
        Path::new("probe.cpp"),
        &Environment::current(),
        &cpp_code_analysis::include::msvc::WindowsLayout::current(),
    );

    if let Some(toolchain) = &found {
        println!(
            "toolchain: {} ({:?}, {})",
            toolchain
                .compiler
                .as_ref()
                .map(|compiler| compiler.display().to_string())
                .unwrap_or_else(|| "no compiler".to_string()),
            toolchain.source,
            toolchain.version.as_deref().unwrap_or("no version"),
        );
        if let Some(note) = &toolchain.note {
            println!("note: {note}");
        }
    }

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
fn a_real_compiler_answers_with_the_macros_it_predefines() {
    // The half of the macro environment that is in no file. `#ifdef _WIN32` and `#if __cplusplus >= 201703L` are
    // questions about *these* names, and asking the compiler is the only way to get them: they are built in, not
    // written down.
    let Some(toolchain) = toolchain_here() else {
        return;
    };

    // **Not a count.** GCC's `-dM` prints ~470 names and MSVC's `/PD` prints 58 — the fact sheet in
    // measured the second one — so a count assertion would be an assertion about *which
    // compiler this machine has*. What every C++ compiler's table must contain is the names it identifies itself
    // by and the language level, because those are what `#ifdef _WIN32` and `#if __cplusplus >= …` ask.
    let has = |name: &str| {
        toolchain
            .builtin_macros
            .iter()
            .any(|define| define.name.as_ref() == name)
    };
    assert!(
        has("__cplusplus"),
        "no compiler leaves `__cplusplus` undefined: {} macros, {:?}",
        toolchain.builtin_macros.len(),
        &toolchain.builtin_macros[..toolchain.builtin_macros.len().min(5)]
    );
    assert!(
        has("__GNUC__") || has("_MSC_VER"),
        "and every one of them says which family it belongs to — which is how the dialect is decided: {:?}",
        toolchain.dialect
    );
    assert_eq!(
        toolchain.dialect.is_some(),
        has("__GNUC__") || has("_MSC_VER"),
        "the dialect comes from the same table"
    );

    let value_of = |name: &str| {
        toolchain
            .builtin_macros
            .iter()
            .find(|define| define.name.as_ref() == name)
            .map(|define| define.value.as_deref())
    };

    // `__cplusplus` is the one every C++ file's conditions are written against, and it has a **value**: a condition
    // like `#if __cplusplus >= 201703L` needs the number, not just the name. Read with the evaluator's own integer
    // reader, because that is what will compare it — `201703L` carries a suffix, and `str::parse` rejects it.
    //
    // **The floor is C++11, not C++17**, and MSVC is why: this asks with no `-std=`/`/std:` at all — no compile
    // database was read — so the answer is the compiler's *default* language level, which for MSVC 19.35 is C++14
    // (`201402L`). That is the honest answer to the question asked; a project's standard travels with the request
    // (`search_paths` documents why), and measured the four combinations.
    let standard = value_of("__cplusplus");
    assert!(
        standard
            .flatten()
            .and_then(cpp_code_analysis::condition::parse_integer)
            .is_some_and(|value| value >= 201103),
        "`__cplusplus` must come back with a number as its value, got {standard:?}"
    );

    // A name is never empty, and never carries a parameter list: `-dM` prints `#define f(x) …` glued together, and
    // a name with parentheses in it is not one any condition tests. (Whether a table has *valueless* macros at all
    // is the compiler's business — GCC prints several, MSVC's table has none — so that shape is pinned by the unit
    // test over a recorded `-dM` rather than here.)
    assert!(
        toolchain.builtin_macros.iter().all(|define| {
            !define.name.is_empty() && !define.name.contains('(') && !define.name.contains(' ')
        }),
        "every macro has a name, and it is only a name"
    );
}

/// A **standard header resolves** once the toolchain has been asked.
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

/// **A project with no build system of any kind is read with the environment the compiler gives it.**
#[test]
fn a_project_with_no_build_system_is_read_with_the_compilers_environment() {
    // The shape MSVC's standard library is written in, in three files that need no compiler to *parse* and one to
    // *read*: `yvals_core.h` defines `_STL_COMPILER_PREPROCESSOR` under
    // `#if defined(RC_INVOKED) || defined(Q_MOC_RUN) || defined(__midl)`, and every header of the library puts its
    // whole body inside `#if _STL_COMPILER_PREPROCESSOR`. Neither name is defined by a file or by the compiler, so
    // that condition is decidable **only** by an environment that has been told the compiler's own table is the
    // whole of the command line — which is the claim a session makes when the compiler answered.
    //
    // What it costs to not make the claim is measured elsewhere (13 declarations in `std` against 2600, and the
    // cooked reading of `<string>` rendering to nothing); what this pins is the *mechanism*: a scope that comes out
    // of a macro body behind a condition naming a built-in.
    let Some(toolchain) = toolchain_here() else {
        return;
    };
    if toolchain.builtin_macros.is_empty() {
        println!(
            "this compiler did not answer with its predefined macros, so there is no environment to be complete \
             about: {:?}",
            toolchain.note
        );
        return;
    }

    let root = std::env::temp_dir().join("cppls-no-build-system");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");

    std::fs::write(
        root.join("gate.h"),
        "#pragma once\n\
         #if defined(RC_INVOKED) || defined(Q_MOC_RUN)\n\
         #define OPEN_STD\n\
         #define CLOSE_STD\n\
         #else\n\
         #define OPEN_STD namespace std {\n\
         #define CLOSE_STD }\n\
         #endif\n",
    )
    .expect("the fixture writes");
    let widget = root.join("widget.h");
    std::fs::write(
        &widget,
        "#include \"gate.h\"\nOPEN_STD\nstruct Widget { int size; };\nCLOSE_STD\n",
    )
    .expect("the fixture writes");
    let main = root.join("main.cpp");
    std::fs::write(&main, "#include \"widget.h\"\n").expect("the fixture writes");

    // **No `compile_commands.json`, no `.cppls.toml`, no CMake cache** — which is the whole point of the test, and
    // the state the user's own workspace is in.
    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );

    if session.toolchain().is_none() {
        println!("no toolchain was discovered for the project directory, so nothing can be claimed about it");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    session.index_everything();

    let scope = session
        .index()
        .summary(&widget)
        .and_then(|summary| {
            summary
                .declarations
                .iter()
                .find(|fact| fact.name == "Widget")
                .map(|fact| fact.scope.clone())
        })
        .expect("`Widget` is declared in the header");

    assert_eq!(
        scope.as_deref(),
        Some("std"),
        "`OPEN_STD`'s body is written under a condition that names two built-ins, and a session that asked the \
         compiler knows neither is defined — so the branch is taken and the namespace it opens is real"
    );
    assert!(
        matches!(
            session.index().definition("std::Widget", &main),
            Known::Yes(_)
        ),
        "and the qualified name is answerable from the file that includes it"
    );

    let _ = std::fs::remove_dir_all(&root);
}

