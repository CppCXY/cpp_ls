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
//! set one without the other — [`Vfs::insert`] takes text and builds the index in the same call, and nothing else
//! can put an entry into the table.
//!
//! # A `FileId` is a stable dense index
//!
//! [`Vfs::table`] is a `Vec<Option<VfsFile>>` and `files[id.index()]` is an array access rather than a hash lookup.
//! The two properties that buys are the ones a consumer needs:
//!
//! * **an id means the same file for as long as the table lives** — nothing is ever renumbered, so a `FileId` a
//!   caller is holding cannot come to mean something else;
//! * **closing a file leaves a hole, not a shift** — [`Vfs::close`] writes `None` into the slot, so every other id
//!   keeps its meaning. The hole costs one `Option` per file the session has ever read, and it is what makes
//!   "closed" answerable: `None` is "not holding this any more", which is a different answer from an empty file.
//!
//! # Why there is no lock here
//!
//! Because there is already one where it belongs: a caller that shares a session between threads locks **the
//! session**, and a caller that does not — a batch tool, a test, the single-threaded consumer this crate is also
//! for — pays nothing. A `RwLock` inside the VFS would charge that second caller for a problem it does not have,
//! and it would suggest that two threads may change a session's file table while a query reads it, which is exactly
//! what must not happen: a file's text and its line index have to move together, and the reader of one must not see
//! the other's next generation.
//!
//! So the methods that change what is held take `&mut self` ([`Vfs::load`], [`Vfs::insert`], [`Vfs::close`]) and
//! the ones that only look take `&self` ([`Vfs::held`], [`Vfs::get`]). A `Session` in a single-threaded program is
//! then borrowed and mutated directly, and a session behind a language server's lock is mutated by whoever holds it
//! for writing — which is the notification path, not the query path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// The file table: a dense vector indexed by [`FileId`], and the path → id map.
#[derive(Debug, Default)]
struct Held {
    files: Vec<Option<VfsFile>>,
    by_path: HashMap<String, FileId>,
}

impl Held {
    fn id_of(&self, path: &Path, case_insensitive: bool) -> Option<FileId> {
        self.by_path
            .get(&normalize_path(path, case_insensitive))
            .copied()
    }

    /// The entry for a path, or `None` when it is not held (or was closed).
    fn get(&self, path: &Path, case_insensitive: bool) -> Option<&VfsFile> {
        self.get_ref(self.id_of(path, case_insensitive)?)
    }

    fn get_ref(&self, id: FileId) -> Option<&VfsFile> {
        self.files.get(id.index())?.as_ref()
    }

    /// The id a path has, minting one if this is the first time it is seen.
    ///
    /// Minting is `files.len()`, so ids are handed out once each and never reused — even after a close, whose slot
    /// stays in the vector as `None`.
    fn id_for(&mut self, path: &Path, case_insensitive: bool) -> FileId {
        match self.id_of(path, case_insensitive) {
            Some(id) => id,
            None => {
                let id = FileId::new(self.files.len() as u32);
                self.by_path
                    .insert(normalize_path(path, case_insensitive), id);
                self.files.push(None);
                id
            }
        }
    }

    /// Put a file's text in, replacing whatever was there — **and build its line index in the same breath**.
    fn insert(&mut self, path: &Path, text: &str, open: bool, case_insensitive: bool) {
        let id = self.id_for(path, case_insensitive);

        self.files[id.index()] = Some(VfsFile {
            id,
            path: path.to_path_buf(),
            text: Arc::from(text),
            line_index: Arc::new(LineIndex::parse(text)),
            open,
        });
    }

    /// Close a file: its slot becomes `None` and its id stays.
    fn close(&mut self, path: &Path, case_insensitive: bool) -> Option<VfsFile> {
        let id = self.id_of(path, case_insensitive)?;
        self.files.get_mut(id.index())?.take()
    }

    fn len(&self) -> usize {
        self.files.iter().filter(|slot| slot.is_some()).count()
    }
}

/// The files an analysis is holding, over a provider chain.
#[derive(Debug)]
pub struct Vfs<F: FileProvider = DiskFiles> {
    files: F,
    held: Held,
}

impl<F: FileProvider> Vfs<F> {
    pub fn new(files: F) -> Self {
        Vfs {
            files,
            held: Held::default(),
        }
    }

    /// The provider chain underneath, for a caller that wants to read without holding.
    pub fn provider(&self) -> &F {
        &self.files
    }

    /// Read a file in and hold it, answering with its id.
    ///
    /// `None` when there is neither a buffer nor a readable file — which is the honest answer for a path the editor
    /// has open but never saved and has now closed, and for one that never existed. A file that is **already held**
    /// is not read again: that is what holding it is for.
    pub fn load(&mut self, path: impl AsRef<Path>) -> Option<FileId> {
        let path = path.as_ref();
        let case_insensitive = self.files.is_case_insensitive();

        if let Some(held) = self.held.get(path, case_insensitive) {
            return Some(held.id);
        }

        let text = self.files.read(path)?;
        // The buffer is the text when there is one: the provider chain decides that, so this only records which
        // half answered.
        let open = self.files.is_open_buffer(path);
        self.held.insert(path, &text, open, case_insensitive);

        self.held.id_of(path, case_insensitive)
    }

    /// Read a file in if it is not held, and answer with it.
    ///
    /// The shape a *writer* path wants — a notification, an indexing step: it is about to work on the file, so it
    /// may as well be the one that holds it.
    pub fn file(&mut self, path: impl AsRef<Path>) -> Option<VfsFile> {
        let id = self.load(path)?;
        self.get(id)
    }

    /// The file at a path, **if it is already held** — no read, and no mutation.
    ///
    /// The shape a *query* wants, and it is `&self` on purpose: the file the client is asking about is held (the
    /// notification path put it there), and one the analysis read while indexing is held too. A path that is not
    /// held has not been read, and "not read yet" is a different answer from "empty" — the same distinction
    /// [`crate::Known::Unknown`] draws one layer up.
    pub fn held(&self, path: impl AsRef<Path>) -> Option<&VfsFile> {
        let case_insensitive = self.files.is_case_insensitive();
        self.held.get(path.as_ref(), case_insensitive)
    }

    /// The file with this id, if its slot is not a hole.
    pub fn get(&self, id: FileId) -> Option<VfsFile> {
        self.held.get_ref(id).cloned()
    }

    /// The file with this id, borrowed.
    pub fn get_ref(&self, id: FileId) -> Option<&VfsFile> {
        self.held.get_ref(id)
    }

    /// Record a file's text — a buffer's edit, or text a caller already has.
    ///
    /// The one way an entry is created or replaced from outside this module, and it takes text rather than an index
    /// for the reason the module documentation gives: the index is derived, and a caller that could supply one
    /// could supply the wrong one.
    pub fn insert(&mut self, path: impl AsRef<Path>, text: &str, open: bool) {
        let case_insensitive = self.files.is_case_insensitive();
        self.held
            .insert(path.as_ref(), text, open, case_insensitive);
    }

    /// Close a file: nothing is held for it until it is read again, and its id stays valid.
    pub fn close(&mut self, path: impl AsRef<Path>) {
        let case_insensitive = self.files.is_case_insensitive();
        self.held.close(path.as_ref(), case_insensitive);
    }

    /// Every file currently held, in id order.
    pub fn files(&self) -> impl Iterator<Item = &VfsFile> {
        self.held.files.iter().filter_map(Option::as_ref)
    }

    /// Every path currently held, in id order.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.files().map(|file| file.path.clone()).collect()
    }

    /// How many files are held — closed slots are not files.
    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many ids have been handed out, holes included.
    ///
    /// The length of the table rather than the number of files in it: an id is never reused, so this is the bound
    /// on an id a caller may still be holding.
    pub fn id_bound(&self) -> usize {
        self.held.files.len()
    }

    /// The table itself, for a caller that wants to walk ids.
    pub fn table(&self) -> &[Option<VfsFile>] {
        &self.held.files
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
        let mut vfs = vfs();

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
        let mut vfs = vfs();

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
        assert_eq!(changed.position_at(20), None, "and offset 20 is past the end");
    }

    #[test]
    fn an_id_is_stable_across_a_close_and_never_reused() {
        // The property the table's shape exists for: ids are a dense index that nothing renumbers. A closed file
        // leaves a hole, and a file read afterwards gets a **new** id rather than the hole's.
        let mut vfs = vfs();

        let first = vfs.file("/p/a.cpp").expect("the file reads");
        let second = vfs.file("/p/b.cpp").expect("the file reads");
        assert_eq!(first.id.index(), 0);
        assert_eq!(second.id.index(), 1);

        vfs.close("/p/a.cpp");
        assert!(
            vfs.held("/p/a.cpp").is_none(),
            "a closed file is not held, and that is answerable"
        );
        assert_eq!(vfs.len(), 1, "and it is not counted as a file any more");
        assert_eq!(vfs.id_bound(), 2, "but its id is still in the table");

        let reopened = vfs.file("/p/a.cpp").expect("it reads again");
        assert_eq!(
            reopened.id, first.id,
            "the same path keeps its id — the table has no reason to renumber it"
        );

        let third = vfs.file("/p/b.cpp").expect("still held");
        assert_eq!(third.id, second.id);
    }

    #[test]
    fn a_query_can_only_see_what_is_held() {
        // What the `&self` half of this API means: a query reads the table, it does not fill it. A path nobody has
        // read is not held, and saying so is the honest answer — the alternative is a `&self` method that mutates,
        // which is what the VFS deliberately does not have.
        let mut vfs = vfs();
        vfs.load("/p/a.cpp").expect("the file reads");

        assert!(vfs.held("/p/a.cpp").is_some());
        assert!(
            vfs.held("/p/b.cpp").is_none(),
            "nobody has read it, so nothing is known about it"
        );
        let held = vfs.held("/p/a.cpp").expect("held").id;
        assert_eq!(
            vfs.get(held).map(|file| file.path),
            Some(PathBuf::from("/p/a.cpp"))
        );
        assert!(
            vfs.get_ref(FileId::new(99)).is_none(),
            "an id the table never handed out is not a file"
        );
    }

    #[test]
    fn a_path_that_is_not_there_is_not_an_entry() {
        let mut vfs = vfs();

        assert!(vfs.file("/p/missing.cpp").is_none());
        assert!(vfs.is_empty(), "a failed read holds nothing");
    }

    #[test]
    fn two_spellings_of_one_path_are_one_file() {
        let mut vfs = Vfs::new(MemoryFiles::new().with_file("/p/sub/a.cpp", "int x;\n"));

        let one = vfs.file("/p/sub/a.cpp").expect("the file reads");
        let other = vfs.file("/p/sub/./a.cpp").expect("the same file");

        assert_eq!(one.id, other.id, "the id is the file's, not the spelling's");
        assert_eq!(vfs.len(), 1);
    }
}

