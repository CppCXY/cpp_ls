mod doc;
mod lua;
mod test;
mod token;

#[allow(unused)]
pub use doc::*;
#[allow(unused)]
pub use lua::*;
#[allow(unused)]
pub use token::*;

use crate::kind::CppSyntaxKind;

use super::{traits::LuaAstNode, LuaSyntaxNode};

#[allow(unused)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LuaAst {
    LuaChunk(LuaChunk),
    LuaBlock(LuaBlock),
    // stats
    LuaAssignStat(LuaAssignStat),
    LuaLocalStat(LuaLocalStat),
    LuaCallExprStat(LuaCallExprStat),
    LuaLabelStat(LuaLabelStat),
    LuaBreakStat(LuaBreakStat),
    LuaGotoStat(LuaGotoStat),
    LuaDoStat(LuaDoStat),
    LuaWhileStat(LuaWhileStat),
    LuaRepeatStat(LuaRepeatStat),
    LuaIfStat(LuaIfStat),
    LuaForStat(LuaForStat),
    LuaForRangeStat(LuaForRangeStat),
    LuaFuncStat(LuaFuncStat),
    LuaLocalFuncStat(LuaLocalFuncStat),
    LuaReturnStat(LuaReturnStat),

    // exprs
    LuaNameExpr(LuaNameExpr),
    LuaIndexExpr(LuaIndexExpr),
    LuaTableExpr(LuaTableExpr),
    LuaBinaryExpr(LuaBinaryExpr),
    LuaUnaryExpr(LuaUnaryExpr),
    LuaParenExpr(LuaParenExpr),
    LuaCallExpr(LuaCallExpr),
    LuaLiteralExpr(LuaLiteralExpr),
    LuaClosureExpr(LuaClosureExpr),

    // other lua struct
    LuaTableField(LuaTableField),
    LuaParamList(LuaParamList),
    LuaParamName(LuaParamName),
    LuaCallArgList(LuaCallArgList),
    LuaLocalName(LuaLocalName),
    LuaLocalAttribute(LuaLocalAttribute),
    LuaElseIfClauseStat(LuaElseIfClauseStat),
    LuaElseClauseStat(LuaElseClauseStat),

    // comment
    LuaComment(LuaComment),
    // doc tag
    LuaDocTagClass(LuaDocTagClass),
    LuaDocTagEnum(LuaDocTagEnum),
    LuaDocTagAlias(LuaDocTagAlias),
    LuaDocTagType(LuaDocTagType),
    LuaDocTagParam(LuaDocTagParam),
    LuaDocTagReturn(LuaDocTagReturn),
    LuaDocTagOverload(LuaDocTagOverload),
    LuaDocTagField(LuaDocTagField),
    LuaDocTagModule(LuaDocTagModule),
    LuaDocTagSee(LuaDocTagSee),
    LuaDocTagDiagnostic(LuaDocTagDiagnostic),
    LuaDocTagDeprecated(LuaDocTagDeprecated),
    LuaDocTagVersion(LuaDocTagVersion),
    LuaDocTagCast(LuaDocTagCast),
    LuaDocTagSource(LuaDocTagSource),
    LuaDocTagOther(LuaDocTagOther),
    LuaDocTagNamespace(LuaDocTagNamespace),
    LuaDocTagUsing(LuaDocTagUsing),
    LuaDocTagMeta(LuaDocTagMeta),
    LuaDocTagNodiscard(LuaDocTagNodiscard),
    LuaDocTagReadonly(LuaDocTagReadonly),
    LuaDocTagOperator(LuaDocTagOperator),
    LuaDocTagGeneric(LuaDocTagGeneric),
    LuaDocTagAsync(LuaDocTagAsync),
    LuaDocTagAs(LuaDocTagAs),
    LuaDocTagReturnCast(LuaDocTagReturnCast),

    // doc type
    LuaDocNameType(LuaDocNameType),
    LuaDocArrayType(LuaDocArrayType),
    LuaDocFuncType(LuaDocFuncType),
    LuaDocObjectType(LuaDocObjectType),
    LuaDocBinaryType(LuaDocBinaryType),
    LuaDocUnaryType(LuaDocUnaryType),
    LuaDocConditionalType(LuaDocConditionalType),
    LuaDocTupleType(LuaDocTupleType),
    LuaDocLiteralType(LuaDocLiteralType),
    LuaDocVariadicType(LuaDocVariadicType),
    LuaDocNullableType(LuaDocNullableType),
    LuaDocGenericType(LuaDocGenericType),
    LuaDocStrTplType(LuaDocStrTplType),
    LuaDocMultiLineUnionType(LuaDocMultiLineUnionType),
    // other structure do not need enum here
}

impl LuaAstNode for LuaAst {
    fn syntax(&self) -> &LuaSyntaxNode {
        match self {
            LuaAst::LuaChunk(node) => node.syntax(),
            LuaAst::LuaBlock(node) => node.syntax(),
            LuaAst::LuaAssignStat(node) => node.syntax(),
            LuaAst::LuaLocalStat(node) => node.syntax(),
            LuaAst::LuaCallExprStat(node) => node.syntax(),
            LuaAst::LuaLabelStat(node) => node.syntax(),
            LuaAst::LuaBreakStat(node) => node.syntax(),
            LuaAst::LuaGotoStat(node) => node.syntax(),
            LuaAst::LuaDoStat(node) => node.syntax(),
            LuaAst::LuaWhileStat(node) => node.syntax(),
            LuaAst::LuaRepeatStat(node) => node.syntax(),
            LuaAst::LuaIfStat(node) => node.syntax(),
            LuaAst::LuaForStat(node) => node.syntax(),
            LuaAst::LuaForRangeStat(node) => node.syntax(),
            LuaAst::LuaFuncStat(node) => node.syntax(),
            LuaAst::LuaLocalFuncStat(node) => node.syntax(),
            LuaAst::LuaReturnStat(node) => node.syntax(),
            LuaAst::LuaNameExpr(node) => node.syntax(),
            LuaAst::LuaIndexExpr(node) => node.syntax(),
            LuaAst::LuaTableExpr(node) => node.syntax(),
            LuaAst::LuaBinaryExpr(node) => node.syntax(),
            LuaAst::LuaUnaryExpr(node) => node.syntax(),
            LuaAst::LuaParenExpr(node) => node.syntax(),
            LuaAst::LuaCallExpr(node) => node.syntax(),
            LuaAst::LuaLiteralExpr(node) => node.syntax(),
            LuaAst::LuaClosureExpr(node) => node.syntax(),
            LuaAst::LuaComment(node) => node.syntax(),
            LuaAst::LuaTableField(node) => node.syntax(),
            LuaAst::LuaParamList(node) => node.syntax(),
            LuaAst::LuaParamName(node) => node.syntax(),
            LuaAst::LuaCallArgList(node) => node.syntax(),
            LuaAst::LuaLocalName(node) => node.syntax(),
            LuaAst::LuaLocalAttribute(node) => node.syntax(),
            LuaAst::LuaElseIfClauseStat(node) => node.syntax(),
            LuaAst::LuaElseClauseStat(node) => node.syntax(),
            LuaAst::LuaDocTagClass(node) => node.syntax(),
            LuaAst::LuaDocTagEnum(node) => node.syntax(),
            LuaAst::LuaDocTagAlias(node) => node.syntax(),
            LuaAst::LuaDocTagType(node) => node.syntax(),
            LuaAst::LuaDocTagParam(node) => node.syntax(),
            LuaAst::LuaDocTagReturn(node) => node.syntax(),
            LuaAst::LuaDocTagOverload(node) => node.syntax(),
            LuaAst::LuaDocTagField(node) => node.syntax(),
            LuaAst::LuaDocTagModule(node) => node.syntax(),
            LuaAst::LuaDocTagSee(node) => node.syntax(),
            LuaAst::LuaDocTagDiagnostic(node) => node.syntax(),
            LuaAst::LuaDocTagDeprecated(node) => node.syntax(),
            LuaAst::LuaDocTagVersion(node) => node.syntax(),
            LuaAst::LuaDocTagCast(node) => node.syntax(),
            LuaAst::LuaDocTagSource(node) => node.syntax(),
            LuaAst::LuaDocTagOther(node) => node.syntax(),
            LuaAst::LuaDocTagNamespace(node) => node.syntax(),
            LuaAst::LuaDocTagUsing(node) => node.syntax(),
            LuaAst::LuaDocTagMeta(node) => node.syntax(),
            LuaAst::LuaDocTagNodiscard(node) => node.syntax(),
            LuaAst::LuaDocTagReadonly(node) => node.syntax(),
            LuaAst::LuaDocTagOperator(node) => node.syntax(),
            LuaAst::LuaDocTagGeneric(node) => node.syntax(),
            LuaAst::LuaDocTagAsync(node) => node.syntax(),
            LuaAst::LuaDocTagAs(node) => node.syntax(),
            LuaAst::LuaDocTagReturnCast(node) => node.syntax(),
            LuaAst::LuaDocNameType(node) => node.syntax(),
            LuaAst::LuaDocArrayType(node) => node.syntax(),
            LuaAst::LuaDocFuncType(node) => node.syntax(),
            LuaAst::LuaDocObjectType(node) => node.syntax(),
            LuaAst::LuaDocBinaryType(node) => node.syntax(),
            LuaAst::LuaDocUnaryType(node) => node.syntax(),
            LuaAst::LuaDocConditionalType(node) => node.syntax(),
            LuaAst::LuaDocTupleType(node) => node.syntax(),
            LuaAst::LuaDocLiteralType(node) => node.syntax(),
            LuaAst::LuaDocVariadicType(node) => node.syntax(),
            LuaAst::LuaDocNullableType(node) => node.syntax(),
            LuaAst::LuaDocGenericType(node) => node.syntax(),
            LuaAst::LuaDocStrTplType(node) => node.syntax(),
            LuaAst::LuaDocMultiLineUnionType(node) => node.syntax(),
        }
    }

    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized,
    {
        match kind {
            CppSyntaxKind::Chunk => true,
            CppSyntaxKind::Block => true,
            CppSyntaxKind::AssignStat => true,
            CppSyntaxKind::LocalStat => true,
            CppSyntaxKind::CallExprStat => true,
            CppSyntaxKind::LabelStat => true,
            CppSyntaxKind::BreakStat => true,
            CppSyntaxKind::GotoStat => true,
            CppSyntaxKind::DoStat => true,
            CppSyntaxKind::WhileStat => true,
            CppSyntaxKind::RepeatStat => true,
            CppSyntaxKind::IfStat => true,
            CppSyntaxKind::ForStat => true,
            CppSyntaxKind::ForRangeStat => true,
            CppSyntaxKind::FuncStat => true,
            CppSyntaxKind::LocalFuncStat => true,
            CppSyntaxKind::ReturnStat => true,
            CppSyntaxKind::NameExpr => true,
            CppSyntaxKind::IndexExpr => true,
            CppSyntaxKind::TableEmptyExpr
            | CppSyntaxKind::TableArrayExpr
            | CppSyntaxKind::TableObjectExpr => true,
            CppSyntaxKind::BinaryExpr => true,
            CppSyntaxKind::UnaryExpr => true,
            CppSyntaxKind::ParenExpr => true,
            CppSyntaxKind::CallExpr
            | CppSyntaxKind::AssertCallExpr
            | CppSyntaxKind::ErrorCallExpr
            | CppSyntaxKind::RequireCallExpr
            | CppSyntaxKind::TypeCallExpr
            | CppSyntaxKind::SetmetatableCallExpr => true,
            CppSyntaxKind::LiteralExpr => true,
            CppSyntaxKind::ClosureExpr => true,
            CppSyntaxKind::ParamList => true,
            CppSyntaxKind::CallArgList => true,
            CppSyntaxKind::LocalName => true,
            CppSyntaxKind::TableFieldAssign | CppSyntaxKind::TableFieldValue => true,
            CppSyntaxKind::ParamName => true,
            CppSyntaxKind::Attribute => true,
            CppSyntaxKind::ElseIfClauseStat => true,
            CppSyntaxKind::ElseClauseStat => true,
            CppSyntaxKind::Comment => true,
            CppSyntaxKind::DocTagClass => true,
            CppSyntaxKind::DocTagEnum => true,
            CppSyntaxKind::DocTagAlias => true,
            CppSyntaxKind::DocTagType => true,
            CppSyntaxKind::DocTagParam => true,
            CppSyntaxKind::DocTagReturn => true,
            CppSyntaxKind::DocTagOverload => true,
            CppSyntaxKind::DocTagField => true,
            CppSyntaxKind::DocTagModule => true,
            CppSyntaxKind::DocTagSee => true,
            CppSyntaxKind::DocTagDiagnostic => true,
            CppSyntaxKind::DocTagDeprecated => true,
            CppSyntaxKind::DocTagVersion => true,
            CppSyntaxKind::DocTagCast => true,
            CppSyntaxKind::DocTagSource => true,
            CppSyntaxKind::DocTagOther => true,
            CppSyntaxKind::DocTagNamespace => true,
            CppSyntaxKind::DocTagUsing => true,
            CppSyntaxKind::DocTagMeta => true,
            CppSyntaxKind::DocTagNodiscard => true,
            CppSyntaxKind::DocTagReadonly => true,
            CppSyntaxKind::DocTagOperator => true,
            CppSyntaxKind::DocTagGeneric => true,
            CppSyntaxKind::DocTagAsync => true,
            CppSyntaxKind::DocTagAs => true,
            CppSyntaxKind::DocTagReturnCast => true,
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
            CppSyntaxKind::Chunk => LuaChunk::cast(syntax).map(LuaAst::LuaChunk),
            CppSyntaxKind::Block => LuaBlock::cast(syntax).map(LuaAst::LuaBlock),
            CppSyntaxKind::AssignStat => LuaAssignStat::cast(syntax).map(LuaAst::LuaAssignStat),
            CppSyntaxKind::LocalStat => LuaLocalStat::cast(syntax).map(LuaAst::LuaLocalStat),
            CppSyntaxKind::CallExprStat => {
                LuaCallExprStat::cast(syntax).map(LuaAst::LuaCallExprStat)
            }
            CppSyntaxKind::LabelStat => LuaLabelStat::cast(syntax).map(LuaAst::LuaLabelStat),
            CppSyntaxKind::BreakStat => LuaBreakStat::cast(syntax).map(LuaAst::LuaBreakStat),
            CppSyntaxKind::GotoStat => LuaGotoStat::cast(syntax).map(LuaAst::LuaGotoStat),
            CppSyntaxKind::DoStat => LuaDoStat::cast(syntax).map(LuaAst::LuaDoStat),
            CppSyntaxKind::WhileStat => LuaWhileStat::cast(syntax).map(LuaAst::LuaWhileStat),
            CppSyntaxKind::RepeatStat => LuaRepeatStat::cast(syntax).map(LuaAst::LuaRepeatStat),
            CppSyntaxKind::IfStat => LuaIfStat::cast(syntax).map(LuaAst::LuaIfStat),
            CppSyntaxKind::ForStat => LuaForStat::cast(syntax).map(LuaAst::LuaForStat),
            CppSyntaxKind::ForRangeStat => {
                LuaForRangeStat::cast(syntax).map(LuaAst::LuaForRangeStat)
            }
            CppSyntaxKind::FuncStat => LuaFuncStat::cast(syntax).map(LuaAst::LuaFuncStat),
            CppSyntaxKind::LocalFuncStat => {
                LuaLocalFuncStat::cast(syntax).map(LuaAst::LuaLocalFuncStat)
            }
            CppSyntaxKind::ReturnStat => LuaReturnStat::cast(syntax).map(LuaAst::LuaReturnStat),
            CppSyntaxKind::NameExpr => LuaNameExpr::cast(syntax).map(LuaAst::LuaNameExpr),
            CppSyntaxKind::IndexExpr => LuaIndexExpr::cast(syntax).map(LuaAst::LuaIndexExpr),
            CppSyntaxKind::TableEmptyExpr
            | CppSyntaxKind::TableArrayExpr
            | CppSyntaxKind::TableObjectExpr => {
                LuaTableExpr::cast(syntax).map(LuaAst::LuaTableExpr)
            }
            CppSyntaxKind::BinaryExpr => LuaBinaryExpr::cast(syntax).map(LuaAst::LuaBinaryExpr),
            CppSyntaxKind::UnaryExpr => LuaUnaryExpr::cast(syntax).map(LuaAst::LuaUnaryExpr),
            CppSyntaxKind::ParenExpr => LuaParenExpr::cast(syntax).map(LuaAst::LuaParenExpr),
            CppSyntaxKind::CallExpr
            | CppSyntaxKind::AssertCallExpr
            | CppSyntaxKind::ErrorCallExpr
            | CppSyntaxKind::RequireCallExpr
            | CppSyntaxKind::TypeCallExpr
            | CppSyntaxKind::SetmetatableCallExpr => {
                LuaCallExpr::cast(syntax).map(LuaAst::LuaCallExpr)
            }
            CppSyntaxKind::LiteralExpr => LuaLiteralExpr::cast(syntax).map(LuaAst::LuaLiteralExpr),
            CppSyntaxKind::ClosureExpr => LuaClosureExpr::cast(syntax).map(LuaAst::LuaClosureExpr),
            CppSyntaxKind::Comment => LuaComment::cast(syntax).map(LuaAst::LuaComment),
            CppSyntaxKind::TableFieldAssign | CppSyntaxKind::TableFieldValue => {
                LuaTableField::cast(syntax).map(LuaAst::LuaTableField)
            }
            CppSyntaxKind::ParamList => LuaParamList::cast(syntax).map(LuaAst::LuaParamList),
            CppSyntaxKind::ParamName => LuaParamName::cast(syntax).map(LuaAst::LuaParamName),
            CppSyntaxKind::CallArgList => LuaCallArgList::cast(syntax).map(LuaAst::LuaCallArgList),
            CppSyntaxKind::LocalName => LuaLocalName::cast(syntax).map(LuaAst::LuaLocalName),
            CppSyntaxKind::Attribute => {
                LuaLocalAttribute::cast(syntax).map(LuaAst::LuaLocalAttribute)
            }
            CppSyntaxKind::ElseIfClauseStat => {
                LuaElseIfClauseStat::cast(syntax).map(LuaAst::LuaElseIfClauseStat)
            }
            CppSyntaxKind::ElseClauseStat => {
                LuaElseClauseStat::cast(syntax).map(LuaAst::LuaElseClauseStat)
            }
            CppSyntaxKind::DocTagClass => LuaDocTagClass::cast(syntax).map(LuaAst::LuaDocTagClass),
            CppSyntaxKind::DocTagEnum => LuaDocTagEnum::cast(syntax).map(LuaAst::LuaDocTagEnum),
            CppSyntaxKind::DocTagAlias => LuaDocTagAlias::cast(syntax).map(LuaAst::LuaDocTagAlias),
            CppSyntaxKind::DocTagType => LuaDocTagType::cast(syntax).map(LuaAst::LuaDocTagType),
            CppSyntaxKind::DocTagParam => LuaDocTagParam::cast(syntax).map(LuaAst::LuaDocTagParam),
            CppSyntaxKind::DocTagReturn => {
                LuaDocTagReturn::cast(syntax).map(LuaAst::LuaDocTagReturn)
            }
            CppSyntaxKind::DocTagOverload => {
                LuaDocTagOverload::cast(syntax).map(LuaAst::LuaDocTagOverload)
            }
            CppSyntaxKind::DocTagField => LuaDocTagField::cast(syntax).map(LuaAst::LuaDocTagField),
            CppSyntaxKind::DocTagModule => {
                LuaDocTagModule::cast(syntax).map(LuaAst::LuaDocTagModule)
            }
            CppSyntaxKind::DocTagSee => LuaDocTagSee::cast(syntax).map(LuaAst::LuaDocTagSee),
            CppSyntaxKind::DocTagDiagnostic => {
                LuaDocTagDiagnostic::cast(syntax).map(LuaAst::LuaDocTagDiagnostic)
            }
            CppSyntaxKind::DocTagDeprecated => {
                LuaDocTagDeprecated::cast(syntax).map(LuaAst::LuaDocTagDeprecated)
            }
            CppSyntaxKind::DocTagVersion => {
                LuaDocTagVersion::cast(syntax).map(LuaAst::LuaDocTagVersion)
            }
            CppSyntaxKind::DocTagCast => LuaDocTagCast::cast(syntax).map(LuaAst::LuaDocTagCast),
            CppSyntaxKind::DocTagSource => {
                LuaDocTagSource::cast(syntax).map(LuaAst::LuaDocTagSource)
            }
            CppSyntaxKind::DocTagOther => LuaDocTagOther::cast(syntax).map(LuaAst::LuaDocTagOther),
            CppSyntaxKind::DocTagNamespace => {
                LuaDocTagNamespace::cast(syntax).map(LuaAst::LuaDocTagNamespace)
            }
            CppSyntaxKind::DocTagUsing => LuaDocTagUsing::cast(syntax).map(LuaAst::LuaDocTagUsing),
            CppSyntaxKind::DocTagMeta => LuaDocTagMeta::cast(syntax).map(LuaAst::LuaDocTagMeta),
            CppSyntaxKind::DocTagNodiscard => {
                LuaDocTagNodiscard::cast(syntax).map(LuaAst::LuaDocTagNodiscard)
            }
            CppSyntaxKind::DocTagReadonly => {
                LuaDocTagReadonly::cast(syntax).map(LuaAst::LuaDocTagReadonly)
            }
            CppSyntaxKind::DocTagOperator => {
                LuaDocTagOperator::cast(syntax).map(LuaAst::LuaDocTagOperator)
            }
            CppSyntaxKind::DocTagGeneric => {
                LuaDocTagGeneric::cast(syntax).map(LuaAst::LuaDocTagGeneric)
            }
            CppSyntaxKind::DocTagAsync => LuaDocTagAsync::cast(syntax).map(LuaAst::LuaDocTagAsync),
            CppSyntaxKind::DocTagAs => LuaDocTagAs::cast(syntax).map(LuaAst::LuaDocTagAs),
            CppSyntaxKind::DocTagReturnCast => {
                LuaDocTagReturnCast::cast(syntax).map(LuaAst::LuaDocTagReturnCast)
            }
            CppSyntaxKind::TypeName => LuaDocNameType::cast(syntax).map(LuaAst::LuaDocNameType),
            CppSyntaxKind::TypeArray => LuaDocArrayType::cast(syntax).map(LuaAst::LuaDocArrayType),
            CppSyntaxKind::TypeFun => LuaDocFuncType::cast(syntax).map(LuaAst::LuaDocFuncType),
            CppSyntaxKind::TypeObject => {
                LuaDocObjectType::cast(syntax).map(LuaAst::LuaDocObjectType)
            }
            CppSyntaxKind::TypeBinary => {
                LuaDocBinaryType::cast(syntax).map(LuaAst::LuaDocBinaryType)
            }
            CppSyntaxKind::TypeUnary => LuaDocUnaryType::cast(syntax).map(LuaAst::LuaDocUnaryType),
            CppSyntaxKind::TypeConditional => {
                LuaDocConditionalType::cast(syntax).map(LuaAst::LuaDocConditionalType)
            }
            CppSyntaxKind::TypeTuple => LuaDocTupleType::cast(syntax).map(LuaAst::LuaDocTupleType),
            CppSyntaxKind::TypeLiteral => {
                LuaDocLiteralType::cast(syntax).map(LuaAst::LuaDocLiteralType)
            }
            CppSyntaxKind::TypeVariadic => {
                LuaDocVariadicType::cast(syntax).map(LuaAst::LuaDocVariadicType)
            }
            CppSyntaxKind::TypeNullable => {
                LuaDocNullableType::cast(syntax).map(LuaAst::LuaDocNullableType)
            }
            CppSyntaxKind::TypeGeneric => {
                LuaDocGenericType::cast(syntax).map(LuaAst::LuaDocGenericType)
            }
            CppSyntaxKind::TypeStringTemplate => {
                LuaDocStrTplType::cast(syntax).map(LuaAst::LuaDocStrTplType)
            }
            CppSyntaxKind::TypeMultiLineUnion => {
                LuaDocMultiLineUnionType::cast(syntax).map(LuaAst::LuaDocMultiLineUnionType)
            }
            _ => None,
        }
    }
}
