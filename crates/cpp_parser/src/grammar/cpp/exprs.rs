use crate::{
    grammar::ParseResult,
    kind::{BinaryOperator, CppSyntaxKind, CppTokenKind, CppOpKind, UnaryOperator},
    parser::{CppParser, MarkerEventContainer},
    parser_error::CppParseError,
};

use super::{expect_token, if_token_bump, parse_compound_stat};

pub fn parse_expr(p: &mut CppParser) -> ParseResult {
    // parse_sub_expr(p, 0)
    todo!()
}

// fn parse_sub_expr(p: &mut LuaParser, limit: i32) -> ParseResult {
//     let uop = LuaOpKind::to_unary_operator(p.current_token());
//     let mut cm = if uop != UnaryOperator::OpNop {
//         let m = p.mark(CppSyntaxKind::UnaryExpr);
//         let range = p.current_token_range();
//         p.bump();
//         match parse_sub_expr(p, UNARY_PRIORITY) {
//             Ok(_) => {}
//             Err(err) => {
//                 p.push_error(LuaParseError::syntax_error_from(
//                     &t!("unary operator not followed by expression"),
//                     range,
//                 ));
//                 return Err(err);
//             }
//         }
//         m.complete(p)
//     } else {
//         parse_simple_expr(p)?
//     };

//     let mut bop = LuaOpKind::to_binary_operator(p.current_token());
//     while bop != BinaryOperator::OpNop && bop.get_priority().left > limit {
//         let range = p.current_token_range();
//         let m = cm.precede(p, CppSyntaxKind::BinaryExpr);
//         p.bump();
//         match parse_sub_expr(p, bop.get_priority().right) {
//             Ok(_) => {}
//             Err(err) => {
//                 p.push_error(LuaParseError::syntax_error_from(
//                     &t!("binary operator not followed by expression"),
//                     range,
//                 ));

//                 return Err(err);
//             }
//         }

//         cm = m.complete(p);
//         bop = LuaOpKind::to_binary_operator(p.current_token());
//     }

//     Ok(cm)
// }

// fn parse_simple_expr(p: &mut LuaParser) -> ParseResult {
//     match p.current_token() {
//         CppTokenKind::TkInt
//         | CppTokenKind::TkFloat
//         | CppTokenKind::TkComplex
//         | CppTokenKind::TkNil
//         | CppTokenKind::TkTrue
//         | CppTokenKind::TkFalse
//         | CppTokenKind::TkDots
//         | CppTokenKind::TkString
//         | CppTokenKind::TkLongString => {
//             let m = p.mark(CppSyntaxKind::LiteralExpr);
//             p.bump();
//             Ok(m.complete(p))
//         }
//         CppTokenKind::TkLeftBrace => parse_table_expr(p),
//         CppTokenKind::TkFunction => parse_closure_expr(p),
//         _ => parse_suffixed_expr(p),
//     }
// }

// pub fn parse_closure_expr(p: &mut LuaParser) -> ParseResult {
//     let m = p.mark(CppSyntaxKind::ClosureExpr);

//     if_token_bump(p, CppTokenKind::TkFunction);
//     parse_param_list(p)?;

//     if p.current_token() != CppTokenKind::TkEnd {
//         parse_block(p)?;
//     }

//     expect_token(p, CppTokenKind::TkEnd)?;
//     Ok(m.complete(p))
// }

// fn parse_param_list(p: &mut LuaParser) -> ParseResult {
//     let m = p.mark(CppSyntaxKind::ParamList);

//     expect_token(p, CppTokenKind::TkLeftParen)?;
//     if p.current_token() != CppTokenKind::TkRightParen {
//         parse_param_name(p)?;
//         while p.current_token() == CppTokenKind::TkComma {
//             p.bump();
//             parse_param_name(p)?;
//         }
//     }

//     expect_token(p, CppTokenKind::TkRightParen)?;
//     Ok(m.complete(p))
// }

// fn parse_param_name(p: &mut LuaParser) -> ParseResult {
//     let m = p.mark(CppSyntaxKind::ParamName);

//     if p.current_token() == CppTokenKind::TkName || p.current_token() == CppTokenKind::TkDots {
//         p.bump();
//     } else {
//         return Err(LuaParseError::syntax_error_from(
//             &t!("expect parameter name"),
//             p.current_token_range(),
//         ));
//     }

//     Ok(m.complete(p))
// }

// fn parse_table_expr(p: &mut LuaParser) -> ParseResult {
//     let mut m = p.mark(CppSyntaxKind::TableEmptyExpr);
//     p.bump();

//     if p.current_token() == CppTokenKind::TkRightBrace {
//         p.bump();
//         return Ok(m.complete(p));
//     }

//     let mut cm = parse_field(p)?;
//     match cm.kind {
//         CppSyntaxKind::TableFieldAssign => {
//             m.set_kind(p, CppSyntaxKind::TableObjectExpr);
//         }
//         CppSyntaxKind::TableFieldValue => {
//             m.set_kind(p, CppSyntaxKind::TableArrayExpr);
//         }
//         _ => {}
//     }

//     while p.current_token() == CppTokenKind::TkComma
//         || p.current_token() == CppTokenKind::TkSemicolon
//     {
//         p.bump();
//         if p.current_token() == CppTokenKind::TkRightBrace {
//             break;
//         }
//         cm = parse_field(p)?;
//         if cm.kind == CppSyntaxKind::TableFieldAssign {
//             m.set_kind(p, CppSyntaxKind::TableObjectExpr);
//         }
//     }

//     expect_token(p, CppTokenKind::TkRightBrace)?;
//     Ok(m.complete(p))
// }

// fn parse_field(p: &mut LuaParser) -> ParseResult {
//     let mut m = p.mark(CppSyntaxKind::TableFieldValue);

//     if p.current_token() == CppTokenKind::TkLeftBracket {
//         m.set_kind(p, CppSyntaxKind::TableFieldAssign);
//         p.bump();
//         parse_expr(p)?;
//         expect_token(p, CppTokenKind::TkRightBracket)?;
//         expect_token(p, CppTokenKind::TkAssign)?;
//         parse_expr(p)?;
//     } else if p.current_token() == CppTokenKind::TkName {
//         if p.peek_next_token() == CppTokenKind::TkAssign {
//             m.set_kind(p, CppSyntaxKind::TableFieldAssign);
//             p.bump();
//             p.bump();
//             parse_expr(p)?;
//         } else {
//             parse_expr(p)?;
//         }
//     } else {
//         parse_expr(p)?;
//     }

//     Ok(m.complete(p))
// }

// fn parse_suffixed_expr(p: &mut LuaParser) -> ParseResult {
//     let mut cm = match p.current_token() {
//         CppTokenKind::TkName => parse_name_or_special_function(p)?,
//         CppTokenKind::TkLeftParen => {
//             let m = p.mark(CppSyntaxKind::ParenExpr);
//             p.bump();
//             parse_expr(p)?;
//             expect_token(p, CppTokenKind::TkRightParen)?;
//             m.complete(p)
//         }
//         _ => {
//             return Err(LuaParseError::syntax_error_from(
//                 &t!("expect primary expression"),
//                 p.current_token_range(),
//             ))
//         }
//     };

//     loop {
//         match p.current_token() {
//             CppTokenKind::TkDot | CppTokenKind::TkColon | CppTokenKind::TkLeftBracket => {
//                 let m = cm.precede(p, CppSyntaxKind::IndexExpr);
//                 parse_index_struct(p)?;
//                 cm = m.complete(p);
//             }
//             CppTokenKind::TkLeftParen
//             | CppTokenKind::TkLongString
//             | CppTokenKind::TkString
//             | CppTokenKind::TkLeftBrace => {
//                 let m = cm.precede(p, CppSyntaxKind::CallExpr);
//                 parse_args(p)?;
//                 cm = m.complete(p);
//             }
//             _ => {
//                 return Ok(cm);
//             }
//         }
//     }
// }

// fn parse_name_or_special_function(p: &mut LuaParser) -> ParseResult {
//     let m = p.mark(CppSyntaxKind::NameExpr);
//     let special_kind = match p.parse_config.get_special_function(p.current_token_text()) {
//         SpecialFunction::Require => CppSyntaxKind::RequireCallExpr,
//         SpecialFunction::Assert => CppSyntaxKind::AssertCallExpr,
//         SpecialFunction::Error => CppSyntaxKind::ErrorCallExpr,
//         SpecialFunction::Type => CppSyntaxKind::TypeCallExpr,
//         SpecialFunction::Setmatable => CppSyntaxKind::SetmetatableCallExpr,
//         _ => CppSyntaxKind::None,
//     };
//     p.bump();
//     let mut cm = m.complete(p);
//     if special_kind == CppSyntaxKind::None {
//         return Ok(cm);
//     }

//     if matches!(
//         p.current_token(),
//         CppTokenKind::TkLeftParen
//             | CppTokenKind::TkLongString
//             | CppTokenKind::TkString
//             | CppTokenKind::TkLeftBrace
//     ) {
//         let m1 = cm.precede(p, special_kind);
//         parse_args(p)?;
//         cm = m1.complete(p);
//     }

//     Ok(cm)
// }

// fn parse_index_struct(p: &mut LuaParser) -> Result<(), LuaParseError> {
//     match p.current_token() {
//         CppTokenKind::TkLeftBracket => {
//             p.bump();
//             parse_expr(p)?;
//             expect_token(p, CppTokenKind::TkRightBracket)?;
//         }
//         CppTokenKind::TkDot => {
//             p.bump();
//             expect_token(p, CppTokenKind::TkName)?;
//         }
//         CppTokenKind::TkColon => {
//             p.bump();
//             expect_token(p, CppTokenKind::TkName)?;
//             if !matches!(
//                 p.current_token(),
//                 CppTokenKind::TkLeftParen
//                     | CppTokenKind::TkLeftBrace
//                     | CppTokenKind::TkString
//                     | CppTokenKind::TkLongString
//             ) {
//                 return Err(LuaParseError::syntax_error_from(
//                     &t!("colon accessor must be followed by a function call or table constructor or string literal"),
//                     p.current_token_range(),
//                 ));
//             }
//         }
//         _ => {
//             return Err(LuaParseError::syntax_error_from(
//                 &t!("expect index struct"),
//                 p.current_token_range(),
//             ));
//         }
//     }

//     Ok(())
// }

// fn parse_args(p: &mut LuaParser) -> ParseResult {
//     let m = p.mark(CppSyntaxKind::CallArgList);
//     match p.current_token() {
//         CppTokenKind::TkLeftParen => {
//             p.bump();
//             if p.current_token() != CppTokenKind::TkRightParen {
//                 parse_expr(p)?;
//                 while p.current_token() == CppTokenKind::TkComma {
//                     p.bump();
//                     if p.current_token() == CppTokenKind::TkRightParen {
//                         p.push_error(LuaParseError::syntax_error_from(
//                             &t!("expect expression"),
//                             p.current_token_range(),
//                         ));
//                         break;
//                     }
//                     parse_expr(p)?;
//                 }
//             }
//             expect_token(p, CppTokenKind::TkRightParen)?;
//         }
//         CppTokenKind::TkLeftBrace => {
//             parse_table_expr(p)?;
//         }
//         CppTokenKind::TkString | CppTokenKind::TkLongString => {
//             let m1 = p.mark(CppSyntaxKind::LiteralExpr);
//             p.bump();
//             m1.complete(p);
//         }
//         _ => {
//             return Err(LuaParseError::syntax_error_from(
//                 &t!("expect args"),
//                 p.current_token_range(),
//             ));
//         }
//     }

//     Ok(m.complete(p))
// }
