//! Paths, file identities, and reading files — behind a trait so that all of it is testable.
//!
//! # Why a trait and not `std::fs`
//!
//! Every interesting property of include resolution is about *which* of several candidate paths is found
//! first: a quoted include preferring the including file's own directory, a project directory shadowing a
//! system one, an include guard stopping a second visit. A test for any of those needs a specific tree of
//! files, and building one on disk is slow, needs cleanup, and makes the test depend on the filesystem's
//! case and separator rules rather than on the rules under test.
//!
//! So reading is a trait, with [`MemoryFiles`] for tests and [`DiskFiles`] for real use. The resolution
//! logic is then ordinary code with ordinary tests, and the part that touches the disk is small enough to
//! read in one sitting.
//!
//! # Why a `FileId` and not a `PathBuf`
//!
//! Two spellings of one file have to compare equal. `./a.h`, `a.h`, `inc/../a.h` and `A.H` on Windows are
//! one file, and a graph keyed by path would hold four nodes for it — four times the analysis, and a
//! reverse-dependency query that finds one of the four. A `FileId` is a number that the path table mints
//! once per distinct file, and everything downstream compares numbers.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// A file's identity within one analysis.
///
/// Minted by [`PathInterner`], stable for as long as that interner lives, and meaningless outside it — a
/// `FileId` is not a handle to anything a caller can open, which is what keeps it small and copyable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(u32);

impl FileId {
    /// The id's number, for a consumer that wants a dense array indexed by file.
    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// Mint an id from a number. For the layer that owns the numbering ([`crate::Vfs`]) and nothing else.
    pub fn new(number: u32) -> FileId {
        FileId(number)
    }
}

/// Reads files. The only thing in this layer that touches a filesystem.
pub trait FileProvider {
    /// The contents of a file, or `None` if it cannot be read.
    ///
    /// `None` rather than an error, because every reason a file cannot be read — missing, a directory,
    /// permissions, a deleted buffer, a path the editor has open but not yet saved — leads to the same
    /// decision: this include did not resolve, and the analysis continues without it.
    fn read(&self, path: &Path) -> Option<String>;

    /// Does this path name a file that can be read?
    ///
    /// Separate from [`read`](Self::read) so that resolution does not have to read a file to learn whether
    /// it exists. A search path of twenty directories, each `stat`ed rather than read, is the difference
    /// between a completion list that appears and one that does not.
    fn exists(&self, path: &Path) -> bool;

    /// Is this filesystem case-insensitive?
    ///
    /// Part of the provider because it is a property of the *filesystem*, not of the platform: a case-
    /// sensitive volume on Windows and a case-folding one on macOS both exist, and resolution is wrong on
    /// one of them whichever way it is hard-coded.
    fn is_case_insensitive(&self) -> bool {
        cfg!(windows)
    }

    /// Is this path one the **editor has open**, rather than one on disk?
    ///
    /// A question only an overlay can answer, which is why the default says `false`: a provider that is not an
    /// overlay reads the disk, and the disk is never a buffer. [`crate::OpenDocuments`] and
    /// [`OverlayFiles`] answer it for real, and the VFS records the answer with each entry —
    /// a view has to be able to say whether the analysis saw what the user sees.
    fn is_open_buffer(&self, _path: &Path) -> bool {
        false
    }
}

/// The real filesystem.
#[derive(Debug, Clone, Default)]
pub struct DiskFiles;

impl FileProvider for DiskFiles {
    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn exists(&self, path: &Path) -> bool {
        path.is_file()
    }
}

/// Files held in memory.
///
/// The test double, and also the shape an editor needs: an unsaved buffer's include must resolve to the
/// buffer, not to whatever is on disk. A caller with open documents puts them here and the rest fall
/// through to [`DiskFiles`] via [`OverlayFiles`].
///
/// Reads are counted, because "did this analysis read that file" is a question about performance that no
/// assertion about the *result* can answer: a walk that read a file and then found nothing in it produces the
/// same graph as one that never looked. [`MemoryFiles::reads_of`] is what tells them apart.
#[derive(Debug, Clone, Default)]
pub struct MemoryFiles {
    files: HashMap<String, String>,
    case_insensitive: bool,
    reads: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    probes: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl MemoryFiles {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_case_insensitive(mut self, value: bool) -> Self {
        self.case_insensitive = value;
        self
    }

    /// How many times a path has been read, by normalized key.
    ///
    /// Shared across clones, so a provider moved into a walk and a copy kept by the test see the same count —
    /// which is the only way this can be used from outside the call that did the reading.
    pub fn reads_of(&self, path: impl AsRef<Path>) -> usize {
        let wanted = normalize_path(path.as_ref(), self.case_insensitive);
        let reads = self.reads.lock().expect("the read log is not poisoned");
        reads.iter().filter(|read| **read == wanted).count()
    }

    /// Every path read, in order, for a failure message that says what happened.
    pub fn reads(&self) -> Vec<String> {
        self.reads
            .lock()
            .expect("the read log is not poisoned")
            .clone()
    }

    /// How many times a path has been **looked for**, by normalized key.
    ///
    /// The counterpart of [`MemoryFiles::reads_of`], and the question it answers is the one the resolver asks: a
    /// candidate path is probed once per `#include` that could name it. So "did anything search for this header"
    /// is answerable here and nowhere else — a cache hit that never calls `exists` is a hit that never resolved
    /// an include, which is a hit that never built a summary.
    pub fn exists_of(&self, path: impl AsRef<Path>) -> usize {
        let wanted = normalize_path(path.as_ref(), self.case_insensitive);
        let probes = self.probes.lock().expect("the probe log is not poisoned");
        probes.iter().filter(|probe| **probe == wanted).count()
    }

    /// Every path looked for, in order.
    pub fn probes(&self) -> Vec<String> {
        self.probes
            .lock()
            .expect("the probe log is not poisoned")
            .clone()
    }

    /// Add a file. The key is normalized the same way resolution normalizes a path.
    pub fn insert(&mut self, path: impl AsRef<Path>, contents: impl Into<String>) -> &mut Self {
        self.files.insert(
            normalize_path(path.as_ref(), self.case_insensitive),
            contents.into(),
        );
        self
    }

    pub fn with_file(mut self, path: impl AsRef<Path>, contents: impl Into<String>) -> Self {
        self.insert(path, contents);
        self
    }

    /// Build from `(path, contents)` pairs.
    pub fn from_files<I, P>(files: I, case_insensitive: bool) -> Self
    where
        I: IntoIterator<Item = (P, &'static str)>,
        P: AsRef<Path>,
    {
        let mut memory = MemoryFiles::new().with_case_insensitive(case_insensitive);
        for (path, contents) in files {
            memory.insert(path, contents);
        }
        memory
    }
}

impl FileProvider for MemoryFiles {
    fn read(&self, path: &Path) -> Option<String> {
        let key = normalize_path(path, self.case_insensitive);

        self.reads
            .lock()
            .expect("the read log is not poisoned")
            .push(key.clone());

        self.files.get(&key).cloned()
    }

    fn exists(&self, path: &Path) -> bool {
        let key = normalize_path(path, self.case_insensitive);

        self.probes
            .lock()
            .expect("the probe log is not poisoned")
            .push(key.clone());

        self.files.contains_key(&key)
    }

    fn is_case_insensitive(&self) -> bool {
        self.case_insensitive
    }
}

/// One provider in front of another: open documents first, then the disk.
///
/// What an editor actually needs. An unsaved buffer's includes have to resolve against what the user has
/// typed, and a buffer that has never been saved has no file on disk at all — so the overlay is the
/// difference between "works while you type" and "works after you save".
#[derive(Debug, Clone)]
pub struct OverlayFiles<F: FileProvider, G: FileProvider> {
    pub overlay: F,
    pub fallback: G,
}

impl<F: FileProvider, G: FileProvider> OverlayFiles<F, G> {
    pub fn new(overlay: F, fallback: G) -> Self {
        OverlayFiles { overlay, fallback }
    }
}

impl<F: FileProvider, G: FileProvider> FileProvider for OverlayFiles<F, G> {
    fn read(&self, path: &Path) -> Option<String> {
        self.overlay.read(path).or_else(|| self.fallback.read(path))
    }

    fn exists(&self, path: &Path) -> bool {
        self.overlay.exists(path) || self.fallback.exists(path)
    }

    fn is_case_insensitive(&self) -> bool {
        self.overlay.is_case_insensitive() || self.fallback.is_case_insensitive()
    }

    /// The overlay's answer, and only the overlay's: the fallback is the filesystem, which has no buffers.
    fn is_open_buffer(&self, path: &Path) -> bool {
        self.overlay.is_open_buffer(path)
    }
}

/// Mints a [`FileId`] per distinct path, and remembers which path that was.
#[derive(Debug, Clone, Default)]
pub struct PathInterner {
    by_normalized: HashMap<String, FileId>,
    paths: Vec<PathBuf>,
    case_insensitive: bool,
}

impl PathInterner {
    pub fn new(case_insensitive: bool) -> Self {
        PathInterner {
            case_insensitive,
            ..PathInterner::default()
        }
    }

    pub fn case_insensitive(&self) -> bool {
        self.case_insensitive
    }

    /// The id for a path, minting one if this is the first time it is seen.
    ///
    /// The path is recorded as it was *first* spelled. A later spelling with different case — the same
    /// file, on a case-insensitive filesystem — resolves to the earlier id and does not replace the stored
    /// path, so a consumer displaying the file's name shows the spelling that was seen first rather than
    /// whichever include happened to be resolved last.
    pub fn intern(&mut self, path: &Path) -> FileId {
        let key = normalize_path(path, self.case_insensitive);

        if let Some(existing) = self.by_normalized.get(&key) {
            return *existing;
        }

        let id = FileId(self.paths.len() as u32);
        self.paths.push(path.to_path_buf());
        self.by_normalized.insert(key, id);
        id
    }

    /// The id for a path, if it has been seen.
    pub fn get(&self, path: &Path) -> Option<FileId> {
        self.by_normalized
            .get(&normalize_path(path, self.case_insensitive))
            .copied()
    }

    pub fn path(&self, id: FileId) -> Option<&Path> {
        self.paths.get(id.index()).map(PathBuf::as_path)
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// Normalize a path for comparison: separators unified, `.` and `..` resolved, case folded if asked.
///
/// # Why this is not `canonicalize`
///
/// `std::fs::canonicalize` touches the disk, follows symlinks, and fails on a file that does not exist —
/// and a path that does not exist is exactly the case resolution has to handle, because it is what makes
/// an include fail. It also resolves symlinks, which would merge two project directories that are
/// deliberately two different trees.
///
/// So this is lexical: `/` and `\` become one, a trailing separator goes, and `.`/`..` are folded without
/// consulting anything. The result compares equal for any two spellings of the same path, which is the
/// whole requirement.
///
/// # Drive letters
///
/// A Windows path has a *prefix* component (`C:`) that is not a directory and must not be joined with a
/// separator: `C:\a` is `C:/a`, and `C://a` is a different string that compares unequal to it. The prefix
/// is therefore attached to the root rather than treated as a segment, which is the one place this
/// function is not purely mechanical.
pub fn normalize_path(path: &Path, case_insensitive: bool) -> String {
    let mut parts: Vec<String> = Vec::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().into_owned())
            }
            Component::RootDir => {
                // A prefix directly before the root is part of it: `C:` + `/` is `C:/`.
                if let Some(last) = parts.last_mut()
                    && last.ends_with(':')
                {
                    last.push('/');
                    continue;
                }
                parts.push("/".to_string());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                // `..` pops a real directory. It is kept when there is nothing to pop — above a relative
                // path's start that changes what the path means — but **not** above the root, where `..`
                // is defined to be the root itself: `/../a.h` and `/a.h` are one file.
                match parts.last() {
                    Some(last) if last == "/" || last.ends_with(":/") => {}
                    Some(last) if last != ".." => {
                        parts.pop();
                    }
                    _ => parts.push("..".to_string()),
                }
            }
            Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
        }
    }

    // A rooted part already carries its own trailing separator when it is a drive; joining it with `/`
    // would double the separator, so the pieces are concatenated around it rather than naive-joined.
    let mut joined = String::new();
    for part in &parts {
        if !joined.is_empty() && !joined.ends_with('/') {
            joined.push('/');
        }
        joined.push_str(part);
    }

    if case_insensitive {
        joined.to_lowercase()
    } else {
        joined
    }
}

/// Join a directory and a relative path, then normalize.
pub fn join_normalized(directory: &Path, relative: &Path, case_insensitive: bool) -> PathBuf {
    let joined = directory.join(relative);
    PathBuf::from(normalize_path(&joined, case_insensitive))
}

/// The directory a path lives in, normalized, or an empty path when there is none.
pub fn parent_normalized(path: &Path, case_insensitive: bool) -> PathBuf {
    match path.parent() {
        Some(parent) => PathBuf::from(normalize_path(parent, case_insensitive)),
        None => PathBuf::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_folds_the_spellings_of_one_path() {
        let cases = [
            ("./a/b.h", "a/b.h"),
            ("a/./b.h", "a/b.h"),
            ("a/c/../b.h", "a/b.h"),
            ("a\\b.h", "a/b.h"),
            ("a//b.h", "a/b.h"),
        ];

        for (input, expected) in cases {
            assert_eq!(normalize_path(Path::new(input), false), expected, "{input}");
        }
    }

    #[test]
    fn normalization_keeps_what_it_cannot_resolve() {
        // Above the root, and above a relative path's start: `..` is part of the meaning.
        assert_eq!(normalize_path(Path::new("/../a.h"), false), "/a.h");
        assert_eq!(normalize_path(Path::new("../a.h"), false), "../a.h");
        assert_eq!(normalize_path(Path::new("a/../../b.h"), false), "../b.h");
    }

    #[test]
    fn normalization_can_fold_case() {
        assert_eq!(normalize_path(Path::new("A/B.H"), true), "a/b.h");
        assert_ne!(
            normalize_path(Path::new("A/B.H"), true),
            normalize_path(Path::new("A/B.H"), false)
        );
    }

    /// A drive prefix is part of the root, not a directory: `C:\a` is `C:/a` and never `C://a`, which would
    /// compare unequal to the same path written with forward slashes.
    #[test]
    fn a_drive_prefix_is_attached_to_the_root() {
        assert_eq!(normalize_path(Path::new("C:\\a\\b.h"), false), "C:/a/b.h");
        assert_eq!(normalize_path(Path::new("C:/a/b.h"), false), "C:/a/b.h");
        assert_eq!(normalize_path(Path::new("C:\\a\\..\\b.h"), false), "C:/b.h");
        assert_eq!(normalize_path(Path::new("C:\\..\\b.h"), false), "C:/b.h");
    }

    #[test]
    fn interning_gives_one_id_per_file() {
        let mut interner = PathInterner::new(false);

        let first = interner.intern(Path::new("a/b.h"));
        let again = interner.intern(Path::new("./a/b.h"));
        let other = interner.intern(Path::new("a/c.h"));

        assert_eq!(first, again, "two spellings of one path are one file");
        assert_ne!(first, other);
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn a_read_and_a_search_are_counted_separately() {
        // The two questions this double exists to answer, and they are not the same one: a file that was *read*
        // was parsed or hashed, and a path that was *looked for* was a candidate of some `#include`. A cache hit
        // is the case where the second number stays put.
        let files = MemoryFiles::new().with_file("/p/a.h", "int x;\n");

        assert_eq!(files.exists_of("/p/a.h"), 0);
        assert!(files.exists(Path::new("/p/a.h")));
        assert!(files.exists(Path::new("/p/./a.h")), "spellings fold");
        assert_eq!(files.exists_of("/p/a.h"), 2);
        assert_eq!(
            files.reads_of("/p/a.h"),
            0,
            "looking for a file is not reading it"
        );

        assert_eq!(files.read(Path::new("/p/a.h")).as_deref(), Some("int x;\n"));
        assert_eq!(files.reads_of("/p/a.h"), 1);
        assert_eq!(files.probes().len(), 2, "and the two logs are separate");
    }

    #[test]
    fn interning_folds_case_when_the_filesystem_does() {
        let mut interner = PathInterner::new(true);

        assert_eq!(
            interner.intern(Path::new("A.H")),
            interner.intern(Path::new("a.h"))
        );
    }

    /// A later spelling does not replace the stored one: a consumer showing the file's name shows the
    /// spelling that was seen first.
    #[test]
    fn the_first_spelling_of_a_path_is_the_one_remembered() {
        let mut interner = PathInterner::new(true);
        let id = interner.intern(Path::new("A.h"));
        interner.intern(Path::new("a.h"));

        assert_eq!(interner.path(id), Some(Path::new("A.h")));
    }

    #[test]
    fn memory_files_normalize_like_the_interner_does() {
        let memory = MemoryFiles::new().with_file("inc/a.h", "contents");

        assert!(memory.exists(Path::new("./inc/a.h")));
        assert!(memory.exists(Path::new("inc/b/../a.h")));
        assert_eq!(
            memory.read(Path::new("inc/a.h")).as_deref(),
            Some("contents")
        );
    }

    #[test]
    fn an_overlay_finds_a_buffer_before_the_disk() {
        let memory = MemoryFiles::new().with_file("a.h", "from memory");
        let disk = MemoryFiles::new().with_file("a.h", "from disk");
        let overlay = OverlayFiles::new(memory, disk);

        assert_eq!(
            overlay.read(Path::new("a.h")).as_deref(),
            Some("from memory")
        );
    }

    #[test]
    fn an_overlay_falls_through_to_the_disk() {
        let memory = MemoryFiles::new().with_file("a.h", "from memory");
        let disk = MemoryFiles::new().with_file("b.h", "from disk");
        let overlay = OverlayFiles::new(memory, disk);

        assert_eq!(overlay.read(Path::new("b.h")).as_deref(), Some("from disk"));
    }
}

