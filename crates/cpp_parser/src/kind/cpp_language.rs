//! The rowan `Language` binding for C++.
//!
//! `rowan` only knows about a single flat `u16` "syntax kind" space, while we keep syntax nodes
//! and tokens in two separate enums. [`CppKind`] glues them together into one `u16`: token kinds
//! live in `0x0000..0x8000`, syntax kinds in `0x8000..0xFFFF`. That is exactly what
//! `CppKind::get_raw`/`from_raw` already implement, so the `Language` impl is a thin wrapper.

use rowan::{Language, SyntaxKind};

use super::{CppKind, CppSyntaxKind, CppTokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CppLanguage;

impl Language for CppLanguage {
    type Kind = CppKind;

    fn kind_from_raw(raw: SyntaxKind) -> Self::Kind {
        CppKind::from_raw(raw.0)
    }

    fn kind_to_raw(kind: Self::Kind) -> SyntaxKind {
        SyntaxKind(kind.get_raw())
    }
}

pub type CppSyntaxNode = rowan::SyntaxNode<CppLanguage>;
pub type CppSyntaxToken = rowan::SyntaxToken<CppLanguage>;
pub type CppSyntaxElement = rowan::NodeOrToken<CppSyntaxNode, CppSyntaxToken>;
pub type CppSyntaxElementChildren = rowan::SyntaxElementChildren<CppLanguage>;
pub type CppSyntaxNodeChildren = rowan::SyntaxNodeChildren<CppLanguage>;
pub type CppSyntaxNodePtr = rowan::ast::SyntaxNodePtr<CppLanguage>;

impl From<CppSyntaxKind> for SyntaxKind {
    fn from(kind: CppSyntaxKind) -> Self {
        SyntaxKind(CppKind::Syntax(kind).get_raw())
    }
}

impl From<CppTokenKind> for SyntaxKind {
    fn from(kind: CppTokenKind) -> Self {
        SyntaxKind(CppKind::Token(kind).get_raw())
    }
}

impl From<CppKind> for SyntaxKind {
    fn from(kind: CppKind) -> Self {
        SyntaxKind(kind.get_raw())
    }
}

impl From<SyntaxKind> for CppKind {
    fn from(kind: SyntaxKind) -> Self {
        CppKind::from_raw(kind.0)
    }
}

// Deliberately *no* `PartialEq<CppSyntaxKind> for SyntaxKind`. rowan hands out `SyntaxKind`, and
// having two blanket-ish comparisons in scope makes `.into()` and `==` ambiguous at every call
// site. Reading a kind out of the tree is spelled explicitly instead:
//
//     CppKind::from(node.kind()) == CppKind::Syntax(CppSyntaxKind::TranslationUnit)
//
// which is unambiguous and reads as what it is.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_raw_roundtrip_never_collides() {
        // Every syntax kind must survive a round trip through the rowan kind space, and no
        // syntax kind may alias a token kind (they share the same `repr(u16)` numbering).
        let samples = [
            CppKind::Syntax(CppSyntaxKind::TranslationUnit),
            CppKind::Syntax(CppSyntaxKind::ErrorNode),
            CppKind::Syntax(CppSyntaxKind::MissingNode),
            CppKind::Token(CppTokenKind::Identifier),
            CppKind::Token(CppTokenKind::Eof),
            CppKind::Token(CppTokenKind::None),
        ];

        for kind in samples {
            let raw = SyntaxKind::from(kind);
            assert_eq!(CppKind::from(raw), kind, "round trip failed for {kind:?}");
        }

        assert_ne!(
            SyntaxKind::from(CppKind::Syntax(CppSyntaxKind::None)),
            SyntaxKind::from(CppKind::Token(CppTokenKind::None)),
            "syntax and token kind spaces must not overlap"
        );
    }

    #[test]
    fn language_kind_roundtrip() {
        let kind = CppKind::Syntax(CppSyntaxKind::TranslationUnit);
        assert_eq!(
            CppLanguage::kind_from_raw(CppLanguage::kind_to_raw(kind)),
            kind
        );
    }
}
