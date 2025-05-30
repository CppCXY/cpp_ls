use crate::{
    grammar::ParseResult,
    kind::{LuaOpKind, CppSyntaxKind, CppTokenKind, LuaTypeBinaryOperator, LuaTypeUnaryOperator},
    lexer::LuaDocLexerState,
    parser::{CompleteMarker, LuaDocParser, MarkerEventContainer},
    parser_error::LuaParseError,
};

use super::{expect_token, if_token_bump, parse_description};

pub fn parse_type(p: &mut LuaDocParser) -> ParseResult {
    if p.current_token() == CppTokenKind::TkDocContinueOr {
        return parse_multi_line_union_type(p);
    }

    let mut cm = parse_sub_type(p, 0)?;

    loop {
        match p.current_token() {
            // <type>?
            CppTokenKind::TkDocQuestion => {
                let m = cm.precede(p, CppSyntaxKind::TypeNullable);
                p.bump();
                cm = m.complete(p);
            }
            // <type> and <true type> or <false type>
            CppTokenKind::TkAnd => {
                let m = cm.precede(p, CppSyntaxKind::TypeConditional);
                p.bump();
                parse_sub_type(p, 0)?;
                expect_token(p, CppTokenKind::TkOr)?;
                parse_sub_type(p, 0)?;
                cm = m.complete(p);
                break;
            }
            CppTokenKind::TkDots => {
                // donot support  'xxx... ...'
                if matches!(cm.kind, CppSyntaxKind::TypeVariadic) {
                    break;
                }

                let m = cm.precede(p, CppSyntaxKind::TypeVariadic);
                p.bump();
                cm = m.complete(p);
                break;
            }
            _ => break,
        }
    }

    Ok(cm)
}

// <type>
// keyof <type>, -1
// <type> | <type> , <type> & <type>, <type> extends <type>, <type> in keyof <type>
fn parse_sub_type(p: &mut LuaDocParser, limit: i32) -> ParseResult {
    let uop = LuaOpKind::to_type_unary_operator(p.current_token());
    let mut cm = if uop != LuaTypeUnaryOperator::None {
        let range = p.current_token_range();
        let m = p.mark(CppSyntaxKind::TypeUnary);
        p.bump();
        match parse_sub_type(p, 0) {
            Ok(_) => {}
            Err(err) => {
                p.push_error(LuaParseError::doc_error_from(
                    &t!("unary operator not followed by type"),
                    range,
                ));
                return Err(err);
            }
        }
        m.complete(p)
    } else {
        parse_simple_type(p)?
    };

    let mut bop = LuaOpKind::to_parse_binary_operator(p.current_token());
    while bop != LuaTypeBinaryOperator::None && bop.get_priority().left > limit {
        let range = p.current_token_range();
        let m = cm.precede(p, CppSyntaxKind::TypeBinary);
        p.bump();
        if p.current_token() != CppTokenKind::TkDocQuestion {
            match parse_sub_type(p, bop.get_priority().right) {
                Ok(_) => {}
                Err(err) => {
                    p.push_error(LuaParseError::doc_error_from(
                        &t!("binary operator not followed by type"),
                        range,
                    ));

                    return Err(err);
                }
            }
        } else {
            let m2 = p.mark(CppSyntaxKind::TypeLiteral);
            p.bump();
            m2.complete(p);
        }

        cm = m.complete(p);
        bop = LuaOpKind::to_parse_binary_operator(p.current_token());
    }

    Ok(cm)
}

pub fn parse_type_list(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocTypeList);
    parse_type(p)?;
    while p.current_token() == CppTokenKind::TkComma {
        p.bump();
        parse_type(p)?;
    }
    Ok(m.complete(p))
}

fn parse_simple_type(p: &mut LuaDocParser) -> ParseResult {
    let cm = parse_primary_type(p)?;

    parse_suffixed_type(p, cm)
}

fn parse_primary_type(p: &mut LuaDocParser) -> ParseResult {
    match p.current_token() {
        CppTokenKind::TkLeftBrace => parse_object_or_mapped_type(p),
        CppTokenKind::TkLeftBracket => parse_tuple_type(p),
        CppTokenKind::TkLeftParen => parse_paren_type(p),
        CppTokenKind::TkString
        | CppTokenKind::TkInt
        | CppTokenKind::TkTrue
        | CppTokenKind::TkFalse => parse_literal_type(p),
        CppTokenKind::TkName => parse_name_or_func_type(p),
        CppTokenKind::TkStringTemplateType => parse_string_template_type(p),
        CppTokenKind::TkDots => parse_vararg_type(p),
        _ => Err(LuaParseError::doc_error_from(
            &t!("expect type"),
            p.current_token_range(),
        )),
    }
}

// { <name>: <type>, ... }
// { <name> : <type>, ... }
fn parse_object_or_mapped_type(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::TypeObject);
    p.bump();

    if p.current_token() != CppTokenKind::TkRightBrace {
        parse_typed_field(p)?;
        while p.current_token() == CppTokenKind::TkComma {
            p.bump();
            if p.current_token() == CppTokenKind::TkRightBrace {
                break;
            }
            parse_typed_field(p)?;
        }
    }

    expect_token(p, CppTokenKind::TkRightBrace)?;

    Ok(m.complete(p))
}

// <name> : <type>
// [<number>] : <type>
// [<string>] : <type>
// [<type>] : <type>
// <name>? : <type>
fn parse_typed_field(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocObjectField);
    match p.current_token() {
        CppTokenKind::TkName => {
            p.bump();
            if_token_bump(p, CppTokenKind::TkDocQuestion);
        }
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
            if_token_bump(p, CppTokenKind::TkDocQuestion);
        }
        _ => {
            return Err(LuaParseError::doc_error_from(
                &t!("expect name or [<number>] or [<string>]"),
                p.current_token_range(),
            ));
        }
    }

    if p.current_token() == CppTokenKind::TkColon {
        p.bump();
        parse_type(p)?;
    }
    Ok(m.complete(p))
}

// [ <type> , <type>  ...]
// [ string, number ]
fn parse_tuple_type(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::TypeTuple);
    p.bump();
    if p.current_token() != CppTokenKind::TkRightBracket {
        parse_type(p)?;
        while p.current_token() == CppTokenKind::TkComma {
            p.bump();
            parse_type(p)?;
        }
    }

    expect_token(p, CppTokenKind::TkRightBracket)?;
    Ok(m.complete(p))
}

// ( <type> )
fn parse_paren_type(p: &mut LuaDocParser) -> ParseResult {
    p.bump();
    let cm = parse_type(p)?;
    expect_token(p, CppTokenKind::TkRightParen)?;
    Ok(cm)
}

// <string> | <integer> | <bool>
fn parse_literal_type(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::TypeLiteral);
    p.bump();
    Ok(m.complete(p))
}

fn parse_name_or_func_type(p: &mut LuaDocParser) -> ParseResult {
    let text = p.current_token_text();
    match text {
        "fun" | "async" => parse_fun_type(p),
        _ => parse_name_type(p),
    }
}

// fun ( <name>: <type>, ... ): <type>, ...
// async fun ( <name>: <type>, ... ) <type>, ...
pub fn parse_fun_type(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::TypeFun);
    if p.current_token_text() == "async" {
        p.bump();
    }

    if p.current_token_text() != "fun" {
        return Err(LuaParseError::doc_error_from(
            &t!("expect fun"),
            p.current_token_range(),
        ));
    }

    p.bump();
    expect_token(p, CppTokenKind::TkLeftParen)?;

    if p.current_token() != CppTokenKind::TkRightParen {
        parse_typed_param(p)?;
        while p.current_token() == CppTokenKind::TkComma {
            p.bump();
            parse_typed_param(p)?;
        }
    }

    expect_token(p, CppTokenKind::TkRightParen)?;

    if p.current_token() == CppTokenKind::TkColon {
        p.bump();

        // compact luals return type (number, integer)
        parse_fun_return_list(p)?;
    }

    Ok(m.complete(p))
}

fn parse_fun_return_list(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocTypeList);
    // compact luals return type (number, integer)
    let parse_paren = if p.current_token() == CppTokenKind::TkLeftParen {
        p.bump();
        true
    } else {
        false
    };

    parse_fun_return_type(p)?;

    while p.current_token() == CppTokenKind::TkComma {
        p.bump();
        parse_fun_return_type(p)?;
    }

    if parse_paren {
        expect_token(p, CppTokenKind::TkRightParen)?;
    }

    Ok(m.complete(p))
}

fn parse_fun_return_type(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocNamedReturnType);
    let cm = parse_type(p)?;
    if cm.kind == CppSyntaxKind::TypeName && p.current_token() == CppTokenKind::TkColon {
        p.bump();
        parse_type(p)?;
    }
    Ok(m.complete(p))
}

// <name> : <type>
// ... : <type>
// <name>
// ...
fn parse_typed_param(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocTypedParameter);
    match p.current_token() {
        CppTokenKind::TkName => {
            p.bump();
            if_token_bump(p, CppTokenKind::TkDocQuestion);
        }
        CppTokenKind::TkDots => {
            p.bump();
            if_token_bump(p, CppTokenKind::TkDocQuestion);
        }
        _ => {
            return Err(LuaParseError::doc_error_from(
                &t!("expect name or ..."),
                p.current_token_range(),
            ));
        }
    }

    if p.current_token() == CppTokenKind::TkColon {
        p.bump();
        parse_type(p)?;
    }

    Ok(m.complete(p))
}

// <name type>
fn parse_name_type(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::TypeName);
    p.bump();
    Ok(m.complete(p))
}

// `<name type>`
fn parse_string_template_type(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::TypeStringTemplate);
    p.bump();
    Ok(m.complete(p))
}

// just compact luals, trivia type
// ...<name type>
fn parse_vararg_type(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::TypeName);
    p.bump();
    parse_name_type(p)?;
    Ok(m.complete(p))
}

// <type>[]
// <name type> < <type_list> >
// <name type> ...
// <prefix name type>`T`
fn parse_suffixed_type(p: &mut LuaDocParser, cm: CompleteMarker) -> ParseResult {
    let mut only_continue_array = false;
    let mut cm = cm;
    loop {
        match p.current_token() {
            CppTokenKind::TkLeftBracket => {
                let mut m = cm.precede(p, CppSyntaxKind::TypeArray);
                p.bump();
                if matches!(
                    p.current_token(),
                    CppTokenKind::TkString | CppTokenKind::TkInt | CppTokenKind::TkName
                ) {
                    m.set_kind(p, CppSyntaxKind::IndexExpr);
                    p.bump();
                }
                expect_token(p, CppTokenKind::TkRightBracket)?;
                cm = m.complete(p);
                only_continue_array = true;
            }
            CppTokenKind::TkLt => {
                if only_continue_array {
                    return Ok(cm);
                }
                if cm.kind != CppSyntaxKind::TypeName {
                    return Ok(cm);
                }

                let m = cm.precede(p, CppSyntaxKind::TypeGeneric);
                p.bump();
                parse_type_list(p)?;
                expect_token(p, CppTokenKind::TkGt)?;
                cm = m.complete(p);
            }
            CppTokenKind::TkDots => {
                if only_continue_array {
                    return Ok(cm);
                }
                if cm.kind != CppSyntaxKind::TypeName {
                    return Ok(cm);
                }

                let m = cm.precede(p, CppSyntaxKind::TypeVariadic);
                p.bump();
                cm = m.complete(p);
                return Ok(cm);
            }
            _ => return Ok(cm),
        }
    }
}

fn parse_multi_line_union_type(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::TypeMultiLineUnion);

    while p.current_token() == CppTokenKind::TkDocContinueOr {
        p.bump();
        parse_one_line_type(p)?;
    }

    Ok(m.complete(p))
}

fn parse_one_line_type(p: &mut LuaDocParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocOneLineField);

    parse_simple_type(p)?;
    if p.current_token() != CppTokenKind::TkDocContinueOr {
        p.set_state(LuaDocLexerState::Description);
        parse_description(p);
        p.set_state(LuaDocLexerState::Normal);
    }

    Ok(m.complete(p))
}
