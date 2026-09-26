//! The files an analysis is holding: their text, and the line index of that text.
//!
//! A cursor query arrives as a **line and column** and every layer below speaks **byte offsets**, so the mapping
//! between the two is on the path of every request. Building it is a scan of the whole file, which is why it
//! belongs to the file's *life* rather than to each query: a file's line index changes exactly when its text does,
//! and both are things this layer already knows about (`didOpen`, `didChange`, `didClose`, a watcher event).
//!
//! ```text
//! text arrives      ──▶ one entry: the text, and the line index built from it, together
//! a query arrives   ──▶ the index is already there: a binary search, not a scan
//! the text changes  ──▶ the entry is replaced as a whole, so the two can never disagree
//! ```
//!
//! # Why "together" is the whole design
//!
//! A line index that is one edit behind its text is worse than no index at all: every position after the edit maps
//! to the wrong offset, silently, and the queries that use it answer about the wrong code. So there is no way to
//! set one without the other — [`Vfs::insert`] takes text and builds the index in the same critical section, and
//! nothing else can put an entry into the map.
//!
//! # Why the provider is still underneath
//!
//! Because the text has to come from *somewhere*, and where it comes from is the provider chain's business: the
//! editor's buffers in front of the filesystem ([`OverlayFiles`](crate::OverlayFiles)). The VFS is the **cache and the identity** —
//! which file this is, what its text is right now, and where its lines are — and the provider remains the thing
//! that knows how to read.
//!
//! # Why the handles are shared
//!
//! [`Vfs::file`] hands back `Arc<str>` and `Arc<LineIndex>`, so a caller can hold a file's text across a query
//! without a borrow of the VFS and without copying it. A `FileView` is the caller that does exactly that
//! ([`crate::FileView`]), which is what makes a view cheap to hold and impossible to leave half-stale.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use cpp_parser::LineIndex;

use super::paths::{DiskFiles, FileId, FileProvider, normalize_path};

/// One file the VFS is holding.
#[derive(Debug, Clone)]
pub struct VfsFile {
    pub id: FileId,
    pub path: PathBuf,
    /// The text as it was when this entry was made.
    pub text: Arc<str>,
    /// The line index **of that text** — built once, when the entry was made, and never out of step with it.
    pub line_index: Arc<LineIndex>,
    /// Did the text come from an open buffer rather than from the file on disk?
    ///
    /// Kept because a consumer that shows a file's own diagnostics needs to know whether the analysis saw what the
    /// user sees, and because closing a buffer is what makes the next read come from the disk again.
    pub open: bool,
}

impl VfsFile {
    /// A byte offset from a **line and column**, both counted from zero.
    ///
    /// A binary search in the index this entry already holds — the reason the VFS exists. `None` for a line the
    /// file does not have, which is a caller's mistake rather than an answer.
    pub fn offset_at(&self, line: usize, column: usize) -> Option<usize> {
        self.line_index
            .get_offset(line, column, &self.text)
            .map(usize::from)
    }

    /// The line and column a byte offset is, both counted from zero.
    ///
    /// The inverse of [`VfsFile::offset_at`], and the direction a diagnostic needs: the parser reports offsets and
    /// a client wants a position. `None` for an offset past the end of the text.
    pub fn position_at(&self, offset: usize) -> Option<(usize, usize)> {
        self.line_index.position_of(offset, &self.text)
    }
}

/// The state inside the lock: which files are held, and by which path.
#[derive(Debug, Default)]
struct Held {
    files: HashMap<FileId, VfsFile>,
    by_path: HashMap<String, FileId>,
    next: u32,
}

impl Held {
    /// The entry for a path, or `None` when it is not held.
    fn get(&self, path: &Path, case_insensitive: bool) -> Option<&VfsFile> {
        let key = normalize_path(path, case_insensitive);
        self.by_path.get(&key).and_then(|id| self.files.get(id))
    }

    /// Put a file's text in, replacing whatever was there — **and build its line index in the same breath**.
    ///
    /// One entry point for "this file is now this text", which is what makes the invariant hold: there is no
    /// operation that changes the text without changing the index, and none that changes the index at all.
    fn insert(&mut self, path: &Path, text: &str, open: bool, case_insensitive: bool) {
        let key = normalize_path(path, case_insensitive);
        let index = Arc::new(LineIndex::parse(text));

        let id = match self.by_path.get(&key) {
            Some(id) => *id,
            None => {
                let id = FileId::new(self.next);
                self.next += 1;
                self.by_path.insert(key, id);
                id
            }
        };

        self.files.insert(
            id,
            VfsFile {
                id,
                path: path.to_path_buf(),
                text: Arc::from(text),
                line_index: index,
                open,
            },
        );
    }

    /// Forget a path: the next read comes from the provider again.
    fn forget(&mut self, path: &Path, case_insensitive: bool) -> Option<VfsFile> {
        let key = normalize_path(path, case_insensitive);
        let id = self.by_path.remove(&key)?;
        self.files.remove(&id)
    }
}

/// The files an analysis is holding, over a provider chain.
///
/// Reachable through `&self` because a query is a read and the cache is a memo: [`Vfs::file`] fills an entry in when
/// it is missing, which is what keeps a `Session::view` a `&self` method — the alternative is a session that must
/// be mutably borrowed to answer a hover.
#[derive(Debug)]
pub struct Vfs<F: FileProvider = DiskFiles> {
    files: F,
    held: RwLock<Held>,
}

impl<F: FileProvider> Vfs<F> {
    pub fn new(files: F) -> Self {
        Vfs {
            files,
            held: RwLock::new(Held::default()),
        }
    }

    /// The provider chain underneath, for a caller that wants to read without caching.
    pub fn provider(&self) -> &F {
        &self.files
    }

    /// The file at a path, reading it if it is not held.
    ///
    /// `None` when there is neither a buffer nor a readable file — which is the honest answer for a path the editor
    /// has open but never saved and has now closed, and for one that never existed.
    pub fn file(&self, path: impl AsRef<Path>) -> Option<VfsFile> {
        let path = path.as_ref();
        let case_insensitive = self.files.is_case_insensitive();

        if let Some(held) = self
            .held
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path, case_insensitive)
        {
            return Some(held.clone());
        }

        let text = self.files.read(path)?;
        // The buffer is the text when there is one: the provider chain is what decides that, so this only has to
        // record which half answered.
        let open = self.overlay_has(path);
        self.insert(path, &text, open);

        self.held
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path, case_insensitive)
            .cloned()
    }

    /// The file at a path **if it is already held**, without reading anything.
    ///
    /// For a caller that wants to know whether the analysis is holding this file, not what its text is.
    pub fn held(&self, path: impl AsRef<Path>) -> Option<VfsFile> {
        let case_insensitive = self.files.is_case_insensitive();
        self.held
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path.as_ref(), case_insensitive)
            .cloned()
    }

    /// The file with this id, if it is held.
    pub fn by_id(&self, id: FileId) -> Option<VfsFile> {
        self.held
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .files
            .get(&id)
            .cloned()
    }

    /// Record a file's text — a buffer's edit, or text a caller already has.
    ///
    /// The one way an entry is created or replaced from outside this module, and it takes text rather than an
    /// index for the reason the module documentation gives: the index is derived, and a caller that could supply
    /// one could supply the wrong one.
    pub fn insert(&self, path: impl AsRef<Path>, text: &str, open: bool) {
        let case_insensitive = self.files.is_case_insensitive();
        self.held
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(path.as_ref(), text, open, case_insensitive);
    }

    /// Forget a file: the next read comes from the provider.
    ///
    /// What a change on disk and a closed buffer both call, because both mean the same thing to this layer — the
    /// text that is held is not the text any more, and the next question should re-read rather than be answered
    /// from a memory of it.
    pub fn forget(&self, path: impl AsRef<Path>) {
        let case_insensitive = self.files.is_case_insensitive();
        self.held
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .forget(path.as_ref(), case_insensitive);
    }

    /// Every path currently held, in an unspecified order.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.held
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .files
            .values()
            .map(|file| file.path.clone())
            .collect()
    }

    /// How many files are held.
    pub fn len(&self) -> usize {
        self.held
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .files
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Does the overlay — the editor's buffers — have this path?
    ///
    /// Asked of the provider chain by *reading* it: the chain is `OverlayFiles` for every caller that has an
    /// editor, and the only way to see through it without knowing its concrete type is to compare what it answers
    /// with what its fallback answers. The trait has no "is this a buffer" question because a provider that is not
    /// an overlay has no answer — a disk read *is* the answer there, so `false` is the honest reply.
    ///
    /// The cost is one extra read of a file that is already in memory (a buffer) or already being read (a file),
    /// and it happens once per entry rather than per query.
    fn overlay_has(&self, path: &Path) -> bool {
        self.files.is_open_buffer(path)
    }
}

/// A VFS is a provider, so that a consumer with a `Vfs` and one with a `DiskFiles` are interchangeable.
///
/// `read` answers from the entry when one is held and from the chain otherwise — **and caches what it read**, since
/// that is the whole point of a VFS: the layer above asks for the same file once per query, and a read that is not
/// remembered is a read that happens again for every request.
impl<F: FileProvider> FileProvider for Vfs<F> {
    fn read(&self, path: &Path) -> Option<String> {
        self.file(path).map(|file| file.text.to_string())
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.exists(path)
    }

    fn is_case_insensitive(&self) -> bool {
        self.files.is_case_insensitive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::paths::MemoryFiles;

    fn vfs() -> Vfs<MemoryFiles> {
        Vfs::new(
            MemoryFiles::new()
                .with_file("/p/a.cpp", "int a;\nint b;\nint c;\n")
                .with_file("/p/b.cpp", "void f() {}\n"),
        )
    }

    #[test]
    fn a_file_read_once_is_held_with_its_line_index() {
        let vfs = vfs();

        assert!(vfs.is_empty(), "nothing is held until something is read");

        let file = vfs.file("/p/a.cpp").expect("the file reads");
        assert_eq!(&*file.text, "int a;\nint b;\nint c;\n");
        assert_eq!(vfs.len(), 1);

        // The index is the text's: each `int x;` line is seven bytes, so line 2 starts at 14 and its column 4 is
        // the `c` of `int c;`.
        assert_eq!(file.offset_at(2, 4), Some(18));
        assert_eq!(file.position_at(18), Some((2, 4)));
        assert_eq!(file.offset_at(9, 0), None, "there is no line 9");
    }

    #[test]
    fn the_line_index_follows_the_text_and_can_never_be_one_edit_behind() {
        // The property the whole module exists for. A second read without a change is the same entry (an `Arc` the
        // caller can compare), and a change replaces **both** halves at once.
        let vfs = vfs();

        let first = vfs.file("/p/a.cpp").expect("the file reads");
        let again = vfs.file("/p/a.cpp").expect("the file is held");
        assert!(
            Arc::ptr_eq(&first.text, &again.text),
            "a file that did not change is not read twice"
        );

        // The text loses a line: every offset after it moves, and the index has to move with it.
        vfs.insert("/p/a.cpp", "int a;\nint c;\n", true);

        let changed = vfs.file("/p/a.cpp").expect("the edit is held");
        assert!(changed.open, "the text came from a buffer");
        assert_eq!(&*changed.text, "int a;\nint c;\n");
        assert_eq!(
            changed.offset_at(1, 4),
            Some(11),
            "the third line is the second line now"
        );
        assert_eq!(changed.position_at(20), None, "and offset 20 is past the end");    }

    #[test]
    fn forgetting_a_file_means_the_next_question_reads_again() {
        let vfs = vfs();

        let before = vfs.file("/p/a.cpp").expect("the file reads");
        assert!(!before.open);

        vfs.forget("/p/a.cpp");
        assert!(vfs.held("/p/a.cpp").is_none());

        // Same content, and it is read again: which is what "the text may have changed, ask the provider" means.
        let after = vfs.file("/p/a.cpp").expect("the file reads again");
        assert!(!Arc::ptr_eq(&before.line_index, &after.line_index));
    }

    #[test]
    fn a_path_that_is_not_there_is_not_an_entry() {
        let vfs = vfs();

        assert!(vfs.file("/p/missing.cpp").is_none());
        assert!(vfs.is_empty(), "a failed read holds nothing");
    }

    #[test]
    fn two_spellings_of_one_path_are_one_file() {
        let vfs = Vfs::new(MemoryFiles::new().with_file("/p/sub/a.cpp", "int x;\n"));

        let one = vfs.file("/p/sub/a.cpp").expect("the file reads");
        let other = vfs.file("/p/sub/./a.cpp").expect("the same file");

        assert_eq!(one.id, other.id, "the id is the file's, not the spelling's");
        assert_eq!(vfs.len(), 1);
    }

    #[test]
    fn reading_through_the_provider_interface_also_holds_the_file() {
        // Because a `Vfs` *is* a provider: the store, the resolver and the session all read through it, and each of
        // those reads is a file the analysis now holds — with its index.
        let vfs = vfs();

        let text = FileProvider::read(&vfs, Path::new("/p/b.cpp")).expect("the file reads");
        assert_eq!(text, "void f() {}\n");

        let held = vfs.held("/p/b.cpp").expect("and it is held");
        assert_eq!(held.offset_at(0, 5), Some(5));
    }
}

