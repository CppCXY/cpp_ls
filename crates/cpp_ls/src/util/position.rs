//! # Positions: the protocol's units against the analysis's
//!
//! LSP counts a line's column in **UTF-16 code units**. Everything below this crate counts **characters**
//! (`FileView::offset_at` explains why the conversion is deliberately not its job) and the parser counts **bytes**.
//! Three unit systems, two mappings, and both of them here:
//!
//! ```text
//! LSP position (line, UTF-16 column)  ──offset_at_position──▶  byte offset
//!                                     ◀──position_at_offset──
//! ```
//!
//! The two column units differ only on a line holding a character outside the basic multilingual plane — an emoji,
//! a rare CJK ideograph, most mathematical symbols — where one character is two code units. That is rare enough
//! that every test written in ASCII misses it and common enough to matter (a comment, a string literal, a
//! non-Latin identifier), which is why it is a module with tests rather than a subtraction at each call site.
//!
//! # What happens at the edges
//!
//! A column past the end of its line **clamps to the line's end**, which is what the protocol says to do with an
//! over-long character value, and a column that lands inside a surrogate pair rounds down to that character's
//! start — a client should never send one, and rounding down keeps the answer inside the line rather than
//! inventing a position between two halves of one character.

use cpp_code_analysis::{FileView, VfsFile};
use cpp_parser::LineIndex;
use lsp_types::Position;

/// A byte offset for a client's position — the mapping every request that takes a cursor starts with.
///
/// `None` when the line does not exist or the offset cannot be mapped (`FileView::offset_at`'s own answer for a
/// position that is not in the file); the caller answers `null`/`Missing` in that case, because a request about a
/// position the file does not have has no answer rather than a wrong one.
///
/// # Two coordinate systems, and this is the seam
///
/// A client's line and column are in the **file** it is showing. A view may be of a **rendering** — the file with
/// its macros replaced, which is what a compiler's parser is handed — and then every offset inside it is the
/// rendering's, because a macro that expands to twenty tokens moves everything after it. The line and the column
/// are therefore resolved against the file's own text and index first, and only then translated into the reading,
/// which is the whole of what makes a view of a rendering usable by an editor.
pub fn offset_at_position(view: &FileView, position: Position) -> Option<usize> {
    let line = position.line as usize;
    // **The text the client is looking at**, which is the file's — not `view.source`, which is the rendering when
    // there is one. Reaching for `source` here is the mistake this module exists to prevent: the two have different
    // line breaks wherever a macro expanded, so a line number from one is a different document's line number.
    let written = view.written_text().unwrap_or(&view.source);
    let body = line_body(written, line)?;
    let column = character_column(body, position.character as usize);
    let in_the_file = view.file_offset_at(line, column)?;
    // …and where the reading is. `None` for a region the rendering does not contain — a branch nobody takes, a
    // comment — which is "no answer here" rather than offset zero.
    view.reading_offset_of(in_the_file)
}

/// **The view and the offset for a client's position** — the pair, resolved together because they must agree.
///
/// # Why this exists rather than eight callers doing it
///
/// Eight handlers held the same two lines — `session.view(&path)?` then `offset_at_position(&view, position)?` —
/// and the second `?` is a trap. A rendering does not contain every byte of the file: the `#include` lines, the
/// comments, and whatever a conditional excluded are all gone from it, so a cursor in one of them has **nowhere to
/// map to** and `offset_at_position` answers `None`. Every one of those eight handlers then returned `None` to the
/// client, which is not a worse answer but **no answer at all** — and the cursor lands in such a region constantly,
/// because the end of a line is a newline and the newline above a function is often inside what the directives took
/// out. The server's own log recorded it: `completion at main.cpp:425 (asked line 23 character 5, on '\n')`.
///
/// `Session::view_of_the_file` is not a fallback in the sense of "less accurate": it is the reading whose coordinates
/// are the ones the client is sending, so where the rendering cannot answer for a position it is the *right* reading
/// rather than a worse one. Both halves are returned together because an offset means nothing without the view it is
/// an offset into.
pub fn view_and_offset_at(
    session: &cpp_code_analysis::Session<cpp_code_analysis::DiskFiles>,
    path: &std::path::Path,
    position: Position,
) -> Option<(cpp_code_analysis::FileView, usize)> {
    let rendering = session.view(path)?;
    if let Some(offset) = offset_at_position(&rendering, position) {
        return Some((rendering, offset));
    }

    // The rendering has no place for this position — see the note above. The file's own tokens do, and the offset
    // that comes back is in their coordinates, which is what the caller's query will be run against.
    let written = session.view_of_the_file(path)?;
    let offset = offset_at_position(&written, position)?;
    Some((written, offset))
}

/// **A client's position for a byte offset in a view's reading** — the other half of the seam
/// [`offset_at_position`] documents.
///
/// The offset a query hands back is in the view's own coordinates, and when the view is of a rendering those are the
/// rendering's. A client is showing the file, so the offset goes back through [`FileView::file_offset_of`] first —
/// and a token a macro produced reports **where the macro was invoked** rather than where its body is written, which
/// is the place the reader can actually see and the only one their buffer contains.
pub fn position_at_offset(view: &FileView, offset: usize) -> Option<Position> {
    let in_the_file = view.file_offset_of(offset)?;
    position_in(
        view.written_text().unwrap_or(&view.source),
        view.written_lines(),
        in_the_file,
    )
}

/// [`position_at_offset`] for a file that has no [`FileView`] behind it — one nobody is editing, which the caller
/// has from the VFS without parsing it.
///
/// This is also the one a [`FileView`]'s own offsets go through: a view *is* a file the VFS is holding plus the parse
/// of it, so `position_in_file(&file, offset)` and a `position_at_offset(&view, offset)` would be the same call — the
/// view-shaped entry point existed and was deleted when the diagnostics channel stopped parsing to answer.
pub fn position_in(text: &str, index: &LineIndex, offset: usize) -> Option<Position> {
    let (line, column) = index.position_of(offset, text)?;
    let body = line_body(text, line)?;
    Some(Position::new(line as u32, utf16_column(body, column)))
}

/// The same, for a file the VFS is holding: its text and its index, which are the two halves a position needs.
pub fn position_in_file(file: &VfsFile, offset: usize) -> Option<Position> {
    position_in(&file.text, &file.line_index, offset)
}

/// One line's text without its terminator: what a column is measured against.
///
/// `None` past the last line. A text that ends in a newline has one more line than it has line bodies, and that
/// last line is empty — the same line the parser's own line map counts (`LineIndex::parse` pushes an offset after
/// every `\n`), so a position at the very end of a file maps onto a real line rather than onto nothing.
pub fn line_body(source: &str, line: usize) -> Option<&str> {
    let mut count = 0;

    for text in source.split_inclusive('\n') {
        if count == line {
            return Some(without_terminator(text));
        }
        count += 1;
    }

    (count == line).then_some("")
}

/// The character column a UTF-16 column is, for one line body.
///
/// A column that lands *inside* a surrogate pair rounds down to that character's start: a client should never send
/// one (the protocol counts whole code units), and the character's start is at least a position in the text, where
/// the middle of a pair is not.
pub fn character_column(body: &str, utf16_column: usize) -> usize {
    let mut units = 0;

    for (characters, character) in body.chars().enumerate() {
        if units >= utf16_column {
            return characters;
        }

        let next = units + character.len_utf16();
        if next > utf16_column {
            return characters;
        }
        units = next;
    }

    body.chars().count()
}

/// The UTF-16 column a character column is, for one line body.
pub fn utf16_column(body: &str, character_column: usize) -> u32 {
    body.chars()
        .take(character_column)
        .map(char::len_utf16)
        .sum::<usize>() as u32
}

fn without_terminator(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_body_is_the_line_without_its_break() {
        let source = "one\nsecond\r\n";
        assert_eq!(line_body(source, 0), Some("one"));
        assert_eq!(line_body(source, 1), Some("second"));
        assert_eq!(
            line_body(source, 2),
            Some(""),
            "the position after the last newline is a line of its own"
        );
        assert_eq!(line_body(source, 3), None);
        assert_eq!(line_body("", 0), Some(""));
        assert_eq!(line_body("", 1), None);
    }

    #[test]
    fn a_column_counts_utf16_units_and_not_characters() {
        // `é` is one code unit and `😀` is two, so the same character column is a different UTF-16 column.
        let body = "a😀b";
        assert_eq!(utf16_column(body, 0), 0);
        assert_eq!(utf16_column(body, 1), 1);
        assert_eq!(utf16_column(body, 2), 3, "the emoji cost two units");
        assert_eq!(utf16_column(body, 3), 4);

        assert_eq!(character_column(body, 0), 0);
        assert_eq!(character_column(body, 1), 1);
        assert_eq!(character_column(body, 3), 2);
        assert_eq!(
            character_column(body, 2),
            1,
            "a column inside the surrogate pair rounds down to the character's start"
        );
        assert_eq!(
            character_column(body, 99),
            3,
            "a column past the end clamps to the line's end"
        );
    }

    /// The round trip over a real parse, on a line whose columns are not its bytes: the position a client sends
    /// and the offset a query takes have to name the same character.
    #[test]
    fn a_client_position_and_a_byte_offset_agree_through_a_view() {
        use cpp_code_analysis::{
            CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
        };

        let files = MemoryFiles::new().with_file("/p/a.cpp", "int x = 1;\n// 😀 emoji\nint y = 2;\n");
        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents, files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.load("/p/a.cpp");
        let view = session.view("/p/a.cpp").expect("the file reads");

        let at = view.source.find("y = 2").expect("the statement is in the text");
        let position =
            position_in(&view.source, &view.line_index, at).expect("the offset maps");

        assert_eq!(
            position,
            Position::new(2, 4),
            "the emoji is on the line above and shifts nothing"
        );
        assert_eq!(offset_at_position(&view, position), Some(at));

        // A position on the emoji's own line: column 4 is the character *after* two UTF-16 units of it.
        let emoji = view.source.find('😀').expect("the emoji is in the text");
        assert_eq!(
            position_in(&view.source, &view.line_index, emoji),
            Some(Position::new(1, 3))
        );
        assert_eq!(offset_at_position(&view, Position::new(1, 5)), Some(emoji + 4));
    }
}
