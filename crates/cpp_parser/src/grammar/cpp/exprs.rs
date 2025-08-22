use crate::{
    grammar::ParseResult,
    kind::{CppSyntaxKind, CppTokenKind},
    parser::{CppParser, MarkerEventContainer},
    parser_error::CppParseError,
};

use super::expect_token;

/// 解析表达式的主要入口点
pub fn parse_expr(p: &mut CppParser) -> ParseResult {
    parse_ternary_expr(p)
}

/// 解析三元表达式 (condition ? true_expr : false_expr)
fn parse_ternary_expr(p: &mut CppParser) -> ParseResult {
    let mut expr = parse_logical_or_expr(p)?;

    if p.current_token() == CppTokenKind::Question {
        let m = expr.precede(p, CppSyntaxKind::TernaryExpr);
        p.bump(); // consume '?'

        parse_expr(p)?; // true expression
        expect_token(p, CppTokenKind::Colon)?;
        parse_ternary_expr(p)?; // false expression

        expr = m.complete(p);
    }

    Ok(expr)
}

/// 解析逻辑或表达式 (||)
fn parse_logical_or_expr(p: &mut CppParser) -> ParseResult {
    parse_binary_expr(p, parse_logical_and_expr, &[CppTokenKind::LogicalOr])
}

/// 解析逻辑与表达式 (&&)
fn parse_logical_and_expr(p: &mut CppParser) -> ParseResult {
    parse_binary_expr(p, parse_bitwise_or_expr, &[CppTokenKind::LogicalAnd])
}

/// 解析按位或表达式 (|)
fn parse_bitwise_or_expr(p: &mut CppParser) -> ParseResult {
    parse_binary_expr(p, parse_bitwise_xor_expr, &[CppTokenKind::Pipe])
}

/// 解析按位异或表达式 (^)
fn parse_bitwise_xor_expr(p: &mut CppParser) -> ParseResult {
    parse_binary_expr(p, parse_bitwise_and_expr, &[CppTokenKind::Caret])
}

/// 解析按位与表达式 (&)
fn parse_bitwise_and_expr(p: &mut CppParser) -> ParseResult {
    parse_binary_expr(p, parse_equality_expr, &[CppTokenKind::Ampersand])
}

/// 解析相等性表达式 (==, !=)
fn parse_equality_expr(p: &mut CppParser) -> ParseResult {
    parse_binary_expr(
        p,
        parse_relational_expr,
        &[CppTokenKind::Equal, CppTokenKind::NotEqual],
    )
}

/// 解析关系表达式 (<, <=, >, >=, <=>)
fn parse_relational_expr(p: &mut CppParser) -> ParseResult {
    parse_binary_expr(
        p,
        parse_shift_expr,
        &[
            CppTokenKind::Less,
            CppTokenKind::LessEqual,
            CppTokenKind::Greater,
            CppTokenKind::GreaterEqual,
            CppTokenKind::Spaceship,
        ],
    )
}

/// 解析移位表达式 (<<, >>)
fn parse_shift_expr(p: &mut CppParser) -> ParseResult {
    parse_binary_expr(
        p,
        parse_additive_expr,
        &[CppTokenKind::LeftShift, CppTokenKind::RightShift],
    )
}

/// 解析加法表达式 (+, -)
fn parse_additive_expr(p: &mut CppParser) -> ParseResult {
    parse_binary_expr(
        p,
        parse_multiplicative_expr,
        &[CppTokenKind::Plus, CppTokenKind::Minus],
    )
}

/// 解析乘法表达式 (*, /, %)
fn parse_multiplicative_expr(p: &mut CppParser) -> ParseResult {
    parse_binary_expr(
        p,
        parse_unary_expr,
        &[
            CppTokenKind::Star,
            CppTokenKind::Slash,
            CppTokenKind::Percent,
        ],
    )
}

/// 解析一元表达式
fn parse_unary_expr(p: &mut CppParser) -> ParseResult {
    match p.current_token() {
        CppTokenKind::LogicalNot
        | CppTokenKind::Tilde
        | CppTokenKind::Plus
        | CppTokenKind::Minus
        | CppTokenKind::PlusPlus
        | CppTokenKind::MinusMinus
        | CppTokenKind::Star
        | CppTokenKind::Ampersand => {
            let m = p.mark(CppSyntaxKind::UnaryExpr);
            p.bump(); // consume unary operator
            parse_unary_expr(p)?; // parse operand recursively
            Ok(m.complete(p))
        }
        CppTokenKind::SizeofKeyword => {
            let m = p.mark(CppSyntaxKind::UnaryExpr);
            p.bump(); // consume 'sizeof'

            if p.current_token() == CppTokenKind::LeftParen {
                p.bump(); // consume '('
                parse_expr(p)?; // parse expression or type
                expect_token(p, CppTokenKind::RightParen)?;
            } else {
                parse_unary_expr(p)?;
            }

            Ok(m.complete(p))
        }
        CppTokenKind::TypeidKeyword => {
            let m = p.mark(CppSyntaxKind::UnaryExpr);
            p.bump(); // consume 'typeid'
            expect_token(p, CppTokenKind::LeftParen)?;
            parse_expr(p)?; // parse expression or type
            expect_token(p, CppTokenKind::RightParen)?;
            Ok(m.complete(p))
        }
        CppTokenKind::NewKeyword => {
            let m = p.mark(CppSyntaxKind::UnaryExpr);
            p.bump(); // consume 'new'
            // TODO: parse placement new and type specifier
            parse_postfix_expr(p)?;
            Ok(m.complete(p))
        }
        CppTokenKind::DeleteKeyword => {
            let m = p.mark(CppSyntaxKind::UnaryExpr);
            p.bump(); // consume 'delete'
            if p.current_token() == CppTokenKind::LeftBracket {
                p.bump(); // consume '['
                expect_token(p, CppTokenKind::RightBracket)?;
            }
            parse_unary_expr(p)?;
            Ok(m.complete(p))
        }
        _ => parse_postfix_expr(p),
    }
}

/// 解析后缀表达式 (函数调用、数组访问、成员访问、后增后减等)
fn parse_postfix_expr(p: &mut CppParser) -> ParseResult {
    let mut expr = parse_primary_expr(p)?;

    loop {
        match p.current_token() {
            CppTokenKind::LeftParen => {
                // 函数调用
                let m = expr.precede(p, CppSyntaxKind::CallExpr);
                p.bump(); // consume '('

                // 解析参数列表
                if p.current_token() != CppTokenKind::RightParen {
                    parse_expr(p)?;
                    while p.current_token() == CppTokenKind::Comma {
                        p.bump(); // consume ','
                        parse_expr(p)?;
                    }
                }

                expect_token(p, CppTokenKind::RightParen)?;
                expr = m.complete(p);
            }
            CppTokenKind::LeftBracket => {
                // 数组访问
                let m = expr.precede(p, CppSyntaxKind::IndexExpr);
                p.bump(); // consume '['
                parse_expr(p)?;
                expect_token(p, CppTokenKind::RightBracket)?;
                expr = m.complete(p);
            }
            CppTokenKind::Dot | CppTokenKind::Arrow => {
                // 成员访问
                let m = expr.precede(p, CppSyntaxKind::IndexExpr);
                p.bump(); // consume '.' or '->'

                if p.current_token() == CppTokenKind::Identifier {
                    p.bump();
                } else {
                    return Err(CppParseError::syntax_error_from(
                        &t!("expected identifier after member access operator"),
                        p.current_token_range(),
                    ));
                }

                expr = m.complete(p);
            }
            CppTokenKind::PlusPlus | CppTokenKind::MinusMinus => {
                // 后增/后减
                let m = expr.precede(p, CppSyntaxKind::UnaryExpr);
                p.bump(); // consume '++' or '--'
                expr = m.complete(p);
            }
            _ => break,
        }
    }

    Ok(expr)
}

/// 解析主表达式 (标识符、字面量、括号表达式等)
fn parse_primary_expr(p: &mut CppParser) -> ParseResult {
    match p.current_token() {
        // 字面量
        CppTokenKind::IntegerLiteral
        | CppTokenKind::FloatingLiteral
        | CppTokenKind::StringLiteral
        | CppTokenKind::CharLiteral
        | CppTokenKind::TrueKeyword
        | CppTokenKind::FalseKeyword => {
            let m = p.mark(CppSyntaxKind::LiteralExpr);
            p.bump();
            Ok(m.complete(p))
        }

        // 标识符
        CppTokenKind::Identifier => {
            let m = p.mark(CppSyntaxKind::IdentifierExpr);
            p.bump();
            Ok(m.complete(p))
        }

        // 括号表达式
        CppTokenKind::LeftParen => {
            let m = p.mark(CppSyntaxKind::ParenExpr);
            p.bump(); // consume '('
            parse_expr(p)?;
            expect_token(p, CppTokenKind::RightParen)?;
            Ok(m.complete(p))
        }

        // this 关键字
        CppTokenKind::ThisKeyword => {
            let m = p.mark(CppSyntaxKind::IdentifierExpr);
            p.bump();
            Ok(m.complete(p))
        }

        _ => Err(CppParseError::syntax_error_from(
            &t!("expected primary expression"),
            p.current_token_range(),
        )),
    }
}

/// 通用的二元表达式解析器
fn parse_binary_expr<F>(
    p: &mut CppParser,
    parse_operand: F,
    operators: &[CppTokenKind],
) -> ParseResult
where
    F: Fn(&mut CppParser) -> ParseResult,
{
    let mut expr = parse_operand(p)?;

    while operators.contains(&p.current_token()) {
        let m = expr.precede(p, CppSyntaxKind::BinaryExpr);
        p.bump(); // consume operator
        parse_operand(p)?; // parse right operand
        expr = m.complete(p);
    }

    Ok(expr)
}
