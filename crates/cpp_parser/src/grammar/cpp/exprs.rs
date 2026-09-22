use crate::{
    grammar::ParseResult,
    kind::{CppSyntaxKind, CppTokenKind},
    parser::{CppParser, MarkerEventContainer},
    parser_error::CppParseError,
};

use super::expect_token;

/// 操作符优先级定义
/// 数值越高，优先级越高
///
/// `p` is consulted for one thing: inside a template argument list, `>` does not mean "greater
/// than". `Vec<1 > 2>` is not a thing, but `Vec<A<B>>` and `Vec<1, 2>` are, and the closing angle
/// must reach the template-argument reader rather than being eaten as an operator. See
/// [`crate::parser::CppParser::is_in_template_arguments`].
fn get_operator_precedence(p: &CppParser, token: CppTokenKind) -> Option<u8> {
    if p.is_in_template_arguments()
        && matches!(
            token,
            CppTokenKind::Greater
                | CppTokenKind::RightShift
                | CppTokenKind::GreaterEqual
                | CppTokenKind::RightShiftAssign
        )
    {
        return None;
    }

    match token {
        // 赋值运算 — 优先级最低,且**右结合**:`a = b = c` 是 `a = (b = c)`。
        //
        // 这一族之前完全不在表里,后果远不止"少一种运算符":`a = b;` 解析出的 `BinaryExpr` 在 `=` 处
        // 停止,于是**表达式语句**以 "expected `;` after expression" 失败,声明/表达式的回溯也救不了
        // 它 —— 表达式读法自己就没读完整。任何含赋值或复合赋值的语句都受影响,这在真实代码里几乎是
        // 每一行。
        CppTokenKind::Assign
        | CppTokenKind::PlusAssign
        | CppTokenKind::MinusAssign
        | CppTokenKind::StarAssign
        | CppTokenKind::SlashAssign
        | CppTokenKind::PercentAssign
        | CppTokenKind::AmpersandAssign
        | CppTokenKind::PipeAssign
        | CppTokenKind::CaretAssign
        | CppTokenKind::LeftShiftAssign
        | CppTokenKind::RightShiftAssign => Some(1),

        // 注意这里**没有**逗号运算符。它确实是 C++ 里优先级最低的二元运算符,但在这个 parser 里加入它是
        // 有害的:实参列表和初始化列表都用逗号分隔,而一个认得逗号的表达式规则会把分隔符吃掉 —— 于是
        // `g(1, 2)` 变成"一个实参的调用",这正是 `get_args().len()` 从 2 变 1 的原因。
        //
        // 要支持逗号表达式,容器规则必须显式声明"读到逗号为止",那是另一处改动。在那之前,少一个几乎没人
        // 写的运算符,好过吃掉所有实参分隔符。
        // 逻辑或
        CppTokenKind::LogicalOr => Some(4),

        // 逻辑与
        CppTokenKind::LogicalAnd => Some(5),

        // 按位或
        CppTokenKind::Pipe => Some(6),

        // 按位异或
        CppTokenKind::Caret => Some(7),

        // 按位与 — 注意与一元 `&` 的区别:作为二元运算符时它在表里,由优先级爬升决定
        CppTokenKind::Ampersand => Some(8),

        // 相等性运算
        CppTokenKind::Equal | CppTokenKind::NotEqual => Some(9),

        // 关系运算
        CppTokenKind::Less
        | CppTokenKind::LessEqual
        | CppTokenKind::Greater
        | CppTokenKind::GreaterEqual
        | CppTokenKind::Spaceship => Some(10),

        // 移位运算
        CppTokenKind::LeftShift | CppTokenKind::RightShift => Some(11),

        // 加法、减法
        CppTokenKind::Plus | CppTokenKind::Minus => Some(12),

        // 乘法、除法、模运算 - 最高优先级
        CppTokenKind::Star | CppTokenKind::Slash | CppTokenKind::Percent => Some(13),

        _ => None,
    }
}

/// Is this operator right-associative?
///
/// Only the assignment family is, and getting it wrong is a shape rather than an error: `a = b = c` read as
/// left-associative produces `(a = b) = c`, which is not a thing anyone writes and is not what the source says.
fn is_right_associative(token: CppTokenKind) -> bool {
    matches!(
        token,
        CppTokenKind::Assign
            | CppTokenKind::PlusAssign
            | CppTokenKind::MinusAssign
            | CppTokenKind::StarAssign
            | CppTokenKind::SlashAssign
            | CppTokenKind::PercentAssign
            | CppTokenKind::AmpersandAssign
            | CppTokenKind::PipeAssign
            | CppTokenKind::CaretAssign
            | CppTokenKind::LeftShiftAssign
            | CppTokenKind::RightShiftAssign
    )
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

    while let Some(prec) = get_operator_precedence(p, p.current_token()) {
        if prec < min_prec {
            break;
        }

        let operator = p.current_token();
        let m = left.precede(p, CppSyntaxKind::BinaryExpr);
        p.bump(); // consume operator

        // 左结合运算符用 `prec + 1`,右结合用 `prec` —— 差值就是"是否允许同级运算符继续往右吃"。
        // C++ 里只有赋值族是右结合,见 [`is_right_associative`]。
        let next_min_prec = if is_right_associative(operator) {
            prec
        } else {
            prec + 1
        };
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
                if p.current_token() == CppTokenKind::Less
                    && super::types::could_start_template_arguments(p)
                    && let Err(err) = super::types::parse_template_argument_list(p)
                {
                    p.close_marks_above(base);
                    return Err(err);
                }

                if p.current_token() == CppTokenKind::Scope {
                    p.bump();
                    continue;
                }
                break;
            }

            Ok(m.complete(p))
        }

        // A parenthesized expression: `(a + b)`, `(x)`, `(f())`.
        //
        // This was simply missing, and nothing about it is subtle — `x = (a + b);` reported "expected primary
        // expression" at the `(`. It went unnoticed because the parenthesised forms the corpus reached were
        // all *statement* parentheses — `if (x)`, `f(a)` — which the statement and postfix rules consume
        // themselves, so only an expression that begins with `(` ever got here.
        //
        // A `(` in an expression is never a declarator: the declaration reading is tried first and rewound
        // before the expression grammar runs, so anything still standing at a `(` here is being *used*.
        //
        // A **comma** inside the parentheses is the comma operator, and the expression rule does not have it —
        // see the comment on the operator table. `(a, b)` therefore does not parse yet, which is the same gap
        // and not a second one.
        CppTokenKind::LeftParen => {
            let m = p.mark(CppSyntaxKind::ParenExpr);
            p.bump();
            if let Err(err) = parse_expr(p) {
                m.undo(p);
                return Err(err);
            }
            if let Err(err) = expect_token(p, CppTokenKind::RightParen) {
                m.undo(p);
                return Err(err);
            }
            Ok(m.complete(p))
        }

        // A lambda: `[capture](params) -> type { body }`.
        //
        // Decided by a shape check before anything is consumed, because `[` in an expression is otherwise an
        // index — and the two are told apart by what follows the `]`: a lambda continues with `(`, `{`, or a
        // qualifier, while an index has an expression in front of it and an operator after.
        CppTokenKind::LeftBracket if starts_a_lambda(p) => parse_lambda(p),

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

/// Does the `[` at the cursor introduce a lambda rather than an index expression?
///
/// The two are the same token and there is no name in front to help, so the decision is made from what follows
/// the matching `]`:
///
/// ```text
/// [x]        { return x; }      a lambda: capture list, then the body
/// [x]        (int y) { ... }    a lambda: capture list, then the parameters
/// [x]        mutable { ... }    a lambda: a qualifier may sit between them
/// arr[i]                        an index — the `[` has an expression before it
/// ```
///
/// A `]` followed by anything else is not a lambda introducer, and the caller's index reading takes it. The scan
/// is bounded and does not descend: a capture list cannot contain an unbalanced `]`, so the first one closes it.
fn starts_a_lambda(p: &CppParser) -> bool {
    use CppTokenKind::{
        Ampersand, Arrow, Assign, CharLiteral, Comma, Ellipsis, FalseKeyword, FloatingLiteral,
        Identifier, IntegerLiteral, LeftBrace, LeftParen, MutableKeyword, NoexceptKeyword,
        NullptrKeyword, RightBracket, RightParen, Scope, Star, StringLiteral, ThisKeyword,
        TrueKeyword,
    };

    // The capture list's own contents, so that a malformed one stops the reading rather than swallowing the
    // rest of the expression. Relative offsets: `0` is the `[` at the cursor, so the scan starts at `1`.
    //
    // A **literal or a call** is admitted too, and only because of the init-capture: `[p = 1]` and
    // `[p = make()]` are single captures whose initialiser is an expression, and refusing every token an
    // expression can contain would refuse those. The risk runs the other way — an index expression like
    // `arr[1]` — and the second half of the check catches it, because `arr[1]` is followed by whatever the
    // statement continues with rather than by `(`, `{` or a qualifier.
    let mut next_after_the_list = None;
    for (offset, kind) in p.peek_token_kind_at(1..64).into_iter().enumerate() {
        match kind {
            Identifier
            | Comma
            | Ampersand
            | Assign
            | Ellipsis
            | ThisKeyword
            | Star
            | Scope
            // An init-capture's initialiser, which is an expression: its literal, or the parentheses of a call.
            | IntegerLiteral
            | FloatingLiteral
            | StringLiteral
            | CharLiteral
            | TrueKeyword
            | FalseKeyword
            | NullptrKeyword
            | LeftParen
            | RightParen => {}
            RightBracket => {
                next_after_the_list = p.peek_token_kind_at(offset + 2..offset + 3).first().copied();
                break;
            }
            // Anything else cannot appear in a capture list at all — a `;`, a `)`, a `}` — so this `[` was not
            // the start of one.
            _ => return false,
        }
    }

    // What follows decides, and only two things can: the parameters or the body, with the qualifiers that may
    // sit between them.
    matches!(
        next_after_the_list,
        Some(LeftParen)
            | Some(LeftBrace)
            | Some(MutableKeyword)
            | Some(NoexceptKeyword)
            | Some(Arrow)
    )
}

/// Parse a lambda expression: `[capture](params) qualifiers -> type { body }`.
///
/// Everything after the capture list is optional, and the body is required — which is what makes the shape check
/// in [`starts_a_lambda`] safe to trust.
fn parse_lambda(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::LambdaExpr);

    if let Err(err) = parse_capture_list(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    // The parameter list, when it is written. A lambda's parameters are the same grammar as a function's, and
    // reusing the rule is what makes a default argument, a pack or a `std::function` parameter work here too.
    if p.current_token() == CppTokenKind::LeftParen
        && let Err(err) = super::decls::parse_parameter_list(p)
    {
        p.close_marks_above(base);
        return Err(err);
    }

    // `mutable`, `constexpr`, `consteval`, `noexcept`, `-> type`, and the attributes that may appear among them.
    super::types::eat_function_qualifiers(p);

    // The body. A lambda's body is a compound statement like any other, so it is parsed by the statement rule —
    // which is also what keeps `return` and every other statement inside it working.
    if let Err(err) = super::stats::parse_compound_stat(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Parse a lambda's capture list: the `[` … `]` at the cursor.
///
/// Kept as its own node so that a consumer can see what a lambda *captures* — the names, the `&` that makes a
/// capture by reference, `this`, and the initialisers of an init-capture — without walking the raw tokens of the
/// whole lambda to find them.
fn parse_capture_list(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::LambdaCaptureList);

    if let Err(err) = expect_token(p, CppTokenKind::LeftBracket) {
        p.close_marks_above(base);
        return Err(err);
    }

    while p.current_token() != CppTokenKind::RightBracket && !p.is_eof() {
        if let Err(err) = parse_capture(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        if p.current_token() == CppTokenKind::Comma {
            p.bump();
            continue;
        }
        break;
    }

    if let Err(err) = expect_token(p, CppTokenKind::RightBracket) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// One capture: `x`, `&x`, `this`, `*this`, `=`, `&`, `...`, or `x = expr`.
fn parse_capture(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::LambdaCapture);

    // The sigil, when there is one. `&` alone captures everything by reference and `=` everything by copy, so
    // neither has to be followed by a name — and `*this` is a capture of the object by copy.
    if matches!(
        p.current_token(),
        CppTokenKind::Ampersand | CppTokenKind::Star
    ) {
        p.bump();
    }

    // The capture itself: a name, `this`, `...`, or the `=` of a by-copy default. None of them is required —
    // `[&]` and `[=]` are complete captures on their own — so this is one optional token, not a chain of cases.
    if matches!(
        p.current_token(),
        CppTokenKind::ThisKeyword
            | CppTokenKind::Ellipsis
            | CppTokenKind::Assign
            | CppTokenKind::Identifier
    ) {
        p.bump();
    }

    // An init-capture: `x = std::move(other)`, or `...args = pack`.
    if p.current_token() == CppTokenKind::Assign {
        p.bump();
        if let Err(err) = parse_expr(p) {
            p.close_marks_above(base);
            return Err(err);
        }
    }

    Ok(m.complete(p))
}
