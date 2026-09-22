//! The `CppStat` sum type and the AST root enum.
//!
//! In C++ a declaration *is* a statement, so [`CppStat`] includes [`CppDeclaration`]. That is not a
//! convenience: `parse_stats` genuinely produces declarations from statement position, and a sum
//! type that excluded them would force every caller to check both.

use crate::{
    kind::CppSyntaxKind,
    syntax::traits::CppAstNode,
    CppSyntaxNode,
};

use super::*;

/// Any statement, including a declaration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CppStat {
    /// A declaration in statement position: `int x = 1;`
    Declaration(CppDeclaration),
    CompoundStat(CppCompoundStat),
    IfStat(CppIfStat),
    ElseStat(CppElseStat),
    WhileStat(CppWhileStat),
    DoWhileStat(CppDoWhileStat),
    ForStat(CppForStat),
    SwitchStat(CppSwitchStat),
    CaseStat(CppCaseStat),
    DefaultStat(CppDefaultStat),
    TryStat(CppTryStat),
    ReturnStat(CppReturnStat),
    BreakStat(CppJumpStat),
    ContinueStat(CppJumpStat),
    GotoStat(CppGotoStat),
    LabelStat(CppLabelStat),
    ThrowStat(CppThrowStat),
    ExpressionStat(CppExpressionStat),
    EmptyStat(CppEmptyStat),
    /// A directive: not a C++ construct, but it sits in statement position.
    PreprocessorDirective(CppPreprocessorDirective),
    ModuleDecl(CppModuleDecl),
    ImportDecl(CppImportDecl),
    ExportBlock(CppExportBlock),
    /// Anything the parser could not classify.
    ErrorNode(CppUnknownStat),
}

impl CppAstNode for CppStat {
    fn syntax(&self) -> &CppSyntaxNode {
        match self {
            CppStat::Declaration(node) => node.syntax(),
            CppStat::CompoundStat(node) => node.syntax(),
            CppStat::IfStat(node) => node.syntax(),
            CppStat::ElseStat(node) => node.syntax(),
            CppStat::WhileStat(node) => node.syntax(),
            CppStat::DoWhileStat(node) => node.syntax(),
            CppStat::ForStat(node) => node.syntax(),
            CppStat::SwitchStat(node) => node.syntax(),
            CppStat::CaseStat(node) => node.syntax(),
            CppStat::DefaultStat(node) => node.syntax(),
            CppStat::TryStat(node) => node.syntax(),
            CppStat::ReturnStat(node) => node.syntax(),
            CppStat::BreakStat(node) => node.syntax(),
            CppStat::ContinueStat(node) => node.syntax(),
            CppStat::GotoStat(node) => node.syntax(),
            CppStat::LabelStat(node) => node.syntax(),
            CppStat::ThrowStat(node) => node.syntax(),
            CppStat::ExpressionStat(node) => node.syntax(),
            CppStat::EmptyStat(node) => node.syntax(),
            CppStat::PreprocessorDirective(node) => node.syntax(),
            CppStat::ModuleDecl(node) => node.syntax(),
            CppStat::ImportDecl(node) => node.syntax(),
            CppStat::ExportBlock(node) => node.syntax(),
            CppStat::ErrorNode(node) => node.syntax(),
        }
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::Declaration
                | CppSyntaxKind::CompoundStat
                | CppSyntaxKind::IfStat
                | CppSyntaxKind::ElseStat
                | CppSyntaxKind::ElseIfStat
                | CppSyntaxKind::WhileStat
                | CppSyntaxKind::DoWhileStat
                | CppSyntaxKind::ForStat
                | CppSyntaxKind::RangeForStat
                | CppSyntaxKind::SwitchStat
                | CppSyntaxKind::CaseStat
                | CppSyntaxKind::DefaultStat
                | CppSyntaxKind::TryStat
                | CppSyntaxKind::ReturnStat
                | CppSyntaxKind::BreakStat
                | CppSyntaxKind::ContinueStat
                | CppSyntaxKind::GotoStat
                | CppSyntaxKind::LabelStat
                | CppSyntaxKind::ThrowStat
                | CppSyntaxKind::ExpressionStat
                | CppSyntaxKind::EmptyStat
                | CppSyntaxKind::PreprocessorDirective
                | CppSyntaxKind::ModuleDecl
                | CppSyntaxKind::ImportDecl
                | CppSyntaxKind::ExportBlock
                | CppSyntaxKind::ErrorNode
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        match CppSyntaxKind::from(syntax.kind()) {
            CppSyntaxKind::Declaration => {
                CppDeclaration::cast(syntax).map(CppStat::Declaration)
            }
            CppSyntaxKind::CompoundStat => {
                CppCompoundStat::cast(syntax).map(CppStat::CompoundStat)
            }
            CppSyntaxKind::IfStat => CppIfStat::cast(syntax).map(CppStat::IfStat),
            CppSyntaxKind::ElseStat | CppSyntaxKind::ElseIfStat => {
                CppElseStat::cast(syntax).map(CppStat::ElseStat)
            }
            CppSyntaxKind::WhileStat => CppWhileStat::cast(syntax).map(CppStat::WhileStat),
            CppSyntaxKind::DoWhileStat => {
                CppDoWhileStat::cast(syntax).map(CppStat::DoWhileStat)
            }
            CppSyntaxKind::ForStat | CppSyntaxKind::RangeForStat => {
                CppForStat::cast(syntax).map(CppStat::ForStat)
            }
            CppSyntaxKind::SwitchStat => CppSwitchStat::cast(syntax).map(CppStat::SwitchStat),
            CppSyntaxKind::CaseStat => CppCaseStat::cast(syntax).map(CppStat::CaseStat),
            CppSyntaxKind::DefaultStat => {
                CppDefaultStat::cast(syntax).map(CppStat::DefaultStat)
            }
            CppSyntaxKind::TryStat => CppTryStat::cast(syntax).map(CppStat::TryStat),
            CppSyntaxKind::ReturnStat => CppReturnStat::cast(syntax).map(CppStat::ReturnStat),
            CppSyntaxKind::BreakStat => CppJumpStat::cast(syntax).map(CppStat::BreakStat),
            CppSyntaxKind::ContinueStat => CppJumpStat::cast(syntax).map(CppStat::ContinueStat),
            CppSyntaxKind::GotoStat => CppGotoStat::cast(syntax).map(CppStat::GotoStat),
            CppSyntaxKind::LabelStat => CppLabelStat::cast(syntax).map(CppStat::LabelStat),
            CppSyntaxKind::ThrowStat => CppThrowStat::cast(syntax).map(CppStat::ThrowStat),
            CppSyntaxKind::ExpressionStat => {
                CppExpressionStat::cast(syntax).map(CppStat::ExpressionStat)
            }
            CppSyntaxKind::EmptyStat => CppEmptyStat::cast(syntax).map(CppStat::EmptyStat),
            CppSyntaxKind::PreprocessorDirective => {
                CppPreprocessorDirective::cast(syntax).map(CppStat::PreprocessorDirective)
            }
            CppSyntaxKind::ModuleDecl => CppModuleDecl::cast(syntax).map(CppStat::ModuleDecl),
            CppSyntaxKind::ImportDecl => CppImportDecl::cast(syntax).map(CppStat::ImportDecl),
            CppSyntaxKind::ExportBlock => CppExportBlock::cast(syntax).map(CppStat::ExportBlock),
            CppSyntaxKind::ErrorNode => CppUnknownStat::cast(syntax).map(CppStat::ErrorNode),
            _ => None,
        }
    }
}

/// Every node type in the tree, for callers that want to dispatch on what a node is without knowing
/// where in the grammar it came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CppAst {
    TranslationUnit(CppTranslationUnit),
    Declaration(CppDeclaration),
    Stat(CppStat),
    Expr(CppExpr),
    TemplateDecl(CppTemplateDecl),
    ParamList(CppParamList),
    ArgList(CppArgList),
    TypeId(CppTypeId),
    DeclSpecifierSeq(CppDeclSpecifierSeq),
    Declarator(CppDeclarator),
    ClassDef(CppClassDef),
    EnumDef(CppEnumDef),
    NamespaceDecl(CppNamespaceDecl),
    PreprocessorDirective(CppPreprocessorDirective),
}

impl CppAstNode for CppAst {
    fn syntax(&self) -> &CppSyntaxNode {
        match self {
            CppAst::TranslationUnit(node) => node.syntax(),
            CppAst::Declaration(node) => node.syntax(),
            CppAst::Stat(node) => node.syntax(),
            CppAst::Expr(node) => node.syntax(),
            CppAst::TemplateDecl(node) => node.syntax(),
            CppAst::ParamList(node) => node.syntax(),
            CppAst::ArgList(node) => node.syntax(),
            CppAst::TypeId(node) => node.syntax(),
            CppAst::DeclSpecifierSeq(node) => node.syntax(),
            CppAst::Declarator(node) => node.syntax(),
            CppAst::ClassDef(node) => node.syntax(),
            CppAst::EnumDef(node) => node.syntax(),
            CppAst::NamespaceDecl(node) => node.syntax(),
            CppAst::PreprocessorDirective(node) => node.syntax(),
        }
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        CppTranslationUnit::can_cast(kind)
            || CppDeclaration::can_cast(kind)
            || CppStat::can_cast(kind)
            || CppExpr::can_cast(kind)
            || CppTemplateDecl::can_cast(kind)
            || CppParamList::can_cast(kind)
            || CppArgList::can_cast(kind)
            || CppTypeId::can_cast(kind)
            || CppDeclSpecifierSeq::can_cast(kind)
            || CppDeclarator::can_cast(kind)
            || CppClassDef::can_cast(kind)
            || CppEnumDef::can_cast(kind)
            || CppNamespaceDecl::can_cast(kind)
            || CppPreprocessorDirective::can_cast(kind)
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        // Order matters where kinds overlap: the most specific wrapper wins, so a `Declaration`
        // does not come back as the generic `CppStat`.
        if let Some(node) = CppTranslationUnit::cast(syntax.clone()) {
            return Some(CppAst::TranslationUnit(node));
        }
        if let Some(node) = CppDeclaration::cast(syntax.clone()) {
            return Some(CppAst::Declaration(node));
        }
        if let Some(node) = CppTemplateDecl::cast(syntax.clone()) {
            return Some(CppAst::TemplateDecl(node));
        }
        if let Some(node) = CppParamList::cast(syntax.clone()) {
            return Some(CppAst::ParamList(node));
        }
        if let Some(node) = CppArgList::cast(syntax.clone()) {
            return Some(CppAst::ArgList(node));
        }
        if let Some(node) = CppTypeId::cast(syntax.clone()) {
            return Some(CppAst::TypeId(node));
        }
        if let Some(node) = CppDeclSpecifierSeq::cast(syntax.clone()) {
            return Some(CppAst::DeclSpecifierSeq(node));
        }
        if let Some(node) = CppDeclarator::cast(syntax.clone()) {
            return Some(CppAst::Declarator(node));
        }
        if let Some(node) = CppClassDef::cast(syntax.clone()) {
            return Some(CppAst::ClassDef(node));
        }
        if let Some(node) = CppEnumDef::cast(syntax.clone()) {
            return Some(CppAst::EnumDef(node));
        }
        if let Some(node) = CppNamespaceDecl::cast(syntax.clone()) {
            return Some(CppAst::NamespaceDecl(node));
        }
        if let Some(node) = CppPreprocessorDirective::cast(syntax.clone()) {
            return Some(CppAst::PreprocessorDirective(node));
        }
        if let Some(node) = CppExpr::cast(syntax.clone()) {
            return Some(CppAst::Expr(node));
        }
        CppStat::cast(syntax).map(CppAst::Stat)
    }
}

/// One `case` label, including the statements that follow it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppCaseStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppCaseStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::CaseStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppCaseStat {
    /// The case value, or values for a range `case 1 ... 5:`.
    pub fn get_exprs(&self) -> CppAstChildren<CppExpr> {
        self.children()
    }
}

/// The `default:` label.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDefaultStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDefaultStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DefaultStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

/// A `throw` statement.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppThrowStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppThrowStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ThrowStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppThrowStat {
    pub fn get_expr(&self) -> Option<CppExpr> {
        self.child()
    }
}

/// An empty statement: `;`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppEmptyStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppEmptyStat {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::EmptyStat
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

/// A statement the parser could not classify.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppUnknownStat {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppUnknownStat {
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
