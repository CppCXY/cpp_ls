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
        CppTokenKind::SizeofKeyword | CppTokenKind::AlignofKeyword => {
            let m = p.mark(CppSyntaxKind::UnaryExpr);
            p.bump(); // consume 'sizeof' / 'alignof'

            if p.current_token() == CppTokenKind::LeftParen {
                p.bump(); // consume '('
                parse_type_id_or_expression(p)?;
                expect_token(p, CppTokenKind::RightParen)?;
            } else {
                // `sizeof x`, `sizeof(int)` without parentheses and `sizeof...` — the operand of the
                // unparenthesised form is an expression, never a type.
                parse_unary_expr(p)?;
            }

            Ok(m.complete(p))
        }
        CppTokenKind::TypeidKeyword => {
            let m = p.mark(CppSyntaxKind::UnaryExpr);
            p.bump(); // consume 'typeid'
            expect_token(p, CppTokenKind::LeftParen)?;
            parse_type_id_or_expression(p)?;
            expect_token(p, CppTokenKind::RightParen)?;
            Ok(m.complete(p))
        }
        // A C-style cast, or a parenthesised expression. `(int)x` and `(x)` are the same first three tokens,
        // and the type reading has to be tried first because it is the one that can be refused: `(x + 1)`
        // parses as neither a type nor an abstract declarator, so the rollback is what makes it an expression.
        CppTokenKind::LeftParen if is_a_type_in_parentheses(p) => {
            let base = p.open_marks();
            let m = p.mark(CppSyntaxKind::CastExpr);
            p.bump(); // `(`
            if let Err(err) = super::types::parse_type_id(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            if let Err(err) = expect_token(p, CppTokenKind::RightParen) {
                p.close_marks_above(base);
                return Err(err);
            }

            // The operand of a cast is a unary expression, which is what keeps `(int)a + b` a sum of a cast
            // and `b` rather than a cast of `a + b`.
            if let Err(err) = parse_unary_expr(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            Ok(m.complete(p))
        }
        // `new`, with everything that can follow it: placement arguments, a type, array bounds and an
        // initializer — and then the postfix suffixes, because `new Widget(1)->run()` is one expression.
        //
        // This used to hand off to [`parse_postfix_expr`], which reads a *value* — and a type is not one, so
        // `new int[4]` reported `expected primary expression` at the `int` and `new Widget(1, 2)` silently
        // read `Widget(1, 2)` as a call, which is a different construct with the same tokens. Nothing in the
        // grammar can read a type except the type rules, so this is where they have to be called.
        CppTokenKind::NewKeyword => parse_new_expr(p),
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

/// Parse what stands inside `sizeof(...)`, `typeid(...)` or a cast's parentheses: a type, or an expression.
///
/// The two grammars overlap completely — `(int)`, `(int*)` and `(x)` are all well-formed readings — and the
/// tokens do not choose between them, so this is a bounded backtrack with a rule for which reading to *try*:
/// the type goes first, because it is the one that can be refused. A name alone parses as both, and for
/// `sizeof(x)` the answer barely matters; for `(int)x` it decides whether the construct is a cast at all,
/// since the expression reading of `(int)` is not an expression and fails.
///
/// # Why the type-id must not stop at one name here
///
/// [`super::types::parse_type_id`] uses the same sequence as a declaration, where a second name is the
/// declarator. That is what is wanted: `sizeof(unsigned long)` and `(MyType)x` both need the type to be read
/// in full, and a `sizeof` of a variable is not a declaration, so there is no declarator to protect.
fn parse_type_id_or_expression(p: &mut CppParser) -> ParseResult {
    let checkpoint = p.checkpoint();
    let before = p.current_token_index();

    if super::types::parse_type_id(p).is_ok() && p.current_token_index() > before {
        return Ok(crate::parser::CompleteMarker::empty());
    }

    p.rollback(checkpoint);
    parse_expr(p)
}

/// Does the `(` at the cursor open a C-style cast rather than a parenthesised expression?
///
/// The question is asked before the parenthesis is consumed, and it is asked precisely, because unlike every
/// other decision in this grammar the *wrong* answer is not recoverable by the parse itself: `(a)` parses as a
/// type-id — one name, no declarator — so a rule that tried the type reading whenever it could would turn
/// every parenthesised variable into a cast of `a`. That is how `x = (a);` came back as `expected primary
/// expression` against its own `)`, and how `(a && b)` became a cast of `b` to `a&&`.
///
/// Two shapes are certain, and only two:
///
/// * a **keyword type** — `(int)x`, `(const char*)p`, `(unsigned long)n`. No expression begins with `int`.
/// * a **name the file declared to be a type**, followed by something a cast can carry — `(MyType)x`,
///   `(MyType*)p`, `(ns::T)x`, `(Vec<int>)v`. The type table's one job, and the reason it exists.
///
/// # The operators that are deliberately not used
///
/// `*`, `&` and `&&` after a name look like they discriminate — a cast has a pointer or reference type, and an
/// expression has an operator — and they do not: `(a && b)` is a conjunction, `(a * b)` a product. All three
/// tokens mean both things in the two grammars, and C++ settles them by looking the name up, which is what the
/// type table does. A `(MyType*)p` cast written in a file that never declares `MyType` is therefore read as an
/// expression; it is the same documented cost as direct-initialisation, in the same direction, and for the same
/// reason.
///
/// Everything else — `(a)`, `(a + 1)`, `((a))`, `(f(x))`, `(a && b)` — belongs to the parenthesised-expression
/// rule, which is where it now goes.
fn is_a_type_in_parentheses(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::LeftParen {
        return false;
    }

    match p.peek_token_kind_at(1..2).first() {
        Some(&kind) if super::types::is_type_specifier_keyword(kind) => true,
        Some(&CppTokenKind::Identifier) => {
            // The name must be one the file declares to be a type: a name alone is the token a type and an
            // expression share, and reading every `(a)` as a cast is the mistake this check exists to avoid.
            //
            // Nothing further is asked. The token after the `)` looks like it could discriminate — a `(` there
            // means an allocation rather than a cast — and the *parse* is what settles that instead: a cast
            // whose operand fails to parse is rewound and read as a parenthesised expression, which is both
            // more accurate and one rule fewer. See the cast branch in `parse_primary_expr`.
            p.is_a_known_type_name(p.peek_token_text_at(1))
        }
        Some(&CppTokenKind::Scope) => true,
        _ => false,
    }
}

/// Parse a `new` expression: `new T`, `new T[4]`, `new T(1, 2)`, `new T{1}`, `new (buf) T()`.
///
/// The node is a [`CppSyntaxKind::NewExpr`] rather than the `UnaryExpr` this used to produce, because `new`
/// is not an operator applied to an operand: its operand is a *type*, and the parentheses that follow it are a
/// constructor call rather than a grouping. `UnaryExpr(NewKeyword, CallExpr(Widget, 1, 2))` — which is what the
/// old rule built for `new Widget(1, 2)` — says the operand is a call, which is a different construct.
fn parse_new_expr(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::NewExpr);
    p.bump(); // `new`

    if let Err(err) = parse_new_type_and_initializer(p) {
        p.close_marks_above(base);
        return Err(err);
    }
    if let Err(err) = parse_new_declarator_suffixes(p) {
        p.close_marks_above(base);
        return Err(err);
    }
    if let Err(err) = parse_new_initializer(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    // `new Widget(1)->run()` and `new int[4][i]` are one expression, so the postfix loop continues from the
    // allocation rather than from whatever follows it.
    let allocation = m.complete(p);
    parse_postfix_suffixes(p, allocation)
}

/// Parse the type a `new` expression allocates, with the placement arguments that may precede it.
///
/// `new` is the one expression whose operand is a **type**, and the type grammar is the only thing that can
/// read one. What makes it awkward rather than trivial is that `new` also takes an optional parenthesised
/// *expression* before the type — placement new, `new (buffer) Widget()` — so the `(` at the cursor is either
/// the start of the allocation type's own declarator or a placement list, and the tokens do not say which:
///
/// ```text
/// new (buf) Widget()     placement: the parentheses hold an expression
/// new (Widget)()         no placement: the parentheses hold a type, and are the initializer
/// ```
///
/// # How the two are told apart
///
/// A cheap check first, then a backtrack for what it cannot decide. The check claims only the shapes that
/// *cannot* be a type — a list with a comma in it, or an element that starts with a literal or a call — which
/// is what both of the placement forms that appear in practice look like: `new (buf, size) Widget()` and
/// `new (1) Widget()`. Everything else is genuinely ambiguous, because `(buf)` and `(Widget)` are the same
/// token sequence, so it is settled by trying: the placement reading is attempted, and if no type follows the
/// parentheses the whole attempt is rewound and the type reading takes them instead.
///
/// The rollback is why the speculative region starts *before* the parentheses rather than after them: in
/// `new (Widget)()` the placement reading consumes `(Widget)`, finds `(` where the type should be, and the
/// rewind has to put back both the parentheses and the expression node built inside them.
fn parse_new_type_and_initializer(p: &mut CppParser) -> ParseResult {
    if p.current_token() == CppTokenKind::LeftParen {
        let checkpoint = p.checkpoint();

        if placement_arguments_at(p) {
            let placement = p.mark(CppSyntaxKind::ArgumentList);
            super::decls::parse_expression_list(p, CppTokenKind::RightParen)?;
            placement.complete(p);
            return parse_a_type_here(p);
        }

        // The parentheses could be either, so the type reading is tried *with* them — which is the reading
        // that makes `new (Widget)(1)` an allocation of `Widget` initialised with `1`, and the one the
        // placement reading cannot produce because it would leave the type nowhere to be.
        //
        // The placement reading is then only available when the type reading refuses, and the two are
        // distinguished by the whole statement rather than by the parentheses: `new (buf) Widget()` fails here
        // at the type — `(buf) Widget` is not a type — and `new (Widget)(1)` succeeds.
        if parse_a_type_here(p).is_ok() {
            return Ok(crate::parser::CompleteMarker::empty());
        }
        p.rollback(checkpoint);

        // `new (buf) Widget()`: a placement list, and then the type it allocates.
        let placement = p.mark(CppSyntaxKind::ArgumentList);
        super::decls::parse_expression_list(p, CppTokenKind::RightParen)?;
        placement.complete(p);
    }

    parse_a_type_here(p)
}

/// Parse a type-id at the cursor, requiring that it consume something.
///
/// [`super::types::parse_type_id`] succeeds having consumed nothing when there is no type, which is the correct
/// answer for a caller that only has to *attach* a type and is the wrong one here: `new` with no type is not a
/// `new` at all. The progress check turns "it parsed" into "there was a type".
/// Parse a type-id at the cursor, requiring that it consume something.
///
/// [`super::types::parse_type_id_with`] succeeds having consumed nothing when there is no type, which is the
/// correct answer for a caller that only has to *attach* a type and the wrong one here: `new` with no type is
/// not a `new` at all. The progress check turns "it parsed" into "there was a type".
///
/// `a_name_may_be_a_type` is `false` and that is the whole reason this wrapper exists: in a `new`, the
/// parentheses before the type may be a **placement list**, so `new (Widget)(1)` and `new (Widget*)()` must be
/// told apart by whether `Widget` is a type the file declares — not by whether it looks like one. See
/// [`super::types::parse_type_id_with`].
fn parse_a_type_here(p: &mut CppParser) -> ParseResult {
    let before = p.current_token_index();

    super::types::parse_type_id_with(p, false)?;
    if p.current_token_index() == before {
        return Err(CppParseError::syntax_error_from(
            "expected a type to allocate",
            p.current_token_range(),
        ));
    }

    Ok(crate::parser::CompleteMarker::empty())
}

/// Are the parentheses at the cursor certainly a placement-argument list rather than the start of a type?
///
/// Only the two shapes that cannot be a type are claimed — see [`parse_new_type_and_initializer`] for why the
/// rest is left to a backtrack. Answered as "cannot be a type" rather than "is an expression" so that the
/// answer stays stable as the type grammar grows: every token it learns to read only makes this *more* often
/// false, which is the safe direction — a placement list read as a type fails the parse and is rewound, while
/// a type read as a placement list would consume the allocation's own type as an expression.
fn placement_arguments_at(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::LeftParen {
        return false;
    }

    // The tokens of the list, with the opening `(` dropped so that position 0 is the first element's first
    // token. That is what makes "is this token at an element start?" a question about the previous token.
    let scan = p.peek_token_kind_at(1..64);
    let mut depth = 0isize;

    for (position, kind) in scan.iter().enumerate() {
        match kind {
            // A `(` where a type would have to be is a call, and a call is a value — so this is the
            // `new (buf()) T()` form. Checked before the bracket arm below, which counts the same token as
            // nesting.
            CppTokenKind::LeftParen if at_element_start(&scan, position) => return true,
            CppTokenKind::LeftParen | CppTokenKind::LeftBracket | CppTokenKind::LeftBrace => {
                depth += 1
            }
            CppTokenKind::RightParen | CppTokenKind::RightBracket | CppTokenKind::RightBrace => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            // A comma at the list's own depth separates elements rather than nesting, and two elements cannot
            // be one type-id.
            CppTokenKind::Comma if depth == 0 => return true,
            // An element that begins with a literal is a value. `new (p, size) T()` is the form this catches.
            CppTokenKind::IntegerLiteral
            | CppTokenKind::FloatingLiteral
            | CppTokenKind::StringLiteral
            | CppTokenKind::CharLiteral
            | CppTokenKind::TrueKeyword
            | CppTokenKind::FalseKeyword
            | CppTokenKind::NullptrKeyword
                if at_element_start(&scan, position) =>
            {
                return true
            }
            CppTokenKind::Eof | CppTokenKind::None => return false,
            _ => {}
        }
    }

    false
}

/// Is the token at `position` the first of an element of the list being scanned?
///
/// The list's first token is one, and so is the token after a top-level `,`. Anything else is in the middle of
/// an element, which is what keeps `(Widget)` from being read as a value: its `W` is an identifier at an
/// element start, and identifiers are not evidence — they are the token that a type and an expression share.
fn at_element_start(scan: &[CppTokenKind], position: usize) -> bool {
    position == 0 || scan.get(position - 1) == Some(&CppTokenKind::Comma)
}

/// Parse the array bounds of a `new` expression: the `[4]` of `new int[4]`, or the `[]` of `new int[]`.
///
/// The bound is optional in both directions: `new int[]` is an array of unknown bound, and a `new` of a
/// non-array type has no brackets at all. A `new int[2][3]` is two bounds, which is why this is a loop rather
/// than one branch.
fn parse_new_declarator_suffixes(p: &mut CppParser) -> ParseResult {
    while p.current_token() == CppTokenKind::LeftBracket {
        let array = p.mark(CppSyntaxKind::ArrayType);
        p.bump(); // `[`

        if p.current_token() != CppTokenKind::RightBracket
            && !p.is_eof()
            && let Err(err) = parse_expr(p)
        {
            array.undo(p);
            return Err(err);
        }
        if let Err(err) = expect_token(p, CppTokenKind::RightBracket) {
            array.undo(p);
            return Err(err);
        }
        array.complete(p);
    }

    Ok(crate::parser::CompleteMarker::empty())
}

/// Parse the initializer a `new` expression may carry: `new Widget(1, 2)`, `new int{3}`.
///
/// An empty `()` is kept as an initializer rather than dropped: `new Widget()` and `new Widget` are the same
/// allocation, and keeping the parentheses is what lets a consumer tell "value-initialised explicitly" from
/// "default-initialised", which is a distinction C++ makes and a formatter has to preserve.
fn parse_new_initializer(p: &mut CppParser) -> ParseResult {
    match p.current_token() {
        CppTokenKind::LeftParen => {
            let init = p.mark(CppSyntaxKind::Initializer);
            if let Err(err) =
                super::decls::parse_expression_list(p, CppTokenKind::RightParen)
            {
                init.undo(p);
                return Err(err);
            }
            init.complete(p);
        }
        CppTokenKind::LeftBrace => {
            let init = p.mark(CppSyntaxKind::Initializer);
            if let Err(err) = super::decls::parse_braced_initializer(p) {
                init.undo(p);
                return Err(err);
            }
            init.complete(p);
        }
        _ => {}
    }

    Ok(crate::parser::CompleteMarker::empty())
}

/// 解析后缀表达式 (函数调用、数组访问、成员访问、后增后减等)
fn parse_postfix_expr(p: &mut CppParser) -> ParseResult {
    let expr = parse_primary_expr(p)?;
    parse_postfix_suffixes(p, expr)
}

/// The suffixes that may follow a complete expression: calls, indexes, member access, `++`/`--`.
///
/// Split out from [`parse_postfix_expr`] because one construct reaches it with an expression already in hand:
/// [`parse_new_expr`] parses `new T(args)` as an allocation and then has to keep reading, since
/// `new Widget(1)->run()` is one expression and `new int[4][i]` another. Having two copies of the suffix loop
/// is how the two would come to disagree about what a suffix is.
///
/// `expr` must be a completed node; it is re-parented by whichever suffix is found.
fn parse_postfix_suffixes(p: &mut CppParser, mut expr: crate::parser::CompleteMarker) -> ParseResult {
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
            // A braced initializer after an expression: `Vec<int>{1, 2}`, `std::string{"x"}`,
            // `std::pair<int, int>{1, 2}`.
            //
            // This is C++11's list-initialisation of a *temporary*, and it is the one place a `{` follows an
            // expression rather than opening a block. Reading it here rather than in the primary rule is what
            // makes the type optional to the parser: `Vec<int>` is a name or a template-id that the expression
            // grammar already reads, and all that was missing was the braces after it.
            //
            // A `{` that is *not* this — the block of a lambda, a compound statement after a declaration —
            // never reaches the loop, because the lambda consumes its own body and a statement's `{` is the
            // next statement rather than a suffix of this expression.
            CppTokenKind::LeftBrace => {
                let marks_before = p.open_marks();
                let m = expr.precede(p, CppSyntaxKind::InitListExpr);
                if let Err(err) = super::decls::parse_braced_initializer(p) {
                    // The node is closed as if its `}` had been present, which is the same recovery every other
                    // rule uses and is *not* the same as dropping it: `precede` may have re-parented the events
                    // of the expression already parsed into this node, and a marker left open over them would
                    // leave a forward reference the tree builder cannot resolve. Detaching the node without an
                    // event is what the mutation tests catch, and closing it is what keeps the braces in the
                    // tree as an initializer that is merely incomplete.
                    p.close_marks_above(marks_before);
                    return Err(err);
                }
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
                // A segment of the name. `~Foo` and `operator+` are segments too — they are how a destructor and
                // an operator are named — and refusing them is what made `Foo::~Foo()` and `Foo::operator+()`
                // report `expected a name after '::'` against perfectly ordinary definitions. Both are written
                // *after* a `::`, so nothing else can be at this position.
                match p.current_token() {
                    CppTokenKind::Identifier => p.bump(),
                    CppTokenKind::Tilde => {
                        p.bump();
                        if p.current_token() == CppTokenKind::Identifier {
                            p.bump();
                        }
                        break;
                    }
                    CppTokenKind::OperatorKeyword => {
                        super::types::parse_operator_name_here(p)?;
                        break;
                    }
                    _ => {
                        p.close_marks_above(base);
                        return Err(CppParseError::syntax_error_from(
                            "expected a name after `::`",
                            p.current_token_range(),
                        ));
                    }
                }

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

        // A C-style cast, or a parenthesised expression. `(int)x` and `(x)` are the same first three tokens,
        // and the type reading has to be tried first because it is the one that can be refused — but it has to
        // be *asked* first as well, or every parenthesised expression would become a cast of a name.
        //
        // Hence two stages: the cheap check says whether a cast is worth trying, and the try itself says
        // whether it worked. A cast that fails is not an error — it is a parenthesised expression, and the
        // rewind hands the tokens to that rule. That is what keeps the check from having to be exact: it only
        // has to avoid the common `(a)`, `(a + b)`, `((a))` shapes, and everything it cannot decide is settled
        // by the parse. `struct Widget {}; (Widget){1}` is the case that made this worth doing: the check says
        // cast, the cast finds no operand, and the expression reading takes over.
        CppTokenKind::LeftParen if is_a_type_in_parentheses(p) => {
            let checkpoint = p.checkpoint();
            let m = p.mark(CppSyntaxKind::CastExpr);
            p.bump(); // `(`

            let parsed = super::types::parse_type_id(p)
                .and_then(|_| expect_token(p, CppTokenKind::RightParen))
                // The operand of a cast is a unary expression, which is what keeps `(int)a + b` a sum of a cast
                // and `b` rather than a cast of `a + b`.
                .and_then(|_| parse_unary_expr(p));

            match parsed {
                Ok(_) => Ok(m.complete(p)),
                Err(_) => {
                    // Not a cast after all: the whole attempt — node, tokens and events — goes back.
                    p.rollback(checkpoint);
                    parse_parenthesized_expression(p)
                }
            }
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
        CppTokenKind::LeftParen => parse_parenthesized_expression(p),

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

/// Parse `( expression )` as a node of its own, keeping the parentheses.
///
/// Shared by the two paths that reach a `(` in expression position: the plain parenthesised expression, and a
/// cast attempt that failed and has to be read that way instead. Two copies of this rule would be how they
/// come to disagree about what a parenthesised expression is.
fn parse_parenthesized_expression(p: &mut CppParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::ParenExpr);
    expect_token(p, CppTokenKind::LeftParen)?;
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
        Ampersand, Arrow, Assign, Caret, CharLiteral, Colon, Comma, Dot, Ellipsis, Equal,
        FalseKeyword, FloatingLiteral, Greater, GreaterEqual, Identifier, IntegerLiteral, LeftBrace,
        LeftBracket, LeftParen, LeftShift, Less, LessEqual, LogicalAnd, LogicalNot, LogicalOr, Minus,
        MinusMinus, MutableKeyword, NoexceptKeyword, NotEqual, NullptrKeyword, Percent, Pipe, Plus,
        PlusPlus, Question, RightBracket, RightParen, RightShift, Scope, Slash, Star, StringLiteral,
        ThisKeyword, Tilde, TrueKeyword,
    };

    // The capture list's own contents, so that a malformed one stops the reading rather than swallowing the
    // rest of the expression. Relative offsets: `0` is the `[` at the cursor, so the scan starts at `1`.
    //
    // The set is "anything an init-capture's initialiser can be made of", and it has to be that wide because
    // `[value = total + 1]` is one capture whose initialiser is an ordinary expression: its *operators* are as
    // much a part of the list as its names. Allowing too little is how this came to read `[value` as an index —
    // the `+` was not in the set, so the `[` was declared not to be a lambda introducer and the index rule took
    // the tokens instead.
    //
    // Allowing too much costs nothing here: the second half of the check is what actually decides, and it asks
    // about the token *after* the `]`. `arr[i] + 1` passes this scan and is still an index, because what follows
    // the bracket is `+` rather than `(`, `{` or a qualifier.
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
            // An init-capture's initialiser, which is an expression: its literals, its parentheses, and the
            // operators between them.
            | IntegerLiteral
            | FloatingLiteral
            | StringLiteral
            | CharLiteral
            | TrueKeyword
            | FalseKeyword
            | NullptrKeyword
            | LeftParen
            | RightParen
            | LeftBracket
            | Plus
            | Minus
            | Slash
            | Percent
            | Caret
            | Pipe
            | Tilde
            | LogicalNot
            | LogicalAnd
            | LogicalOr
            | Equal
            | NotEqual
            | Less
            | Greater
            | LessEqual
            | GreaterEqual
            | LeftShift
            | RightShift
            | PlusPlus
            | MinusMinus
            | Arrow
            | Dot
            | Question
            | Colon => {}
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
