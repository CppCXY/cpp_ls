mod expr;
mod path_trait;
mod stat;
mod test;

use crate::{
    kind::{CppSyntaxKind, CppTokenKind},
    syntax::traits::{LuaAstChildren, LuaAstNode, LuaAstToken},
    LuaCommentOwner, LuaSyntaxNode,
};

pub use expr::*;
pub use path_trait::*;
use rowan::TextRange;
pub use stat::*;

use super::{LuaLiteralToken, LuaNameToken, LuaNumberToken, LuaStringToken};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaChunk {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaChunk {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::Chunk
    }

    fn cast(syntax: LuaSyntaxNode) -> Option<Self>
    where
        Self: Sized,
    {
        if syntax.kind() == CppSyntaxKind::Chunk.into() {
            Some(Self { syntax })
        } else {
            None
        }
    }
}

impl LuaChunk {
    pub fn get_block(&self) -> Option<LuaBlock> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaBlock {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaBlock {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::Block
    }

    fn cast(syntax: LuaSyntaxNode) -> Option<Self>
    where
        Self: Sized,
    {
        if syntax.kind() == CppSyntaxKind::Block.into() {
            Some(Self { syntax })
        } else {
            None
        }
    }
}

impl LuaBlock {
    pub fn get_stats(&self) -> LuaAstChildren<LuaStat> {
        self.children()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaLocalName {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaLocalName {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::LocalName
    }

    fn cast(syntax: LuaSyntaxNode) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(Self { syntax })
        } else {
            None
        }
    }
}

impl LuaLocalName {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn get_attrib(&self) -> Option<LuaLocalAttribute> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaCallArgList {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaCallArgList {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::CallArgList
    }

    fn cast(syntax: LuaSyntaxNode) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(Self { syntax })
        } else {
            None
        }
    }
}

impl LuaCallArgList {
    pub fn is_single_arg_no_parens(&self) -> bool {
        self.token_by_kind(CppTokenKind::TkLeftParen).is_none()
    }

    pub fn get_args(&self) -> LuaAstChildren<LuaExpr> {
        self.children()
    }

    pub fn get_single_arg_expr(&self) -> Option<LuaSingleArgExpr> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaLocalAttribute {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaLocalAttribute {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::Attribute
    }

    fn cast(syntax: LuaSyntaxNode) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(Self { syntax })
        } else {
            None
        }
    }
}

impl LuaLocalAttribute {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn is_close(&self) -> bool {
        match self.get_name_token() {
            None => false,
            Some(name_token) => name_token.get_name_text() == "close",
        }
    }

    pub fn is_const(&self) -> bool {
        match self.get_name_token() {
            None => false,
            Some(name_token) => name_token.get_name_text() == "const",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaTableField {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaTableField {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TableFieldAssign || kind == CppSyntaxKind::TableFieldValue
    }

    fn cast(syntax: LuaSyntaxNode) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(Self { syntax })
        } else {
            None
        }
    }
}

impl LuaCommentOwner for LuaTableField {}

impl LuaTableField {
    pub fn is_assign_field(&self) -> bool {
        self.syntax().kind() == CppSyntaxKind::TableFieldAssign.into()
    }

    pub fn is_value_field(&self) -> bool {
        self.syntax().kind() == CppSyntaxKind::TableFieldValue.into()
    }

    pub fn get_field_key(&self) -> Option<LuaIndexKey> {
        if !self.is_assign_field() {
            let parent_table = self.get_parent::<LuaTableExpr>()?;
            let fields = parent_table.get_fields();
            let mut idx = 1;
            for field in fields {
                if field.is_value_field() {
                    if field.syntax() == self.syntax() {
                        return Some(LuaIndexKey::Idx(idx));
                    }
                    idx += 1;
                }
            }

            return None;
        }

        let mut meet_left_bracket = false;
        for child in self.syntax.children_with_tokens() {
            if meet_left_bracket {
                match child {
                    rowan::NodeOrToken::Node(node) => {
                        if LuaLiteralExpr::can_cast(node.kind().into()) {
                            let literal_expr = LuaLiteralExpr::cast(node.clone()).unwrap();
                            if let Some(literal_token) = literal_expr.get_literal() {
                                match literal_token {
                                    LuaLiteralToken::String(token) => {
                                        return Some(LuaIndexKey::String(token.clone()));
                                    }
                                    LuaLiteralToken::Number(token) => {
                                        return Some(LuaIndexKey::Integer(token.clone()));
                                    }
                                    _ => {}
                                }
                            }
                        }

                        return Some(LuaIndexKey::Expr(LuaExpr::cast(node).unwrap()));
                    }
                    _ => return None,
                }
            } else if let Some(token) = child.as_token() {
                if token.kind() == CppTokenKind::TkLeftBracket.into() {
                    meet_left_bracket = true;
                } else if token.kind() == CppTokenKind::TkName.into() {
                    return Some(LuaIndexKey::Name(
                        LuaNameToken::cast(token.clone()).unwrap(),
                    ));
                }
            }
        }

        None
    }

    pub fn get_value_expr(&self) -> Option<LuaExpr> {
        if self.is_assign_field() {
            self.children().last()
        } else {
            self.child()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LuaIndexKey {
    Name(LuaNameToken),
    String(LuaStringToken),
    Integer(LuaNumberToken),
    Expr(LuaExpr),
    Idx(usize),
}

impl LuaIndexKey {
    pub fn is_name(&self) -> bool {
        matches!(self, LuaIndexKey::Name(_))
    }

    pub fn is_string(&self) -> bool {
        matches!(self, LuaIndexKey::String(_))
    }

    pub fn is_integer(&self) -> bool {
        matches!(self, LuaIndexKey::Integer(_))
    }

    pub fn is_expr(&self) -> bool {
        matches!(self, LuaIndexKey::Expr(_))
    }

    pub fn get_name(&self) -> Option<&LuaNameToken> {
        match self {
            LuaIndexKey::Name(token) => Some(token),
            _ => None,
        }
    }

    pub fn get_string(&self) -> Option<&LuaStringToken> {
        match self {
            LuaIndexKey::String(token) => Some(token),
            _ => None,
        }
    }

    pub fn get_integer(&self) -> Option<&LuaNumberToken> {
        match self {
            LuaIndexKey::Integer(token) => Some(token),
            _ => None,
        }
    }

    pub fn get_expr(&self) -> Option<&LuaExpr> {
        match self {
            LuaIndexKey::Expr(expr) => Some(expr),
            _ => None,
        }
    }

    pub fn get_path_part(&self) -> String {
        match self {
            LuaIndexKey::String(s) => s.get_value(),
            LuaIndexKey::Name(name) => name.get_name_text().to_string(),
            LuaIndexKey::Integer(i) => {
                format!("[{}]", i.get_int_value())
            }
            LuaIndexKey::Expr(expr) => {
                format!("[{}]", expr.syntax().text())
            }
            LuaIndexKey::Idx(i) => {
                format!("[{}]", i)
            }
        }
    }

    pub fn get_range(&self) -> Option<TextRange> {
        match self {
            LuaIndexKey::Name(token) => Some(token.get_range()),
            LuaIndexKey::String(token) => Some(token.get_range()),
            LuaIndexKey::Integer(token) => Some(token.get_range()),
            LuaIndexKey::Expr(expr) => Some(expr.syntax().text_range()),
            LuaIndexKey::Idx(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaParamName {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaParamName {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::ParamName
    }

    fn cast(syntax: LuaSyntaxNode) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(Self { syntax })
        } else {
            None
        }
    }
}

impl LuaParamName {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn is_dots(&self) -> bool {
        self.token_by_kind(CppTokenKind::TkDots).is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaParamList {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaParamList {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::ParamList
    }

    fn cast(syntax: LuaSyntaxNode) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(Self { syntax })
        } else {
            None
        }
    }
}

impl LuaParamList {
    pub fn get_params(&self) -> LuaAstChildren<LuaParamName> {
        self.children()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LuaIndexMemberExpr {
    IndexExpr(LuaIndexExpr),
    TableField(LuaTableField),
}

impl LuaAstNode for LuaIndexMemberExpr {
    fn syntax(&self) -> &LuaSyntaxNode {
        match self {
            LuaIndexMemberExpr::IndexExpr(expr) => expr.syntax(),
            LuaIndexMemberExpr::TableField(field) => field.syntax(),
        }
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        LuaIndexExpr::can_cast(kind) || LuaTableField::can_cast(kind)
    }

    fn cast(syntax: LuaSyntaxNode) -> Option<Self>
    where
        Self: Sized,
    {
        if LuaIndexExpr::can_cast(syntax.kind().into()) {
            Some(Self::IndexExpr(LuaIndexExpr::cast(syntax).unwrap()))
        } else if LuaTableField::can_cast(syntax.kind().into()) {
            Some(Self::TableField(LuaTableField::cast(syntax).unwrap()))
        } else {
            None
        }
    }
}

impl LuaIndexMemberExpr {
    pub fn get_index_expr(&self) -> Option<LuaIndexExpr> {
        match self {
            LuaIndexMemberExpr::IndexExpr(expr) => Some(expr.clone()),
            _ => None,
        }
    }

    pub fn get_table_field(&self) -> Option<LuaTableField> {
        match self {
            LuaIndexMemberExpr::TableField(field) => Some(field.clone()),
            _ => None,
        }
    }

    pub fn get_index_key(&self) -> Option<LuaIndexKey> {
        match self {
            LuaIndexMemberExpr::IndexExpr(expr) => expr.get_index_key(),
            LuaIndexMemberExpr::TableField(field) => field.get_field_key(),
        }
    }

    pub fn get_prefix_expr(&self) -> Option<LuaExpr> {
        match self {
            LuaIndexMemberExpr::IndexExpr(expr) => expr.get_prefix_expr(),
            LuaIndexMemberExpr::TableField(field) => field.get_parent(),
        }
    }
}
