//! Expression nodes.
//!
//! C++ expressions form a deep nesting of *forms* rather than a single `Expr` node kind with an
//! operator field: `a + b` is a `BinaryExpr` whose children are two expressions, `f(x)` is a
//! `CallExpr` whose first child is the callee. That makes the tree easy to walk structurally, which
//! is what an editor wants — "give me everything inside this call's arguments" is a child query, not
//! a re-parse.
//!
//! [`CppExpr`] is the sum type over all of them. Its `cast` is what makes
//! [`CppAstNode::child`] work for expressions, so every node kind that can appear in expression
//! position has to be listed there.

use crate::{
    kind::{CppKind, CppSyntaxKind, CppTokenKind},
    syntax::traits::{CppAstChildren, CppAstNode, CppAstToken},
    CppSyntaxNode,
};

use super::{CppDeclaration, CppDeclarator, CppTypeId};
use crate::syntax::node::token::{CppLiteralToken, CppNameToken, CppOperatorToken};

/// Any expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CppExpr {
    LiteralExpr(CppLiteralExpr),
    NameExpr(CppNameExpr),
    ParenExpr(CppParenExpr),
    UnaryExpr(CppUnaryExpr),
    BinaryExpr(CppBinaryExpr),
    TernaryExpr(CppTernaryExpr),
    CallExpr(CppCallExpr),
    IndexExpr(CppIndexExpr),
    MemberExpr(CppMemberExpr),
    InitListExpr(CppInitListExpr),
    DesignatedInitExpr(CppDesignatedInitExpr),
    LambdaExpr(CppLambdaExpr),
    ThisExpr(CppThisExpr),
    /// An expression the parser could not classify, kept so the tree stays lossless.
    ErrorNode(CppUnknownExpr),
}

impl CppAstNode for CppExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        match self {
            CppExpr::LiteralExpr(node) => node.syntax(),
            CppExpr::NameExpr(node) => node.syntax(),
            CppExpr::ParenExpr(node) => node.syntax(),
            CppExpr::UnaryExpr(node) => node.syntax(),
            CppExpr::BinaryExpr(node) => node.syntax(),
            CppExpr::TernaryExpr(node) => node.syntax(),
            CppExpr::CallExpr(node) => node.syntax(),
            CppExpr::IndexExpr(node) => node.syntax(),
            CppExpr::MemberExpr(node) => node.syntax(),
            CppExpr::InitListExpr(node) => node.syntax(),
            CppExpr::DesignatedInitExpr(node) => node.syntax(),
            CppExpr::LambdaExpr(node) => node.syntax(),
            CppExpr::ThisExpr(node) => node.syntax(),
            CppExpr::ErrorNode(node) => node.syntax(),
        }
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::LiteralExpr
                | CppSyntaxKind::IdentifierExpr
                | CppSyntaxKind::NameExpr
                | CppSyntaxKind::ParenExpr
                | CppSyntaxKind::UnaryExpr
                | CppSyntaxKind::BinaryExpr
                | CppSyntaxKind::TernaryExpr
                | CppSyntaxKind::CallExpr
                | CppSyntaxKind::IndexExpr
                | CppSyntaxKind::MemberExpr
                | CppSyntaxKind::ArrowExpr
                | CppSyntaxKind::InitListExpr
                | CppSyntaxKind::DesignatedInitExpr
                | CppSyntaxKind::LambdaExpr
                | CppSyntaxKind::ClosureExpr
                | CppSyntaxKind::ThisExpr
                | CppSyntaxKind::ErrorNode
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        match CppSyntaxKind::from(syntax.kind()) {
            CppSyntaxKind::LiteralExpr => {
                CppLiteralExpr::cast(syntax).map(CppExpr::LiteralExpr)
            }
            CppSyntaxKind::IdentifierExpr | CppSyntaxKind::NameExpr => {
                CppNameExpr::cast(syntax).map(CppExpr::NameExpr)
            }
            CppSyntaxKind::ParenExpr => CppParenExpr::cast(syntax).map(CppExpr::ParenExpr),
            CppSyntaxKind::UnaryExpr => CppUnaryExpr::cast(syntax).map(CppExpr::UnaryExpr),
            CppSyntaxKind::BinaryExpr => CppBinaryExpr::cast(syntax).map(CppExpr::BinaryExpr),
            CppSyntaxKind::TernaryExpr => CppTernaryExpr::cast(syntax).map(CppExpr::TernaryExpr),
            CppSyntaxKind::CallExpr => CppCallExpr::cast(syntax).map(CppExpr::CallExpr),
            CppSyntaxKind::IndexExpr => CppIndexExpr::cast(syntax).map(CppExpr::IndexExpr),
            CppSyntaxKind::MemberExpr | CppSyntaxKind::ArrowExpr => {
                CppMemberExpr::cast(syntax).map(CppExpr::MemberExpr)
            }
            CppSyntaxKind::InitListExpr => {
                CppInitListExpr::cast(syntax).map(CppExpr::InitListExpr)
            }
            CppSyntaxKind::DesignatedInitExpr => {
                CppDesignatedInitExpr::cast(syntax).map(CppExpr::DesignatedInitExpr)
            }
            CppSyntaxKind::LambdaExpr | CppSyntaxKind::ClosureExpr => {
                CppLambdaExpr::cast(syntax).map(CppExpr::LambdaExpr)
            }
            CppSyntaxKind::ThisExpr => CppThisExpr::cast(syntax).map(CppExpr::ThisExpr),
            CppSyntaxKind::ErrorNode => {
                CppUnknownExpr::cast(syntax).map(CppExpr::ErrorNode)
            }
            _ => None,
        }
    }
}

/// An expression that can name something: the left-hand side of an assignment, and the target of a
/// "go to definition".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CppVarExpr {
    NameExpr(CppNameExpr),
    IndexExpr(CppIndexExpr),
    MemberExpr(CppMemberExpr),
}

impl CppAstNode for CppVarExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        match self {
            CppVarExpr::NameExpr(node) => node.syntax(),
            CppVarExpr::IndexExpr(node) => node.syntax(),
            CppVarExpr::MemberExpr(node) => node.syntax(),
        }
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::IdentifierExpr
                | CppSyntaxKind::NameExpr
                | CppSyntaxKind::IndexExpr
                | CppSyntaxKind::MemberExpr
                | CppSyntaxKind::ArrowExpr
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        match CppSyntaxKind::from(syntax.kind()) {
            CppSyntaxKind::IdentifierExpr | CppSyntaxKind::NameExpr => {
                CppNameExpr::cast(syntax).map(CppVarExpr::NameExpr)
            }
            CppSyntaxKind::IndexExpr => CppIndexExpr::cast(syntax).map(CppVarExpr::IndexExpr),
            CppSyntaxKind::MemberExpr | CppSyntaxKind::ArrowExpr => {
                CppMemberExpr::cast(syntax).map(CppVarExpr::MemberExpr)
            }
            _ => None,
        }
    }
}

impl From<CppVarExpr> for CppExpr {
    fn from(expr: CppVarExpr) -> Self {
        match expr {
            CppVarExpr::NameExpr(node) => CppExpr::NameExpr(node),
            CppVarExpr::IndexExpr(node) => CppExpr::IndexExpr(node),
            CppVarExpr::MemberExpr(node) => CppExpr::MemberExpr(node),
        }
    }
}

/// A literal: number, character, string, boolean or `nullptr`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppLiteralExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppLiteralExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::LiteralExpr
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppLiteralExpr {
    pub fn get_literal_token(&self) -> Option<CppLiteralToken> {
        self.token()
    }

    /// The literal as written, including any suffix. No interpretation: whether `1'000` is 1000 or
    /// something else is the semantic layer's business.
    pub fn get_literal_text(&self) -> Option<String> {
        self.get_literal_token()
            .map(|it| it.get_literal_text().to_string())
    }

    pub fn is_null_literal(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| {
                matches!(token.kind(), CppKind::Token(CppTokenKind::NullptrKeyword) | CppKind::Token(CppTokenKind::NullptrLiteral))
            })
    }

    pub fn is_bool_literal(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| {
                matches!(token.kind(), CppKind::Token(CppTokenKind::TrueKeyword) | CppKind::Token(CppTokenKind::FalseKeyword) | CppKind::Token(CppTokenKind::BoolLiteral))
            })
    }
}

/// A name used as an expression: `foo`, `ns::foo`, `Foo<int>`, `operator+`.
///
/// This is also the node the parser produces for a *type* name, because without a symbol table the
/// two are the same shape. A consumer that needs to tell them apart has to look at the parent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppNameExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppNameExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::IdentifierExpr | CppSyntaxKind::NameExpr
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppNameExpr {
    /// The identifier this name refers to.
    ///
    /// For a qualified name (`ns::foo`) this is the *first* identifier, and for
    /// [`CppNameExpr::get_last_name_token`] the last. Both are useful: the first is what the name
    /// starts with, the last is the entity being named.
    pub fn get_name_token(&self) -> Option<CppNameToken> {
        self.token()
    }

    pub fn get_last_name_token(&self) -> Option<CppNameToken> {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .filter_map(CppNameToken::cast)
            .last()
    }

    pub fn get_name_text(&self) -> Option<String> {
        self.get_name_token()
            .map(|it| it.get_name_text().to_string())
    }

    /// The whole name as written, without whitespace: `std::vector<int>`.
    ///
    /// Taken from the node's text rather than from its direct tokens, because a template-id carries
    /// a `TemplateArgumentList` *node*: walking tokens alone would drop `<int>` and report plain
    /// `std::vector` for a name that is not plain at all.
    pub fn get_qualified_name(&self) -> String {
        self.syntax()
            .text()
            .to_string()
            .chars()
            .filter(|it| !it.is_whitespace())
            .collect()
    }

    /// Is this a qualified name (`ns::foo`)?
    pub fn is_qualified(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == CppKind::Token(CppTokenKind::Scope))
    }

    /// The template argument lists in this name, for `Foo<int>`.
    pub fn get_template_arg_lists(&self) -> CppAstChildren<super::CppTemplateArgList> {
        self.children()
    }
}

/// A parenthesized expression. Also what the parser produces for an `if`/`while`/`switch`
/// condition, so check the parent before assuming it is an expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppParenExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppParenExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ParenExpr
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppParenExpr {
    /// The expression inside the parentheses.
    pub fn get_expr(&self) -> Option<CppExpr> {
        self.child()
    }

    /// A declaration inside the parentheses, for `if (Foo* p = f())`.
    pub fn get_declaration(&self) -> Option<CppDeclaration> {
        self.child()
    }
}

/// A unary expression: `-x`, `!flag`, `*ptr`, `&var`, `++i`, `i++`.
///
/// Prefix and postfix forms share this type; `is_prefix` tells them apart, which matters because
/// only the prefix form has the operator before its operand in the token stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppUnaryExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppUnaryExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::UnaryExpr
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppUnaryExpr {
    pub fn get_operator_token(&self) -> Option<CppOperatorToken> {
        self.token()
    }

    pub fn get_operator_text(&self) -> Option<String> {
        self.get_operator_token()
            .map(|it| it.get_operator_text().to_string())
    }

    /// The operand.
    pub fn get_operand(&self) -> Option<CppExpr> {
        self.child()
    }

    /// Is the operator written before its operand?
    pub fn is_prefix(&self) -> bool {
        let Some(operator) = self
            .syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|token| !crate::syntax::node::traits::is_trivia(token.kind().into()))
        else {
            return true;
        };
        let Some(operand) = self.get_operand() else {
            return true;
        };
        operator.text_range().start() < operand.syntax().text_range().start()
    }
}

/// A binary expression: `a + b`, `x == y`, `a && b`, and the member-access forms.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppBinaryExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppBinaryExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::BinaryExpr
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppBinaryExpr {
    /// The left operand.
    pub fn get_lhs(&self) -> Option<CppExpr> {
        self.children().next()
    }

    /// The right operand.
    pub fn get_rhs(&self) -> Option<CppExpr> {
        self.children().nth(1)
    }

    pub fn get_operator_token(&self) -> Option<CppOperatorToken> {
        self.token()
    }

    pub fn get_operator_text(&self) -> Option<String> {
        self.get_operator_token()
            .map(|it| it.get_operator_text().to_string())
    }
}

/// A conditional expression: `cond ? a : b`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppTernaryExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppTernaryExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::TernaryExpr
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppTernaryExpr {
    pub fn get_condition(&self) -> Option<CppExpr> {
        self.children().next()
    }

    pub fn get_then_expr(&self) -> Option<CppExpr> {
        self.children().nth(1)
    }

    pub fn get_else_expr(&self) -> Option<CppExpr> {
        self.children().nth(2)
    }
}

/// A function call: `f(x)`, `obj.method(x)`, `T{}`.
///
/// The callee is the first child expression, which is what a "go to definition" on a call site has
/// to resolve.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppCallExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppCallExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::CallExpr
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppCallExpr {
    /// What is being called.
    pub fn get_callee(&self) -> Option<CppExpr> {
        self.children().next()
    }

    /// The argument list, if the parser wrapped the arguments in one.
    pub fn get_arg_list(&self) -> Option<CppArgList> {
        self.child()
    }

    /// The arguments, in order.
    ///
    /// The parser does not create an `ArgList` node: the arguments are direct children of the call,
    /// after the callee and between the parentheses. `get_arg_list` is kept for grammars that do
    /// wrap them, and this handles both so callers do not have to know which they are looking at.
    pub fn get_args(&self) -> Vec<CppExpr> {
        if let Some(list) = self.get_arg_list() {
            return list.get_args().collect();
        }

        self.children::<CppExpr>().skip(1).collect()
    }
}

/// An argument list: `(a, b, c)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppArgList {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppArgList {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::ArgumentList | CppSyntaxKind::CallArgList
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppArgList {
    pub fn get_args(&self) -> CppAstChildren<CppExpr> {
        self.children()
    }
}

/// An index expression: `arr[i]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppIndexExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppIndexExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::IndexExpr
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppIndexExpr {
    /// What is being indexed.
    pub fn get_object(&self) -> Option<CppExpr> {
        self.children().next()
    }

    /// The subscript.
    pub fn get_index(&self) -> Option<CppExpr> {
        self.children().nth(1)
    }
}

/// A member access: `obj.member` or `ptr->member`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppMemberExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppMemberExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::MemberExpr | CppSyntaxKind::ArrowExpr
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppMemberExpr {
    /// The object whose member is accessed.
    pub fn get_object(&self) -> Option<CppExpr> {
        self.children().next()
    }

    /// The member name.
    pub fn get_member_name(&self) -> Option<CppNameToken> {
        self.token()
    }

    /// Is this `->` rather than `.`?
    pub fn is_arrow(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == CppKind::Token(CppTokenKind::Arrow))
    }
}

/// A braced initializer list: `{1, 2, 3}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppInitListExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppInitListExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::InitListExpr
                | CppSyntaxKind::TableArrayExpr
                | CppSyntaxKind::TableEmptyExpr
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppInitListExpr {
    /// The elements, in order.
    pub fn get_elements(&self) -> CppAstChildren<CppExpr> {
        self.children()
    }

    /// The designated initializers (`.field = v`, `[i] = v`).
    pub fn get_designators(&self) -> CppAstChildren<CppDesignatedInitExpr> {
        self.children()
    }

    pub fn is_empty(&self) -> bool {
        self.get_elements().next().is_none()
    }
}

/// A designated initializer: `.field = value` or `[index] = value`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDesignatedInitExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDesignatedInitExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DesignatedInitExpr
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppDesignatedInitExpr {
    /// The field or index designator.
    pub fn get_designator(&self) -> Option<CppNameToken> {
        self.token()
    }

    /// The initializing value.
    pub fn get_value(&self) -> Option<CppExpr> {
        self.child()
    }
}

/// A lambda: `[capture](params) -> ret { body }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppLambdaExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppLambdaExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::LambdaExpr | CppSyntaxKind::ClosureExpr
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppLambdaExpr {
    pub fn get_param_list(&self) -> Option<super::CppParamList> {
        self.child()
    }

    pub fn get_body(&self) -> Option<super::CppCompoundStat> {
        self.child()
    }

    pub fn get_trailing_return_type(&self) -> Option<super::CppTypeId> {
        crate::syntax::node::traits::first_child_of_kind(self.syntax(), &[CppSyntaxKind::TrailingReturnType]).and_then(CppTypeId::cast)
    }
}

/// `this`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppThisExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppThisExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ThisExpr || kind == CppSyntaxKind::IdentifierExpr
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        // `this` is parsed as an identifier expression when it is not the whole expression, so this
        // only casts when the token really is `this`.
        let is_this = syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == CppKind::Token(CppTokenKind::ThisKeyword));
        (Self::can_cast(syntax.kind().into()) && is_this).then_some(Self { syntax })
    }
}

/// An expression the parser could not classify.
///
/// Exists so that [`CppExpr::cast`] is total over the kinds the parser can emit in expression
/// position: an `ErrorNode` is still an expression as far as an editor is concerned, and a caller
/// walking a broken file should not have to special-case it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppUnknownExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppUnknownExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ErrorNode
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

/// A `declarator` seen through an expression: the parser reuses `Declarator` inside expressions in a
/// few places (parenthesized declarators), and this lets callers reach it.
pub type CppExprDeclarator = CppDeclarator;
