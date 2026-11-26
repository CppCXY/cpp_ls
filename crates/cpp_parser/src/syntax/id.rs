use crate::kind;
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

    pub fn from_ptr(ptr: kind::CppSyntaxNodePtr) -> Self {
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

/// **The schema this serialised form is written in**, and the only one [`from_hex`] accepts.
///
/// The kind is a **discriminant**, which means the spelling `1:0:3` is only an id because this version of the crate says
/// what `1` is. A variant inserted in the middle of the enum - an ordinary thing to do - would silently change what every
/// stored id meant, and no reader could tell (`docs/worklog.md` P2-5). The tag makes that a rejection
/// instead: a cache written by another schema is refused, and the reader says so rather than pointing at the wrong node.
const WIRE_SCHEMA: &str = "v1";

impl std::fmt::Display for CppSyntaxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let start = u32::from(self.range.start());
        let end = u32::from(self.range.end());
        write!(
            f,
            "{WIRE_SCHEMA}:{:x}:{:x}:{:x}",
            self.kind.get_raw(),
            start,
            end
        )
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

/// **Read the `v1:kind:start:end` spelling**, which is what [`CppSyntaxId`] serialises to.
///
/// Fallible, and every way it can fail is a value that a *reader* must reject rather than act on: this is the shape a
/// cache file or another process hands over, so a malformed string is input and not a bug in the program. The three
/// things that used to be unchecked here were
///
/// ```text
/// schema a tag this reader does not know — see `WIRE_SCHEMA`
/// kind   CppKind::from_raw panics for a discriminant outside the kind space
/// range  TextRange::new panics when the end is before the start
/// shape  a string with the wrong number of fields
/// ```
///
/// — the first two inside a `serde` deserialiser, which is to say a malformed cache entry could take the reader process
/// down instead of returning an error (`docs/worklog.md` P1-5).
fn from_hex(value: &str) -> Result<CppSyntaxId, String> {
    let mut parts = value.split(':');
    let (Some(schema), Some(kind), Some(start), Some(end), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(format!(
            "expected '{WIRE_SCHEMA}:kind:start:end', got {value:?}"
        ));
    };
    if schema != WIRE_SCHEMA {
        return Err(format!(
            "this id was written in schema {schema:?}, and this reader only knows {WIRE_SCHEMA:?}"
        ));
    }

    let kind = u16::from_str_radix(kind, 16).map_err(|error| format!("invalid kind: {error}"))?;
    let start =
        u32::from_str_radix(start, 16).map_err(|error| format!("invalid start: {error}"))?;
    let end = u32::from_str_radix(end, 16).map_err(|error| format!("invalid end: {error}"))?;

    // A range is an ordered pair; anything else would make `TextRange::new` panic.

    if start > end {
        return Err(format!(
            "a range that ends before it starts: {start:x}..{end:x}"
        ));
    }

    let kind = CppKind::try_from_raw(kind)
        .ok_or_else(|| format!("no kind with discriminant {kind:#06x}"))?;

    Ok(CppSyntaxId {
        kind,
        range: TextRange::new(TextSize::new(start), TextSize::new(end)),
    })
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
                from_hex(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(CppSyntaxIdVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The serialised spelling round-trips, and every malformed one is an error rather than a panic.**
    ///
    /// The input is what a cache file or another process hands over — see `from_hex` — so each of these used to be a
    /// way to take the reader down: an unknown discriminant (which `from_raw` panics on) and a range that ends before it
    /// starts (which `TextRange::new` panics on).
    #[test]
    fn a_serialised_id_is_read_or_refused_without_panicking() {
        let id = CppSyntaxId::new(
            CppKind::Syntax(CppSyntaxKind::TranslationUnit),
            TextRange::new(TextSize::new(0x10), TextSize::new(0x20)),
        );
        assert_eq!(
            from_hex(&id.to_string()).expect("its own spelling reads back"),
            id
        );

        for (value, why) in [
            ("", "an empty string"),
            ("v1:1:2", "three fields"),
            ("v1:1:2:3:4", "five fields"),
            ("1:2:3", "no schema tag — the form before the tag existed"),
            ("v2:1:2:3", "a schema this reader does not know"),
            ("v1:zz:0:1", "a kind that is not hex"),
            ("v1:1:zz:2", "a start that is not hex"),
            ("v1:1:2:zz", "an end that is not hex"),
            ("v1:ffff:0:1", "a discriminant outside the kind space"),
            ("v1:1:2:1", "a range that ends before it starts"),
        ] {
            let refused = from_hex(value);
            assert!(
                refused.is_err(),
                "{why} is refused, not parsed: {value:?} -> {refused:?}"
            );
        }
    }

    #[test]
    fn id_roundtrips_through_text() {
        let id = CppSyntaxId::new(
            CppKind::Syntax(CppSyntaxKind::TranslationUnit),
            TextRange::new(TextSize::new(0), TextSize::new(42)),
        );

        // Mirrors the `Deserialize` implementation, so this stays dependency free.
        let text = id.to_string();
        assert_eq!(
            from_hex(&text).expect("its own spelling reads back"),
            id,
            "and the spelling starts with the schema: {text}"
        );
        let parsed = from_hex(&text).expect("an id");

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
