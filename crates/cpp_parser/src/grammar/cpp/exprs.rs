use crate::{
    grammar::ParseResult,
    kind::{CppSyntaxKind, CppTokenKind},
    parser::{CppParser, MarkerEventContainer},
    parser_error::CppParseError,
};

use super::expect_token;

/// 操作符优先级定义
/// 数值越高，优先级越高
fn get_operator_precedence(token: CppTokenKind) -> Option<u8> {
    match token {
        // 乘法、除法、模运算 - 最高优先级
        CppTokenKind::Star | CppTokenKind::Slash | CppTokenKind::Percent => Some(13),
        
        // 加法、减法
        CppTokenKind::Plus | CppTokenKind::Minus => Some(12),
        
        // 移位运算
        CppTokenKind::LeftShift | CppTokenKind::RightShift => Some(11),
        
        // 关系运算
        CppTokenKind::Less | CppTokenKind::LessEqual | 
        CppTokenKind::Greater | CppTokenKind::GreaterEqual | 
        CppTokenKind::Spaceship => Some(10),
        
        // 相等性运算
        CppTokenKind::Equal | CppTokenKind::NotEqual => Some(9),
        
        // 按位与
        CppTokenKind::Ampersand => Some(8),
        
        // 按位异或
        CppTokenKind::Caret => Some(7),
        
        // 按位或
        CppTokenKind::Pipe => Some(6),
        
        // 逻辑与
        CppTokenKind::LogicalAnd => Some(5),
        
        // 逻辑或
        CppTokenKind::LogicalOr => Some(4),
        
        _ => None,
    }
}

/// 解析表达式的主要入口点
pub fn parse_expr(p: &mut CppParser) -> ParseResult {
    parse_ternary_expr(p)
}

/// 解析三元表达式 (condition ? true_expr : false_expr)
fn parse_ternary_expr(p: &mut CppParser) -> ParseResult {
    let mut expr = parse_binary_expr_with_precedence(p, 0)?;

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

/// 使用优先级爬升算法解析二元表达式
/// min_prec: 当前最小优先级
fn parse_binary_expr_with_precedence(p: &mut CppParser, min_prec: u8) -> ParseResult {
    let mut left = parse_unary_expr(p)?;

    while let Some(prec) = get_operator_precedence(p.current_token()) {
        if prec < min_prec {
            break;
        }

        let m = left.precede(p, CppSyntaxKind::BinaryExpr);
        p.bump(); // consume operator

        // 对于左结合运算符，使用 prec + 1；对于右结合运算符，使用 prec
        // C++ 中大部分二元运算符都是左结合的
        let next_min_prec = prec + 1;
        parse_binary_expr_with_precedence(p, next_min_prec)?;

        left = m.complete(p);
    }

    Ok(left)
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
        | CppTokenKind::FalseKeyword
        | CppTokenKind::NullptrKeyword => {
            let m = p.mark(CppSyntaxKind::LiteralExpr);
            p.bump();
            Ok(m.complete(p))
        }

        // A name, possibly qualified (`std::vector`), and possibly a template-id
        // (`std::vector<int>`). Both forms appear as expressions — `std::move(x)`,
        // `std::vector<int>::size_type` — so the expression grammar has to accept them, not just the
        // type grammar.
        CppTokenKind::Identifier | CppTokenKind::Scope if !is_expression_keyword(p) => {
            let base = p.open_marks();
            let m = p.mark(CppSyntaxKind::IdentifierExpr);

            // A leading `::` makes the name fully qualified.
            if p.current_token() == CppTokenKind::Scope {
                p.bump();
            }

            loop {
                if p.current_token() != CppTokenKind::Identifier {
                    p.close_marks_above(base);
                    return Err(CppParseError::syntax_error_from(
                        "expected a name after `::`",
                        p.current_token_range(),
                    ));
                }
                p.bump();

                // A template-id: `vector<int>`.
                if p.current_token() == CppTokenKind::Less && super::types::could_start_template_arguments(p)
                {
                    if let Err(err) = super::types::parse_template_argument_list(p) {
                        p.close_marks_above(base);
                        return Err(err);
                    }
                }

                if p.current_token() == CppTokenKind::Scope {
                    p.bump();
                    continue;
                }
                break;
            }

            Ok(m.complete(p))
        }

        // `this`
        CppTokenKind::ThisKeyword => {
            let m = p.mark(CppSyntaxKind::ThisExpr);
            p.bump();
            Ok(m.complete(p))
        }

        _ => Err(CppParseError::syntax_error_from(
            &t!("expected primary expression"),
            p.current_token_range(),
        )),
    }
}

/// Keywords that look like a name start to the dispatch above but are their own expression forms.
///
/// `nullptr` is handled as a literal; the rest are parsed as unary or postfix operators. Listing
/// them here keeps the name branch from swallowing them.
fn is_expression_keyword(p: &CppParser) -> bool {
    matches!(
        p.current_token(),
        CppTokenKind::NullptrKeyword
            | CppTokenKind::SizeofKeyword
            | CppTokenKind::AlignofKeyword
            | CppTokenKind::TypeidKeyword
            | CppTokenKind::NewKeyword
            | CppTokenKind::DeleteKeyword
            | CppTokenKind::ThrowKeyword
            | CppTokenKind::CoAwaitKeyword
            | CppTokenKind::RequiresKeyword
    )
}
