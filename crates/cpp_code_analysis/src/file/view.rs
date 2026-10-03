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

use cpp_parser::{CppDocComment, CppParseError, CppSyntaxNode, CppSyntaxTree, LineIndex};

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
    ///
    /// **This is the *reading*, and the two are not always the same text.** For a view of a file's own tokens they
    /// are the file, and every offset in this struct is a file offset. For a view of a **rendering** — what a
    /// preprocessor produced, which is what a compiler's parser is handed — they are the rendering, and the offsets
    /// are the rendering's; [`FileView::file_offset_of`] and [`FileView::reading_offset_of`] are the way between the
    /// two, and [`FileView::written`] is the text on the other side.
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
    /// **The file's own text, when [`FileView::source`] is a rendering of it.**
    ///
    /// `None` — the ordinary case, and every view until a rendering one existed — means the two are the same text
    /// and the mapping below is the identity.
    written: Option<Arc<str>>,
    /// **One entry per token of the stream the rendering was made from**, in the same order, each saying where the
    /// spelling is in the rendering and where to act on it in the file.
    ///
    /// Empty exactly when [`FileView::written`] is `None`. It is an `Arc<[_]>` rather than a `Vec` because a view is
    /// cloned per request — every handler takes one — and a header's rendering is hundreds of thousands of tokens.
    reading: Arc<[crate::preprocess::cooked::RenderedSpan]>,
}

impl FileView {
    /// **Where in the file's own text the token at `in_the_reading` was written.**
    ///
    /// The question a client's position has to be answered in: a rendering's offsets are its own, and an editor
    /// speaks the file's. `None` — no rendering, or an offset past the end — means the identity, because a view of
    /// a file's own tokens has one coordinate system and it is the file's.
    ///
    /// The span's [`reported`](crate::preprocess::cooked::RenderedSpan::reported) rather than its
    /// [`written`](crate::preprocess::cooked::RenderedSpan::written), and the choice is the whole of what a reader
    /// means by "here": a token a macro produced was *written* in the header that defines the macro and *acts*
    /// where the macro was invoked, and a client shown the header would be shown a place its own buffer does not
    /// contain.
    pub fn file_offset_of(&self, in_the_reading: usize) -> Option<usize> {
        if self.written.is_none() {
            return Some(in_the_reading);
        }
        Some(self.span_at(in_the_reading)?.reported.start_offset)
    }

    /// **Where in the rendering the file's own offset `in_the_file` is acted on.**
    ///
    /// The other direction, and the one a client's position needs: an editor sends a line and a column in the file,
    /// and every layer below this struct speaks the reading. `None` when the offset is in a region the rendering
    /// does not contain — a `#if` branch nobody takes, a comment — which a caller must read as "no answer here"
    /// rather than as an offset of zero.
    pub fn reading_offset_of(&self, in_the_file: usize) -> Option<usize> {
        if self.written.is_none() {
            return Some(in_the_file);
        }
        // The first span that **acts** at or after the offset asked about. A file offset a macro's expansion
        // stands for has several spans reporting it — every token of the replacement list — and the first is the
        // one the cursor is nearest, which is what an editor means by pointing there.
        self.reading
            .iter()
            .find(|span| span.reported.end_offset() > in_the_file)
            .map(|span| span.cooked.start_offset)
    }

    /// The span whose spelling covers `in_the_reading`, by binary search.
    ///
    /// The spans are in stream order, so their `cooked` ranges are sorted and do not overlap — which is what makes
    /// this a search rather than a scan. A rendering of a standard-library header is a few hundred thousand tokens
    /// and a query about a position is answered per keystroke, so a scan would be the wrong shape.
    fn span_at(&self, in_the_reading: usize) -> Option<&crate::preprocess::cooked::RenderedSpan> {
        let at = self
            .reading
            .partition_point(|span| span.cooked.end_offset() <= in_the_reading);
        self.reading.get(at)
    }

    /// **The file's own text**, when this view is of a rendering of it.
    ///
    /// `None` for a view of the file's own tokens, where [`FileView::source`] is already that text. A consumer needs
    /// it for the one thing the rendering cannot answer: showing the reader the lines **they wrote**, with the macro
    /// still in them, rather than the expansion the compiler read.
    pub fn written_text(&self) -> Option<&str> {
        self.written.as_deref()
    }
}

/// **The documentation of what is declared at `offset`** — the comment a hover shows above a declaration.
///
/// The token's ancestors are asked, innermost first, because a cursor can be anywhere on a declaration: on the
/// name, on the type, on a `*`, on the `;`. Asking every ancestor is what makes the answer the same for all of
/// them, and it cannot answer with the *wrong* node: a node inside a declarator has no documentation of its own —
/// a comment is a sibling of the declaration, see [`cpp_parser::documentation_of`] — and that accessor verifies
/// every candidate against the forward rule, so only the node the comment actually documents matches.
///
/// The comment is returned as the parser's own node rather than as text: delimiters, line markers and `@param` are
/// the documentation layer's reading, and a rendering here would be a second implementation of it.
pub fn documentation_at(root: &CppSyntaxNode, offset: usize) -> Option<CppDocComment> {
    let ancestors = match cpp_parser::token_at(root, offset) {
        Some(token) => token.parent_ancestors().collect::<Vec<_>>(),
        // No token at that offset (past the end of the file): the node walk still answers, and the root always has
        // a range.
        None => root.ancestors().collect(),
    };

    ancestors
        .into_iter()
        .find_map(|node| cpp_parser::documentation_of(&node))
}

/// Could a comment be attached to the construct at `offset` in `text`?
///
/// A **filter, not a reading.** It exists so that a question about a declaration in a file this session has not
/// parsed does not parse a whole header to find out that the declaration has no documentation — and it is only
/// ever allowed to be *too permissive*: `false` is final, `true` means the parse decides.
///
/// Which is why it is loose on purpose. It walks up over the lines a comment can be separated from a declaration
/// by — blank lines and preprocessor directives, both of which the parser's own rule steps over — and accepts a
/// line that starts a line comment or that holds the end of a block comment. A `////` banner, a string literal
/// containing `*/`, a comment that turns out to document something else: each costs one parse, and the other
/// mistake is not symmetric — a `false` where a comment is would lose the documentation silently.
pub fn might_be_documented(text: &str, offset: usize) -> bool {
    let before = text
        .get(..offset.min(text.len()))
        .unwrap_or_default()
        .trim_end();

    // A block comment on the same line as the declaration: `/** Doc. */ int x;`.
    if before.ends_with("*/") {
        return true;
    }

    // Otherwise the comment is above, with only blank lines and directives in between.
    let mut rest = before;
    loop {
        let line = match rest.rfind('\n') {
            Some(newline) => &rest[newline + 1..],
            None => rest,
        };
        let line = line.trim();

        if !line.is_empty() && !line.starts_with('#') {
            return line.starts_with("//") || line.contains("*/");
        }

        // Only blank and directive lines so far: the previous line, or no comment at all.
        match rest.rfind('\n') {
            Some(newline) => rest = &rest[..newline],
            None => return false,
        }
    }
}

impl FileView {
    /// Parse a file the VFS is holding, and build its scopes.
    ///
    /// The one constructor, and it takes a [`VfsFile`] rather than text so that a view cannot be made from text
    /// that has no index — the two arrive together or not at all.
    ///
    /// # The scopes are built with **no macro evidence**, and that is an answer rather than a gap
    ///
    /// A view is a buffer and its index entry; `_STD_BEGIN`'s replacement list is in a header, and nothing here has
    /// read the include graph. So this file's scopes claim exactly what this file's tokens say — which is the same
    /// reading [`crate::build_scopes`] produced before the evidence parameter existed. Declarations reached
    /// *through* a query come from their own files' summaries, where the reading was made with the closure in hand,
    /// so `std::vector` still resolves from a buffer that never mentions `std`.
    pub fn parse(file: &VfsFile) -> FileView {
        let source = file.text.clone();
        let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());
        let root = tree.get_red_root();
        let scopes =
            crate::sema::scopes::build_scopes(&root, &crate::sema::scopes::NoMacroBodies);

        FileView {
            file: file.id,
            path: file.path.clone(),
            source,
            line_index: file.line_index.clone(),
            tree,
            root,
            scopes,
            open: file.open,
            written: None,
            reading: Arc::from(Vec::new()),
        }
    }

    /// **The same reading, with the macros the file's includes define.**
    ///
    /// The note above says a view has no macro evidence because "nothing here has read the include graph" — and
    /// that is true of a view made from a [`VfsFile`] alone. A view made **by a session** is a different
    /// situation: the closure is in its index, so the bodies are one lookup away, and handing them over is what
    /// turns
    ///
    /// ```cpp
    /// _STD addressof(*p)          // to a reader of this file: a name, then a call
    /// _MY_BEGIN struct thing {};  // to a reader of this file: two names and a brace
    /// ```
    ///
    /// into the qualified name and the namespace a compiler sees. `_STD` is `::std::` (`yvals_core.h:1906`) and
    /// `_STD_BEGIN` is `namespace std {`, so the difference is not cosmetic: the first makes a name resolve and the
    /// second is the only thing that says **which scope** a declaration belongs to.
    ///
    /// # Both readers get the same value
    ///
    /// The parse reads it through `ParserConfig::with_macros_from_includes` and the scope walk through
    /// [`MacroBodies`](crate::MacroBodies) — the two consumers [`crate::index::FileIndexer`]'s note describes,
    /// given one environment so they cannot disagree. That is the same arrangement the indexer uses; this is the
    /// constructor for callers that have an environment but not a `FileIndexer`.
    /// # The scope walk is what is wired, and the parse is not
    ///
    /// The two readers a body reader can serve are the **parse** and the **scope walk**, and
    /// [`crate::index::FileIndexer`]'s note describes giving one value to both. This constructor gives it to both.
    ///
    /// # Why the offsets are right here and were not before
    ///
    /// [`crate::index::FileIndexer`] carries a warning against passing an environment to a parse — "the version
    /// that passed a positional environment here was answering a *stream* offset against a *file* timeline" — and
    /// the warning is about a **`MacroView`**, the timeline of a unit, whose events carry `at` in each event's own
    /// file's coordinates (see `crate::summary::MacroView::applies_here`). Those are two different rulers and
    /// comparing them is the trap.
    ///
    /// The environment this constructor takes is the other kind: `macros_from_the_closure_with_bodies` builds
    /// [`cpp_parser::IncludedMacro::from_offset`] as **the end of the `#include` that brought the definition in**,
    /// which is an offset in *this* file — the same ruler the parse is reading with. So "is `_STD` a macro here" is
    /// answerable, and answerable correctly.
    pub fn parse_with(file: &VfsFile, macros: &cpp_parser::MacroEnvironment) -> FileView {
        let source = file.text.clone();
        let config = cpp_parser::ParserConfig::default().with_macros_from_includes(macros);
        let tree = cpp_parser::CppParser::parse(&source, config);
        let root = tree.get_red_root();
        let scopes = crate::sema::scopes::build_scopes(&root, macros);

        FileView {
            file: file.id,
            path: file.path.clone(),
            source,
            line_index: file.line_index.clone(),
            tree,
            root,
            scopes,
            open: file.open,
            written: None,
            reading: Arc::from(Vec::new()),
        }
    }

    /// **A view of what the preprocessor produced**, rather than of the file's own tokens.
    ///
    /// This is the reading a compiler's parser is handed, and the reason it is worth having one of: a parser given
    /// a rendering never sees a macro invocation at all — `_STD` is `::std::`, `_STD_BEGIN` is `namespace std {` —
    /// so every rule in the grammar that exists to guess which identifier is a macro has nothing to do here. The
    /// scopes are built with [`crate::NoMacroBodies`] for exactly that reason: there is no body to consult, because
    /// the body has already been read.
    ///
    /// # The two coordinate systems
    ///
    /// The offsets in this view — in the tree, in the scopes, in the line index — are the **rendering's**, and the
    /// rendering is not the file: a macro that expands to twenty tokens makes everything after it sit twenty
    /// positions further along. [`FileView::file_offset_of`] and [`FileView::reading_offset_of`] are the way
    /// between the two, and [`FileView::written`] is the file's own text.
    ///
    /// A caller that ignores the difference gets an answer about the wrong place rather than no answer, which is
    /// the worse of the two failures — so the mapping is on the struct rather than a convention.
    pub fn parse_rendering(
        file: &VfsFile,
        rendered: &crate::preprocess::cooked::RenderedCooked,
    ) -> FileView {
        let source: Arc<str> = Arc::from(rendered.text.as_str());
        let line_index = Arc::new(LineIndex::parse(&source));
        let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());
        let root = tree.get_red_root();
        // **No macro bodies**, and the answer is the point rather than a simplification: the text being parsed has
        // no invocations in it, so a body reader would be consulted for names that were already replaced.
        let scopes = crate::sema::scopes::build_scopes(&root, &crate::sema::scopes::NoMacroBodies);

        FileView {
            file: file.id,
            path: file.path.clone(),
            source,
            line_index,
            tree,
            root,
            scopes,
            open: file.open,
            written: Some(file.text.clone()),
            reading: Arc::from(rendered.spans.as_slice()),
        }
    }

    /// The parse diagnostics, as the parser reported them.
    ///
    /// A tolerant parser reports rather than fails, so this is empty for most files and non-empty for a file being
    /// typed at — which is not the same thing as a file that does not compile.
    pub fn errors(&self) -> &[CppParseError] {
        self.tree.get_errors()
    }

    /// **The ranges to select at `offset`, innermost first** — the chain an editor walks when the user expands a
    /// selection.
    ///
    /// # The ladder
    ///
    /// ```text
    /// the token under the cursor   `size` in `w.size`          a word before anything larger, which is what every
    ///                                                          editor selects first and what a node walk would skip:
    ///                                                          a token is not a node
    /// then each node that contains it, from the innermost out   the member access, the expression, the statement,
    ///                                                          the body, the function, the class, …  the file
    /// ```
    ///
    /// # The two things that make it a *chain* rather than a dump of ranges
    ///
    /// * **Strictly growing**: a recovered tree has nodes whose range equals their parent's (a wrapper around the
    ///   same tokens), and expanding the selection to the same range twice is a keystroke that does nothing. Only a
    ///   range that **strictly contains** the one before it is added.
    /// * **Every range contains the cursor**: an ancestor's range always does, by construction — but a range that
    ///   does not is dropped anyway, because the client is being asked to *select* it, and selecting something else
    ///   than what the user is looking at is worse than not expanding.
    ///
    /// A cursor in whitespace starts the chain at the enclosing node rather than with the whitespace token: there is
    /// no word under the cursor to select, and the next useful thing is what surrounds it.
    pub fn selection_chain(&self, offset: usize) -> Vec<cpp_parser::SourceRange> {
        let mut chain: Vec<cpp_parser::SourceRange> = Vec::new();
        let add = |range: cpp_parser::SourceRange, chain: &mut Vec<cpp_parser::SourceRange>| {
            if range.length == 0 {
                return;
            }
            // Inside the cursor, and strictly inside the previous range — see the note above.
            if offset < range.start_offset || offset >= range.end_offset() {
                return;
            }
            // Strictly inside the previous range — see the note above. A range that merely *equals* the previous
            // one adds nothing: expanding the selection to the same text twice is a keystroke that does nothing.
            let already_covered = chain.last().is_some_and(|previous| {
                range.start_offset >= previous.start_offset
                    && range.end_offset() <= previous.end_offset()
            });
            if !already_covered {
                chain.push(range);
            }
        };

        let token = cpp_parser::token_at(&self.root, offset);

        let ancestors = match &token {
            Some(token) => {
                if !cpp_parser::is_trivia(token.kind().into()) {
                    add(cpp_parser::source_range(token.text_range()), &mut chain);
                }
                token
                    .parent_ancestors()
                    .map(|node| cpp_parser::source_range(node.text_range()))
                    .collect::<Vec<_>>()
            }
            // No token at that offset (past the end of the file): the node walk still answers, and the root is
            // always a range.
            None => self
                .root
                .ancestors()
                .map(|node| cpp_parser::source_range(node.text_range()))
                .collect(),
        };

        for range in ancestors {
            add(range, &mut chain);
        }

        chain
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
        let mut vfs = Vfs::new(MemoryFiles::new().with_file("/p/a.cpp", "int a;\nint b;\nint c;\n"));
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
        let mut vfs = Vfs::new(MemoryFiles::new().with_file("/p/a.cpp", "int a;\n"));
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

