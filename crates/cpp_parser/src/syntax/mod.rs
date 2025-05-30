mod comment_trait;
mod node;
mod traits;
mod tree;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::iter::successors;

use rowan::{Language, TextRange, TextSize};

use crate::kind::{LuaKind, CppSyntaxKind, CppTokenKind};
pub use comment_trait::*;
pub use node::*;
pub use traits::*;
pub use tree::{LuaSyntaxTree, LuaTreeBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LuaLanguage;

impl Language for LuaLanguage {
    type Kind = LuaKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        LuaKind::from_raw(raw.0)
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind.get_raw())
    }
}

pub type LuaSyntaxNode = rowan::SyntaxNode<LuaLanguage>;
pub type LuaSyntaxToken = rowan::SyntaxToken<LuaLanguage>;
pub type LuaSyntaxElement = rowan::NodeOrToken<LuaSyntaxNode, LuaSyntaxToken>;
pub type LuaSyntaxElementChildren = rowan::SyntaxElementChildren<LuaLanguage>;
pub type LuaSyntaxNodeChildren = rowan::SyntaxNodeChildren<LuaLanguage>;
pub type LuaSyntaxNodePtr = rowan::ast::SyntaxNodePtr<LuaLanguage>;

impl From<CppSyntaxKind> for rowan::SyntaxKind {
    fn from(kind: CppSyntaxKind) -> Self {
        let lua_kind = LuaKind::from(kind);
        rowan::SyntaxKind(lua_kind.get_raw())
    }
}

impl From<rowan::SyntaxKind> for CppSyntaxKind {
    fn from(kind: rowan::SyntaxKind) -> Self {
        LuaKind::from_raw(kind.0).into()
    }
}

impl From<CppTokenKind> for rowan::SyntaxKind {
    fn from(kind: CppTokenKind) -> Self {
        let lua_kind = LuaKind::from(kind);
        rowan::SyntaxKind(lua_kind.get_raw())
    }
}

impl From<rowan::SyntaxKind> for CppTokenKind {
    fn from(kind: rowan::SyntaxKind) -> Self {
        LuaKind::from_raw(kind.0).into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LuaSyntaxId {
    kind: LuaKind,
    range: TextRange,
}

impl LuaSyntaxId {
    pub fn new(kind: LuaKind, range: TextRange) -> Self {
        LuaSyntaxId { kind, range }
    }

    pub fn from_ptr(ptr: LuaSyntaxNodePtr) -> Self {
        LuaSyntaxId {
            kind: ptr.kind(),
            range: ptr.text_range(),
        }
    }

    pub fn from_node(node: &LuaSyntaxNode) -> Self {
        LuaSyntaxId {
            kind: node.kind(),
            range: node.text_range(),
        }
    }

    pub fn from_token(token: &LuaSyntaxToken) -> Self {
        LuaSyntaxId {
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

    pub fn to_node(&self, tree: &LuaSyntaxTree) -> Option<LuaSyntaxNode> {
        let root = tree.get_red_root();
        if root.parent().is_some() {
            return None;
        }
        self.to_node_from_root(&root)
    }

    pub fn to_node_from_root(&self, root: &LuaSyntaxNode) -> Option<LuaSyntaxNode> {
        successors(Some(root.clone()), |node| {
            node.child_or_token_at_range(self.range)?.into_node()
        })
        .find(|it| it.text_range() == self.range && it.kind() == self.kind)
    }

    pub fn to_token(&self, tree: &LuaSyntaxTree) -> Option<LuaSyntaxToken> {
        let root = tree.get_red_root();
        if root.parent().is_some() {
            return None;
        }
        self.to_token_from_root(&root)
    }

    pub fn to_token_from_root(&self, root: &LuaSyntaxNode) -> Option<LuaSyntaxToken> {
        let mut current_node = Some(root.clone());
        while let Some(node) = current_node {
            let node_or_token = node.child_or_token_at_range(self.range)?;
            match node_or_token {
                rowan::NodeOrToken::Node(node) => {
                    current_node = Some(node);
                }
                rowan::NodeOrToken::Token(token) => {
                    if token.text_range() == self.range && token.kind() == self.kind {
                        return Some(token);
                    }
                    return None;
                }
            }
        }
        None
    }

    pub fn to_node_at_range(root: &LuaSyntaxNode, range: TextRange) -> Option<LuaSyntaxNode> {
        successors(Some(root.clone()), |node| {
            node.child_or_token_at_range(range)?.into_node()
        })
        .find(|it| it.text_range() == range)
    }
}

impl Serialize for LuaSyntaxId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let kind_raw = self.kind.get_raw();
        let start = u32::from(self.range.start());
        let end = u32::from(self.range.end());
        let range_combined = ((start as u64) << 32) | (end as u64);
        let value = format!("{:x}:{:x}", kind_raw, range_combined);
        serializer.serialize_str(&value)
    }
}

impl<'de> Deserialize<'de> for LuaSyntaxId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LuaSyntaxIdVisitor;

        impl Visitor<'_> for LuaSyntaxIdVisitor {
            type Value = LuaSyntaxId;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string with format 'kind:range'")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let parts: Vec<&str> = value.split(':').collect();
                if parts.len() != 2 {
                    return Err(E::custom("expected format 'kind:range'"));
                }

                let kind_raw = u16::from_str_radix(parts[0], 16)
                    .map_err(|e| E::custom(format!("invalid kind: {}", e)))?;
                let range_combined = u64::from_str_radix(parts[1], 16)
                    .map_err(|e| E::custom(format!("invalid range: {}", e)))?;

                let start = TextSize::new(((range_combined >> 32) & 0xFFFFFFFF) as u32);
                let end = TextSize::new((range_combined & 0xFFFFFFFF) as u32);

                Ok(LuaSyntaxId {
                    kind: LuaKind::from_raw(kind_raw),
                    range: TextRange::new(start, end),
                })
            }
        }

        deserializer.deserialize_str(LuaSyntaxIdVisitor)
    }
}
