use crate::{
    grammar::ParseResult,
    kind::{CppSyntaxKind, CppTokenKind},
    lexer::LuaDocLexerState,
    parser::{CompleteMarker, LuaDocParser, MarkerEventContainer},
    parser_error::LuaParseError,
};

use super::{
    expect_token, if_token_bump, parse_description,
    types::{parse_fun_type, parse_type, parse_type_list},
};

pub fn parse_tag(p: &mut LuaDocParser) {
    let level = p.get_mark_level();
    match parse_tag_detail(p) {
        Ok(_) => {}
        Err(error) => {
            p.push_error(error);
            let current_level = p.get_mark_level();
            for _ in 0..(current_level - level) {
                p.push_node_end();
            }
        }
    }
}

pub fn parse_long_tag(p: &mut LuaDocParser) {
    parse_tag(p);
}

fn parse_tag_detail(p: &mut LuaDocParser) -> ParseResult {
    match p.current_token() {
        // main tag
        CppTokenKind::TkTagClass | CppTokenKind::TkTagInterface => parse_tag_class(p),
        CppTokenKind::TkTagEnum => parse_tag_enum(p),
        CppTokenKind::TkTagAlias => parse_tag_alias(p),
        CppTokenKind::TkTagField => parse_tag_field(p),
        CppTokenKind::TkTagType => parse_tag_type(p),
        CppTokenKind::TkTagParam => parse_tag_param(p),
        CppTokenKind::TkTagReturn => parse_tag_return(p),
        CppTokenKind::TkTagReturnCast => parse_tag_return_cast(p),
        // other tag
        CppTokenKind::TkTagModule => parse_tag_module(p),
        CppTokenKind::TkTagSee => parse_tag_see(p),
        CppTokenKind::TkTagGeneric => parse_tag_generic(p),
        CppTokenKind::TkTagAs => parse_tag_as(p),
        CppTokenKind::TkTagOverload => parse_tag_overload(p),
        CppTokenKind::TkTagCast => parse_tag_cast(p),
        CppTokenKind::TkTagSource => parse_tag_source(p),
        CppTokenKind::TkTagDiagnostic => parse_tag_diagnostic(p),
        CppTokenKind::TkTagVersion => parse_tag_version(p),
        CppTokenKind::TkTagOperator => parse_tag_operator(p),
        CppTokenKind::TkTagMapping => parse_tag_mapping(p),
        CppTokenKind::TkTagNamespace => parse_tag_namespace(p),
        CppTokenKind::TkTagUsing => parse_tag_using(p),
        CppTokenKind::TkTagMeta => parse_tag_meta(p),

        // simple tag
        CppTokenKind::TkTagVisibility => parse_tag_simple(p, CppSyntaxKind::DocTagVisibility),
        CppTokenKind::TkTagReadonly => parse_tag_simple(p, CppSyntaxKind::DocTagReadonly),
        CppTokenKind::TkTagDeprecated => parse_tag_simple(p, CppSyntaxKind::DocTagDeprecated),
        CppTokenKind::TkTagAsync => parse_tag_simple(p, CppSyntaxKind::DocTagAsync),
        CppTokenKind::TkTagNodiscard => parse_tag_simple(p, CppSyntaxKind::DocTagNodiscard),
        CppTokenKind::TkTagOther => parse_tag_simple(p, CppSyntaxKind::DocTagOther),
        _ => Ok(CompleteMarker::empty()),
    }
}

fn parse_tag_simple(p: &mut LuaDocParser, kind: CppSyntaxKind) -> ParseResult {
    let m = p.mark(kind);
    p.bump();
    p.set_state(LuaDocLexerState::Description);
    parse_description(p);

    Ok(m.complete(p))
}

// ---@class <class name>
fn parse_tag_class(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagClass);
    p.bump();
    if p.current_token() == CppTokenKind::TkLeftParen {
        parse_tag_attribute(p)?;
    }

    expect_token(p, CppTokenKind::TkName)?;
    // TODO suffixed
    if p.current_token() == CppTokenKind::TkLt {
        parse_generic_decl_list(p, true)?;
    }

    if p.current_token() == CppTokenKind::TkColon {
        p.bump();
        parse_type_list(p)?;
    }

    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// (partial, global, local)
fn parse_tag_attribute(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocAttribute);
    p.bump();
    expect_token(p, CppTokenKind::TkName)?;
    while p.current_token() == CppTokenKind::TkComma {
        p.bump();
        expect_token(p, CppTokenKind::TkName)?;
    }

    expect_token(p, CppTokenKind::TkRightParen)?;
    Ok(m.complete(p))
}

// <T, R, C: AAA>
fn parse_generic_decl_list(p: &mut LuaDocParser, allow_angle_brackets: bool) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocGenericDeclareList);
    if allow_angle_brackets {
        expect_token(p, CppTokenKind::TkLt)?;
    }
    parse_generic_param(p)?;
    while p.current_token() == CppTokenKind::TkComma {
        p.bump();
        parse_generic_param(p)?;
    }
    if allow_angle_brackets {
        expect_token(p, CppTokenKind::TkGt)?;
    }
    Ok(m.complete(p))
}

// A : type
// A
fn parse_generic_param(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocGenericParameter);
    expect_token(p, CppTokenKind::TkName)?;
    if p.current_token() == CppTokenKind::TkColon {
        p.bump();
        parse_type(p)?;
    }
    Ok(m.complete(p))
}

// ---@enum A
// ---@enum A : number
fn parse_tag_enum(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagEnum);
    p.bump();
    if p.current_token() == CppTokenKind::TkLeftParen {
        parse_tag_attribute(p)?;
    }

    expect_token(p, CppTokenKind::TkName)?;
    if p.current_token() == CppTokenKind::TkColon {
        p.bump();
        parse_type(p)?;
    }

    if p.current_token() == CppTokenKind::TkDocContinueOr {
        parse_enum_field_list(p)?;
    }

    p.set_state(LuaDocLexerState::Description);
    parse_description(p);

    Ok(m.complete(p))
}

fn parse_enum_field_list(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocEnumFieldList);

    while p.current_token() == CppTokenKind::TkDocContinueOr {
        p.bump();
        parse_enum_field(p)?;
    }
    Ok(m.complete(p))
}

fn parse_enum_field(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocEnumField);
    if matches!(
        p.current_token(),
        CppTokenKind::TkName | CppTokenKind::TkString | CppTokenKind::TkInt
    ) {
        p.bump();
    }

    if p.current_token() == CppTokenKind::TkDocDetail {
        p.bump();
    }

    Ok(m.complete(p))
}

// ---@alias A string
// ---@alias A<T> keyof T
fn parse_tag_alias(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagAlias);
    p.bump();
    expect_token(p, CppTokenKind::TkName)?;
    if p.current_token() == CppTokenKind::TkLt {
        parse_generic_decl_list(p, true)?;
    }

    parse_type(p)?;

    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@module "aaa.bbb.ccc" force variable be "aaa.bbb.ccc"
fn parse_tag_module(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagModule);
    p.bump();

    expect_token(p, CppTokenKind::TkString)?;

    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@field aaa string
// ---@field aaa? number
// ---@field [string] number
// ---@field [1] number
fn parse_tag_field(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::FieldStart);
    let m = p.mark(CppSyntaxKind::DocTagField);
    p.bump();
    if p.current_token() == CppTokenKind::TkLeftParen {
        parse_tag_attribute(p)?;
    }

    p.set_state(LuaDocLexerState::Normal);
    if_token_bump(p, CppTokenKind::TkDocVisibility);
    match p.current_token() {
        CppTokenKind::TkName => p.bump(),
        CppTokenKind::TkLeftBracket => {
            p.bump();
            if p.current_token() == CppTokenKind::TkInt
                || p.current_token() == CppTokenKind::TkString
            {
                p.bump();
            } else {
                parse_type(p)?;
            }
            expect_token(p, CppTokenKind::TkRightBracket)?;
        }
        _ => {
            return Err(LuaParseError::doc_error_from(
                &t!(
                    "expect field name or '[', but get %{current}",
                    current = p.current_token()
                ),
                p.current_token_range(),
            ))
        }
    }
    if_token_bump(p, CppTokenKind::TkDocQuestion);
    parse_type(p)?;

    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@type string
// ---@type number, string
fn parse_tag_type(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagType);
    p.bump();
    parse_type(p)?;
    while p.current_token() == CppTokenKind::TkComma {
        p.bump();
        parse_type(p)?;
    }

    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@param a number
// ---@param a? number
// ---@param ... string
fn parse_tag_param(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagParam);
    p.bump();
    if matches!(
        p.current_token(),
        CppTokenKind::TkName | CppTokenKind::TkDots
    ) {
        p.bump();
    } else {
        return Err(LuaParseError::doc_error_from(
            &t!(
                "expect param name or '...', but get %{current}",
                current = p.current_token()
            ),
            p.current_token_range(),
        ));
    }

    if_token_bump(p, CppTokenKind::TkDocQuestion);

    parse_type(p)?;

    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@return number
// ---@return number, string
// ---@return number <name> , this just compact luals
fn parse_tag_return(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagReturn);
    p.bump();

    parse_type(p)?;

    if_token_bump(p, CppTokenKind::TkName);

    while p.current_token() == CppTokenKind::TkComma {
        p.bump();
        parse_type(p)?;
        if_token_bump(p, CppTokenKind::TkName);
    }

    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@return_cast <param name> <type>
fn parse_tag_return_cast(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagReturnCast);
    p.bump();
    expect_token(p, CppTokenKind::TkName)?;

    parse_op_type(p)?;
    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@generic T
// ---@generic T, R
// ---@generic T, R : number
fn parse_tag_generic(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagGeneric);
    p.bump();

    parse_generic_decl_list(p, false)?;

    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@see <name>
// ---@see <name>#<name>
// ---@see <any content>
fn parse_tag_see(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::See);
    let m = p.mark(CppSyntaxKind::DocTagSee);
    p.bump();
    expect_token(p, CppTokenKind::TkDocSeeContent)?;
    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@as number
// --[[@as number]]
fn parse_tag_as(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagAs);
    p.bump();
    parse_type(p)?;

    if_token_bump(p, CppTokenKind::TkLongCommentEnd);
    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@overload fun(a: number): string
// ---@overload async fun(a: number): string
fn parse_tag_overload(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagOverload);
    p.bump();
    parse_fun_type(p)?;
    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@cast a number
// ---@cast a +string
// ---@cast a -string
// ---@cast a +?
// ---@cast a +string, -number
fn parse_tag_cast(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagCast);
    p.bump();
    expect_token(p, CppTokenKind::TkName)?;

    parse_op_type(p)?;
    while p.current_token() == CppTokenKind::TkComma {
        p.bump();
        parse_op_type(p)?;
    }

    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// +<type>, -<type>, +?, <type>
fn parse_op_type(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocOpType);
    if p.current_token() == CppTokenKind::TkPlus || p.current_token() == CppTokenKind::TkMinus {
        p.bump();
        if p.current_token() == CppTokenKind::TkDocQuestion {
            p.bump();
        } else {
            parse_type(p)?;
        }
    } else {
        parse_type(p)?;
    }

    Ok(m.complete(p))
}

// ---@source <path>
// ---@source "<path>"
fn parse_tag_source(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Source);

    let m = p.mark(CppSyntaxKind::DocTagSource);
    p.bump();
    expect_token(p, CppTokenKind::TKDocPath)?;

    Ok(m.complete(p))
}

// ---@diagnostic <action>: <diagnostic-code>, ...
fn parse_tag_diagnostic(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagDiagnostic);
    p.bump();
    expect_token(p, CppTokenKind::TkName)?;
    if p.current_token() == CppTokenKind::TkColon {
        p.bump();
        parse_diagnostic_code_list(p)?;
    }

    Ok(m.complete(p))
}

fn parse_diagnostic_code_list(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocDiagnosticCodeList);
    expect_token(p, CppTokenKind::TkName)?;
    while p.current_token() == CppTokenKind::TkComma {
        p.bump();
        expect_token(p, CppTokenKind::TkName)?;
    }
    Ok(m.complete(p))
}

// ---@version Lua 5.1
// ---@version Lua JIT
// ---@version 5.1, JIT
// ---@version > Lua 5.1, Lua JIT
// ---@version > 5.1, 5.2, 5.3
fn parse_tag_version(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Version);
    let m = p.mark(CppSyntaxKind::DocTagVersion);
    p.bump();
    parse_version(p)?;
    while p.current_token() == CppTokenKind::TkComma {
        p.bump();
        parse_version(p)?;
    }
    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// 5.1
// JIT
// > 5.1
// < 5.4
// > Lua 5.1
fn parse_version(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocVersion);
    if matches!(p.current_token(), CppTokenKind::TkLt | CppTokenKind::TkGt) {
        p.bump();
    }

    if p.current_token() == CppTokenKind::TkName {
        p.bump();
    }

    expect_token(p, CppTokenKind::TkDocVersionNumber)?;
    Ok(m.complete(p))
}

// ---@operator add(number): number
// ---@operator call: number
fn parse_tag_operator(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagOperator);
    p.bump();
    expect_token(p, CppTokenKind::TkName)?;
    if p.current_token() == CppTokenKind::TkLeftParen {
        p.bump();
        parse_type_list(p)?;
        expect_token(p, CppTokenKind::TkRightParen)?;
    }

    if p.current_token() == CppTokenKind::TkColon {
        p.bump();
        parse_type(p)?;
    }

    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@mapping <new name>
fn parse_tag_mapping(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagMapping);
    p.bump();
    expect_token(p, CppTokenKind::TkName)?;
    p.set_state(LuaDocLexerState::Description);
    parse_description(p);
    Ok(m.complete(p))
}

// ---@namespace path
// ---@namespace System.Net
fn parse_tag_namespace(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagNamespace);
    p.bump();
    expect_token(p, CppTokenKind::TkName)?;
    Ok(m.complete(p))
}

// ---@using path
fn parse_tag_using(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagUsing);
    p.bump();
    expect_token(p, CppTokenKind::TkName)?;
    Ok(m.complete(p))
}

fn parse_tag_meta(p: &mut LuaDocParser) -> ParseResult {
    p.set_state(LuaDocLexerState::Normal);
    let m = p.mark(CppSyntaxKind::DocTagMeta);
    p.bump();
    if_token_bump(p, CppTokenKind::TkName);
    Ok(m.complete(p))
}
