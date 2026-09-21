mod cpp_language;
mod cpp_language_level;
mod cpp_operator_kind;
mod cpp_syntax_kind;
mod cpp_token_kind;

pub use cpp_language::{
    CppLanguage, CppSyntaxElement, CppSyntaxElementChildren, CppSyntaxNode, CppSyntaxNodeChildren,
    CppSyntaxNodePtr, CppSyntaxToken,
};
pub use cpp_language_level::CppLanguageLevel;
pub use cpp_operator_kind::{CppBinaryOperator, CppUnaryOperator};
pub use cpp_syntax_kind::CppSyntaxKind;
pub use cpp_token_kind::CppTokenKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum CppKind {
    Syntax(CppSyntaxKind),
    Token(CppTokenKind),
}

impl From<CppSyntaxKind> for CppKind {
    fn from(kind: CppSyntaxKind) -> Self {
        CppKind::Syntax(kind)
    }
}

impl From<CppTokenKind> for CppKind {
    fn from(kind: CppTokenKind) -> Self {
        CppKind::Token(kind)
    }
}

impl From<CppKind> for CppSyntaxKind {
    fn from(val: CppKind) -> Self {
        match val {
            CppKind::Syntax(kind) => kind,
            _ => CppSyntaxKind::None,
        }
    }
}

impl From<CppKind> for CppTokenKind {
    fn from(val: CppKind) -> Self {
        match val {
            CppKind::Token(kind) => kind,
            _ => CppTokenKind::None,
        }
    }
}

/// Tag bit distinguishing syntax-node kinds from token kinds in the flat `u16` space rowan uses.
const SYNTAX_KIND_TAG: u16 = 0x8000;

impl CppKind {
    pub fn is_syntax(self) -> bool {
        matches!(self, CppKind::Syntax(_))
    }

    pub fn is_token(self) -> bool {
        matches!(self, CppKind::Token(_))
    }

    pub fn get_raw(self) -> u16 {
        match self {
            CppKind::Syntax(kind) => kind as u16 | SYNTAX_KIND_TAG,
            CppKind::Token(kind) => kind as u16,
        }
    }

    /// Inverse of [`CppKind::get_raw`].
    ///
    /// # Safety contract
    ///
    /// The `transmute`s below are sound only because `raw` is produced by `get_raw` from a valid
    /// `CppSyntaxKind`/`CppTokenKind`, or received from rowan which stores exactly those values.
    /// Feeding an arbitrary `u16` (e.g. from a corrupted cache) is undefined behaviour; use
    /// [`CppKind::try_from_raw`] when the value comes from untrusted input.
    pub fn from_raw(raw: u16) -> CppKind {
        Self::try_from_raw(raw).unwrap_or_else(|| {
            panic!("invalid CppKind discriminant {raw:#06x}; the kind space only has two tag bits")
        })
    }

    /// Fallible variant of [`CppKind::from_raw`] for deserialisation paths.
    pub fn try_from_raw(raw: u16) -> Option<CppKind> {
        if raw & SYNTAX_KIND_TAG != 0 {
            CppKind::syntax_from_raw(raw & !SYNTAX_KIND_TAG).map(CppKind::Syntax)
        } else {
            CppKind::token_from_raw(raw).map(CppKind::Token)
        }
    }

    fn syntax_from_raw(raw: u16) -> Option<CppSyntaxKind> {
        // `MissingNode` is the last variant, so a discriminant is valid exactly when it is <= it.
        if raw > CppSyntaxKind::MissingNode as u16 {
            return None;
        }
        // Safety: `raw` is within the enum's discriminant range, and `CppSyntaxKind` is a
        // fieldless `repr(u16)` enum, so every such value is a valid inhabitant.
        Some(unsafe { std::mem::transmute::<u16, CppSyntaxKind>(raw) })
    }

    fn token_from_raw(raw: u16) -> Option<CppTokenKind> {
        // `Error` is the last variant, so a discriminant is valid exactly when it is <= it.
        if raw > CppTokenKind::Error as u16 {
            return None;
        }
        // Safety: same reasoning as `syntax_from_raw`.
        Some(unsafe { std::mem::transmute::<u16, CppTokenKind>(raw) })
    }
}

#[derive(Debug)]
pub struct PriorityTable {
    pub left: i32,
    pub right: i32,
}

#[derive(Debug, PartialEq)]
pub enum CppOpKind {
    None,
    Unary(CppUnaryOperator),
    Binary(CppBinaryOperator),
}

impl From<CppUnaryOperator> for CppOpKind {
    fn from(op: CppUnaryOperator) -> Self {
        CppOpKind::Unary(op)
    }
}

impl From<CppBinaryOperator> for CppOpKind {
    fn from(op: CppBinaryOperator) -> Self {
        CppOpKind::Binary(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_roundtrip_covers_both_spaces() {
        for kind in [
            CppSyntaxKind::None,
            CppSyntaxKind::TranslationUnit,
            CppSyntaxKind::ErrorNode,
            CppSyntaxKind::MissingNode,
        ] {
            let raw = CppKind::Syntax(kind).get_raw();
            assert_eq!(CppKind::from_raw(raw), CppKind::Syntax(kind));
        }

        for kind in [
            CppTokenKind::None,
            CppTokenKind::Identifier,
            CppTokenKind::Eof,
            CppTokenKind::Error,
        ] {
            let raw = CppKind::Token(kind).get_raw();
            assert_eq!(CppKind::from_raw(raw), CppKind::Token(kind));
        }
    }

    #[test]
    fn out_of_range_raw_is_rejected() {
        assert!(CppKind::try_from_raw(CppSyntaxKind::MissingNode as u16 + 1 + SYNTAX_KIND_TAG).is_none());
        assert!(CppKind::try_from_raw(CppTokenKind::Error as u16 + 1).is_none());
        assert!(CppKind::try_from_raw(0xFFFF).is_none());
    }

    #[test]
    fn kind_spaces_fit_below_the_tag_bit() {
        // If either enum ever grows past 0x7FFF variants the tagging scheme breaks, and it would
        // break silently: token and syntax kinds would start colliding in the rowan kind space.
        assert!((CppSyntaxKind::MissingNode as u32) < SYNTAX_KIND_TAG as u32);
        assert!((CppTokenKind::Error as u32) < SYNTAX_KIND_TAG as u32);
    }
}
