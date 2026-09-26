use rowan::TextSize;

#[derive(Debug, Clone)]
pub struct LineIndex {
    line_offsets: Vec<TextSize>,
    line_only_ascii_vec: Vec<bool>,
}

impl LineIndex {
    pub fn parse(text: &str) -> LineIndex {
        let mut line_offsets = Vec::new();
        let mut line_only_ascii_vec = Vec::new();
        let mut offset = 0;

        line_offsets.push(TextSize::from(offset as u32));

        let mut is_line_only_ascii = true;
        for (i, c) in text.char_indices() {
            if c == '\n' {
                offset = i + 1; // 记录每行的字节偏移量
                line_offsets.push(TextSize::from(offset as u32));
                line_only_ascii_vec.push(is_line_only_ascii);
                is_line_only_ascii = true;
            } else if !c.is_ascii() {
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
            let text = &source_text[usize::from(start_offset)..usize::from(offset)];
            Some(text.chars().count())
        }
    }

    // get line and col base 0
    pub fn get_line_col(&self, offset: TextSize, source_text: &str) -> Option<(usize, usize)> {
        let (line, start_offset) = self.get_line_with_start_offset(offset)?;
        if self.is_line_only_ascii(line.try_into().unwrap()) {
            Some((line, usize::from(offset - start_offset)))
        } else {
            let text = &source_text[usize::from(start_offset)..usize::from(offset)];
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

