//! Working out what a project is, from the four things that can say: its own file, its build system, its compiler,
//! and — when none of those answer — the conventions of the operating system.
//!
//! This module is the **join**: [`crate::project::config`] reads `.cppls.toml`, [`crate::project::build`] finds the
//! compile database and CMake's cache, and [`crate::include::toolchain`] asks a compiler about itself. What is left
//! is the part that only exists once they are put together — which of them wins, what the flags end up being, and
//! what a person should be told about the answer.
//!
//! ```text
//! ProjectDiscovery {
//!     config      what .cppls.toml said, and everything wrong with it
//!     database    the compile database, where it was found, and how
//!     cmake       CMake's cache, when there was one and it was this project's
//!     problems    everything that could not be worked out, in words a user can act on
//! }
//! ```
//!
//! # Why the problems are a list rather than log lines
//!
//! Because the caller with a user in front of it is not this crate (`cpp_code_analysis` has no logger on purpose),
//! and because a problem is the difference between "this analysis is about your project" and "this analysis is
//! about a project nobody described". A CMake project that never exported a compile database is the case that
//! makes this concrete: the analysis falls back to the compiler's own headers, which *works*, and the answer is
//! worse than it needs to be for a reason the user can fix in one command — so the fix is spelled out here.

use std::path::{Path, PathBuf};

use crate::file::paths::FileProvider;

use super::build::{self, CmakeCache, Database};
use super::config::{ConfigReport, load_config};

/// Everything that can be worked out about a project without asking a compiler.
///
/// `Clone` because a caller that reports on it — a language server logging what it thinks the project is — reads it
/// out of a session it only has a shared borrow of, and copying a few paths and a list of sentences is cheaper than
/// holding a lock across the formatting of them.
#[derive(Debug, Clone)]
pub struct ProjectDiscovery {
    /// The workspace root this was worked out for.
    pub root: PathBuf,
    /// What the project's own configuration file said.
    pub config: ConfigReport,
    /// The compile database, when there was one.
    pub database: Option<Database>,
    /// CMake's cache, when there was one that belongs to this project.
    pub cmake: Option<CmakeCache>,
    /// Everything a user should know: what could not be read, what was looked for and not found, what was found
    /// and not used.
    pub problems: Vec<DiscoveryProblem>,
}

/// One thing about a project's discovery that a person should be told.
///
/// Deliberately not a severity ladder beyond [`Severity`](super::Severity): the useful distinction is "part of what
/// you said is not in effect" (an error, shown to the client) against "here is what I did instead" (a warning, for
/// the log) — and a caller that wants more than that is inventing a taxonomy nobody acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryProblem {
    pub severity: super::Severity,
    pub message: String,
}

impl DiscoveryProblem {
    fn warning(message: impl Into<String>) -> Self {
        DiscoveryProblem {
            severity: super::Severity::Warning,
            message: message.into(),
        }
    }
}

impl ProjectDiscovery {
    /// The flags the project's compilation is described by, before a compiler is consulted.
    ///
    /// The order is the decision `CompileSection::args` records:
    ///
    /// ```text
    /// 1. [compile].args           a person wrote the compilation down — it wins outright, and the database's
    ///                             flags are not merged into it (see that field's documentation for the cost)
    /// 2. the compile database     the project's build, as it ran
    /// 3. CMake's cache            what CMake was configured with, minus whatever a target adds
    /// 4. nothing                  the compiler's own defaults
    /// ```
    pub fn flags(&self) -> Vec<String> {
        if let Some(args) = &self.config.config.compile.args {
            return args.clone();
        }

        if let Some(database) = &self.database
            && let Some(command) = database.commands.commands.first()
        {
            return command.arguments.clone();
        }

        self.cmake
            .as_ref()
            .map(|cmake| cmake.flags.clone())
            .unwrap_or_default()
    }

    /// The working directory the flags were written for, when one was.
    ///
    /// From the database entry (the directory the build ran in), because a relative `-I` is resolved against it.
    /// CMake's cache has no equivalent — its flags are absolute or relative to the build tree, and the build tree
    /// is where the cache is, which is a claim this layer does not make.
    pub fn working_directory(&self) -> Option<PathBuf> {
        self.database
            .as_ref()
            .and_then(|database| database.commands.commands.first())
            .and_then(|command| command.directory.clone())
    }

    /// The language standard the project's build states, as `c++NN`.
    ///
    /// CMake records it as a **number** (`CMAKE_CXX_STANDARD=20`), and it is not a flag — the spelling is the
    /// compiler's (`-std=c++20`, `/std:c++20`), which is why it travels as [`CompilerConfig::standard`] and not as
    /// an argument: the toolchain renders it for whichever compiler answered.
    ///
    /// It matters more than it looks: `__cplusplus` takes the value of the language version a compiler was
    /// **invoked** for, so without this every `#if __cplusplus >= 202002L` in every header is decided against the
    /// compiler's default rather than against what the project builds with.
    ///
    /// [`CompilerConfig::standard`]: crate::CompilerConfig::standard
    pub fn standard(&self) -> Option<String> {
        self.cmake
            .as_ref()
            .and_then(|cmake| cmake.standard.clone())
    }

    /// The compilers the **project** named, in the order they should be tried, each with where it came from.
    ///
    /// These are the three sources that rank above `CXX`: what a person wrote in `.cppls.toml`, what the build
    /// actually ran, and what CMake was configured with. A source that names nothing is not in the list, and a name
    /// that turns out not to exist on this machine is skipped by the discovery and reported there.
    pub fn named_compilers(&self) -> Vec<(PathBuf, crate::ToolchainSource)> {
        let mut named = Vec::new();

        if let Some(compiler) = self.config.config.compile.named_compiler() {
            named.push((
                PathBuf::from(compiler),
                crate::ToolchainSource::Configuration,
            ));
        }

        if let Some(database) = &self.database
            && let Some(program) = database
                .commands
                .commands
                .first()
                .and_then(|command| command.arguments.first())
        {
            named.push((
                PathBuf::from(program),
                crate::ToolchainSource::CompileDatabase,
            ));
        }

        if let Some(cmake) = &self.cmake
            && let Some(compiler) = &cmake.compiler
        {
            named.push((compiler.clone(), crate::ToolchainSource::BuildCache));
        }

        named
    }

    /// The problems worth showing a user without being asked, which are the errors from the configuration file plus
    /// every discovery problem.
    pub fn errors(&self) -> impl Iterator<Item = &str> {
        self.config
            .errors()
            .map(|problem| problem.message.as_str())
            .chain(
                self.problems
                    .iter()
                    .filter(|problem| problem.severity == super::Severity::Error)
                    .map(|problem| problem.message.as_str()),
            )
    }
}

/// Work out what a project is, without running a compiler.
///
/// Split from the toolchain step — which *does* run one, and which [`crate::Session::open`] does next — because the
/// two fail differently and a caller wants to see the first half's answer even when the second half finds nothing.
pub fn discover(
    files: &impl FileProvider,
    root: &Path,
    config_file: Option<&Path>,
) -> ProjectDiscovery {
    let config = load_config(files, root, config_file);
    let mut problems: Vec<DiscoveryProblem> = Vec::new();

    let (database, database_problems) = build::find_database(
        files,
        root,
        config.config.compile.database.as_deref(),
    );
    problems.extend(database_problems.into_iter().map(DiscoveryProblem::warning));

    let (cmake, cmake_problems) = build::find_cmake_cache(files, root);
    problems.extend(cmake_problems.into_iter().map(DiscoveryProblem::warning));

    // The three things worth telling a person about, each a sentence they can act on.
    if let Some(cmake) = &cmake {
        if !cmake.exports_compile_commands && database.is_none() {
            problems.push(DiscoveryProblem::warning(format!(
                "{} was configured with CMAKE_EXPORT_COMPILE_COMMANDS=OFF, so there is no compile database and \
                 this project is analysed with the compiler's own defaults: re-run the configuration with \
                 -DCMAKE_EXPORT_COMPILE_COMMANDS=ON, or write [compile].args in .cppls.toml",
                cmake.path.display()
            )));
        } else if cmake.exports_compile_commands && database.is_none() {
            problems.push(DiscoveryProblem::warning(format!(
                "{} says the project exports a compile database, and none was found in the tree: run the build \
                 once, or name it with [compile].database in .cppls.toml",
                cmake.path.display()
            )));
        } else if database.is_none() {
            // A cache that says nothing about the database at all (an older CMake, or a hand-written file).
            problems.push(DiscoveryProblem::warning(format!(
                "no compile database was found; {} supplies the compiler and the project-wide flags, and \
                 whatever a target adds is missing",
                cmake.path.display()
            )));
        }
    }

    if let Some(database) = &database
        && database.origin == build::DatabaseOrigin::BuildDirectory
    {
        problems.push(DiscoveryProblem::warning(format!(
            "the compile database was found in a build directory: {}",
            database.path.display()
        )));
    }

    ProjectDiscovery {
        root: root.to_path_buf(),
        config,
        database,
        cmake,
        problems,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryFiles;

    #[test]
    fn the_files_flags_win_and_the_cache_is_the_last_thing_consulted() {
        let files = MemoryFiles::new().with_file(
            "/p/.cppls.toml",
            "[compile]\nargs = [\"-DFROM_CONFIG\"]\n",
        );

        let found = discover(&files, Path::new("/p"), None);
        assert_eq!(found.flags(), ["-DFROM_CONFIG"]);
        assert!(
            found.database.is_none() && found.cmake.is_none(),
            "a file that says how it is built needs no build system to be found"
        );
    }

    #[test]
    fn cmake_supplies_the_compiler_and_the_flags_when_no_database_does() {
        // The case `CMAKE_EXPORT_COMPILE_COMMANDS=OFF` produces, which is common enough to be worth a test: no
        // database, and CMake's cache as the only statement about how the project is built.
        //
        // The cache is at the **root** here because the search for one in a build directory is a filesystem
        // question — `build::find_cmake_cache` walks directories — and `MemoryFiles` has none. That walk has its
        // own test, on a real tree, in `crate::project::build`.
        let files = MemoryFiles::new().with_file(
            "/p/CMakeCache.txt",
            "CMAKE_HOME_DIRECTORY:INTERNAL=/p\n\
             CMAKE_CXX_COMPILER:FILEPATH=/usr/bin/clang++\n\
             CMAKE_CXX_FLAGS:STRING=-Wall\n\
             CMAKE_CXX_STANDARD:STRING=20\n\
             CMAKE_EXPORT_COMPILE_COMMANDS:BOOL=OFF\n",
        );

        let found = discover(&files, Path::new("/p"), None);

        assert_eq!(found.flags(), ["-Wall"]);
        assert_eq!(
            found.named_compilers(),
            [(PathBuf::from("/usr/bin/clang++"), crate::ToolchainSource::BuildCache)]
        );
        assert_eq!(
            found
                .problems
                .iter()
                .filter(|problem| problem.message.contains("CMAKE_EXPORT_COMPILE_COMMANDS"))
                .count(),
            1,
            "and the fix is spelled out: {:?}",
            found.problems
        );
    }

    #[test]
    fn a_project_that_names_everything_names_it_in_one_order() {
        let files = MemoryFiles::new()
            .with_file(
                "/p/.cppls.toml",
                "[compile]\ncompiler = \"/opt/gcc/bin/g++\"\n",
            )
            .with_file(
                "/p/compile_commands.json",
                "[\n  {\"directory\": \"/p\", \"file\": \"/p/a.cpp\", \"arguments\": [\"clang++\", \"-c\", \"/p/a.cpp\"]}\n]\n",
            )
            .with_file(
                "/p/CMakeCache.txt",
                "CMAKE_HOME_DIRECTORY:INTERNAL=/p\nCMAKE_CXX_COMPILER:FILEPATH=/usr/bin/c++\n",
            );

        let found = discover(&files, Path::new("/p"), None);
        let sources: Vec<crate::ToolchainSource> = found
            .named_compilers()
            .into_iter()
            .map(|(_, source)| source)
            .collect();

        assert_eq!(
            sources,
            [
                crate::ToolchainSource::Configuration,
                crate::ToolchainSource::CompileDatabase,
                crate::ToolchainSource::BuildCache,
            ],
            "the order the discovery follows, and the labels it reports"
        );
    }

    #[test]
    fn a_database_in_a_build_directory_is_said_out_loud() {
        // Because "which compilation is this analysis about" is the first question a wrong answer raises, and a
        // database found away from the root is the answer a user would not guess.
        let root = std::env::temp_dir().join("cppls-discovery-build-dir");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("build")).expect("the fixture directory");
        std::fs::write(
            root.join("build/compile_commands.json"),
            "[\n  {\"directory\": \"/p\", \"file\": \"/p/a.cpp\", \"arguments\": [\"g++\", \"-c\", \"/p/a.cpp\"]}\n]\n",
        )
        .expect("the fixture writes");

        let found = discover(&crate::DiskFiles, &root, None);

        assert_eq!(
            found.database.as_ref().map(|database| database.origin),
            Some(build::DatabaseOrigin::BuildDirectory)
        );
        assert!(
            found
                .problems
                .iter()
                .any(|problem| problem.message.contains("build directory")),
            "{:?}",
            found.problems
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}

