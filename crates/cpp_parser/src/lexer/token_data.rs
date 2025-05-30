use crate::{kind::CppTokenKind, text::SourceRange};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CppTokenData {
    pub kind: CppTokenKind,
    pub range: SourceRange,
}

impl CppTokenData {
    pub fn new(kind: CppTokenKind, range: SourceRange) -> Self {
        CppTokenData { kind, range }
    }
}
