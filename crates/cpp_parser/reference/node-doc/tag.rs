use crate::{
    kind::CppSyntaxKind, syntax::traits::LuaAstNode, BinaryOperator, LuaAstChildren, LuaAstToken,
    LuaAstTokenChildren, LuaBinaryOpToken, LuaDocVersionNumberToken, LuaDocVisibilityToken,
    LuaGeneralToken, LuaKind, LuaNameToken, LuaNumberToken, LuaPathToken, LuaStringToken,
    LuaSyntaxNode, CppTokenKind, LuaVersionCondition,
};

use super::{
    description::{LuaDocDescriptionOwner, LuaDocDetailOwner},
    LuaDocAttribute, LuaDocGenericDeclList, LuaDocOpType, LuaDocType, LuaDocTypeList,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LuaDocTag {
    Class(LuaDocTagClass),
    Enum(LuaDocTagEnum),
    Alias(LuaDocTagAlias),
    Type(LuaDocTagType),
    Param(LuaDocTagParam),
    Return(LuaDocTagReturn),
    Overload(LuaDocTagOverload),
    Field(LuaDocTagField),
    Module(LuaDocTagModule),
    See(LuaDocTagSee),
    Diagnostic(LuaDocTagDiagnostic),
    Deprecated(LuaDocTagDeprecated),
    Version(LuaDocTagVersion),
    Cast(LuaDocTagCast),
    Source(LuaDocTagSource),
    Other(LuaDocTagOther),
    Namespace(LuaDocTagNamespace),
    Using(LuaDocTagUsing),
    Meta(LuaDocTagMeta),
    Nodiscard(LuaDocTagNodiscard),
    Readonly(LuaDocTagReadonly),
    Operator(LuaDocTagOperator),
    Generic(LuaDocTagGeneric),
    Async(LuaDocTagAsync),
    As(LuaDocTagAs),
    Visibility(LuaDocTagVisibility),
    ReturnCast(LuaDocTagReturnCast),
}

impl LuaAstNode for LuaDocTag {
    fn syntax(&self) -> &LuaSyntaxNode {
        match self {
            LuaDocTag::Class(it) => it.syntax(),
            LuaDocTag::Enum(it) => it.syntax(),
            LuaDocTag::Alias(it) => it.syntax(),
            LuaDocTag::Type(it) => it.syntax(),
            LuaDocTag::Param(it) => it.syntax(),
            LuaDocTag::Return(it) => it.syntax(),
            LuaDocTag::Overload(it) => it.syntax(),
            LuaDocTag::Field(it) => it.syntax(),
            LuaDocTag::Module(it) => it.syntax(),
            LuaDocTag::See(it) => it.syntax(),
            LuaDocTag::Diagnostic(it) => it.syntax(),
            LuaDocTag::Deprecated(it) => it.syntax(),
            LuaDocTag::Version(it) => it.syntax(),
            LuaDocTag::Cast(it) => it.syntax(),
            LuaDocTag::Source(it) => it.syntax(),
            LuaDocTag::Other(it) => it.syntax(),
            LuaDocTag::Namespace(it) => it.syntax(),
            LuaDocTag::Using(it) => it.syntax(),
            LuaDocTag::Meta(it) => it.syntax(),
            LuaDocTag::Nodiscard(it) => it.syntax(),
            LuaDocTag::Readonly(it) => it.syntax(),
            LuaDocTag::Operator(it) => it.syntax(),
            LuaDocTag::Generic(it) => it.syntax(),
            LuaDocTag::Async(it) => it.syntax(),
            LuaDocTag::As(it) => it.syntax(),
            LuaDocTag::Visibility(it) => it.syntax(),
            LuaDocTag::ReturnCast(it) => it.syntax(),
        }
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagClass
            || kind == CppSyntaxKind::DocTagEnum
            || kind == CppSyntaxKind::DocTagAlias
            || kind == CppSyntaxKind::DocTagType
            || kind == CppSyntaxKind::DocTagParam
            || kind == CppSyntaxKind::DocTagReturn
            || kind == CppSyntaxKind::DocTagOverload
            || kind == CppSyntaxKind::DocTagField
            || kind == CppSyntaxKind::DocTagModule
            || kind == CppSyntaxKind::DocTagSee
            || kind == CppSyntaxKind::DocTagDiagnostic
            || kind == CppSyntaxKind::DocTagDeprecated
            || kind == CppSyntaxKind::DocTagVersion
            || kind == CppSyntaxKind::DocTagCast
            || kind == CppSyntaxKind::DocTagSource
            || kind == CppSyntaxKind::DocTagOther
            || kind == CppSyntaxKind::DocTagNamespace
            || kind == CppSyntaxKind::DocTagUsing
            || kind == CppSyntaxKind::DocTagMeta
            || kind == CppSyntaxKind::DocTagNodiscard
            || kind == CppSyntaxKind::DocTagReadonly
            || kind == CppSyntaxKind::DocTagOperator
            || kind == CppSyntaxKind::DocTagGeneric
            || kind == CppSyntaxKind::DocTagAsync
            || kind == CppSyntaxKind::DocTagAs
            || kind == CppSyntaxKind::DocTagVisibility
            || kind == CppSyntaxKind::DocTagReturnCast
    }

    fn cast(syntax: LuaSyntaxNode) -> Option<Self>
    where
        Self: Sized,
    {
        match syntax.kind().into() {
            CppSyntaxKind::DocTagClass => {
                Some(LuaDocTag::Class(LuaDocTagClass::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagEnum => {
                Some(LuaDocTag::Enum(LuaDocTagEnum::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagAlias => {
                Some(LuaDocTag::Alias(LuaDocTagAlias::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagType => {
                Some(LuaDocTag::Type(LuaDocTagType::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagParam => {
                Some(LuaDocTag::Param(LuaDocTagParam::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagReturn => {
                Some(LuaDocTag::Return(LuaDocTagReturn::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagOverload => Some(LuaDocTag::Overload(
                LuaDocTagOverload::cast(syntax).unwrap(),
            )),
            CppSyntaxKind::DocTagField => {
                Some(LuaDocTag::Field(LuaDocTagField::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagModule => {
                Some(LuaDocTag::Module(LuaDocTagModule::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagSee => Some(LuaDocTag::See(LuaDocTagSee::cast(syntax).unwrap())),
            CppSyntaxKind::DocTagDiagnostic => Some(LuaDocTag::Diagnostic(
                LuaDocTagDiagnostic::cast(syntax).unwrap(),
            )),
            CppSyntaxKind::DocTagDeprecated => Some(LuaDocTag::Deprecated(
                LuaDocTagDeprecated::cast(syntax).unwrap(),
            )),
            CppSyntaxKind::DocTagVersion => {
                Some(LuaDocTag::Version(LuaDocTagVersion::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagCast => {
                Some(LuaDocTag::Cast(LuaDocTagCast::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagSource => {
                Some(LuaDocTag::Source(LuaDocTagSource::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagOther => {
                Some(LuaDocTag::Other(LuaDocTagOther::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagNamespace => Some(LuaDocTag::Namespace(
                LuaDocTagNamespace::cast(syntax).unwrap(),
            )),
            CppSyntaxKind::DocTagUsing => {
                Some(LuaDocTag::Using(LuaDocTagUsing::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagMeta => {
                Some(LuaDocTag::Meta(LuaDocTagMeta::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagNodiscard => Some(LuaDocTag::Nodiscard(
                LuaDocTagNodiscard::cast(syntax).unwrap(),
            )),
            CppSyntaxKind::DocTagReadonly => Some(LuaDocTag::Readonly(
                LuaDocTagReadonly::cast(syntax).unwrap(),
            )),
            CppSyntaxKind::DocTagOperator => Some(LuaDocTag::Operator(
                LuaDocTagOperator::cast(syntax).unwrap(),
            )),
            CppSyntaxKind::DocTagGeneric => {
                Some(LuaDocTag::Generic(LuaDocTagGeneric::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagAsync => {
                Some(LuaDocTag::Async(LuaDocTagAsync::cast(syntax).unwrap()))
            }
            CppSyntaxKind::DocTagAs => Some(LuaDocTag::As(LuaDocTagAs::cast(syntax).unwrap())),
            CppSyntaxKind::DocTagVisibility => Some(LuaDocTag::Visibility(
                LuaDocTagVisibility::cast(syntax).unwrap(),
            )),
            CppSyntaxKind::DocTagReturnCast => Some(LuaDocTag::ReturnCast(
                LuaDocTagReturnCast::cast(syntax).unwrap(),
            )),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagClass {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagClass {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagClass
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

impl LuaDocDescriptionOwner for LuaDocTagClass {}

impl LuaDocTagClass {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn get_generic_decl(&self) -> Option<LuaDocGenericDeclList> {
        self.child()
    }

    pub fn get_supers(&self) -> Option<LuaDocTypeList> {
        self.child()
    }

    pub fn get_attrib(&self) -> Option<LuaDocAttribute> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagEnum {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagEnum {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagEnum
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

impl LuaDocDescriptionOwner for LuaDocTagEnum {}

impl LuaDocTagEnum {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn get_base_type(&self) -> Option<LuaDocType> {
        self.child()
    }

    pub fn get_fields(&self) -> Option<LuaDocEnumField> {
        self.child()
    }

    pub fn get_attrib(&self) -> Option<LuaDocAttribute> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocEnumField {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocEnumField {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocEnumField
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

impl LuaDocDetailOwner for LuaDocEnumField {}

impl LuaDocEnumField {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn get_type(&self) -> Option<LuaDocType> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagAlias {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagAlias {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagAlias
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

impl LuaDocDescriptionOwner for LuaDocTagAlias {}

impl LuaDocTagAlias {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn get_generic_decl_list(&self) -> Option<LuaDocGenericDeclList> {
        self.child()
    }

    pub fn get_type(&self) -> Option<LuaDocType> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagType {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagType {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagType
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

impl LuaDocDescriptionOwner for LuaDocTagType {}

impl LuaDocTagType {
    pub fn get_type_list(&self) -> LuaAstChildren<LuaDocType> {
        self.children()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagParam {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagParam {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagParam
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

impl LuaDocDescriptionOwner for LuaDocTagParam {}

impl LuaDocTagParam {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn is_vararg(&self) -> bool {
        self.token_by_kind(CppTokenKind::TkDots).is_some()
    }

    pub fn is_nullable(&self) -> bool {
        self.token_by_kind(CppTokenKind::TkDocQuestion).is_some()
    }

    pub fn get_type(&self) -> Option<LuaDocType> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagReturn {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagReturn {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagReturn
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

impl LuaDocDescriptionOwner for LuaDocTagReturn {}

impl LuaDocTagReturn {
    pub fn get_first_type(&self) -> Option<LuaDocType> {
        self.child()
    }

    pub fn get_types(&self) -> LuaAstChildren<LuaDocType> {
        self.children()
    }

    pub fn get_type_and_name_list(&self) -> Vec<(LuaDocType, Option<LuaNameToken>)> {
        let mut result = Vec::new();
        let mut current_type = None;
        let mut current_name = None;
        for child in self.syntax.children_with_tokens() {
            match child.kind() {
                LuaKind::Token(CppTokenKind::TkComma) => {
                    if let Some(type_) = current_type {
                        result.push((type_, current_name));
                    }
                    current_type = None;
                    current_name = None;
                }
                LuaKind::Token(CppTokenKind::TkName) => {
                    current_name = Some(LuaNameToken::cast(child.into_token().unwrap()).unwrap());
                }
                k if LuaDocType::can_cast(k.into()) => {
                    current_type = Some(LuaDocType::cast(child.into_node().unwrap()).unwrap());
                }

                _ => {}
            }
        }

        if let Some(type_) = current_type {
            result.push((type_, current_name));
        }

        result
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagOverload {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagOverload {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagOverload
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

impl LuaDocDescriptionOwner for LuaDocTagOverload {}

impl LuaDocTagOverload {
    // todo use luaFuncType
    pub fn get_type(&self) -> Option<LuaDocType> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagField {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagField {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagField
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

impl LuaDocDescriptionOwner for LuaDocTagField {}

impl LuaDocTagField {
    pub fn get_field_key(&self) -> Option<LuaDocFieldKey> {
        let mut meet_left_bracket = false;
        for child in self.syntax.children_with_tokens() {
            if meet_left_bracket {
                match child {
                    rowan::NodeOrToken::Node(node) => {
                        if LuaDocType::can_cast(node.kind().into()) {
                            return Some(LuaDocFieldKey::Type(LuaDocType::cast(node).unwrap()));
                        }
                    }
                    rowan::NodeOrToken::Token(token) => match token.kind().into() {
                        CppTokenKind::TkString => {
                            return Some(LuaDocFieldKey::String(
                                LuaStringToken::cast(token.clone()).unwrap(),
                            ));
                        }
                        CppTokenKind::TkInt => {
                            return Some(LuaDocFieldKey::Integer(
                                LuaNumberToken::cast(token.clone()).unwrap(),
                            ));
                        }
                        _ => {}
                    },
                }
            } else if let Some(token) = child.as_token() {
                if token.kind() == CppTokenKind::TkLeftBracket.into() {
                    meet_left_bracket = true;
                } else if token.kind() == CppTokenKind::TkName.into() {
                    return Some(LuaDocFieldKey::Name(
                        LuaNameToken::cast(token.clone()).unwrap(),
                    ));
                }
            }
        }

        None
    }

    pub fn get_field_key_range(&self) -> Option<rowan::TextRange> {
        self.get_field_key().map(|key| match key {
            LuaDocFieldKey::Name(name) => name.get_range(),
            LuaDocFieldKey::String(string) => string.get_range(),
            LuaDocFieldKey::Integer(integer) => integer.get_range(),
            LuaDocFieldKey::Type(typ) => typ.get_range(),
        })
    }

    pub fn get_type(&self) -> Option<LuaDocType> {
        self.children().last()
    }

    pub fn is_nullable(&self) -> bool {
        self.token_by_kind(CppTokenKind::TkDocQuestion).is_some()
    }

    pub fn get_visibility_token(&self) -> Option<LuaDocVisibilityToken> {
        self.token()
    }

    pub fn get_attrib(&self) -> Option<LuaDocAttribute> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LuaDocFieldKey {
    Name(LuaNameToken),
    String(LuaStringToken),
    Integer(LuaNumberToken),
    Type(LuaDocType),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagModule {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagModule {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagModule
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

impl LuaDocDescriptionOwner for LuaDocTagModule {}

impl LuaDocTagModule {
    pub fn get_string_token(&self) -> Option<LuaStringToken> {
        self.token()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagSee {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagSee {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagSee
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

impl LuaDocDescriptionOwner for LuaDocTagSee {}

impl LuaDocTagSee {
    pub fn get_see_content(&self) -> Option<LuaGeneralToken> {
        self.token_by_kind(CppTokenKind::TkDocSeeContent)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagDiagnostic {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagDiagnostic {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagDiagnostic
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

impl LuaDocDescriptionOwner for LuaDocTagDiagnostic {}

impl LuaDocTagDiagnostic {
    pub fn get_action_token(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn get_code_list(&self) -> Option<LuaDocDiagnosticCodeList> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocDiagnosticCodeList {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocDiagnosticCodeList {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocDiagnosticCodeList
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

impl LuaDocDiagnosticCodeList {
    pub fn get_codes(&self) -> LuaAstTokenChildren<LuaNameToken> {
        self.tokens()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagDeprecated {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagDeprecated {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagDeprecated
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

impl LuaDocDescriptionOwner for LuaDocTagDeprecated {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagVersion {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagVersion {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagVersion
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

impl LuaDocDescriptionOwner for LuaDocTagVersion {}

impl LuaDocTagVersion {
    pub fn get_version_list(&self) -> LuaAstChildren<LuaDocVersion> {
        self.children()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocVersion {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocVersion {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocVersion
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

impl LuaDocVersion {
    pub fn get_op(&self) -> Option<LuaBinaryOpToken> {
        self.token()
    }

    pub fn get_frame_name(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn get_version(&self) -> Option<LuaDocVersionNumberToken> {
        self.token()
    }

    pub fn get_version_condition(&self) -> Option<LuaVersionCondition> {
        let op = self.get_op();
        let version_token = self.get_version()?;
        let version_number = version_token.get_version_number()?;
        if op.is_none() {
            return Some(LuaVersionCondition::Eq(version_number));
        }

        let op = op.unwrap();
        // You might find it strange, but that's the logic of luals.
        match op.get_op() {
            BinaryOperator::OpGt => Some(LuaVersionCondition::Gte(version_number)),
            BinaryOperator::OpLt => Some(LuaVersionCondition::Lte(version_number)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagCast {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagCast {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagCast
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

impl LuaDocDescriptionOwner for LuaDocTagCast {}

impl LuaDocTagCast {
    pub fn get_op_types(&self) -> LuaAstChildren<LuaDocOpType> {
        self.children()
    }

    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagSource {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagSource {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagSource
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

impl LuaDocDescriptionOwner for LuaDocTagSource {}

impl LuaDocTagSource {
    pub fn get_path_token(&self) -> Option<LuaPathToken> {
        self.token()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagOther {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagOther {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagOther
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

impl LuaDocDescriptionOwner for LuaDocTagOther {}

impl LuaDocTagOther {
    pub fn get_tag_name(&self) -> Option<String> {
        let token = self.token_by_kind(CppTokenKind::TkTagOther)?;
        Some(token.get_text().to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagNamespace {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagNamespace {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagNamespace
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

impl LuaDocDescriptionOwner for LuaDocTagNamespace {}

impl LuaDocTagNamespace {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagUsing {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagUsing {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagUsing
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

impl LuaDocDescriptionOwner for LuaDocTagUsing {}

impl LuaDocTagUsing {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagMeta {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagMeta {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagMeta
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

impl LuaDocTagMeta {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagNodiscard {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagNodiscard {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagNodiscard
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

impl LuaDocDescriptionOwner for LuaDocTagNodiscard {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagReadonly {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagReadonly {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagReadonly
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

impl LuaDocDescriptionOwner for LuaDocTagReadonly {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagOperator {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagOperator {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagOperator
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

impl LuaDocDescriptionOwner for LuaDocTagOperator {}

impl LuaDocTagOperator {
    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }

    pub fn get_param_list(&self) -> Option<LuaDocTypeList> {
        self.child()
    }

    pub fn get_return_type(&self) -> Option<LuaDocType> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagGeneric {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagGeneric {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        kind == CppSyntaxKind::DocTagGeneric
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

impl LuaDocDescriptionOwner for LuaDocTagGeneric {}

impl LuaDocTagGeneric {
    pub fn get_generic_decl_list(&self) -> Option<LuaDocGenericDeclList> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagAsync {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagAsync {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DocTagAsync
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagAs {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagAs {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DocTagAs
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

impl LuaDocTagAs {
    pub fn get_type(&self) -> Option<LuaDocType> {
        self.child()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagVisibility {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagVisibility {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DocTagVisibility
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

impl LuaDocTagVisibility {
    pub fn get_visibility_token(&self) -> Option<LuaDocVisibilityToken> {
        self.token()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTagReturnCast {
    syntax: LuaSyntaxNode,
}

impl LuaAstNode for LuaDocTagReturnCast {
    fn syntax(&self) -> &LuaSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DocTagReturnCast
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

impl LuaDocDescriptionOwner for LuaDocTagReturnCast {}

impl LuaDocTagReturnCast {
    pub fn get_op_type(&self) -> Option<LuaDocOpType> {
        self.child()
    }

    pub fn get_name_token(&self) -> Option<LuaNameToken> {
        self.token()
    }
}
