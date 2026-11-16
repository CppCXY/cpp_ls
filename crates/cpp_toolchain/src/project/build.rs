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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::file::paths::FileProvider;
use crate::include::config::{CompileCommands, parse_compile_commands};

/// The name a compile database is conventionally written under.
pub const COMPILE_DATABASE_FILE: &str = "compile_commands.json";

/// How deep the search for a database goes, relative to the project root.
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
    let mut database = parse_compile_commands(&json);

    // **The response files, read here rather than wherever the flags are used.** CMake writes a target's include
    // paths and defines into `@CMakeFiles/<target>.dir/includes_CXX.rsp` and names that file in the command, for the
    // Makefile and Ninja generators alike — measured on a real project: **every one** of its 108 entries was that
    // shape. A reader that does not follow the indirection therefore sees a project with **no include paths at all**:
    // its own headers stop resolving and the file list stops being the build's.
    //
    // Expanded once, where the database is read, because everything downstream — the session's flags, the per-file
    // `command_for`, the toolchain's standard — reads these same arguments, and each of them would otherwise have to
    // remember. The working directory is the entry's own, which is what a compiler resolves a relative `@` against.
    for command in &mut database.commands {
        let directory = command.directory.clone();
        command.arguments = crate::include::config::expand_response_files(
            &command.arguments,
            directory.as_deref(),
            &mut |named| files.read(named),
        );
    }

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
                && !NEVER_SEARCHED
                    .iter()
                    .any(|skip| skip.eq_ignore_ascii_case(name))
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryFiles;

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
        std::fs::write(
            root.join("build/deep/compile_commands.json"),
            entry("/p/deep.cpp"),
        )
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

        let (found, problems) = find_database(
            &files,
            Path::new("/p"),
            Some(Path::new("build/compile_commands.json")),
        );
        let found = found.expect("the named database is read");
        assert_eq!(found.origin, DatabaseOrigin::Configured);
        assert!(problems.is_empty());

        let (none, problems) =
            find_database(&files, Path::new("/p"), Some(Path::new("elsewhere.json")));
        assert!(none.is_none());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("elsewhere.json"));
    }
}
