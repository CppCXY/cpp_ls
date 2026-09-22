use crate::{
    LuaAstChildren, LuaAstNode, LuaAstToken, LuaDocTypeBinaryToken, LuaDocTypeUnaryToken,
    LuaLiteralToken, LuaNameToken, CppSyntaxKind, LuaSyntaxNode, CppTokenKind,
};

use super::{LuaDocDescription, LuaDocObjectField, LuaDocTypeList};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LuaDocType {
    Name(LuaDocNameType),
    Array(LuaDocArrayType),
    Func(LuaDocFuncType),
    Object(LuaDocObjectType),
    Binary(LuaDocBinaryType),
    Unary(LuaDocUnaryType),
    Conditional(LuaDocConditionalType),
    Tuple(LuaDocTupleType),
    Literal(LuaDocLiteralType),
    Variadic(LuaDocVariadicType),
    Nullable(LuaDocNullableType),
    Generic(LuaDocGenericType),
    StrTpl(LuaDocStrTplType),
    MultiLineUnion(LuaDocMultiLineUnionType),
}

impl LuaAstNode for LuaDocType {
    fn syntax(&self) -> &LuaSyntaxNode {
        match self {
            LuaDocType::Name(it) => it.syntax(),
            LuaDocType::Array(it) => it.syntax(),
            LuaDocType::Func(it) => it.syntax(),
            LuaDocType::Object(it) => it.syntax(),
            LuaDocType::Binary(it) => it.syntax(),
            LuaDocType::Unary(it) => it.syntax(),
            LuaDocType::Conditional(it) => it.syntax(),
            LuaDocType::Tuple(it) => it.syntax(),
            LuaDocType::Literal(it) => it.syntax(),
            LuaDocType::Variadic(it) => it.syntax(),
            LuaDocType::Nullable(it) => it.syntax(),
            LuaDocType::Generic(it) => it.syntax(),
            LuaDocType::StrTpl(it) => it.syntax(),
            LuaDocType::MultiLineUnion(it) => it.syntax(),
        }
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        match kind {
            CppSyntaxKind::TypeName => true,
            CppSyntaxKind::TypeArray => true,
            CppSyntaxKind::TypeFun => true,
            CppSyntaxKind::TypeObject => true,
            CppSyntaxKind::TypeBinary => true,
            CppSyntaxKind::TypeUnary => true,
            CppSyntaxKind::TypeConditional => true,
            CppSyntaxKind::TypeTuple => true,
            CppSyntaxKind::TypeLiteral => true,
            CppSyntaxKind::TypeVariadic => true,
            CppSyntaxKind::TypeNullable => true,
            CppSyntaxKind::TypeGeneric => true,
            CppSyntaxKind::TypeStringTemplate => true,
            CppSyntaxKind::TypeMultiLineUnion => true,
            _ => false,
        }
    }

    fn cast(syntax: LuaSyntaxNode) -> Option<Self>
    where
        Self: Sized,
    {
        match syntax.kind().into() {
            CppSyntaxKind::TypeName => Some(LuaDocType::Name(LuaDocNameType::cast(syntax)?)),
            CppSyntaxKind::TypeArray => Some(LuaDocType::Array(LuaDocArrayType::cast(syntax)?)),
            CppSyntaxKind::TypeFun => Some(LuaDocType::Func(LuaDocFuncType::cast(syntax)?)),
            CppSyntaxKind::TypeObject => Some(LuaDocType::Object(LuaDocObjectType::cast(syntax)?)),
            CppSyntaxKind::TypeBinary => Some(LuaDocType::Binary(LuaDocBinaryType::cast(syntax)?)),
            CppSyntaxKind::TypeUnary => Some(LuaDocType::Unary(LuaDocUnaryType::cast(syntax)?)),
            CppSyntaxKind::TypeConditional => Some(LuaDocType::Conditional(
                LuaDocConditionalType::cast(syntax)?,
            )),
            CppSyntaxKind::TypeTuple => Some(LuaDocType::Tuple(LuaDocTupleType::cast(syntax)?)),
            CppSyntaxKind::TypeLiteral => {
                Some(LuaDocType::Literal(LuaDocLiteralType::cast(syntax)?))
            }
            CppSyntaxKind::TypeVariadic => {
                Some(LuaDocType::Variadic(LuaDocVariadicType::cast(syntax)?))
            }
            CppSyntaxKind::TypeNullable => {
                Some(LuaDocType::Nullable(LuaDocNullableType::cast(syntax)?))
            }
            CppSyntaxKind::TypeGeneric => {
                Some(LuaDocType::Generic(LuaDocGenericType::cast(syntax)?))
            }
            CppSyntaxKind::TypeStringTemplate => {
                Some(LuaDocType::StrTpl(LuaDocStrTplType::cast(syntax)?))
            }
            CppSyntaxKind::TypeMultiLineUnion => Some(LuaDocType::MultiLineUnion(
                LuaDocMultiLineUnionType::cast(syntax)?,
            )),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocNameType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocNameType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeName
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

impl LuaDocNameType {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn get_name_text(&self) -> Option<String> {
        self.get_name_token()
            .map(|it| it.get_name_text().to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocArrayType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocArrayType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeArray
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

impl LuaDocArrayType {
    pub fn get_type(&self) -> Option<LuaDocType> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocFuncType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocFuncType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeFun
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

impl LuaDocFuncType {
    pub fn is_async(&self) -> bool {
        match self.token::<LuaNameToken>() {
            Some(it) => it.get_name_text() == "async",
            None => false,
        }
    }

    pub fn get_params(&self) -> LuaAstChildren<LuaDocTypeParam> {
        self.children()
    }

    pub fn get_return_type_list(&self) -> Option<LuaDocTypeList> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTypeParam {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTypeParam {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTypedParameter
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

impl LuaDocTypeParam {
    pub fn is_dots(&self) -> bool {
        self.token_by_kind(CppTokenKind::TkDots).is_some()
    }

    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn get_type(&self) -> Option<LuaDocType> {
        self.child()
    }

    pub fn is_nullable(&self) -> bool {
        self.token_by_kind(CppTokenKind::TkDocQuestion).is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocObjectType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocObjectType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeObject
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

impl LuaDocObjectType {
    pub fn get_fields(&self) -> LuaAstChildren<LuaDocObjectField> {
        self.children()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocBinaryType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocBinaryType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeBinary
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

impl LuaDocBinaryType {
    pub fn get_op_token(&self) -> Option<LuaDocTypeBinaryToken> {
        self.token()
    }

    pub fn get_types(&self) -> Option<(LuaDocType, LuaDocType)> {
        let mut children = self.children();
        let left = children.next()?;
        let right = children.next()?;
        Some((left, right))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocUnaryType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocUnaryType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeUnary
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

impl LuaDocUnaryType {
    pub fn get_op_token(&self) -> Option<LuaDocTypeUnaryToken> {
        self.token()
    }

    pub fn get_type(&self) -> Option<LuaDocType> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocConditionalType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocConditionalType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeConditional
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

impl LuaDocConditionalType {
    pub fn get_types(&self) -> Option<(LuaDocType, LuaDocType, LuaDocType)> {
        let mut children = self.children();
        let condition = children.next()?;
        let true_type = children.next()?;
        let false_type = children.next()?;
        Some((condition, true_type, false_type))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTupleType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTupleType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeTuple
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

impl LuaDocTupleType {
    pub fn get_types(&self) -> LuaAstChildren<LuaDocType> {
        self.children()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocLiteralType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocLiteralType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeLiteral
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

impl LuaDocLiteralType {
    pub fn get_literal(&self) -> Option<LuaLiteralToken> {
        self.token()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocVariadicType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocVariadicType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeVariadic
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

impl LuaDocVariadicType {
    pub fn get_type(&self) -> Option<LuaDocType> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocNullableType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocNullableType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeNullable
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

impl LuaDocNullableType {
    pub fn get_type(&self) -> Option<LuaDocType> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocGenericType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocGenericType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeGeneric
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

impl LuaDocGenericType {
    pub fn get_name_type(&self) -> Option<LuaDocNameType> {
        self.child()
    }

    pub fn get_generic_types(&self) -> Option<LuaDocTypeList> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocStrTplType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocStrTplType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeStringTemplate
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

impl LuaDocStrTplType {
    /// `T` or  xxx.`T` or xxx.`T`.xxxx
    pub fn get_name(&self) -> (Option<String>, Option<String>, Option<String>) {
        let str_tpl = self.token_by_kind(CppTokenKind::TkStringTemplateType);
        if str_tpl.is_none() {
            return (None, None, None);
        }
        let str_tpl = str_tpl.unwrap();
        let text = str_tpl.get_text();
        let mut iter = text.split('`');
        let first = iter.next().map(|it| it.to_string());
        let second = iter.next().map(|it| it.to_string());
        let third = iter.next().map(|it| it.to_string());

        (first, second, third)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocMultiLineUnionType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocMultiLineUnionType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::TypeMultiLineUnion
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

impl LuaDocMultiLineUnionType {
    pub fn get_fields(&self) -> LuaAstChildren<LuaDocOneLineField> {
        self.children()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocOneLineField {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocOneLineField {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocOneLineField
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

impl LuaDocOneLineField {
    pub fn get_type(&self) -> Option<LuaDocType> {
        self.child()
    }

    pub fn get_description(&self) -> Option<LuaDocDescription> {
        self.child()
    }
}
