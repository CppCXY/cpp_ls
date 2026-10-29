
/// **C11's generic selection** — `_Generic(expr, type: value, …, default: value)`.
///
/// Its own expression kind rather than an `ErrorNode`, because everything that walks expressions has to see it:
/// `CppCallExpr::get_args` filters its children through [`CppExpr::cast`], so a selection the cast did not know was
/// dropped from a call's argument list — `strlen(TYPE_NAME(x))` counted **zero** arguments for `strlen`, which takes
/// one, and `argument-count` reported five of those calls on a nine-file C corpus.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppGenericSelectionExpr {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppGenericSelectionExpr {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::GenericSelectionExpr
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}
