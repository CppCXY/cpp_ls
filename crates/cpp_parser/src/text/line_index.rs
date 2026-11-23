use rowan::TextSize;

#[derive(Debug, Clone)]
pub struct LineIndex {
    line_offsets: Vec<TextSize>,
    line_only_ascii_vec: Vec<bool>,
}

impl LineIndex {
    /// **The bytes this index itself holds** — one offset per line plus one byte per line.
    ///
    /// Asked by the analysis's memory report (`cpp_code_analysis::MemoryReport`): a VFS entry is text *and* an index,
    /// and a report that counted only the text would blame the wrong half on a project of many small files.
    pub fn heap_bytes(&self) -> usize {
        self.line_offsets.capacity() * std::mem::size_of::<TextSize>()
            + self.line_only_ascii_vec.capacity()
    }

    /// Build the index for `text`, treating **every spelling of a line break** as one.
    ///
    /// `\n`, `\r\n`, a lone `\r`, and the `\n\r` pair are all one break — the same four the lexer's `eat_newline`
    /// accepts. This used to count only `\n`, so a file with old-Mac `\r` endings was **one line** to the index and many
    /// to the lexer: every diagnostic in it pointed at line 0, every LSP range was a column of a line the client had
    /// never heard of, and `get_offset(line, col)` answered positions inside a different line's syntax
    /// (`docs/worklog.md` P2-3).
    pub fn parse(text: &str) -> LineIndex {
        let mut line_offsets = Vec::new();
        let mut line_only_ascii_vec = Vec::new();

        line_offsets.push(TextSize::from(0u32));

        let mut is_line_only_ascii = true;
        // The first half of a break whose second half may still come: `\r` before a `\n`, or the other way round. Two
        // different characters in a row are **one** break, not two — that is what keeps `\r\n` from producing an empty
        // line between its halves.
        let mut first_half: Option<char> = None;

        for (i, c) in text.char_indices() {
            if c == '\n' || c == '\r' {
                if first_half.is_some_and(|first| first != c) {
                    // The pair closes here, and the line starts after the second byte.
                    if let Some(last) = line_offsets.last_mut() {
                        *last = TextSize::from((i + 1) as u32);
                    }
                    first_half = None;
                } else {
                    line_offsets.push(TextSize::from((i + 1) as u32));
                    line_only_ascii_vec.push(is_line_only_ascii);
                    is_line_only_ascii = true;
                    first_half = Some(c);
                }
                continue;
            }

            first_half = None;
            if !c.is_ascii() {
                is_line_only_ascii = false;
            }
        }

        line_only_ascii_vec.push(is_line_only_ascii);

        assert_eq!(line_offsets.len(), line_only_ascii_vec.len());
        LineIndex {
            line_offsets,
            line_only_ascii_vec,
        }
    }

    pub fn get_line_offset(&self, line: usize) -> Option<TextSize> {
        let line_index = line;
        if line_index < self.line_offsets.len() {
            let line_offset = self.line_offsets[line_index];
            Some(line_offset)
        } else {
            None
        }
    }

    // get line base 0
    pub fn get_line(&self, offset: TextSize) -> Option<usize> {
        let offset_value = usize::from(offset);
        match self
            .line_offsets
            .binary_search(&TextSize::from(offset_value as u32))
        {
            Ok(line) => Some(line),
            Err(line) => {
                if line > 0 {
                    Some(line - 1)
                } else {
                    None
                }
            }
        }
    }

    pub fn get_line_with_start_offset(&self, offset: TextSize) -> Option<(usize, TextSize)> {
        let line = self.get_line(offset)?;
        let start_offset = self.line_offsets[line];
        Some((line, start_offset))
    }

    pub fn is_line_only_ascii(&self, line: TextSize) -> bool {
        let line_index = usize::from(line);
        if line_index < self.line_only_ascii_vec.len() {
            self.line_only_ascii_vec[line_index]
        } else {
            false
        }
    }

    pub fn line_count(&self) -> usize {
        self.line_offsets.len()
    }

    // get col base 0
    pub fn get_col(&self, offset: TextSize, source_text: &str) -> Option<usize> {
        let (line, start_offset) = self.get_line_with_start_offset(offset)?;
        if self.is_line_only_ascii(line.try_into().unwrap()) {
            Some(usize::from(offset - start_offset))
        } else {
            // **An offset that lands inside a character has no column, and slicing there panics.** This reached a
            // language server as an internal error on `textDocument/diagnostic`: a range whose end was two bytes
            // into `译` (bytes 376..379) made `&source_text[376..378]` panic. The floor is the character the offset
            // is in, which is what a column meant at that byte.
            let end = floor_char_boundary(source_text, usize::from(offset));
            let text = &source_text[usize::from(start_offset)..end];
            Some(text.chars().count())
        }
    }

    // get line and col base 0
    pub fn get_line_col(&self, offset: TextSize, source_text: &str) -> Option<(usize, usize)> {
        let (line, start_offset) = self.get_line_with_start_offset(offset)?;
        if self.is_line_only_ascii(line.try_into().unwrap()) {
            Some((line, usize::from(offset - start_offset)))
        } else {
            // The same floor as [`LineIndex::get_col`], for the same crash.
            let end = floor_char_boundary(source_text, usize::from(offset));
            let text = &source_text[usize::from(start_offset)..end];
            Some((line, text.chars().count()))
        }
    }

    /// The line and column of a byte offset given as a plain `usize` — the same answer as
    /// [`LineIndex::get_line_col`], for a caller that never sees a [`TextSize`].
    ///
    /// Every layer above this one spells an offset as a `usize`: [`crate::SourceRange`] is two of them, a parse
    /// error is one ([`crate::CppParseError::offsets`]), and a language server's positions are line and column
    /// numbers rather than a rowan type. This is the single conversion point, which is why it exists instead of a
    /// `TextSize` import wherever a diagnostic is turned into a protocol range.
    ///
    /// An offset past the end of the text answers `None`. The alternative — the last line's column, which is what
    /// the line lookup alone would give — is a *plausible* position for an offset that is not in the file, and a
    /// caller that got one would build a range pointing at code the offset never named. The end of the text is
    /// itself a valid offset: it is where a zero-width range at EOF sits.
    pub fn position_of(&self, offset: usize, source_text: &str) -> Option<(usize, usize)> {
        if offset > source_text.len() {
            return None;
        }

        self.get_line_col(TextSize::from(u32::try_from(offset).ok()?), source_text)
    }

    /// The offset of a **line and column**, both counted from zero.
    ///
    /// The mapping a client's positions come in through, and the one that has to be strict about where a line ends:
    /// `col` is a column *of that line*, so a column past the line's end is clamped to the line's end (which is
    /// what the protocol says to do with an over-long character value) — it is **not** allowed to walk into the next
    /// line. The version this replaced clamped against the whole *text* instead, so `get_offset(0, 99)` on a
    /// three-line file answered with an offset inside a later line: a position that is not in the file the caller
    /// asked about, and one whose characters belong to a different line's syntax.
    ///
    /// `None` for a line the text does not have.
    pub fn get_offset(&self, line: usize, col: usize, source_text: &str) -> Option<TextSize> {
        let start_offset = self.get_line_offset(line)?;
        if col == 0 {
            return Some(start_offset);
        }

        // The line's own extent: up to the next line's start, or to the end of the text.
        let line_end = self
            .get_line_offset(line + 1)
            .map_or(source_text.len(), usize::from)
            .min(source_text.len());
        let line_text = source_text.get(usize::from(start_offset)..line_end)?;
        let body = line_text.strip_suffix('\n').unwrap_or(line_text);
        let body = body.strip_suffix('\r').unwrap_or(body);

        if self.is_line_only_ascii(line.try_into().unwrap()) {
            let col = col.min(body.len());
            Some(start_offset + TextSize::from(col as u32))
        } else {
            let mut offset = 0;
            let mut col = col;
            for character in body.chars() {
                if col == 0 {
                    break;
                }

                offset += character.len_utf8();
                col -= 1;
            }
            Some(start_offset + TextSize::from(offset as u32))
        }
    }
}
/// **The nearest character boundary at or before `at`**, so a slice ending there cannot panic.
///
/// `str::floor_char_boundary` is nightly, and this is the whole of it: an offset inside a multi-byte character
/// belongs to that character, and a reader asking for its column is asking about the character it is in.
fn floor_char_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::LineIndex;

    /// The line starts a text produces, which is what every other answer is derived from.
    fn starts(text: &str) -> Vec<usize> {
        let index = LineIndex::parse(text);
        (0..index.line_count())
            .map(|line| usize::from(index.get_line_offset(line).expect("a line")))
            .collect()
    }

    /// **Every spelling of a line break is one break** — `\n`, `\r\n`, a lone `\r`, and the `\n\r` pair the lexer also
    /// accepts. Counting only `\n` made an old-Mac file one line to the index and many to the parser
    /// (`docs/worklog.md` P2-3).
    #[test]
    fn every_spelling_of_a_line_break_starts_one_line() {
        assert_eq!(starts("a\nb"), [0, 2], "LF");
        assert_eq!(starts("a\r\nb"), [0, 3], "CRLF is one break, not two");
        assert_eq!(starts("a\rb"), [0, 2], "a lone CR is one");
        assert_eq!(starts("a\n\rb"), [0, 3], "and so is the LFCR pair");
        assert_eq!(
            starts("a\nb\n"),
            [0, 2, 4],
            "a trailing break opens an empty line"
        );
        assert_eq!(starts(""), [0], "an empty text is one empty line");
    }

    /// The line of an offset, and the offset of a line and column, agree with those starts — the two directions a
    /// diagnostic and a client position each need.
    #[test]
    fn offsets_and_lines_agree() {
        let text = "a\r\nbc\rd\n\re";
        let index = LineIndex::parse(text);
        assert_eq!(index.line_count(), 4, "{text:?}");

        // `b` is on line 1 and at column 0; `c` at line 1, column 1.
        let b = text.find('b').expect("b");
        let c = text.find('c').expect("c");
        assert_eq!(index.position_of(b, text), Some((1, 0)));
        assert_eq!(index.position_of(c, text), Some((1, 1)));
        assert_eq!(
            usize::from(index.get_offset(1, 1, text).expect("an offset")),
            c
        );

        // The line's end is the line's end: a column past it does not walk into the next line.
        let end_of_line_0 = usize::from(index.get_offset(0, 99, text).expect("an offset"));
        assert_eq!(
            end_of_line_0,
            text.find('\r').expect("the first break"),
            "clamped to this line"
        );
    }

    /// A non-ASCII line is flagged, and its column counts **characters** rather than bytes.
    #[test]
    fn a_non_ascii_line_counts_characters() {
        let text = "éé\r\nxy";
        let index = LineIndex::parse(text);
        assert!(!index.is_line_only_ascii(rowan::TextSize::from(0)));
        assert!(index.is_line_only_ascii(rowan::TextSize::from(1)));
        assert_eq!(index.position_of(0, text), Some((0, 0)));
        // The second `é` starts at byte 2 and is column 1.
        assert_eq!(index.position_of(2, text), Some((0, 1)));
    }
}
