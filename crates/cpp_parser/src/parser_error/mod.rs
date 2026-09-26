use rowan::TextRange;

use crate::text::SourceRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CppParseErrorKind {
    SyntaxError,
    DocError,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CppParseError {
    pub kind: CppParseErrorKind,
    pub message: String,
    pub range: TextRange,
}

impl CppParseError {
    pub fn new(kind: CppParseErrorKind, message: &str, range: TextRange) -> Self {
        CppParseError {
            kind,
            message: message.to_string(),
            range,
        }
    }

    /// The error's byte offsets — `(start, end)` — as every layer above this crate spells an offset.
    ///
    /// [`CppParseError::range`] is a rowan `TextRange`, which a consumer outside the parser cannot name without
    /// depending on rowan; a language server wants offsets (and then, through [`crate::LineIndex::position_of`], a
    /// line and a column), so this is the door it comes through.
    pub fn offsets(&self) -> (usize, usize) {
        (
            u32::from(self.range.start()) as usize,
            u32::from(self.range.end()) as usize,
        )
    }

    pub fn syntax_error_from(message: &str, range: SourceRange) -> Self {
        CppParseError {
            kind: CppParseErrorKind::SyntaxError,
            message: message.to_string(),
            range: range.into(),
        }
    }

    pub fn doc_error_from(message: &str, range: SourceRange) -> Self {
        CppParseError {
            kind: CppParseErrorKind::DocError,
            message: message.to_string(),
            range: range.into(),
        }
    }
}
