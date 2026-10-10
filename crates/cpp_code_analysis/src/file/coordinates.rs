//! **The two coordinate systems, as types.**
//!
//! A file is a sequence of bytes. A **view** of it may be of a **rendering** — the file with its directives resolved
//! and its macros expanded, which is what a compiler's parser is handed — and then the same text has two numberings:
//! the **file's**, which is the only one a client speaks, and the **reading's**, which is what every query in this
//! crate reads.
//!
//! They coincide exactly when nothing was replaced, and they differ **silently**:
//!
//! ```text
//!   a file offset used against the reading's tree     a panic inside rowan —
//!                                                     `Bad offset: range 0..106 offset 126`
//!   a reading offset converted with the file's lines  a three-line file described by two-line coordinates
//!   `view.source` where the written text was meant    `textDocument/foldingRange` answering `[]`
//!   a comment looked for in the reading               no documentation, ever
//! ```
//!
//! Not one of those failed loudly. Each produced an answer that *looked* like an answer, because nothing at the
//! boundary knew it had been handed the wrong ruler. A `usize` cannot say which ruler it is, and this crate has nine
//! queries and twenty-odd handlers that all pass one of them around. Two newtypes say it, and the compiler then
//! refuses the mixtures above.
//!
//! # How to cross
//!
//! Only through a [`FileView`](crate::FileView), in the two directions it offers:
//! [`reading_offset_of`](crate::FileView::reading_offset_of) for what a client sent, and
//! [`file_offset_of`](crate::FileView::file_offset_of) for what a client is shown. A view that is not of a rendering
//! crosses both as the identity, which is why a caller never has to ask whether there is one.
//!
//! There is deliberately **no `From<usize>`**: a conversion that happens by accident is the defect this module exists
//! to prevent, so every crossing is a call that says which way it goes. `new` and `get` are for the edges — a byte
//! offset that arrived from outside, a number handed to a lexer or a protocol — and using them in the middle of a
//! query is a decision that has to be readable in the diff.

use std::fmt;

/// **A byte offset in the file** — the numbering a client's positions resolve to, and the one an editor's text has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct FileOffset(usize);

/// **A byte offset in the reading** — the numbering of the text a compiler is handed, and the one every query in this
/// crate reads: `&view.root` is the rendering's tree, and so are the scope tree and every fact derived from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ReadingOffset(usize);

impl FileOffset {
    /// A byte offset in a file's own text, from an edge that has nothing better.
    pub const fn new(at: usize) -> Self {
        FileOffset(at)
    }

    /// The number, for an edge that speaks plain offsets: a lexer, a parser, a protocol.
    pub const fn get(self) -> usize {
        self.0
    }
}

impl ReadingOffset {
    /// A byte offset in a reading, from an edge that has nothing better.
    pub const fn new(at: usize) -> Self {
        ReadingOffset(at)
    }

    /// The number, for an edge that speaks plain offsets.
    pub const fn get(self) -> usize {
        self.0
    }
}

impl fmt::Display for FileOffset {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}", self.0)
    }
}

impl fmt::Display for ReadingOffset {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}", self.0)
    }
}
