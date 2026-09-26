//! What a project's build system has already worked out, and where it left it.
//!
//! Two files, in the order of how much they say:
//!
//! ```text
//! compile_commands.json   every file's own command, exactly as the build ran it — the whole answer
//! CMakeCache.txt          what CMake was configured with — the compiler, the standard, the global flags
//! ```
//!
//! # Where they are, and why not only in the root
//!
//! `compile_commands.json` is *conventionally* beside `CMakeLists.txt`, and in practice it is wherever the build
//! directory is: `build/`, `cmake-build-debug/`, `out/build/linux-release/`. A server that looked only in the root
//! would find nothing in most CMake projects — and a project that exports the database and puts it somewhere a
//! convention cannot predict has `compile.database` in `.cppls.toml` for exactly that.
//!
//! So the search is a **bounded walk** — depth 3, at most [`MAX_DIRECTORIES`] directories visited, sorted so that
//! two runs and two machines agree — rather than a list of guessed directory names. The list would have to include
//! `build`, `out`, `cmake-build-*`, `_build`, `Build`, `x64`, `Debug`, and would still miss the one the project
//! uses; a walk finds it and costs a few hundred `read_dir` calls on a project whose build tree is already there.
//!
//! It deliberately does **not** use the session's [`WatchFilter`](crate::WatchFilter): a project's `exclude` list
//! usually names its own build tree, and the compile database is inside it. What the walk skips is what no
//! database can be inside — `.git`, the cache directory, and directories that are themselves a checkout
//! (`node_modules`).
//!
//! # What CMakeCache lets us say, and what it does not
//!
//! The cache holds what `cmake` was *configured* with: the compiler, `CMAKE_CXX_FLAGS`, the C++ standard, and
//! whether the project exports a database. It does **not** hold what a target adds — `target_compile_definitions`,
//! `target_include_directories`, an `-I` a module needs — because those are resolved per target, and the file that
//! resolves them is the database this module went looking for. So a cache is a *better than nothing* answer, and
//! the report says which parts of the configuration came from it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::include::config::{CompileCommands, CompilerConfig, parse_compile_commands, split_command_line};
use crate::file::paths::FileProvider;

/// The name a compile database is conventionally written under.
pub const COMPILE_DATABASE_FILE: &str = "compile_commands.json";

/// The name CMake writes its configuration cache under.
pub const CMAKE_CACHE_FILE: &str = "CMakeCache.txt";

/// How deep the search for a database goes, relative to the project root.
///
/// Three, because that is what the conventions need and no more: `build/compile_commands.json` is 1,
/// `out/build/linux/compile_commands.json` is 3, and a build tree nested deeper than that is one nobody finds by
/// convention either.
pub const MAX_DEPTH: usize = 3;

/// How many directories one search may visit.
///
/// A bound on a walk whose size is a fact about a machine rather than about the project: a checkout with a
/// generated tree in it can be arbitrarily large, and a server that spent a second listing it would delay the
/// first answer for a reason the user cannot see. Two thousand directories is far past the deepest real project
/// measured here, and hitting the bound is reported rather than silent.
pub const MAX_DIRECTORIES: usize = 2000;

/// Directory names a compile database is never inside.
const NEVER_SEARCHED: &[&str] = &[".git", crate::CACHE_DIRECTORY, "node_modules"];

/// A compile database, and how it was found.
///
/// Not `PartialEq`: a `CompileCommands` is a list of commands, and comparing two of them is a question nobody asks
/// (the session keeps the one it read, and a test compares the *origin* and the entries it needs).
#[derive(Debug, Clone)]
pub struct Database {
    pub path: PathBuf,
    pub commands: CompileCommands,
    /// How this file came to be the one that was read.
    pub origin: DatabaseOrigin,
}

/// Where the database was, which is the first thing to check when it is the wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseOrigin {
    /// `compile.database` in `.cppls.toml` named it.
    Configured,
    /// The project's root.
    Root,
    /// A build directory under the root, found by the bounded walk.
    BuildDirectory,
}

impl DatabaseOrigin {
    pub fn words(self) -> &'static str {
        match self {
            DatabaseOrigin::Configured => "named by compile.database in .cppls.toml",
            DatabaseOrigin::Root => "in the project root",
            DatabaseOrigin::BuildDirectory => "found in a build directory",
        }
    }
}

/// What CMake was configured with, read out of its cache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CmakeCache {
    pub path: PathBuf,
    /// `CMAKE_HOME_DIRECTORY` — the source directory the cache was configured for.
    ///
    /// The field that keeps a *vendored* build tree from configuring this project: a `CMakeCache.txt` under
    /// `third_party/foo/build` describes `third_party/foo`, and using its compiler and flags for the project
    /// around it would be a confident wrong answer.
    pub home: Option<PathBuf>,
    /// `CMAKE_CXX_COMPILER` — the compiler the build ran, which is what the toolchain discovery should be asked
    /// about.
    pub compiler: Option<PathBuf>,
    /// `CMAKE_CXX_FLAGS` plus the flags of the configured build type, already split into arguments.
    pub flags: Vec<String>,
    /// `CMAKE_CXX_STANDARD`, as `-std=` spells it.
    pub standard: Option<String>,
    /// `CMAKE_BUILD_TYPE`, for the report: which set of flags `flags` came from.
    pub build_type: Option<String>,
    /// `CMAKE_GENERATOR`, for the report: who wrote this build tree.
    pub generator: Option<String>,
    /// `CMAKE_EXPORT_COMPILE_COMMANDS` — false means the build system *could* have written a database and was not
    /// asked to, which is a fixable problem rather than a missing one.
    pub exports_compile_commands: bool,
}

impl CmakeCache {
    /// The keys this cache holds, for a report that has to say what was read.
    pub fn keys(&self) -> Vec<(&'static str, String)> {
        let mut keys = vec![
            (
                "CMAKE_CXX_COMPILER",
                self.compiler
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            ),
            ("CMAKE_CXX_FLAGS", self.flags.join(" ")),
            ("CMAKE_CXX_STANDARD", self.standard.clone().unwrap_or_default()),
            ("CMAKE_BUILD_TYPE", self.build_type.clone().unwrap_or_default()),
            ("CMAKE_GENERATOR", self.generator.clone().unwrap_or_default()),
            (
                "CMAKE_EXPORT_COMPILE_COMMANDS",
                self.exports_compile_commands.to_string(),
            ),
        ];
        keys.retain(|(_, value)| !value.is_empty());
        keys
    }

    /// The flags as a configuration, with `section`'s `extra_args`/`remove_args` applied.
    pub fn config(&self, section: &crate::project::CompileSection) -> CompilerConfig {
        crate::include::config::project_config_from_flags(&self.flags, None, section)
    }
}

/// Find a compile database for `root`.
///
/// `configured` is `compile.database` from `.cppls.toml`; when the project named one, only that path is read —
/// because a file the project pointed at is the project's answer, and falling back to a search would silently
/// analyse a different compilation than the one it asked for.
///
/// The search order is: the root, then the tree in a deterministic order. **Shallowest first**, because a database
/// beside the sources is the one a single-configuration project has, and lexicographic within a depth so that two
/// runs agree.
pub fn find_database(
    files: &impl FileProvider,
    root: &Path,
    configured: Option<&Path>,
) -> (Option<Database>, Vec<String>) {
    let mut problems = Vec::new();

    if let Some(named) = configured {
        let path = if named.is_absolute() {
            named.to_path_buf()
        } else {
            root.join(named)
        };

        return match read_database(files, &path) {
            Some(commands) => (
                Some(Database {
                    path,
                    commands,
                    origin: DatabaseOrigin::Configured,
                }),
                problems,
            ),
            None => {
                problems.push(format!(
                    "compile.database names {}, which could not be read as a compile database",
                    path.display()
                ));
                (None, problems)
            }
        };
    }

    let conventional = root.join(COMPILE_DATABASE_FILE);
    if let Some(commands) = read_database(files, &conventional) {
        return (
            Some(Database {
                path: conventional,
                commands,
                origin: DatabaseOrigin::Root,
            }),
            problems,
        );
    }

    let (found, truncated) = search_for_database(root);
    if truncated {
        problems.push(format!(
            "the search for {COMPILE_DATABASE_FILE} stopped after {MAX_DIRECTORIES} directories; \
             compile.database in .cppls.toml can name one directly"
        ));
    }

    for path in found {
        if let Some(commands) = read_database(files, &path) {
            return (
                Some(Database {
                    path,
                    commands,
                    origin: DatabaseOrigin::BuildDirectory,
                }),
                problems,
            );
        }
    }

    (None, problems)
}

/// Read one path as a compile database, or `None` when it is not usable.
///
/// A database that parses to nothing usable is treated as absent: its whole purpose is to say how files are
/// compiled, and a project with a malformed one is better analysed the way a project with none is.
fn read_database(files: &impl FileProvider, path: &Path) -> Option<CompileCommands> {
    let json = files.read(path)?;
    let database = parse_compile_commands(&json);

    (!database.is_empty()).then_some(database)
}

/// Every `compile_commands.json` under `root`, shallowest first, bounded.
///
/// Returns the paths and whether the bound was hit, so that a search which gave up can say so.
fn search_for_database(root: &Path) -> (Vec<PathBuf>, bool) {
    let mut found = Vec::new();
    let mut visited = 0usize;
    let mut frontier = vec![(root.to_path_buf(), 0usize)];
    let mut truncated = false;

    while let Some((directory, depth)) = frontier.pop() {
        if visited >= MAX_DIRECTORIES {
            truncated = true;
            break;
        }
        visited += 1;

        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };

        // Sorted, so that "the first database found" means the same thing on two machines: a directory listing's
        // order is the filesystem's, and an analysis whose configuration depends on it cannot be compared between
        // two runs.
        let mut children: BTreeSet<PathBuf> = BTreeSet::new();
        let mut here: Option<PathBuf> = None;

        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };

            let Ok(kind) = entry.file_type() else {
                continue;
            };

            if kind.is_symlink() {
                continue;
            }

            if kind.is_file() {
                if name == COMPILE_DATABASE_FILE {
                    here = Some(path);
                }
                continue;
            }

            if kind.is_dir()
                && depth < MAX_DEPTH
                && !NEVER_SEARCHED.iter().any(|skip| skip.eq_ignore_ascii_case(name))
            {
                children.insert(path);
            }
        }

        if let Some(path) = here {
            found.push(path);
        }

        // Depth-first through a sorted frontier, so the shallowest files come out first: the frontier is a stack,
        // and children are pushed in reverse so the smallest is popped first.
        for child in children.into_iter().rev() {
            frontier.push((child, depth + 1));
        }
    }

    found.sort_by_key(|path| {
        (
            path.components().count(),
            path.to_string_lossy().to_lowercase(),
        )
    });

    (found, truncated)
}

/// Find and read a `CMakeCache.txt` for `root`.
///
/// Shallowest first, and **only a cache that was configured for this project** is used: `CMAKE_HOME_DIRECTORY` has to
/// be the root (or a directory containing it), because a vendored subproject's build tree describes that
/// subproject. A cache that does not match is reported, not used — its compiler is real, but it is not this
/// project's.
pub fn find_cmake_cache(
    files: &impl FileProvider,
    root: &Path,
) -> (Option<CmakeCache>, Vec<String>) {
    let mut problems = Vec::new();
    let mut candidates = vec![root.join(CMAKE_CACHE_FILE)];
    candidates.extend(search_for_file(root, CMAKE_CACHE_FILE));

    let mut foreign: Vec<PathBuf> = Vec::new();

    for path in candidates {
        let Some(text) = files.read(&path) else {
            continue;
        };

        let cache = parse_cmake_cache(&text, path.clone());
        match &cache.home {
            Some(home) if !home_matches(home, root) => {
                foreign.push(path);
                continue;
            }
            // A cache with no `CMAKE_HOME_DIRECTORY` at all is a hand-written one: usable, and worth saying.
            None => problems.push(format!(
                "{} has no CMAKE_HOME_DIRECTORY, so it cannot be checked against this project",
                path.display()
            )),
            Some(_) => {}
        }

        return (Some(cache), problems);
    }

    for path in foreign {
        problems.push(format!(
            "{} was configured for another source directory, so it was not used",
            path.display()
        ));
    }

    (None, problems)
}

/// Is this cache's source directory this project?
///
/// `root` may be *inside* the configured source directory (a workspace folder opened on a subdirectory), which is
/// still this project's cache; the reverse is not accepted, because a cache configured for a parent directory is a
/// different build.
fn home_matches(home: &Path, root: &Path) -> bool {
    let home = crate::file::paths::normalize_path(home, cfg!(windows));
    let root = crate::file::paths::normalize_path(root, cfg!(windows));

    root == home || root.starts_with(&format!("{home}/"))
}

/// Every file called `name` under `root`, shallowest first, bounded — the same walk as [`search_for_database`].
fn search_for_file(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut visited = 0usize;
    let mut frontier = vec![(root.to_path_buf(), 0usize)];

    while let Some((directory, depth)) = frontier.pop() {
        if visited >= MAX_DIRECTORIES {
            break;
        }
        visited += 1;

        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };

        let mut children: BTreeSet<PathBuf> = BTreeSet::new();

        for entry in entries.flatten() {
            let path = entry.path();
            let Some(child_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };

            let Ok(kind) = entry.file_type() else {
                continue;
            };

            if kind.is_symlink() {
                continue;
            }

            if kind.is_file() {
                if child_name == name {
                    found.push(path);
                }
                continue;
            }

            if kind.is_dir()
                && depth < MAX_DEPTH
                && !NEVER_SEARCHED
                    .iter()
                    .any(|skip| skip.eq_ignore_ascii_case(child_name))
            {
                children.insert(path);
            }
        }

        for child in children.into_iter().rev() {
            frontier.push((child, depth + 1));
        }
    }

    found.sort_by_key(|path| {
        (
            path.components().count(),
            path.to_string_lossy().to_lowercase(),
        )
    });

    found
}

/// Read CMake's cache format: one `KEY:TYPE=VALUE` per line, `//` comments, `#` too.
///
/// A hand-written reader rather than a dependency, and for once the reason is the *format* rather than the
/// failure mode: it is a flat list of assignments, and the keys this analysis reads are six of them. What it is
/// careful about is the two things that make a flat reader wrong — a value containing `=` (split at the first
/// one) and a value containing `:` before the type (split at the **last** `:` before the first `=`), which is what
/// a Windows path in a cache looks like.
pub fn parse_cmake_cache(text: &str, path: PathBuf) -> CmakeCache {
    let mut cache = CmakeCache {
        path,
        exports_compile_commands: true,
        ..CmakeCache::default()
    };

    let mut build_type = None;
    let mut type_flags: Vec<String> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = split_entry(line) else {
            continue;
        };

        match key {
            "CMAKE_HOME_DIRECTORY" => cache.home = Some(PathBuf::from(value)),
            "CMAKE_CXX_COMPILER" => cache.compiler = Some(PathBuf::from(value)),
            "CMAKE_CXX_FLAGS" => cache.flags.extend(split_command_line(value)),
            "CMAKE_CXX_STANDARD" => {
                if !value.is_empty() {
                    cache.standard = Some(format!("c++{value}"));
                }
            }
            "CMAKE_BUILD_TYPE" => {
                if !value.is_empty() {
                    build_type = Some(value.to_string());
                }
            }
            "CMAKE_GENERATOR" => cache.generator = Some(value.to_string()),
            "CMAKE_EXPORT_COMPILE_COMMANDS" => {
                cache.exports_compile_commands = !matches!(
                    value.to_ascii_uppercase().as_str(),
                    "OFF" | "FALSE" | "0" | "NO"
                );
            }
            other => {
                // `CMAKE_CXX_FLAGS_DEBUG`, `_RELEASE`, `_RELWITHDEBINFO`, `_MINSIZEREL`: the flags of **one**
                // configuration. Which of them applies is `CMAKE_BUILD_TYPE`, and a multi-configuration generator
                // (Visual Studio, Xcode) has no single answer — so they are collected and only used for the build
                // type the cache names, if it names one.
                if let Some(configuration) = other
                    .strip_prefix("CMAKE_CXX_FLAGS_")
                    .map(str::to_ascii_uppercase)
                    && build_type
                        .as_deref()
                        .is_some_and(|named| named.eq_ignore_ascii_case(&configuration))
                {
                    type_flags.extend(split_command_line(value));
                }
            }
        }
    }

    cache.build_type = build_type;
    cache.flags.extend(type_flags);
    cache
}

/// Split one `KEY:TYPE=VALUE` line at the type marker and the first `=`.
fn split_entry(line: &str) -> Option<(&str, &str)> {
    let equals = line.find('=')?;
    let (left, value) = line.split_at(equals);
    let value = &value[1..];

    // `KEY:TYPE`, and a key never contains `:` — so the type is what follows the **last** colon. A Windows path in
    // the key position cannot happen; a Windows path in the *value* is why the split is on the first `=`.
    let key = match left.rfind(':') {
        Some(colon) => &left[..colon],
        None => left,
    };

    (!key.is_empty()).then_some((key, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryFiles;

    fn cache(text: &str) -> CmakeCache {
        parse_cmake_cache(text, PathBuf::from("/p/build/CMakeCache.txt"))
    }

    #[test]
    fn a_cache_hands_over_the_compiler_the_flags_and_the_standard() {
        let parsed = cache(
            "// a comment\n\
             # another\n\
             CMAKE_CXX_COMPILER:FILEPATH=C:/Program Files/LLVM/bin/clang++.exe\n\
             CMAKE_CXX_FLAGS:STRING=-Wall -Wextra  -DFROM_CACHE=1\n\
             CMAKE_CXX_STANDARD:STRING=20\n\
             CMAKE_BUILD_TYPE:STRING=Debug\n\
             CMAKE_GENERATOR:INTERNAL=Ninja\n\
             CMAKE_EXPORT_COMPILE_COMMANDS:BOOL=ON\n\
             CMAKE_HOME_DIRECTORY:INTERNAL=/p\n",
        );

        assert_eq!(
            parsed.compiler.as_deref(),
            Some(Path::new("C:/Program Files/LLVM/bin/clang++.exe"))
        );
        assert_eq!(parsed.flags, ["-Wall", "-Wextra", "-DFROM_CACHE=1"]);
        assert_eq!(parsed.standard.as_deref(), Some("c++20"));
        assert_eq!(parsed.build_type.as_deref(), Some("Debug"));
        assert_eq!(parsed.generator.as_deref(), Some("Ninja"));
        assert!(parsed.exports_compile_commands);
        assert_eq!(parsed.home.as_deref(), Some(Path::new("/p")));
    }

    #[test]
    fn only_the_configured_build_types_flags_are_taken() {
        // Four sets of flags live in one cache and exactly one of them applies. Taking all four would put
        // `-O3 -DNDEBUG` and `-g` in one configuration, which is a compilation that does not exist.
        let parsed = cache(
            "CMAKE_BUILD_TYPE:STRING=Release\n\
             CMAKE_CXX_FLAGS:STRING=-Wall\n\
             CMAKE_CXX_FLAGS_DEBUG:STRING=-g\n\
             CMAKE_CXX_FLAGS_RELEASE:STRING=-O3 -DNDEBUG\n",
        );

        assert_eq!(parsed.flags, ["-Wall", "-O3", "-DNDEBUG"]);
    }

    #[test]
    fn a_cache_with_no_build_type_takes_no_configurations_flags() {
        let parsed = cache(
            "CMAKE_CXX_FLAGS:STRING=-Wall\n\
             CMAKE_CXX_FLAGS_RELEASE:STRING=-O3\n",
        );

        assert_eq!(parsed.flags, ["-Wall"], "nothing says which set applies");
    }

    #[test]
    fn a_value_may_contain_an_equals_and_a_key_may_not_contain_the_type() {
        let parsed = cache("CMAKE_CXX_FLAGS:STRING=-DFOO=bar -I/opt/a=b\n");

        assert_eq!(parsed.flags, ["-DFOO=bar", "-I/opt/a=b"]);
    }

    #[test]
    fn an_empty_standard_is_not_a_standard() {
        let parsed = cache("CMAKE_CXX_STANDARD:STRING=\n");
        assert_eq!(parsed.standard, None);
    }

    #[test]
    fn a_cache_that_does_not_export_a_database_says_so() {
        let parsed = cache("CMAKE_EXPORT_COMPILE_COMMANDS:BOOL=OFF\n");
        assert!(!parsed.exports_compile_commands);
    }

    #[test]
    fn a_database_in_a_build_directory_is_found_and_the_root_wins_when_it_has_one() {
        // The walk reads directories, so this one needs a real filesystem: `find_database` searches with
        // `read_dir` and reads with the provider, exactly like the project scan (`project_files`) — a question
        // about a directory is not a question a provider of file *contents* can answer.
        let root = std::env::temp_dir().join("cppls-build-search");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("build/deep")).expect("the fixture directories");
        std::fs::create_dir_all(root.join("cmake-build-debug")).expect("the fixture directories");

        let entry = |file: &str| {
            format!(
                "[\n  {{\"directory\": \"/p\", \"file\": \"{file}\", \
                 \"arguments\": [\"g++\", \"-c\", \"{file}\"]}}\n]\n"
            )
        };

        // Two build directories, and the walk has to pick one deterministically.
        std::fs::write(root.join("build/deep/compile_commands.json"), entry("/p/deep.cpp"))
            .expect("the fixture writes");
        std::fs::write(
            root.join("cmake-build-debug/compile_commands.json"),
            entry("/p/cmake.cpp"),
        )
        .expect("the fixture writes");

        let (found, problems) = find_database(&crate::DiskFiles, &root, None);
        assert!(problems.is_empty(), "{problems:?}");
        let found = found.expect("a database is found");
        assert_eq!(found.origin, DatabaseOrigin::BuildDirectory);
        assert_eq!(found.commands.len(), 1);
        assert!(
            crate::file::paths::normalize_path(&found.path, cfg!(windows))
                .ends_with("cmake-build-debug/compile_commands.json"),
            "the shallower of the two, and the lexicographically first: {}",
            found.path.display()
        );

        // And the root's own database wins over both.
        std::fs::write(root.join("compile_commands.json"), entry("/p/root.cpp"))
            .expect("the fixture writes");
        let (found, _) = find_database(&crate::DiskFiles, &root, None);
        let found = found.expect("a database is found");
        assert_eq!(found.origin, DatabaseOrigin::Root);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_configured_database_is_the_only_one_read() {
        // A file the project named is the project's answer: falling back to a search would analyse a compilation
        // nobody asked for.
        let files = MemoryFiles::new().with_file(
            "/p/build/compile_commands.json",
            "[\n  {\"directory\": \"/p\", \"file\": \"/p/a.cpp\", \"arguments\": [\"g++\", \"-c\", \"/p/a.cpp\"]}\n]\n",
        );

        let (found, problems) = find_database(&files, Path::new("/p"), Some(Path::new("build/compile_commands.json")));
        let found = found.expect("the named database is read");
        assert_eq!(found.origin, DatabaseOrigin::Configured);
        assert!(problems.is_empty());

        let (none, problems) = find_database(&files, Path::new("/p"), Some(Path::new("elsewhere.json")));
        assert!(none.is_none());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("elsewhere.json"));
    }

    #[test]
    fn a_vendored_build_tree_does_not_configure_the_project_around_it() {
        let root = std::env::temp_dir().join("cppls-build-foreign");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("third_party/foo/build")).expect("the fixture directories");
        std::fs::write(
            root.join("third_party/foo/build/CMakeCache.txt"),
            "CMAKE_HOME_DIRECTORY:INTERNAL=/somewhere/else\nCMAKE_CXX_COMPILER:FILEPATH=/usr/bin/g++\n",
        )
        .expect("the fixture writes");

        let (cache, problems) = find_cmake_cache(&crate::DiskFiles, &root);

        assert!(cache.is_none(), "a cache for another project is not this project's");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("another source directory"), "{problems:?}");

        let _ = std::fs::remove_dir_all(&root);
    }
}

