//! **Finding an `#include` the search path cannot, by matching the tail of what was written against the files that
//! are there.**
//!
//! # Why this exists
//!
//! An include resolves in one of two ways: a compile database says where the compiler looked, or the `include/`
//! convention beside `src/` is inferred ([`crate::include::include_roots_above`]). Both are *exact* — the file has to
//! be at the spelling asked for, under a directory something named.
//!
//! The gap is a project that states neither. Measured: a real CMake project with **no** `compile_commands.json` —
//! the ordinary state of a checkout nobody has configured, and the *permanent* state of a Visual Studio solution,
//! which CMake cannot export a database for at all — had a closure of **14 files instead of the whole program**,
//! because `#include "Session/StandardIOSession.h"` was searched for beside the including file and in MSVC's own
//! directories, and the file is at `CodeFormatServer/include/Session/StandardIOSession.h`.
//!
//! The configuration cost of a compile database is what makes this matter. A user who has to run a build system's
//! export target before the language server works is a user who will not, so the server has to do something useful
//! with the information the project *does* give it — which is that the file exists somewhere in the tree.
//!
//! # What it matches, and what it will not
//!
//! The rule is **the tail of the path, component by component, backwards**:
//!
//! ```text
//! written                                  candidates, best first
//! "Session/StandardIOSession.h"      ->    CodeFormatServer/include/Session/StandardIOSession.h
//!                                          (2 components matched, and nothing above them to disagree)
//! "detail/win_iocp_file_service.hpp" ->    asio/detail/win_iocp_file_service.hpp
//! "vector"                           ->    never matched: a name with no directory in it is not evidence
//! ```
//!
//! **A suffix match cannot be *wrong* about the name** — the last component is compared whole, case-insensitively on
//! the platforms where the filesystem is — and everything above it is required to match in order. What it can be is
//! *ambiguous*: two files called `config.h` under different directories both end with `detail/config.h`, and the
//! answer picks one. That is why the resolution records the path it chose and why this is a **fallback**:
//!
//! * it runs only after every exact candidate has been tried and failed, so it can never shadow a real `-I`;
//! * it is only reached at all when the config says [`fuzzy_includes`](crate::CompilerConfig::fuzzy_includes), so a
//!   project that has a database is never affected by it;
//! * a target that is a **bare file name** is refused outright, because `#include <vector>` would otherwise match
//!   the first `vector` anybody has — and angle includes are the ones that would.
//!
//! # The index, and why it is keyed by the first component of the file name
//!
//! Building one requires listing the project's files, which is a filesystem walk. That is done **once per root** and
//! shared process-wide ([`index_for`]), because the alternative is a walk per unresolved include and there are
//! hundreds of thousands of those in one index.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// How deep [`index_for`] will walk. Deep enough for `3rd/<library>-<version>/include/<library>/detail/`, which is
/// the deepest ordinary layout, and shallow enough that a project at the root of a drive cannot collect the drive.
const MAX_DEPTH: usize = 8;

/// How many files [`index_for`] will hold. A bound rather than a promise: a tree past this is one whose fuzzy
/// matches would be guesses among thousands of same-named files, and the walk stops rather than pretending.
const MAX_FILES: usize = 200_000;

/// **A file name, lowercased, mapped to every file under the root that ends with it.**
///
/// Keyed by the **last component** rather than by the whole path, because that is the part a wrong guess cannot get
/// wrong and because it is what makes a lookup a hash rather than a scan: `Session/StandardIOSession.h` looks up
/// `standardiosession.h`, and the handful of paths that come back are checked in full.
#[derive(Debug, Default)]
pub struct FuzzyIndex {
    /// `(path, lowercased last component)` — the component is stored rather than recomputed, because a lookup
    /// compares it against the include's own last component and the comparison is the one the key was built from.
    by_name: HashMap<String, Vec<PathBuf>>,
    /// The root the walk started from, so a path can be turned into the components a match is made of.
    root: PathBuf,
}

/// The indexes built so far, by root. Process-wide because a resolver is built per file and a walk per file is the
/// cost this exists to avoid.
fn cache() -> &'static Mutex<HashMap<PathBuf, Arc<FuzzyIndex>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<FuzzyIndex>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The index for `root`, building it on first use.
pub fn index_for(root: &Path) -> Option<Arc<FuzzyIndex>> {
    if let Ok(held) = cache().lock()
        && let Some(found) = held.get(root)
    {
        return Some(Arc::clone(found));
    }

    let built = Arc::new(FuzzyIndex::build(root));
    if built.by_name.is_empty() {
        // **Nothing to remember, and `None` rather than an empty index.** A root with no readable files would
        // otherwise be a cache entry that every later lookup hashes into for no answer, and the caller can skip the
        // work of asking.
        return None;
    }

    if let Ok(mut held) = cache().lock() {
        held.insert(root.to_path_buf(), Arc::clone(&built));
    }
    Some(built)
}

/// Forget every index. For tests, which build roots in a temporary directory that another test may later reuse.
pub fn forget_everything() {
    if let Ok(mut held) = cache().lock() {
        held.clear();
    }
}

impl FuzzyIndex {
    /// Walk `root` and record every file whose name could later be asked about.
    ///
    /// Uses `std::fs` rather than the session's `FileProvider`, and that is a deliberate limit rather than an
    /// oversight: a fuzzy match is a statement about what is on **disk**, and a provider that answered from open
    /// buffers would make the answer depend on which files an editor happened to have loaded. The cost is that a
    /// project which exists only in memory has no fuzzy matching, which the tests work around by building roots on
    /// disk — see `include::mod`'s fuzzy tests.
    fn build(root: &Path) -> FuzzyIndex {
        let mut by_name: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut stack = vec![(root.to_path_buf(), 0usize)];
        let mut files = 0usize;

        while let Some((directory, depth)) = stack.pop() {
            if depth > MAX_DEPTH || files >= MAX_FILES {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(kind) = entry.file_type() else { continue };
                if kind.is_dir() {
                    // **Build trees are skipped by name.** A generated directory holds copies of the project's own
                    // headers — `build/.../foo.h` beside `include/foo.h` — and indexing them would make every fuzzy
                    // match ambiguous between the file a person wrote and the one a compiler copied.
                    if is_generated(&path) {
                        continue;
                    }
                    stack.push((path, depth + 1));
                } else if kind.is_file() {
                    let Some(name) = path.file_name().map(|name| name.to_string_lossy().to_lowercase())
                    else {
                        continue;
                    };
                    files += 1;
                    by_name.entry(name).or_default().push(path);
                }
            }
        }

        FuzzyIndex {
            by_name,
            root: root.to_path_buf(),
        }
    }

    /// **The files whose tail matches `written`, best first.**
    ///
    /// The comparison is component by component from the end: the last component must be equal (the key already
    /// guaranteed that), and every component before it must find its equal at the same distance from the end of the
    /// candidate. So `Session/StandardIOSession.h` matches a candidate whose last two components are exactly those,
    /// and does not match one that merely contains the words somewhere.
    ///
    /// `written` is the include's target as the file spelled it — `Session/StandardIOSession.h` — and a target with a
    /// single component is refused before any of this, by [`wanted_components`].
    pub fn matching(&self, written: &str) -> Vec<PathBuf> {
        let Some(components) = wanted_components(written) else {
            return Vec::new();
        };
        let Some(last) = components.last() else {
            return Vec::new();
        };

        let Some(candidates) = self.by_name.get(last.as_str()) else {
            return Vec::new();
        };

        let mut found: Vec<(usize, &PathBuf)> = candidates
            .iter()
            .filter(|candidate| ends_with_components(candidate, &components))
            .map(|candidate| (candidate.components().count(), candidate))
            .collect();

        // **Shallowest first**, because the shortest path that matches is the one closest to the root the project
        // named, and a deep match is more likely to be a vendored copy of the same header. The count is of
        // components and not of bytes, so a long directory name does not outrank a short one.
        found.sort_unstable_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));

        found
            .into_iter()
            .map(|(_, path)| path.clone())
            .collect()
    }

    /// The root this index describes.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// How many distinct file names are indexed. For a report, and for a test that has to know the walk happened.
    pub fn len(&self) -> usize {
        self.by_name.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// **The components of an include worth matching on**, or `None` when there is nothing to match.
///
/// `None` for a target with no directory in it — `vector`, `stdio.h` — and that is the rule that keeps this from
/// being dangerous. A bare name is spelled that way *because* the search path is supposed to find it, so a miss
/// means the search path is wrong, not that the file is somewhere else. Matching it against every same-named file
/// under the root would turn `#include <vector>` into whichever `vector` was found first, which is the confident
/// wrong answer this whole module is written to avoid.
///
/// Backslashes are treated as separators, because a project written on Windows spells includes that way and the
/// standard says the spelling is implementation-defined.
fn wanted_components(written: &str) -> Option<Vec<String>> {
    let parts: Vec<String> = written
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .map(str::to_lowercase)
        .collect();

    // One component is a bare name, and two are the shortest thing that names a place.
    if parts.len() < 2 {
        return None;
    }
    Some(parts)
}

/// Does `candidate` end with exactly `components`, in order?
fn ends_with_components(candidate: &Path, components: &[String]) -> bool {
    let parts: Vec<String> = candidate
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
        .collect();

    if parts.len() < components.len() {
        return false;
    }
    let tail = &parts[parts.len() - components.len()..];
    tail == components
}

/// **Is this directory a build tree**, and therefore one whose headers are copies?
///
/// By name, because that is what a build tree has: `build`, `out`, `.git`, `CMakeFiles`, `node_modules`. Skipping
/// them is not an optimisation — a generated `build/foo/bar.h` and a written `include/bar.h` both end with
/// `foo/bar.h` under some spelling, and indexing both would make the answer depend on which came back first.
fn is_generated(path: &Path) -> bool {
    const GENERATED: [&str; 9] = [
        "build",
        "out",
        ".git",
        ".svn",
        "cmakefiles",
        "node_modules",
        "target",
        ".cache",
        "cmake-build-debug",
    ];
    path.file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .is_some_and(|name| GENERATED.contains(&name.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A root with files in it, on disk — see `FuzzyIndex::build` for why this cannot be a `MemoryFiles` root.
    fn root_with(name: &str, files: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join("cppls-fuzzy-tests").join(name);
        let _ = std::fs::remove_dir_all(&root);
        for file in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().expect("a file has a parent")).expect("a directory");
            std::fs::write(&path, "// a header\n").expect("a file");
        }
        root
    }

    #[test]
    fn a_bare_file_name_is_never_matched() {
        // **The rule that makes this safe**, and the reason it is a function of its own: `#include <vector>` spelled
        // without a directory means the search path was supposed to find it. Matching it against whatever `vector`
        // is under the root would answer a question nobody asked with the first file that came back.
        let root = root_with("bare", &["include/vector", "detail/vector"]);
        let index = index_for(&root).expect("a root with files has an index");

        assert!(index.matching("vector").is_empty());
        assert!(index.matching("stdio.h").is_empty());
        // …and the same name *with* a directory is matched, so the rule is about the spelling and not the name.
        assert_eq!(index.matching("detail/vector").len(), 1);
    }

    #[test]
    fn the_tail_is_matched_component_by_component() {
        let root = root_with(
            "tail",
            &[
                "CodeFormatServer/include/Session/StandardIOSession.h",
                "other/Session/StandardIOSession.h",
                "Session/StandardIOSession.h",
                "not/Session/Other.h",
            ],
        );
        let index = index_for(&root).expect("an index");

        let found = index.matching("Session/StandardIOSession.h");
        // Three files end with those two components; the fourth does not, and a match that only compared the file
        // name would have returned it.
        assert_eq!(found.len(), 3);
        assert!(found.iter().all(|path| path.ends_with("Session/StandardIOSession.h")));
    }

    #[test]
    fn the_shallowest_match_comes_first() {
        // The shortest path that matches is the one nearest the root the project named; a deep one is more likely a
        // vendored copy of the same header.
        let root = root_with(
            "shallow",
            &["deep/nested/very/far/down/detail/config.h", "detail/config.h"],
        );
        let index = index_for(&root).expect("an index");

        let found = index.matching("detail/config.h");
        assert_eq!(found.len(), 2);
        assert!(
            found[0].ends_with("detail/config.h") && !found[0].to_string_lossy().contains("nested"),
            "expected the shallow one first, got {:?}",
            found[0]
        );
    }

    #[test]
    fn a_directory_that_merely_contains_the_word_does_not_match() {
        // A suffix match must be a suffix: `MySession/StandardIOSession.h` ends with `StandardIOSession.h` but its
        // parent is not `Session`, so the two components asked for are not the two it has.
        let root = root_with("suffix", &["MySession/StandardIOSession.h"]);
        let index = index_for(&root).expect("an index");

        assert!(index.matching("Session/StandardIOSession.h").is_empty());
        assert_eq!(index.matching("MySession/StandardIOSession.h").len(), 1);
    }

    #[test]
    fn a_build_tree_is_not_indexed() {
        // A generated directory holds copies of the project's own headers, so indexing it would make every match
        // ambiguous between the file a person wrote and the one a compiler copied.
        let root = root_with(
            "generated",
            &["include/Widget.h", "build/include/Widget.h", "out/include/Widget.h"],
        );
        let index = index_for(&root).expect("an index");

        let found = index.matching("include/Widget.h");
        assert_eq!(found.len(), 1, "expected only the written header, got {found:?}");
        assert!(!found[0].to_string_lossy().contains("build"));
        assert!(!found[0].to_string_lossy().contains("out"));
    }

    #[test]
    fn a_root_with_nothing_in_it_has_no_index() {
        // `None` rather than an empty index, so a caller can skip the work of asking rather than hashing into a table
        // that answers nothing.
        let root = std::env::temp_dir().join("cppls-fuzzy-tests").join("empty");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a directory");

        assert!(index_for(&root).is_none());
    }
}
