//! Working out what a project is, from the three things that can say: its own file, its build, its compiler, and —
//! when none of those answer — the conventions of the operating system.
//!
//! This module is the **join**: [`crate::project::config`] reads `.cppls.toml`, [`crate::project::build`] finds the
//! compile database, and [`crate::include::toolchain`] asks a compiler about itself. What is left is the part that
//! only exists once they are put together — which of them wins, what the flags end up being, and what a person
//! should be told about the answer.
//!
//! ```text
//! ProjectDiscovery {
//!     config      what .cppls.toml said, and everything wrong with it
//!     database    the compile database, where it was found, and how
//!     problems    everything that could not be worked out, in words a user can act on
//! }
//! ```
//!
//! # Two ways in, and why there are not more
//!
//! A compilation database is the one description of a build that every build system can produce, and `.cppls.toml`
//! is where a project says what no database can: a compiler to use, a standard, arguments to add, paths to search.
//! A server that also read *this* build system's cache and *that* one's project file would have to be taught every
//! build system in turn — and would still be wrong about the one nobody wrote an adapter for. The database is what
//! they agree on; a project that generates none says so in `.cppls.toml`, and a project that needs neither is
//! analysed against its compiler's own defaults.
//!
//! # Why the problems are a list rather than log lines
//!
//! Because the caller with a user in front of it is not this crate (`cpp_code_analysis` has no logger on purpose),
//! and because a problem is the difference between "this analysis is about your project" and "this analysis is
//! about a project nobody described". A project whose build exports no compilation database is the case that makes
//! this concrete: the analysis falls back to the compiler's own headers, which *works*, and the answer is worse
//! than it needs to be for a reason the user can fix — so the fix is spelled out here.

use std::path::{Path, PathBuf};

use crate::file::paths::FileProvider;

use super::build::{self, Database};
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
    /// 3. nothing                  the compiler's own defaults
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

        Vec::new()
    }

    /// The working directory the flags were written for, when one was.
    ///
    /// From the database entry — the directory the build ran in — because a relative `-I` is resolved against it.
    /// A project that describes its compilation in `.cppls.toml` alone has no such directory, and one is not
    /// invented for it: the root is not where a build ran.
    pub fn working_directory(&self) -> Option<PathBuf> {
        self.database
            .as_ref()
            .and_then(|database| database.commands.commands.first())
            .and_then(|command| command.directory.clone())
    }

    /// The language standard the project's build states, as `-std=` spells it — `c++20`.
    ///
    /// Read from the **compilation database's own command line**, which is where a project states it: the database
    /// is the build as it ran, so the standard is one of its arguments. (It used to be read out of CMake's cache —
    /// the one place this crate looked at a build system's internals, and the reason a project that used any other
    /// build system could not state its standard at all.)
    ///
    /// It matters more than it looks: `__cplusplus` takes the value of the language version a compiler was
    /// **invoked** for, so without this every `#if __cplusplus >= 202002L` in every header is decided against the
    /// compiler's default rather than against what the project builds with.
    ///
    /// [`CompilerConfig::standard`]: crate::CompilerConfig::standard
    pub fn standard(&self) -> Option<String> {
        let command = self
            .database
            .as_ref()?
            .commands
            .commands
            .first()?
            .arguments
            .clone();

        standard_in(&command)
    }

    /// The compilers the **project** named, in the order they should be tried, each with where it came from.
    ///
    /// These are the two sources that rank above `CXX`: what a person wrote in `.cppls.toml`, and what the build
    /// actually ran. A source that names nothing is not in the list, and a name that turns out not to exist on this
    /// machine is skipped by the discovery and reported there.
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

/// **The language standard inside one compilation command**, as `-std=` spells it.
///
/// Every spelling a compiler accepts is looked for, because the database is whatever the build wrote: `-std=c++20`,
/// `-std c++20` (separated), `/std:c++20` and `/std:c++latest`. `None` when the build states none, which is not a
/// failure — it means the compiler's own default is what the project builds with.
fn standard_in(arguments: &[String]) -> Option<String> {
    const JOINED: &[&str] = &["-std=", "-std:", "/std:", "/std="];
    const SEPARATED: &[&str] = &["-std", "/std"];

    for (at, argument) in arguments.iter().enumerate() {
        for spelling in JOINED {
            if let Some(value) = argument.strip_prefix(spelling) {
                return non_empty(value);
            }
        }
        if SEPARATED.contains(&argument.as_str()) {
            if let Some(value) = arguments.get(at + 1) {
                return non_empty(value);
            }
        }
    }
    None
}

/// A standard that is written down is a standard; an empty argument after `-std` is not.
fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
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

    let (database, database_problems) =
        build::find_database(files, root, config.config.compile.database.as_deref());
    problems.extend(database_problems.into_iter().map(DiscoveryProblem::warning));

    // **The one thing worth telling a person**: no database and no `[compile].args` means the analysis is about the
    // compiler's defaults rather than about their project — which works, and is worse than it needs to be for a
    // reason they can fix. Said only when neither was found, because a project that wrote its own flags down has
    // answered this question already.
    if database.is_none() && config.config.compile.args.is_none() {
        problems.push(DiscoveryProblem::warning(
            "no compile database was found, so this project is analysed with the compiler's own defaults: export \
             one (`compile_commands.json`, which every build system can produce), or write [compile].args in \
             .cppls.toml",
        ));
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
        problems,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryFiles;

    #[test]
    fn the_files_flags_win_and_nothing_else_is_consulted() {
        let files = MemoryFiles::new()
            .with_file("/p/.cppls.toml", "[compile]\nargs = [\"-DFROM_CONFIG\"]\n");

        let found = discover(&files, Path::new("/p"), None);
        assert_eq!(found.flags(), ["-DFROM_CONFIG"]);
        assert!(
            found.database.is_none(),
            "a file that says how it is built needs no build to be found"
        );
        assert!(
            found
                .problems
                .iter()
                .all(|problem| !problem.message.contains("no compile database")),
            "and is not told to export one: {:?}",
            found.problems
        );
    }

    #[test]
    fn a_project_with_neither_a_database_nor_its_own_flags_is_told_what_to_do() {
        let files = MemoryFiles::new();

        let found = discover(&files, Path::new("/p"), None);

        assert!(
            found
                .problems
                .iter()
                .any(|problem| problem.message.contains("no compile database")),
            "{:?}",
            found.problems
        );
    }

    #[test]
    fn the_standard_is_read_out_of_the_builds_own_command_line() {
        // Every spelling a compiler accepts, since the database is whatever the build wrote.
        assert_eq!(
            standard_in(&["g++".into(), "-std=c++20".into()]),
            Some("c++20".into())
        );
        assert_eq!(
            standard_in(&["clang++".into(), "-std".into(), "c++17".into()]),
            Some("c++17".into())
        );
        assert_eq!(
            standard_in(&["cl.exe".into(), "/std:c++latest".into()]),
            Some("c++latest".into())
        );
        assert_eq!(standard_in(&["g++".into(), "-O2".into()]), None);
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
                "[\n  {\"directory\": \"/p\", \"file\": \"/p/a.cpp\", \"arguments\": [\"clang++\", \"-std=c++20\", \"-c\", \"/p/a.cpp\"]}\n]\n",
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
            ],
            "the order the discovery follows, and the labels it reports"
        );
        assert_eq!(found.standard(), Some("c++20".to_string()));
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
