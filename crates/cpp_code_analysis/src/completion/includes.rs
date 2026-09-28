//! **The headers a `#include` may name** — the search path, read once.
//!
//! `#include` completion is the one place where the answer is not in the program: what follows `#include <` is a
//! *file name*, chosen from the directories the compiler was told to search. Nothing in the index answers it — the
//! index holds the files somebody has already read, and a reader typing `#include <chro` wants the header that is
//! there, whether or not any file has included it yet.
//!
//! ```text
//! the search path   the `-I` and `-isystem` directories, in the order the compiler was given them
//! the names         every file under them with a source extension, spelled **relative to its directory**
//! the two forms     `<name>` searches all of them; `"name"` searches the including file's own directory first
//! ```
//!
//! # What this is not
//!
//! Not an include *resolver*: that lives in [`crate::include`] and is what turns an `#include` into a path. This is
//! the other direction — a list of spellings that could be written — and it deliberately does not ask whether a
//! spelling resolves, because the walk that produced it already knows it does.
//!
//! Not complete, and it says so: a directory that could not be read, or a tree deeper than `MAX_HEADER_DEPTH`,
//! contributes nothing, and the amount left out is recorded ([`HeaderIndex::unread`]) rather than hidden. The
//! budget is what keeps a completion from walking a `node_modules`-sized tree on a keystroke.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How deep under an include directory a header is looked for.
///
/// Six is not a limit anybody hits: the standard library's own layout is `bits/…` and `c++/12/bits/…`,
/// `sys/types.h` is two, and a project that nests its headers deeper than this is one whose include spelling is a
/// path rather than a name. The bound exists so that a directory which is not an include directory — somebody's
/// `-I` pointing at a home directory — cannot turn a keystroke into a full filesystem walk.
const MAX_HEADER_DEPTH: usize = 6;

/// How many headers one index will hold.
///
/// A number large enough for the whole of a C++ toolchain (MSVC's `include` is around 1 500 files, libstdc++'s
/// with its target directories around 4 000) and small enough that a wrong `-I` costs a bounded amount of memory
/// rather than a bounded amount of patience. What is dropped is counted in [`HeaderIndex::unread`].
const MAX_HEADERS: usize = 12_000;

/// The extensions a header may have.
///
/// `.h` and its C++ relatives, plus the extensionless and `.hpp`/`.hxx` families, plus the two module-interface
/// spellings (`.cppm`, `.ixx`). A file without one of these is not offered: a `#include <README>` is legal and
/// pointless, and a list that contained every file in the directory would be mostly things nobody includes.
const HEADER_EXTENSIONS: &[&str] = &[
    "h", "hh", "hpp", "hxx", "h++", "inl", "ipp", "tcc", "tpp", "inc", "def", "cppm", "ixx", "mpp", "cuh",
];

/// One header the search path can name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Header {
    /// The spelling to write between the delimiters: `vector`, `sys/types.h`, `bits/stl_vector.h`.
    pub name: String,
    /// Which include directory it was found under, as an index into the paths the index was built from — or
    /// [`HeaderIndex::PROJECT`] for a header of the project's own.
    pub directory: usize,
    /// How deep under that directory it is, which is what orders two candidates whose names both match.
    pub depth: usize,
}

/// The headers the search path holds, sorted by name.
#[derive(Debug, Clone, Default)]
pub struct HeaderIndex {
    headers: Vec<Header>,
    /// The project's **own** headers, kept apart from the search path's.
    ///
    /// Apart because they are re-derived rather than re-scanned: the search path is a filesystem walk that happens
    /// once, and the project's file list is a `Vec` a session drops at its own door whenever a build system hands
    /// over another translation unit — see [`crate::Session::add_project_files`]. One list, two lifetimes.
    project: Vec<PathBuf>,
    /// The directory the project's own headers are spelled from.
    root: PathBuf,
    /// How many directories could not be read, or were cut short by the budget.
    unread: usize,
    /// Whether the budget was reached, so that "these are all of them" is not claimed when it is not true.
    truncated: bool,
}

/// A **shared, mutable** header index — what a session holds.
///
/// Behind a lock rather than plain because the project half changes and the search-path half does not: a session
/// adds files when a build system tells it about them, and a completion can be asked for at any moment before,
/// during or after that. The lock is held for the length of one lookup, and the answer it guards is a `Vec` of
/// strings.
pub type SharedHeaders = Arc<std::sync::RwLock<HeaderIndex>>;

impl HeaderIndex {
    /// The directory index used for the project's own headers — see [`HeaderIndex::with_project`].
    ///
    /// A constant rather than an offset into the search path, because the project's headers are not *found* by
    /// searching: `#include "widget.h"` is resolved against the including file's own directory, and that is a
    /// different rule from every other entry in the list.
    pub const PROJECT: usize = usize::MAX;

    /// Read every header under these directories.
    ///
    /// The walk is breadth-first with a depth bound and a file budget, so it is bounded twice: a directory tree
    /// that is a symlink loop costs the budget, and one that is merely enormous costs the budget too. Symlinked
    /// *directories* are not followed, for the reason [`crate::session`]'s project scan does not follow them — the
    /// cheap test is the entry's own type, and it is also what keeps one file from being listed under two names.
    pub fn read<I>(directories: I, root: &Path) -> HeaderIndex
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut headers: Vec<Header> = Vec::new();
        let mut names: BTreeSet<(String, usize)> = BTreeSet::new();
        let mut unread = 0usize;
        let mut truncated = false;

        for (directory, root) in directories.into_iter().enumerate() {
            let mut pending: std::collections::VecDeque<(PathBuf, String, usize)> =
                std::collections::VecDeque::from([(root.clone(), String::new(), 0)]);

            while let Some((path, prefix, depth)) = pending.pop_front() {
                if headers.len() >= MAX_HEADERS {
                    truncated = true;
                    break;
                }

                let Ok(entries) = std::fs::read_dir(&path) else {
                    unread += 1;
                    continue;
                };

                for entry in entries.flatten() {
                    let Ok(kind) = entry.file_type() else {
                        continue;
                    };
                    if kind.is_symlink() {
                        continue;
                    }

                    let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
                        continue;
                    };
                    let name = if prefix.is_empty() {
                        file_name.clone()
                    } else {
                        format!("{prefix}/{file_name}")
                    };

                    if kind.is_dir() {
                        if depth < MAX_HEADER_DEPTH {
                            pending.push_back((entry.path(), name, depth + 1));
                        }
                        continue;
                    }

                    if !kind.is_file() || !is_a_header(&file_name) {
                        continue;
                    }

                    if headers.len() >= MAX_HEADERS {
                        truncated = true;
                        break;
                    }

                    // One spelling per directory: the same file reached twice through a link is one header, and a
                    // header found under two `-I` directories is two — because they are two different files to a
                    // compiler, and offering the first one is the search order's job rather than this list's.
                    if names.insert((name.clone(), directory)) {
                        headers.push(Header {
                            name,
                            directory,
                            depth,
                        });
                    }
                }
            }
        }

        headers.sort_by(|one, other| {
            one.name
                .cmp(&other.name)
                .then_with(|| one.directory.cmp(&other.directory))
        });

        HeaderIndex {
            headers,
            project: Vec::new(),
            root: root.to_path_buf(),
            unread,
            truncated,
        }
    }

    /// **Say which files the project is made of**, so that its own headers can be offered beside the search
    /// path's.
    ///
    /// The reason this exists at all: `#include "config/version.h"` is the ordinary way a project includes its own
    /// header, and the directory that spelling is relative to is the project root — which is not usually an `-I`
    /// directory, and is never the same as the *including* file's directory when the two sit in different
    /// subdirectories. The spelling produced here is the one a reader at the root would write.
    ///
    /// Called again whenever the file list grows, which is why the index a session holds is behind a lock: see
    /// [`crate::Session::add_project_files`].
    pub fn with_project(mut self, files: impl IntoIterator<Item = PathBuf>) -> HeaderIndex {
        self.project = files.into_iter().filter(|file| is_a_header(&file_name_of(file))).collect();

        // Sorted and deduplicated for the same reason the search path is: the walk's order is the filesystem's,
        // and an answer whose order depends on which directory entry the OS returned first cannot be compared
        // between two runs.
        self.project.sort();
        self.project.dedup();

        self
    }

    /// The headers whose name matches a spelling being typed, best first, at most `limit`.
    ///
    /// # The rules, and the second one is what makes the list usable
    ///
    /// 1. **How the spelling matched** (`match_rank`): the segment that *is* what was typed, then one that begins
    ///    with it, then one that contains it. This decides first, because a reader who has finished typing a name
    ///    means the header called that.
    /// 2. **The project's own headers before the search path's**, when the match is equally good — a reader typing
    ///    in a project means that project's files far more often than the standard library's, and which is which is
    ///    said in the item's detail either way.
    /// 3. **Shallower first**: among equally good matches, the header nearer the top of its search directory wins,
    ///    which is what puts `experimental/vector.h` above `bits/stl_vector.h` for the prefix `vector`.
    /// 4. **Then the spelling**, so that the order is the same on every run and a reader can find a header by
    ///    looking where its name is.
    ///
    /// An empty prefix matches everything at the same rank, so the answer to `#include <` with nothing typed is the
    /// project's own headers and then the search path, shallowest first — a list a reader can look something up in,
    /// rather than a sample of it in filesystem order.
    pub fn matching(&self, prefix: &str, limit: usize) -> Vec<Header> {
        let mut ranked: Vec<(u8, u8, usize, String, usize)> = Vec::new();

        for (name, directory) in self.spellings() {
            let Some(rank) = match_rank(&name, prefix) else {
                continue;
            };

            // `0` for the project's own, `1` for the search path's: the whole of the second rule, expressed as a
            // sort key so that the comparison below is one `sort` rather than three branches. It sorts *after* the
            // rank, because a project header that merely contains the letters must not be preferred to a standard
            // header the reader has named exactly.
            let from_the_search_path = u8::from(directory != Self::PROJECT);
            let depth = if directory == Self::PROJECT {
                // A project header has no depth under a search directory: `config/version.h` is written from the
                // project root, and how many directories that is says nothing about how near it is.
                0
            } else {
                name.matches('/').count()
            };

            ranked.push((rank, from_the_search_path, depth, name, directory));
        }

        ranked.sort();

        ranked
            .into_iter()
            .take(limit)
            .map(|(_, _, _, name, directory)| Header {
                depth: name.matches('/').count(),
                name,
                directory,
            })
            .collect()
    }

    /// Every header the index can name, as a **spelling and where it was found**.
    ///
    /// The project's own are spelled here rather than stored as [`Header`]s because the spelling depends on the
    /// root, and the list can be replaced wholesale when a build system hands over more files. The search path's
    /// are stored, because reading that tree is the expensive half and it happens once.
    ///
    /// A project file whose path is not under the root is spelled **absolutely**: it is a real header a reader may
    /// include, and a spelling produced by stripping a prefix that is not there would be a lie about where it is.
    fn spellings(&self) -> Vec<(String, usize)> {
        let mut found: Vec<(String, usize)> = self
            .project
            .iter()
            .map(|file| {
                let spelling = file
                    .strip_prefix(&self.root)
                    .map(|relative| relative.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| file.to_string_lossy().replace('\\', "/"));

                (spelling, Self::PROJECT)
            })
            .collect();

        found.extend(
            self.headers
                .iter()
                .map(|header| (header.name.clone(), header.directory)),
        );

        found
    }

    /// How many headers the index can name — the search path's, plus the project's.
    pub fn len(&self) -> usize {
        self.headers.len() + self.project.len()
    }

    pub fn is_empty(&self) -> bool {
        self.headers.is_empty() && self.project.is_empty()
    }

    /// How many directories could not be read — the part of the answer that is missing rather than empty.
    pub fn unread(&self) -> usize {
        self.unread
    }

    /// Did the walk stop at the budget, so that the list is a prefix of the truth rather than a sample of it?
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// A file's name, as a `String`, or empty when the path has none.
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// How well a header name matches a spelling being typed.
///
/// ```text
/// 0   a path segment **is** what was typed            `vector`   for the prefix `vector`
/// 1   a path segment begins with it                   `vector_thing.h`
/// 2   a path segment contains it elsewhere            `stl_vector.h`
/// 3   the match spans a `/`                           the `e/b` of `include/boost.h`
/// ```
///
/// A **segment** is what a reader thinks in. `#include <sys/types.h>` is a path somebody is writing, and a plain
/// substring test on the whole spelling would rank `some_vector_thing.h` above `bits/stl_vector.h` for the prefix
/// `vector` — the wrong way round, because in the second the reader has finished typing `vector` and in the first
/// they have not started.
///
/// The first tier exists because the whole of `vector` being one segment is a stronger claim than the beginning of
/// `vector_thing.h`: it is the header named exactly that.
fn match_rank(name: &str, prefix: &str) -> Option<u8> {
    if prefix.is_empty() {
        return Some(0);
    }

    let prefix = prefix.to_ascii_lowercase();
    let lowered = name.to_ascii_lowercase();

    if lowered.split('/').any(|segment| segment == prefix) {
        return Some(0);
    }

    if lowered
        .split('/')
        .any(|segment| segment.starts_with(&prefix))
    {
        return Some(1);
    }

    if lowered
        .split('/')
        .any(|segment| segment.contains(&prefix))
    {
        return Some(2);
    }

    lowered.contains(&prefix).then_some(3)
}

/// Does this file name look like a header?
fn is_a_header(file_name: &str) -> bool {
    let Some((stem, extension)) = file_name.rsplit_once('.') else {
        // A name with no dot: `iostream` has none, and neither does a directory. Matched against the list of
        // extensions rather than rejected, because the C++ standard's own headers are spelled without one in the
        // standard and *with* one in every implementation — the extensionless spelling is a real header name in
        // the `<vector>` sense and a real file in a project that writes `#include "config"`.
        return !file_name.is_empty() && !file_name.starts_with('.');
    };

    if stem.is_empty() {
        return false;
    }

    HEADER_EXTENSIONS.iter().any(|known| extension.eq_ignore_ascii_case(known))
}

/// The include directories of a configuration, as paths to read.
pub fn search_directories(config: &crate::CompilerConfig) -> Vec<PathBuf> {
    config
        .include_paths
        .iter()
        .map(|path| path.directory.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_of(names: &[&str]) -> HeaderIndex {
        HeaderIndex {
            headers: names
                .iter()
                .map(|name| Header {
                    name: name.to_string(),
                    directory: 0,
                    // The depth is a **fact about the spelling**, not a parameter: how deep a header is under its
                    // search directory is what the last tie-break is about, and a fixture passing a depth unrelated
                    // to the name would be testing an index no walk can produce.
                    depth: name.matches('/').count(),
                })
                .collect(),
            project: Vec::new(),
            root: PathBuf::from("/p"),
            unread: 0,
            truncated: false,
        }
    }

    /// **A top-level header beats one buried under implementation directories**, which is the difference between a
    /// usable list and a list of two thousand near-misses: a reader typing `vector` means `<vector>`, and second to
    /// that a header with `vector` in its own name rather than one that merely contains the letters.
    #[test]
    fn the_nearest_match_comes_first() {
        let index = index_of(&[
            "experimental/vector",
            "bits/stl_vector.h",
            "sys/types.h",
            "vector",
            "experimental/vector_thing.h",
        ]);

        let found: Vec<String> = index
            .matching("vector", 10)
            .into_iter()
            .map(|header| header.name)
            .collect();
        assert_eq!(
            found,
            vec![
                "vector",
                "experimental/vector",
                "experimental/vector_thing.h",
                "bits/stl_vector.h",
            ],
            "the header named for the word, then the shallower of the two that begin with it, \
             then the one that merely contains it"
        );
    }

    /// A spelling that names a **segment** matches, because that is how a path is typed — and two such headers are
    /// ordered by their spelling, so the order is the same on every run.
    #[test]
    fn a_segment_that_starts_with_the_prefix_matches() {
        let index = index_of(&["sys/types.h", "linux/types.h", "ctype.h"]);

        let found: Vec<String> = index
            .matching("types", 10)
            .into_iter()
            .map(|header| header.name)
            .collect();
        assert_eq!(found, vec!["linux/types.h", "sys/types.h"]);
    }

    /// Nothing typed yet: the **shallowest** headers first, alphabetically within a level. The list a reader gets
    /// from `#include <` is the top level of the search path and then the level below it, rather than the twelve
    /// thousand a deep walk finds — which is the ordering, and the budget, doing the same job from two directions.
    #[test]
    fn an_empty_prefix_offers_the_search_path_shallowest_first() {
        let index = index_of(&["bits/stl_vector.h", "vector", "cstddef"]);

        let found: Vec<String> = index
            .matching("", 3)
            .into_iter()
            .map(|header| header.name)
            .collect();
        assert_eq!(
            found,
            vec!["cstddef", "vector", "bits/stl_vector.h"],
            "the top level in spelling order, then the depth below it"
        );
    }

    #[test]
    fn a_wrong_prefix_matches_nothing() {
        let index = index_of(&["vector"]);
        assert!(index.matching("widget", 10).is_empty());
    }

    #[test]
    fn a_header_without_an_extension_is_a_header() {
        assert!(is_a_header("iostream"));
        assert!(is_a_header("vector.hpp"));
        assert!(is_a_header("stl_vector.h"));
        assert!(!is_a_header(".gitignore"));
        assert!(!is_a_header("main.cpp2"));
    }

    /// The list is **bounded**, and it says so rather than claiming to be complete — the two properties that make
    /// it safe to put behind a keystroke on a directory nobody meant to point `-I` at.
    #[test]
    fn a_directory_that_cannot_be_read_is_counted_rather_than_ignored() {
        let index = HeaderIndex::read([PathBuf::from("/no/such/directory/exists/here")], Path::new("/p"));
        assert!(index.is_empty());
        assert_eq!(index.unread(), 1, "the gap is recorded");
        assert!(!index.truncated());
    }

    /// A project's own headers are offered **beside** the search path's and ahead of them, spelled relative to the
    /// project root — which is what a reader writes in `#include "config/version.h"`.
    #[test]
    fn the_projects_own_headers_are_spelled_from_the_root_and_offered_first() {
        let index = index_of(&["version"]).with_project([
            PathBuf::from("/p/config/version.h"),
            PathBuf::from("/p/main.cpp"),
            PathBuf::from("/p/vector"),
        ]);

        let found: Vec<(String, usize)> = index
            .matching("version", 10)
            .into_iter()
            .map(|header| (header.name, header.directory))
            .collect();
        assert_eq!(
            found,
            vec![
                ("version".to_string(), 0),
                ("config/version.h".to_string(), HeaderIndex::PROJECT),
            ],
            "the search path's exact match first, then the project's own file — the rank decides, and \
             where a header comes from only breaks a tie"
        );

        assert!(
            index
                .matching("", 10)
                .iter()
                .any(|header| header.name == "vector"),
            "a header with no extension is a header of the project too"
        );
        assert!(
            !index
                .matching("", 10)
                .iter()
                .any(|header| header.name == "main.cpp"),
            "and a source file is not"
        );
    }
}
