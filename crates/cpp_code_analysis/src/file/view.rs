//! One file, parsed — what a cursor query is answered against.
//!
//! A query about a *position* needs three things the index does not hold: the file's **text** (the buffer when the
//! user is typing in it), its **scopes** (which declaration a name at an offset means — a summary records facts
//! about declarations, not the syntax a position resolves against), and the **line index** that turns a client's
//! line and column into the byte offset every layer below speaks.
//!
//! ```text
//! FileView {
//!     file        which file in the VFS this is — its identity, not its spelling
//!     path        where it is, for the layers that still speak paths
//!     source      the text, shared: no copy per request
//!     line_index  the index *of that text*, shared, built when the VFS took the file in
//!     tree/root   the parse of it
//!     scopes      the file's scopes, built from the same tree
//!     open        did the text come from an editor buffer?
//! }
//! ```
//!
//! # Why it borrows nothing
//!
//! A view is the VFS's entry plus a parse, and the two shared halves are `Arc`s — so a view **outlives the call
//! that made it** without holding a lock or a borrow of the session. That is what a request needs: the handler
//! resolves a position, asks several questions about one file, and hands a location back to the client, all while
//! other requests read the same session.
//!
//! # Why the line index is not derived here
//!
//! It used to be: `offset_at` built a `LineIndex` per call, which is a **scan of the whole file** for every
//! position a client asked about — the wrong side of the boundary, because the file's lines change exactly when its
//! text does, and the VFS is what knows about that. See [`crate::file::vfs`]: the text and its index are made
//! together and replaced together, so a view's index can never be one edit behind its text.

use std::path::PathBuf;
use std::sync::Arc;

use cpp_parser::{CppParseError, CppSyntaxNode, CppSyntaxTree, LineIndex};

use crate::file::paths::FileId;
use crate::file::vfs::VfsFile;
use crate::symbol::ScopeTree;

/// A file in the VFS, parsed, with its scopes.
#[derive(Debug, Clone)]
pub struct FileView {
    /// The file's identity inside the VFS — stable across spellings of its path, and what the view is *of*.
    pub file: FileId,
    pub path: PathBuf,
    /// The text analysed: the buffer when open, the file otherwise. Shared with the VFS, so a view costs a pointer.
    pub source: Arc<str>,
    /// The line index **of [`FileView::source`]** — the same one the VFS built when it took this text in.
    pub line_index: Arc<LineIndex>,
    /// The parse of [`FileView::source`], kept because a caller may want the diagnostics or the token list.
    pub tree: CppSyntaxTree,
    /// The tree's root. Exactly `tree.get_red_root()`, kept because every query wants it and re-deriving a red root
    /// per query would allocate one per query.
    pub root: CppSyntaxNode,
    /// The file's scopes, built from the same tree.
    pub scopes: ScopeTree,
    /// Did the text come from an open buffer rather than from the file?
    pub open: bool,
}

impl FileView {
    /// Parse a file the VFS is holding, and build its scopes.
    ///
    /// The one constructor, and it takes a [`VfsFile`] rather than text so that a view cannot be made from text
    /// that has no index — the two arrive together or not at all.
    pub fn parse(file: &VfsFile) -> FileView {
        let source = file.text.clone();
        let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());
        let root = tree.get_red_root();
        let scopes = crate::sema::scopes::build_scopes(&root);

        FileView {
            file: file.id,
            path: file.path.clone(),
            source,
            line_index: file.line_index.clone(),
            tree,
            root,
            scopes,
            open: file.open,
        }
    }

    /// The parse diagnostics, as the parser reported them.
    ///
    /// A tolerant parser reports rather than fails, so this is empty for most files and non-empty for a file being
    /// typed at — which is not the same thing as a file that does not compile.
    pub fn errors(&self) -> &[CppParseError] {
        self.tree.get_errors()
    }

    /// A byte offset from a **line and column**, both counted from zero.
    ///
    /// The mapping a client's positions need, and the one place LSP's own rule is *not* implemented: the columns
    /// this counts are characters, while the protocol counts UTF-16 code units, and the two differ on a line with
    /// an emoji or any character outside the basic plane. That conversion is the protocol layer's — doing it here
    /// would put a rule about a wire format in the crate that has no wire format.
    ///
    /// A lookup in the index the view already holds: no scan, and no way for the answer to be about a different
    /// text than the one parsed.
    pub fn offset_at(&self, line: usize, column: usize) -> Option<usize> {
        self.line_index
            .get_offset(line, column, &self.source)
            .map(usize::from)
    }

    /// A byte offset back to a **line and column**, both counted from zero — the inverse of
    /// [`FileView::offset_at`], and the direction a diagnostic needs: the parser reports offsets and a client wants
    /// a position.
    ///
    /// A position past the end of the text answers `None` rather than clamping: an offset that is not in the file
    /// is a caller's mistake, and a range built from a clamped one would point at the wrong code.
    pub fn position_at(&self, offset: usize) -> Option<(usize, usize)> {
        self.line_index.position_of(offset, &self.source)
    }

    /// The text, for a caller that wants a `&str` rather than the shared handle.
    pub fn source(&self) -> &str {
        &self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::paths::MemoryFiles;
    use crate::file::vfs::Vfs;

    #[test]
    fn a_view_maps_positions_through_the_index_the_vfs_built() {
        let vfs = Vfs::new(MemoryFiles::new().with_file("/p/a.cpp", "int a;\nint b;\nint c;\n"));
        let file = vfs.file("/p/a.cpp").expect("the file reads");

        let view = FileView::parse(&file);

        assert_eq!(view.file, file.id, "the view is *of* a file in the VFS");
        assert!(
            Arc::ptr_eq(&view.line_index, &file.line_index),
            "and it shares that file's index rather than making one"
        );
        assert!(Arc::ptr_eq(&view.source, &file.text));

        assert_eq!(view.offset_at(2, 4), Some(18));
        assert_eq!(view.position_at(18), Some((2, 4)));
        assert_eq!(view.offset_at(9, 0), None);
        assert!(view.errors().is_empty());
    }

    #[test]
    fn a_view_of_an_edited_buffer_maps_positions_in_the_edited_text() {
        // The property a language server depends on: the text a view maps positions in is the text the user is
        // typing, and its line index is the *same* generation of that text.
        let vfs = Vfs::new(MemoryFiles::new().with_file("/p/a.cpp", "int a;\n"));
        let before = FileView::parse(&vfs.file("/p/a.cpp").expect("the file reads"));
        assert_eq!(
            before.offset_at(1, 0),
            Some(7),
            "a text that ends in a newline has an empty last line, at its end"
        );
        assert_eq!(before.offset_at(2, 0), None, "and no line after that");

        vfs.insert("/p/a.cpp", "int a;\nint b;\n", true);
        let after = FileView::parse(&vfs.file("/p/a.cpp").expect("the edit is held"));

        assert!(after.open, "and the view says the text is a buffer");
        assert_eq!(after.offset_at(1, 4), Some(11), "`b` of the new second line");
        assert_eq!(
            before.offset_at(1, 4),
            Some(7),
            "the older generation's line 1 was the empty line after the final newline, so column 4 clamps to its \
             start — the end of the text"
        );
    }
}
