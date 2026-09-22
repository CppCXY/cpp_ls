//! Statement nodes.
//!
//! Statements are the largest family of node types, and they are where a C++ parser's tolerance
//! shows: an editor sees half-written `if`s and unclosed blocks constantly, so every accessor here
//! returns `Option` and nothing asserts structure.

use crate::{
    kind::CppSyntaxKind,
    syntax::traits::{CppAstChildren, CppAstNode},
    CppSyntaxNode,
};

use super::{CppExpr, CppNameToken, CppParamList, CppParenExpr};
use crate::syntax::node::CppStat;
// ============================================================================

/// A compound statement: `{ ... }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppCompoundStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppCompoundStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::CompoundStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppCompoundStat {
    /// The statements in the block, in order.
    ///
    /// Returns the statement sum type rather than a `CppStat` wrapper, because C++ statements and
    /// declarations are the same construct: `int x;` inside a block is a `CppDeclaration`.
    pub fn get_stats(&self) -> CppAstChildren<CppStat> {
        self.children()
    }
}

/// An `if` statement, including any `else`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppIfStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppIfStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::IfStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppIfStat {
    /// The condition. It may be an expression or a declaration (`if (Foo* p = f())`).
    pub fn get_condition(&self) -> Option<CppParenExpr> {
        self.child()
    }

    /// The `then` branch.
    pub fn get_then_branch(&self) -> Option<CppStat> {
        self.children::<CppStat>().find(|stat| !matches!(CppSyntaxKind::from(stat.syntax().kind()), CppSyntaxKind::ElseStat | CppSyntaxKind::ParenExpr))
    }

    pub fn get_else_branch(&self) -> Option<CppElseStat> {
        self.child()
    }
}

/// An `else` clause.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppElseStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppElseStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(kind, CppSyntaxKind::ElseStat | CppSyntaxKind::ElseIfStat)
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppElseStat {
    /// The body of the else clause. For `else if`, this is the nested `if`.
    pub fn get_stat(&self) -> Option<CppStat> {
        self.children()
            .find(|stat: &CppStat| CppSyntaxKind::from(stat.syntax().kind()) != CppSyntaxKind::ParenExpr)
    }

    pub fn is_else_if(&self) -> bool {
        CppSyntaxKind::from(self.syntax().kind()) == CppSyntaxKind::ElseIfStat
    }
}

/// A `while` loop.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppWhileStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppWhileStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::WhileStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppWhileStat {
    pub fn get_condition(&self) -> Option<CppParenExpr> {
        self.child()
    }

    pub fn get_body(&self) -> Option<CppStat> {
        self.children()
            .find(|stat: &CppStat| CppSyntaxKind::from(stat.syntax().kind()) != CppSyntaxKind::ParenExpr)
    }
}

/// A `do ... while` loop.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDoWhileStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDoWhileStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DoWhileStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppDoWhileStat {
    pub fn get_body(&self) -> Option<CppStat> {
        self.children()
            .find(|stat: &CppStat| CppSyntaxKind::from(stat.syntax().kind()) != CppSyntaxKind::ParenExpr)
    }

    pub fn get_condition(&self) -> Option<CppParenExpr> {
        self.child()
    }
}

/// A `for` loop. Check `is_range_for` to tell the two forms apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppForStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppForStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(kind, CppSyntaxKind::ForStat | CppSyntaxKind::RangeForStat)
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppForStat {
    /// Is this `for (decl : range)`?
    pub fn is_range_for(&self) -> bool {
        CppSyntaxKind::from(self.syntax().kind()) == CppSyntaxKind::RangeForStat
    }

    pub fn get_body(&self) -> Option<CppStat> {
        self.children::<CppStat>().find(|stat| !matches!(CppSyntaxKind::from(stat.syntax().kind()), CppSyntaxKind::ParenExpr | CppSyntaxKind::Declaration | CppSyntaxKind::ExpressionStat))
    }
}

/// A `switch` statement.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppSwitchStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppSwitchStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::SwitchStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppSwitchStat {
    pub fn get_condition(&self) -> Option<CppParenExpr> {
        self.child()
    }

    /// The `case` and `default` labels.
    pub fn get_labels(&self) -> Vec<CppStat> {
        self.syntax()
            .children()
            .filter(|node| {
                matches!(CppSyntaxKind::from(node.kind()),
                    CppSyntaxKind::CaseStat | CppSyntaxKind::DefaultStat
                )
            })
            .filter_map(CppStat::cast)
            .collect()
    }
}

/// A `try` block with its handlers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppTryStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppTryStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::TryStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppTryStat {
    pub fn get_body(&self) -> Option<CppCompoundStat> {
        self.child()
    }

    pub fn get_catch_handlers(&self) -> CppAstChildren<CppCatchStat> {
        self.children()
    }
}

/// A `catch` handler.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppCatchStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppCatchStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::CatchStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppCatchStat {
    pub fn get_param_list(&self) -> Option<CppParamList> {
        self.child()
    }

    pub fn get_body(&self) -> Option<CppCompoundStat> {
        self.child()
    }
}

/// A `return` statement.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppReturnStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppReturnStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ReturnStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppReturnStat {
    /// The returned expression. `return;` has none.
    pub fn get_expr(&self) -> Option<CppExpr> {
        self.child()
    }
}

/// A `goto` statement.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppGotoStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppGotoStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::GotoStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppGotoStat {
    pub fn get_label(&self) -> Option<CppNameToken> {
        self.token()
    }
}

/// A label: `again:`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppLabelStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppLabelStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::LabelStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppLabelStat {
    pub fn get_label(&self) -> Option<CppNameToken> {
        self.token()
    }
}

/// An expression statement: `f(x);`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppExpressionStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppExpressionStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ExpressionStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppExpressionStat {
    pub fn get_expr(&self) -> Option<CppExpr> {
        self.child()
    }
}

/// A `break` or `continue` statement.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppJumpStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppJumpStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::BreakStat | CppSyntaxKind::ContinueStat
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}