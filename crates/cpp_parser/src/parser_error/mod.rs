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
