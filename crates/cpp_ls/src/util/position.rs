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

use cpp_code_analysis::FileView;
use cpp_parser::LineIndex;
use lsp_types::Position;

/// A byte offset for a client's position — the mapping every request that takes a cursor starts with.
///
/// `None` when the line does not exist or the offset cannot be mapped (`FileView::offset_at`'s own answer for a
/// position that is not in the file); the caller answers `null`/`Missing` in that case, because a request about a
/// position the file does not have has no answer rather than a wrong one.
pub fn offset_at_position(view: &FileView, position: Position) -> Option<usize> {
    let line = position.line as usize;
    let body = line_body(&view.source, line)?;
    let column = character_column(body, position.character as usize);
    view.offset_at(line, column)
}

/// A client's position for a byte offset, using a line index the caller built once.
///
/// The index is the caller's because a caller mapping a *list* of offsets — every diagnostic in a file, a
/// declaration's two ends — should build it once ([`LineIndex::parse`]); a single-position caller builds one and
/// passes it.
pub fn position_at_offset(view: &FileView, index: &LineIndex, offset: usize) -> Option<Position> {
    position_in(&view.source, index, offset)
}

/// [`position_at_offset`] for text that has no [`FileView`] behind it.
///
/// A file the analysis reads by path — the file a declaration lives in, which nobody is editing and which therefore
/// does not need a parse — is the ordinary case: `Session::text` gives the text, a line index gives the lines, and
/// this gives the position.
pub fn position_in(text: &str, index: &LineIndex, offset: usize) -> Option<Position> {
    let (line, column) = index.position_of(offset, text)?;
    let body = line_body(text, line)?;
    Some(Position::new(line as u32, utf16_column(body, column)))
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
        let session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        let view = session.view("/p/a.cpp").expect("the file reads");

        let at = view.source.find("y = 2").expect("the statement is in the text");
        let index = LineIndex::parse(&view.source);
        let position = position_at_offset(&view, &index, at).expect("the offset maps");

        assert_eq!(
            position,
            Position::new(2, 4),
            "the emoji is on the line above and shifts nothing"
        );
        assert_eq!(offset_at_position(&view, position), Some(at));

        // A position on the emoji's own line: column 4 is the character *after* two UTF-16 units of it.
        let emoji = view.source.find('😀').expect("the emoji is in the text");
        assert_eq!(position_at_offset(&view, &index, emoji), Some(Position::new(1, 3)));
        assert_eq!(offset_at_position(&view, Position::new(1, 5)), Some(emoji + 4));
    }
}
