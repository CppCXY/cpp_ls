//! Typed token wrappers.
//!
//! A token type exists when the token has behaviour beyond its kind: a name has text, a number has a
//! value, an operator has a precedence. Tokens that carry nothing extra use [`CppGeneralToken`].
//!
//! The shape follows the reference implementation (`reference/README.md`): a newtype over a
//! [`CppSyntaxToken`] plus a `CppAstToken` impl, with accessors as inherent methods.

use crate::{
    CppSyntaxToken,
    kind::{CppKind, CppSyntaxKind, CppTokenKind},
    syntax::traits::CppAstToken,
};

/// Any token. The fallback for tokens with no special behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppGeneralToken {
    token: CppSyntaxToken,
}

impl CppAstToken for CppGeneralToken {
    fn syntax(&self) -> &CppSyntaxToken {
        &self.token
    }

    fn can_cast(_: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        true
    }

    fn cast(syntax: CppSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        Some(CppGeneralToken { token: syntax })
    }
}

impl CppGeneralToken {
    pub fn get_text(&self) -> &str {
        self.token.text()
    }
}

/// An identifier: a name being declared or referenced.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppNameToken {
    token: CppSyntaxToken,
}

impl CppAstToken for CppNameToken {
    fn syntax(&self) -> &CppSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::Identifier
    }

    fn cast(syntax: CppSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(CppNameToken { token: syntax })
        } else {
            None
        }
    }
}

impl CppNameToken {
    pub fn get_name_text(&self) -> &str {
        self.token.text()
    }
}

/// A keyword token, for the handful of places that need to look at one directly.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppKeywordToken {
    token: CppSyntaxToken,
}

impl CppAstToken for CppKeywordToken {
    fn syntax(&self) -> &CppSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        is_keyword(kind)
    }

    fn cast(syntax: CppSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(CppKeywordToken { token: syntax })
        } else {
            None
        }
    }
}

impl CppKeywordToken {
    pub fn get_keyword_text(&self) -> &str {
        self.token.text()
    }

    pub fn get_keyword_kind(&self) -> CppKind {
        self.token.kind()
    }
}

/// Is this token kind one of the keyword kinds?
///
/// `false` for `final` and `override`: they *look* like keywords but are identifiers with special
/// meaning, which is why the parser matches them by text.
pub fn is_keyword(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::AutoKeyword
            | CppTokenKind::BreakKeyword
            | CppTokenKind::CaseKeyword
            | CppTokenKind::CatchKeyword
            | CppTokenKind::CharKeyword
            | CppTokenKind::ClassKeyword
            | CppTokenKind::ConstKeyword
            | CppTokenKind::ConstexprKeyword
            | CppTokenKind::ConstevalKeyword
            | CppTokenKind::ConstinitKeyword
            | CppTokenKind::ContinueKeyword
            | CppTokenKind::DecltypeKeyword
            | CppTokenKind::DefaultKeyword
            | CppTokenKind::DeleteKeyword
            | CppTokenKind::DoKeyword
            | CppTokenKind::DoubleKeyword
            | CppTokenKind::ElseKeyword
            | CppTokenKind::EnumKeyword
            | CppTokenKind::ExplicitKeyword
            | CppTokenKind::ExportKeyword
            | CppTokenKind::ExternKeyword
            | CppTokenKind::FalseKeyword
            | CppTokenKind::FloatKeyword
            | CppTokenKind::ForKeyword
            | CppTokenKind::FriendKeyword
            | CppTokenKind::GotoKeyword
            | CppTokenKind::IfKeyword
            | CppTokenKind::InlineKeyword
            | CppTokenKind::IntKeyword
            | CppTokenKind::LongKeyword
            | CppTokenKind::MutableKeyword
            | CppTokenKind::NamespaceKeyword
            | CppTokenKind::NewKeyword
            | CppTokenKind::NoexceptKeyword
            | CppTokenKind::NullptrKeyword
            | CppTokenKind::OperatorKeyword
            | CppTokenKind::PrivateKeyword
            | CppTokenKind::ProtectedKeyword
            | CppTokenKind::PublicKeyword
            | CppTokenKind::RegisterKeyword
            | CppTokenKind::ReturnKeyword
            | CppTokenKind::ShortKeyword
            | CppTokenKind::SignedKeyword
            | CppTokenKind::SizeofKeyword
            | CppTokenKind::StaticKeyword
            | CppTokenKind::StaticAssertKeyword
            | CppTokenKind::StructKeyword
            | CppTokenKind::SwitchKeyword
            | CppTokenKind::TemplateKeyword
            | CppTokenKind::ThisKeyword
            | CppTokenKind::ThreadLocalKeyword
            | CppTokenKind::ThrowKeyword
            | CppTokenKind::TrueKeyword
            | CppTokenKind::TryKeyword
            | CppTokenKind::TypedefKeyword
            | CppTokenKind::TypeidKeyword
            | CppTokenKind::TypenameKeyword
            | CppTokenKind::UnionKeyword
            | CppTokenKind::UnsignedKeyword
            | CppTokenKind::UsingKeyword
            | CppTokenKind::VirtualKeyword
            | CppTokenKind::VoidKeyword
            | CppTokenKind::VolatileKeyword
            | CppTokenKind::WhileKeyword
            | CppTokenKind::AlignasKeyword
            | CppTokenKind::AlignofKeyword
            | CppTokenKind::CoAwaitKeyword
            | CppTokenKind::CoReturnKeyword
            | CppTokenKind::CoYieldKeyword
    )
}

/// Is this token kind a type specifier keyword?
///
/// `class`, `struct`, `union` and `enum` are included: they name a type, and `Foo` in
/// `class Foo;` is a class *type*.
pub fn is_type_keyword(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::VoidKeyword
            | CppTokenKind::CharKeyword
            | CppTokenKind::ShortKeyword
            | CppTokenKind::IntKeyword
            | CppTokenKind::LongKeyword
            | CppTokenKind::FloatKeyword
            | CppTokenKind::DoubleKeyword
            | CppTokenKind::SignedKeyword
            | CppTokenKind::UnsignedKeyword
            | CppTokenKind::BoolLiteral
            | CppTokenKind::AutoKeyword
            | CppTokenKind::DecltypeKeyword
            | CppTokenKind::ClassKeyword
            | CppTokenKind::StructKeyword
            | CppTokenKind::UnionKeyword
            | CppTokenKind::EnumKeyword
            | CppTokenKind::TypenameKeyword
    )
}

/// An operator token, in either a unary or a binary position.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppOperatorToken {
    token: CppSyntaxToken,
}

impl CppAstToken for CppOperatorToken {
    fn syntax(&self) -> &CppSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        is_operator(kind)
    }

    fn cast(syntax: CppSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(CppOperatorToken { token: syntax })
        } else {
            None
        }
    }
}

impl CppOperatorToken {
    pub fn get_operator_text(&self) -> &str {
        self.token.text()
    }

    pub fn get_operator_kind(&self) -> CppKind {
        self.token.kind()
    }
}

/// Is this token kind an operator?
pub fn is_operator(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Plus
            | CppTokenKind::Minus
            | CppTokenKind::Star
            | CppTokenKind::Slash
            | CppTokenKind::Percent
            | CppTokenKind::Assign
            | CppTokenKind::PlusAssign
            | CppTokenKind::MinusAssign
            | CppTokenKind::StarAssign
            | CppTokenKind::SlashAssign
            | CppTokenKind::PercentAssign
            | CppTokenKind::Equal
            | CppTokenKind::NotEqual
            | CppTokenKind::Less
            | CppTokenKind::LessEqual
            | CppTokenKind::Greater
            | CppTokenKind::GreaterEqual
            | CppTokenKind::Spaceship
            | CppTokenKind::LogicalAnd
            | CppTokenKind::LogicalOr
            | CppTokenKind::LogicalNot
            | CppTokenKind::Ampersand
            | CppTokenKind::Pipe
            | CppTokenKind::Caret
            | CppTokenKind::Tilde
            | CppTokenKind::LeftShift
            | CppTokenKind::RightShift
            | CppTokenKind::AmpersandAssign
            | CppTokenKind::PipeAssign
            | CppTokenKind::CaretAssign
            | CppTokenKind::LeftShiftAssign
            | CppTokenKind::RightShiftAssign
            | CppTokenKind::PlusPlus
            | CppTokenKind::MinusMinus
            | CppTokenKind::Dot
            | CppTokenKind::Arrow
            | CppTokenKind::DotStar
            | CppTokenKind::ArrowStar
            | CppTokenKind::Scope
            | CppTokenKind::Question
    )
}

/// A punctuation token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppPunctuationToken {
    token: CppSyntaxToken,
}

impl CppAstToken for CppPunctuationToken {
    fn syntax(&self) -> &CppSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        is_punctuation(kind)
    }

    fn cast(syntax: CppSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(CppPunctuationToken { token: syntax })
        } else {
            None
        }
    }
}

/// Is this token kind a delimiter?
pub fn is_punctuation(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::LeftParen
            | CppTokenKind::RightParen
            | CppTokenKind::LeftBrace
            | CppTokenKind::RightBrace
            | CppTokenKind::LeftBracket
            | CppTokenKind::RightBracket
            | CppTokenKind::Semicolon
            | CppTokenKind::Comma
            | CppTokenKind::Colon
            | CppTokenKind::Ellipsis
            | CppTokenKind::Hash
            | CppTokenKind::HashHash
    )
}

/// A literal token: a number, character, string, boolean or `nullptr`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppLiteralToken {
    token: CppSyntaxToken,
}

impl CppAstToken for CppLiteralToken {
    fn syntax(&self) -> &CppSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        is_literal(kind)
    }

    fn cast(syntax: CppSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(CppLiteralToken { token: syntax })
        } else {
            None
        }
    }
}

impl CppLiteralToken {
    pub fn get_literal_text(&self) -> &str {
        self.token.text()
    }
}

/// Is this token kind a literal?
pub fn is_literal(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::IntegerLiteral
            | CppTokenKind::FloatingLiteral
            | CppTokenKind::CharLiteral
            | CppTokenKind::StringLiteral
            | CppTokenKind::UserDefinedLiteral
            | CppTokenKind::BoolLiteral
            | CppTokenKind::NullptrLiteral
            | CppTokenKind::TrueKeyword
            | CppTokenKind::FalseKeyword
            | CppTokenKind::NullptrKeyword
    )
}

/// Is this token kind a comment?
pub fn is_comment(kind: CppTokenKind) -> bool {
    matches!(kind, CppTokenKind::LineComment | CppTokenKind::BlockComment)
}

/// The kind of a comment token, for the comment section of the AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CppCommentKind {
    Line,
    Block,
}

impl From<CppKind> for CppCommentKind {
    fn from(kind: CppKind) -> Self {
        match kind {
            CppKind::Token(CppTokenKind::BlockComment) => CppCommentKind::Block,
            _ => CppCommentKind::Line,
        }
    }
}

#[allow(dead_code)]
impl From<CppTokenKind> for CppCommentKind {
    fn from(kind: CppTokenKind) -> Self {
        match kind {
            CppTokenKind::BlockComment => CppCommentKind::Block,
            _ => CppCommentKind::Line,
        }
    }
}

/// A comment token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppCommentToken {
    token: CppSyntaxToken,
}

impl CppAstToken for CppCommentToken {
    fn syntax(&self) -> &CppSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        is_comment(kind)
    }

    fn cast(syntax: CppSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(CppCommentToken { token: syntax })
        } else {
            None
        }
    }
}

impl CppCommentToken {
    pub fn get_comment_kind(&self) -> CppCommentKind {
        CppCommentKind::from(self.token.kind())
    }

    /// The comment text without its delimiters, and without the leading `*` most block comments put
    /// on continuation lines.
    pub fn get_comment_text(&self) -> String {
        let raw = self.token.text();
        match self.get_comment_kind() {
            CppCommentKind::Line => raw.trim_start_matches('/').trim().to_string(),
            CppCommentKind::Block => {
                let inner = raw
                    .strip_prefix("/*")
                    .and_then(|it| it.strip_suffix("*/"))
                    .unwrap_or(raw);
                inner
                    .lines()
                    .map(|line| line.trim().trim_start_matches('*').trim())
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }

    /// Is this a documentation comment (`///`, `//!`, `/**`, `/*!`)?
    ///
    /// The distinction matters because only these are handed to the Doxygen layer; an ordinary
    /// comment is trivia and nothing more.
    pub fn is_doc_comment(&self) -> bool {
        let raw = self.token.text();
        match self.get_comment_kind() {
            CppCommentKind::Line => raw.starts_with("///") || raw.starts_with("//!"),
            CppCommentKind::Block => raw.starts_with("/**") || raw.starts_with("/*!"),
        }
    }
}

/// The `::` in a qualified name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppScopeToken {
    token: CppSyntaxToken,
}

impl CppAstToken for CppScopeToken {
    fn syntax(&self) -> &CppSyntaxToken {
        &self.token
    }

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized,
    {
        kind == CppTokenKind::Scope
    }

    fn cast(syntax: CppSyntaxToken) -> Option<Self>
    where
        Self: Sized,
    {
        if Self::can_cast(syntax.kind().into()) {
            Some(CppScopeToken { token: syntax })
        } else {
            None
        }
    }
}

/// Nodes have no tokens of their own; this alias exists so `CppAstToken` implementations can name
/// the syntax kind of a token's parent without importing rowan.
pub type CppTokenParentKind = CppSyntaxKind;
