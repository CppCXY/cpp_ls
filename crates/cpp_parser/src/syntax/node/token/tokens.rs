use crate::{
    kind::{BinaryOperator, CppTokenKind, UnaryOperator},
    syntax::traits::LuaAstToken,
    LuaOpKind, LuaSyntaxToken, LuaTypeBinaryOperator, LuaTypeUnaryOperator, LuaVersionNumber,
    VisibilityKind,
};

use super::{float_token_value, int_token_value, string_token_value};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaGeneralToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaGeneralToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(_: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        true
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        Some(LuaGeneralToken { token: syntax })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaNameToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaNameToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::TkName
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaNameToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaNameToken {
    pub fn get_name_text(&self) -> &str {
        self.token.text()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaStringToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaStringToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::TkString || kind == CppTokenKind::TkLongString
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaStringToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaStringToken {
    pub fn get_value(&self) -> String {
        string_token_value(&self.token).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaNumberToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaNumberToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::TkFloat || kind == CppTokenKind::TkInt
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaNumberToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaNumberToken {
    pub fn is_float(&self) -> bool {
        self.token.kind() == CppTokenKind::TkFloat.into()
    }

    pub fn is_int(&self) -> bool {
        self.token.kind() == CppTokenKind::TkInt.into()
    }

    pub fn get_float_value(&self) -> f64 {
        if !self.is_float() {
            return 0.0;
        }
        float_token_value(&self.token).unwrap_or(0.0)
    }

    pub fn get_int_value(&self) -> i64 {
        if !self.is_int() {
            return 0;
        }
        int_token_value(&self.token).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaBinaryOpToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaBinaryOpToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        LuaOpKind::to_binary_operator(kind) != BinaryOperator::OpNop
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaBinaryOpToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaBinaryOpToken {
    pub fn get_op(&self) -> BinaryOperator {
        LuaOpKind::to_binary_operator(self.token.kind().into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaUnaryOpToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaUnaryOpToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        LuaOpKind::to_unary_operator(kind) != UnaryOperator::OpNop
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaUnaryOpToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaUnaryOpToken {
    pub fn get_op(&self) -> UnaryOperator {
        LuaOpKind::to_unary_operator(self.token.kind().into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaKeywordToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaKeywordToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        match kind {
            CppTokenKind::TkAnd
            | CppTokenKind::TkBreak
            | CppTokenKind::TkDo
            | CppTokenKind::TkElse
            | CppTokenKind::TkElseIf
            | CppTokenKind::TkEnd
            | CppTokenKind::TkFalse
            | CppTokenKind::TkFor
            | CppTokenKind::TkFunction
            | CppTokenKind::TkGoto
            | CppTokenKind::TkIf
            | CppTokenKind::TkIn
            | CppTokenKind::TkLocal
            | CppTokenKind::TkNil
            | CppTokenKind::TkNot
            | CppTokenKind::TkOr
            | CppTokenKind::TkRepeat
            | CppTokenKind::TkReturn
            | CppTokenKind::TkThen
            | CppTokenKind::TkTrue
            | CppTokenKind::TkUntil
            | CppTokenKind::TkWhile => true,
            _ => false,
        }
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaKeywordToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaKeywordToken {
    pub fn get_keyword(&self) -> CppTokenKind {
        self.token.kind().into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaBoolToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaBoolToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::TkTrue || kind == CppTokenKind::TkFalse
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaBoolToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaBoolToken {
    pub fn is_true(&self) -> bool {
        self.token.kind() == CppTokenKind::TkTrue.into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaNilToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaNilToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::TkNil
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaNilToken { token: syntax })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LuaLiteralToken {
    String(LuaStringToken),
    Number(LuaNumberToken),
    Bool(LuaBoolToken),
    Nil(LuaNilToken),
    Dots(LuaGeneralToken),
    Question(LuaGeneralToken),
}

impl LuaAstToken for LuaLiteralToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        match self {
            LuaLiteralToken::String(token) => token.syntax(),
            LuaLiteralToken::Number(token) => token.syntax(),
            LuaLiteralToken::Bool(token) => token.syntax(),
            LuaLiteralToken::Nil(token) => token.syntax(),
            LuaLiteralToken::Dots(token) => token.syntax(),
            LuaLiteralToken::Question(token) => token.syntax(),
        }
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        match kind {
            CppTokenKind::TkInt
            | CppTokenKind::TkFloat
            | CppTokenKind::TkComplex
            | CppTokenKind::TkNil
            | CppTokenKind::TkTrue
            | CppTokenKind::TkFalse
            | CppTokenKind::TkDots
            | CppTokenKind::TkString
            | CppTokenKind::TkLongString
            | CppTokenKind::TkDocQuestion => true,
            _ => false,
        }
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        match syntax.kind().into() {
            CppTokenKind::TkString | CppTokenKind::TkLongString => {
                LuaStringToken::cast(syntax).map(LuaLiteralToken::String)
            }
            CppTokenKind::TkFloat | CppTokenKind::TkInt | CppTokenKind::TkComplex => {
                LuaNumberToken::cast(syntax).map(LuaLiteralToken::Number)
            }
            CppTokenKind::TkTrue | CppTokenKind::TkFalse => {
                LuaBoolToken::cast(syntax).map(LuaLiteralToken::Bool)
            }
            CppTokenKind::TkNil => LuaNilToken::cast(syntax).map(LuaLiteralToken::Nil),
            CppTokenKind::TkDots => LuaGeneralToken::cast(syntax).map(LuaLiteralToken::Dots),
            CppTokenKind::TkDocQuestion => {
                LuaGeneralToken::cast(syntax).map(LuaLiteralToken::Question)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaSpaceToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaSpaceToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        match kind {
            CppTokenKind::TkWhitespace | CppTokenKind::TkEndOfLine => true,
            _ => false,
        }
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaSpaceToken { token: syntax })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaIndexToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaIndexToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::TkDot
            || kind == CppTokenKind::TkColon
            || kind == CppTokenKind::TkLeftBracket
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaIndexToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaIndexToken {
    pub fn is_dot(&self) -> bool {
        self.token.kind() == CppTokenKind::TkDot.into()
    }

    pub fn is_colon(&self) -> bool {
        self.token.kind() == CppTokenKind::TkColon.into()
    }

    pub fn is_left_bracket(&self) -> bool {
        self.token.kind() == CppTokenKind::TkLeftBracket.into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocDetailToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaDocDetailToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::TkDocDetail
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaDocDetailToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaDocDetailToken {
    pub fn get_detail(&self) -> &str {
        self.token.text()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocVisibilityToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaDocVisibilityToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::TkDocVisibility || kind == CppTokenKind::TkTagVisibility
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaDocVisibilityToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaDocVisibilityToken {
    pub fn get_visibility(&self) -> VisibilityKind {
        VisibilityKind::to_visibility_kind(self.token.text())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocVersionNumberToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaDocVersionNumberToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::TkDocVersionNumber
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaDocVersionNumberToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaDocVersionNumberToken {
    pub fn get_version_number(&self) -> Option<LuaVersionNumber> {
        let text = self.token.text();
        LuaVersionNumber::from_str(text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTypeBinaryToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaDocTypeBinaryToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::TkDocOr
            || kind == CppTokenKind::TkDocAnd
            || kind == CppTokenKind::TkDocExtends
            || kind == CppTokenKind::TkDocIn
            || kind == CppTokenKind::TkDocContinueOr
            || kind == CppTokenKind::TkPlus
            || kind == CppTokenKind::TkMinus
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaDocTypeBinaryToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaDocTypeBinaryToken {
    pub fn get_op(&self) -> LuaTypeBinaryOperator {
        LuaOpKind::to_type_binary_operator(self.token.kind().into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaDocTypeUnaryToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaDocTypeUnaryToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::TkDocKeyOf || kind == CppTokenKind::TkMinus
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaDocTypeUnaryToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaDocTypeUnaryToken {
    pub fn get_op(&self) -> LuaTypeUnaryOperator {
        LuaOpKind::to_type_unary_operator(self.token.kind().into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LuaPathToken {
    token: LuaSyntaxToken,
}

impl LuaAstToken for LuaPathToken {
    fn syntax(&self) -> &LuaSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::TKDocPath
    }

    fn cast(syntax: LuaSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(LuaPathToken { token: syntax })
        } else {
            None
        }
    }
}

impl LuaPathToken {
    pub fn get_path(&self) -> &str {
        let text = self.token.text();
        if text.starts_with('\"') || text.starts_with('\'') {
            &text[1..text.len() - 1]
        } else {
            text
        }
    }
}
