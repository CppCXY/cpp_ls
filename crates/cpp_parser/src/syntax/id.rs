use rowan::{TextRange, TextSize};

use crate::{
    kind::{CppKind, CppSyntaxKind, CppTokenKind},
    syntax::{CppSyntaxNode, CppSyntaxToken, CppSyntaxTree},
};

/// A cheap, storable pointer to a syntax node or token: `(kind, range)`.
///
/// Red nodes borrow their tree, so they cannot be cached in the LSP layer; `CppSyntaxId` is the
/// serialisable substitute. It is resolvable back to a red node as long as the tree is unchanged,
/// which is why it must only ever be stored alongside an edit generation counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CppSyntaxId {
    kind: CppKind,
    range: TextRange,
}

impl CppSyntaxId {
    pub fn new(kind: CppKind, range: TextRange) -> Self {
        CppSyntaxId { kind, range }
    }

    pub fn from_ptr(ptr: crate::kind::CppSyntaxNodePtr) -> Self {
        CppSyntaxId {
            kind: ptr.kind(),
            range: ptr.text_range(),
        }
    }

    pub fn from_node(node: &CppSyntaxNode) -> Self {
        CppSyntaxId {
            kind: node.kind(),
            range: node.text_range(),
        }
    }

    pub fn from_token(token: &CppSyntaxToken) -> Self {
        CppSyntaxId {
            kind: token.kind(),
            range: token.text_range(),
        }
    }

    pub fn get_kind(&self) -> CppSyntaxKind {
        self.kind.into()
    }

    pub fn get_token_kind(&self) -> CppTokenKind {
        self.kind.into()
    }

    pub fn is_token(&self) -> bool {
        self.kind.is_token()
    }

    pub fn is_node(&self) -> bool {
        self.kind.is_syntax()
    }

    pub fn get_range(&self) -> TextRange {
        self.range
    }

    /// Resolve back to a red node. Returns `None` if this id pointed at a token, if the tree is
    /// empty, or if the tree has changed shape since the id was created.
    pub fn to_node(&self, tree: &CppSyntaxTree) -> Option<CppSyntaxNode> {
        let root = tree.get_red_root();
        self.to_node_from_root(&root)
    }

    pub fn to_node_from_root(&self, root: &CppSyntaxNode) -> Option<CppSyntaxNode> {
        let mut current = root.clone();
        loop {
            if current.text_range() == self.range && current.kind() == self.kind {
                return Some(current);
            }

            current = current
                .children()
                .find(|child| child.text_range().contains_range(self.range))?;
        }
    }

    pub fn to_token(&self, tree: &CppSyntaxTree) -> Option<CppSyntaxToken> {
        let root = tree.get_red_root();
        self.to_token_from_root(&root)
    }

    pub fn to_token_from_root(&self, root: &CppSyntaxNode) -> Option<CppSyntaxToken> {
        let mut current = root.clone();
        loop {
            let found = current
                .children_with_tokens()
                .find(|child| child.text_range().contains_range(self.range))?;

            match found {
                rowan::NodeOrToken::Node(node) => current = node,
                rowan::NodeOrToken::Token(token) => {
                    return (token.text_range() == self.range && token.kind() == self.kind)
                        .then_some(token);
                }
            }
        }
    }

    pub fn to_node_at_range(root: &CppSyntaxNode, range: TextRange) -> Option<CppSyntaxNode> {
        let mut current = root.clone();
        loop {
            if current.text_range() == range {
                return Some(current);
            }

            current = current
                .children()
                .find(|child| child.text_range().contains_range(range))?;
        }
    }
}

impl std::fmt::Display for CppSyntaxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let start = u32::from(self.range.start());
        let end = u32::from(self.range.end());
        write!(f, "{:x}:{:x}:{:x}", self.kind.get_raw(), start, end)
    }
}

/// Serialised as `kind:start:end`, all hex.
impl serde::Serialize for CppSyntaxId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for CppSyntaxId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct CppSyntaxIdVisitor;

        impl serde::de::Visitor<'_> for CppSyntaxIdVisitor {
            type Value = CppSyntaxId;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a syntax id string formatted as 'kind:start:end' in hex")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let parts: Vec<&str> = value.split(':').collect();
                if parts.len() != 3 {
                    return Err(E::custom("expected format 'kind:start:end'"));
                }

                let kind = u16::from_str_radix(parts[0], 16)
                    .map_err(|e| E::custom(format!("invalid kind: {e}")))?;
                let start = u32::from_str_radix(parts[1], 16)
                    .map_err(|e| E::custom(format!("invalid start: {e}")))?;
                let end = u32::from_str_radix(parts[2], 16)
                    .map_err(|e| E::custom(format!("invalid end: {e}")))?;

                Ok(CppSyntaxId {
                    kind: CppKind::from_raw(kind),
                    range: TextRange::new(TextSize::new(start), TextSize::new(end)),
                })
            }
        }

        deserializer.deserialize_str(CppSyntaxIdVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_roundtrips_through_text() {
        let id = CppSyntaxId::new(
            CppKind::Syntax(CppSyntaxKind::TranslationUnit),
            TextRange::new(TextSize::new(0), TextSize::new(42)),
        );

        // Mirrors the `Deserialize` implementation, so this stays dependency free.
        let text = id.to_string();
        let parts: Vec<&str> = text.split(':').collect();
        let parsed = CppSyntaxId {
            kind: CppKind::from_raw(u16::from_str_radix(parts[0], 16).unwrap()),
            range: TextRange::new(
                TextSize::new(u32::from_str_radix(parts[1], 16).unwrap()),
                TextSize::new(u32::from_str_radix(parts[2], 16).unwrap()),
            ),
        };

        assert_eq!(id, parsed);
    }

    #[test]
    fn token_and_node_ids_do_not_alias() {
        let node = CppSyntaxId::new(
            CppKind::Syntax(CppSyntaxKind::TranslationUnit),
            TextRange::new(TextSize::new(0), TextSize::new(1)),
        );
        let token = CppSyntaxId::new(
            CppKind::Token(CppTokenKind::None),
            TextRange::new(TextSize::new(0), TextSize::new(1)),
        );

        assert!(node.is_node() && !node.is_token());
        assert!(token.is_token() && !token.is_node());
        assert_ne!(node.to_string(), token.to_string());
    }
}
