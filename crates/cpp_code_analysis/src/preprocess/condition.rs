//! `#if`, evaluated the way a preprocessor evaluates it — including the parts that are not obvious.
//!
//! # Why this is not the expression grammar
//!
//! A conditional directive's expression looks like C++ and is not:
//!
//! * **`defined` is an operator here and nothing anywhere else.** Its operand is *not* macro-expanded,
//!   and `defined X` without parentheses is legal — so it cannot be handled by expanding first or by
//!   reusing a function-call rule.
//! * **An undefined identifier is `0`, not an error.** `#if FOO` with no `FOO` is false, silently. The
//!   whole feature-flag idiom depends on it.
//! * **Arithmetic is on `intmax_t`/`uintmax_t`**, not on the types written, and division by zero is
//!   defined to be a diagnosed error rather than a trap.
//! * **`true` and `false` are not keywords**; in a conditional they are just identifiers, and are `0`
//!   unless a macro defines them.
//!
//! So the tokens are read here, by a small recursive-descent parser over the directive's tokens.
//!
//! # Three values, not two
//!
//! [`Value::Unknown`] is not an approximation of `true` or `false` — it is the honest answer for the
//! case that dominates real code:
//!
//! ```text
//! #if _MSC_VER > 1900        // what is _MSC_VER on this machine? Not "0".
//! #if __cplusplus >= 202002L // depends which -std the build actually used.
//! ```
//!
//! A consumer that folded `Unknown` into `false` would grey out code that is being compiled, and one
//! that folded it into `true` would grey out the other half. Both are wrong, and both are worse than
//! saying "I do not know", so the caller gets to decide what to do about it.

use cpp_parser::{CppTokenKind, SourceRange};

use crate::{macros::MacroDef, token::Token};

/// The value of a conditional expression, or the reason there is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    /// A number. All arithmetic in a conditional is on `intmax_t`.
    Known(i128),
    /// The answer depends on something not known here: a macro whose value cannot be read, an
    /// identifier that is defined but not as a number, an operand that failed to parse.
    Unknown,
}

impl Value {
    /// Is the expression true?
    ///
    /// `#if` is true when its value is non-zero, so this is the whole use of an evaluated condition.
    pub fn is_true(self) -> Option<bool> {
        match self {
            Value::Known(value) => Some(value != 0),
            Value::Unknown => None,
        }
    }

    pub fn known(self) -> Option<i128> {
        match self {
            Value::Known(value) => Some(value),
            Value::Unknown => None,
        }
    }
}

/// Why a condition could not be parsed.
///
/// Not an error the user should see. A condition that does not parse means the region's visibility is
/// unknown, which is the normal state of a file being typed — `#if ` with the cursor after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// The tokens ended in the middle of an expression.
    UnexpectedEnd,
    /// A token appeared where an operand was expected.
    ExpectedOperand { found: CppTokenKind },
    /// An operator was left without its right-hand side.
    ExpectedOperator { found: CppTokenKind },
    /// Tokens were left over after a complete expression.
    TrailingTokens { count: usize },
    /// `defined` was not followed by a name.
    ExpectedDefinedName,
    /// An unterminated `(`.
    UnclosedParenthesis,
}

/// One node of a parsed condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionExpr {
    /// A literal.
    Number(i128),
    /// `defined(NAME)` or `defined NAME`.
    Defined(Box<str>),
    /// An identifier that is not `defined`: a macro to expand, or a name that is `0`.
    Identifier(Box<str>),
    /// A parenthesised expression. Kept as a node so that evaluation re-parses nothing.
    Group(Box<ConditionExpr>),
    Unary {
        op: UnaryOp,
        operand: Box<ConditionExpr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<ConditionExpr>,
        right: Box<ConditionExpr>,
    },
    Conditional {
        condition: Box<ConditionExpr>,
        then_value: Box<ConditionExpr>,
        else_value: Box<ConditionExpr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Plus,
    Minus,
    LogicalNot,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Multiply,
    Divide,
    Remainder,
    Add,
    Subtract,
    ShiftLeft,
    ShiftRight,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
    BitAnd,
    BitXor,
    BitOr,
    LogicalAnd,
    LogicalOr,
}

/// What a table has to say about one name.
///
/// Three answers rather than an `Option`, because the two ways of *not* having a name are different
/// answers and folding them together is how a wrong answer gets produced:
///
/// ```text
/// #ifdef _WIN32      with _WIN32 nowhere in the table
///   Undefined   — this table has read everything there is, and _WIN32 is not defined: false.
///   Unanswered  — this table has read a file, or an index, and _WIN32 may be defined elsewhere
///                 (the compiler predefines it): the honest answer is `Value::Unknown`.
/// ```
///
/// A table built from *one file's own directives* knows everything that file says and nothing about
/// what its `#include`s brought in, so which variant it returns is a statement about the table, not
/// about the name. See [`MacroValues`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup<'a> {
    /// Defined, with the definition: `defined(NAME)` is true, and its body may give a value.
    Defined(&'a MacroDef),
    /// Defined, and this table does not hold the body — so `defined(NAME)` is true and the *value* is
    /// not known. `#define NAME` and `#define NAME 1` are the same answer here, and a condition that
    /// asks for the value (`#if NAME == 1`) comes out [`Value::Unknown`] rather than as a guess.
    ///
    /// This is what an **index** can say about a name: a summary stores macro facts without bodies
    /// (see `MacroFact`), so "is this a macro here" is answerable and "what is its value" is not.
    DefinedWithoutAValue,
    /// Certainly not defined: the standard's `0`, and `defined(NAME)` is false.
    Undefined,
    /// Not known from here — neither `0` nor false, so the condition becomes [`Value::Unknown`].
    Unanswered,
}

impl<'a> Lookup<'a> {
    /// The body, when this table holds one: what an expansion pastes, and what a value is read from.
    ///
    /// `None` for the other three answers, for the same reason in each case: there is nothing to paste, and
    /// pasting nothing would delete the use rather than leave it in place.
    pub fn definition(self) -> Option<&'a MacroDef> {
        match self {
            Lookup::Defined(definition) => Some(definition),
            Lookup::DefinedWithoutAValue | Lookup::Undefined | Lookup::Unanswered => None,
        }
    }

    /// Is the name certainly a macro? The question `defined(NAME)`, `#ifdef` and `#ifndef` ask.
    ///
    /// `None` when the table cannot say — the answer that must not be folded into `false`.
    pub fn is_defined(self) -> Option<bool> {
        match self {
            Lookup::Defined(_) | Lookup::DefinedWithoutAValue => Some(true),
            Lookup::Undefined => Some(false),
            Lookup::Unanswered => None,
        }
    }
}

/// Look up macros while evaluating.
///
/// A trait rather than a `&MacroTable` because evaluation happens in more than one place with
/// different tables: over a file's own directives, over a file's directives plus everything its
/// includes brought in, and — at query time — over what a compiler predefines. The evaluator should
/// not have to know which, and [`Lookup`] is what lets each of them answer at its own strength: the
/// **answer** carries whether the table is speaking about a name it read or about one it never saw, so
/// no caller has to decide that on the table's behalf.
pub trait MacroValues {
    /// What this table knows about `name` at the position being evaluated.
    fn lookup(&self, name: &str) -> Lookup<'_>;
}

/// A table that defines nothing, so every identifier is `0`.
///
/// The useful default: `#if FOO` in a file with no macros at all is `0` by the standard, and a caller
/// evaluating a condition with no context should get that answer rather than `Unknown`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoMacros;

impl MacroValues for NoMacros {
    fn lookup(&self, _name: &str) -> Lookup<'_> {
        Lookup::Undefined
    }
}

impl MacroValues for crate::macros::MacroTable {
    fn lookup(&self, name: &str) -> Lookup<'_> {
        self.get(name).map_or(Lookup::Undefined, Lookup::Defined)
    }
}

/// Parse a condition from the tokens that follow `#if` or `#elif`.
pub fn parse_condition(tokens: &[Token]) -> Result<ConditionExpr, EvalError> {
    let mut parser = Parser { tokens, index: 0 };
    let expr = parser.conditional()?;

    if parser.index < tokens.len() {
        return Err(EvalError::TrailingTokens {
            count: tokens.len() - parser.index,
        });
    }

    Ok(expr)
}

/// Parse and evaluate in one step.
///
/// A condition that does not parse is [`Value::Unknown`] rather than an error, because that is what
/// the answer means to every caller: this region's visibility cannot be decided.
pub fn evaluate(tokens: &[Token], macros: &impl MacroValues) -> Value {
    match parse_condition(tokens) {
        Ok(expr) => eval(&expr, macros),
        Err(_) => Value::Unknown,
    }
}

/// Evaluate a parsed condition.
pub fn eval(expr: &ConditionExpr, macros: &impl MacroValues) -> Value {
    match expr {
        ConditionExpr::Number(value) => Value::Known(*value),
        // `defined` asks one question — is the name a macro — and the two ways of being a macro answer it
        // the same way. `Unanswered` is not `false`: see [`Lookup`].
        ConditionExpr::Defined(name) => match macros.lookup(name).is_defined() {
            Some(defined) => Value::Known(i128::from(defined)),
            None => Value::Unknown,
        },
        ConditionExpr::Identifier(name) => match macros.lookup(name) {
            // Undefined is `0`. This is the rule the whole feature-flag idiom rests on, and it is not
            // an error, which is why it is a `Known` value and not `Unknown`.
            Lookup::Undefined => Value::Known(0),
            // A name that is a macro whose body this table does not hold: `#define FOO` and `#define FOO 1`
            // are the same answer here, and reading a value out of an empty body would be a guess.
            Lookup::DefinedWithoutAValue => Value::Unknown,
            Lookup::Defined(definition) => macro_value(definition),
            // The name may be defined by something this table has not read, and the standard's `0` is a
            // statement about a *complete* input. So: not known, rather than not defined.
            Lookup::Unanswered => Value::Unknown,
        },
        ConditionExpr::Group(inner) => eval(inner, macros),
        ConditionExpr::Unary { op, operand } => {
            let Some(value) = eval(operand, macros).known() else {
                return Value::Unknown;
            };
            Value::Known(match op {
                UnaryOp::Plus => value,
                UnaryOp::Minus => value.wrapping_neg(),
                UnaryOp::LogicalNot => i128::from(value == 0),
                UnaryOp::BitNot => !value,
            })
        }
        // `&&` and `||` short-circuit, and that is observable: `#if defined(X) && X > 2` is legal
        // even when `X` is undefined, because the right side is never evaluated.
        ConditionExpr::Binary {
            op: BinaryOp::LogicalAnd,
            left,
            right,
        } => match eval(left, macros).is_true() {
            Some(false) => Value::Known(0),
            Some(true) => match eval(right, macros).is_true() {
                Some(value) => Value::Known(i128::from(value)),
                None => Value::Unknown,
            },
            None => Value::Unknown,
        },
        ConditionExpr::Binary {
            op: BinaryOp::LogicalOr,
            left,
            right,
        } => match eval(left, macros).is_true() {
            Some(true) => Value::Known(1),
            Some(false) => match eval(right, macros).is_true() {
                Some(value) => Value::Known(i128::from(value)),
                None => Value::Unknown,
            },
            None => Value::Unknown,
        },
        ConditionExpr::Binary { op, left, right } => {
            let (Some(left), Some(right)) =
                (eval(left, macros).known(), eval(right, macros).known())
            else {
                return Value::Unknown;
            };
            eval_binary(*op, left, right)
        }
        ConditionExpr::Conditional {
            condition,
            then_value,
            else_value,
        } => match eval(condition, macros).is_true() {
            Some(true) => eval(then_value, macros),
            Some(false) => eval(else_value, macros),
            // Both branches are evaluated only when neither can be ruled out — which is the same rule
            // the visibility layer applies to an `#if`/`#else` pair, one level down.
            None => {
                let then = eval(then_value, macros);
                let otherwise = eval(else_value, macros);
                match (then.known(), otherwise.known()) {
                    (Some(a), Some(b)) if a == b => Value::Known(a),
                    _ => Value::Unknown,
                }
            }
        },
    }
}

/// The numeric value of a defined macro.
///
/// A macro whose body is a single number is that number. Anything else — an empty body, an expression,
/// a string — is `Unknown`: the standard says the macro is *expanded* and the result re-read, and
/// doing that properly is the expansion engine's job, not this evaluator's. Guessing here would be
/// worse than not answering, because `#if FOO` deciding "true" on no evidence would visibly compile
/// the wrong branch.
fn macro_value(definition: &MacroDef) -> Value {
    let mut significant = definition.body.significant();

    let Some(first) = significant.next() else {
        // `#define FOO` with an empty body. In `#if FOO` this is a syntax error in a real
        // preprocessor (an empty expression); treating it as `Unknown` lets the caller stay quiet.
        return Value::Unknown;
    };
    if significant.next().is_some() {
        return Value::Unknown;
    }

    match first.kind {
        CppTokenKind::IntegerLiteral => {
            parse_integer(first.text()).map_or(Value::Unknown, Value::Known)
        }
        _ => Value::Unknown,
    }
}

fn eval_binary(op: BinaryOp, left: i128, right: i128) -> Value {
    let value = match op {
        BinaryOp::Multiply => left.wrapping_mul(right),
        // Division by zero in a conditional is a diagnosed error in the standard. There is no error
        // channel here, so it becomes `Unknown`, which has the same effect on the caller — the region
        // cannot be decided — without inventing a value.
        BinaryOp::Divide => match left.checked_div(right) {
            Some(value) => value,
            None => return Value::Unknown,
        },
        BinaryOp::Remainder => match left.checked_rem(right) {
            Some(value) => value,
            None => return Value::Unknown,
        },
        BinaryOp::Add => left.wrapping_add(right),
        BinaryOp::Subtract => left.wrapping_sub(right),
        // A shift past the width is undefined in C; saturating keeps it total without pretending the
        // result is meaningful, because the result of the *comparison* is usually what is asked.
        BinaryOp::ShiftLeft => left.wrapping_shl(right.clamp(0, 127) as u32),
        BinaryOp::ShiftRight => left.wrapping_shr(right.clamp(0, 127) as u32),
        BinaryOp::Less => i128::from(left < right),
        BinaryOp::LessEqual => i128::from(left <= right),
        BinaryOp::Greater => i128::from(left > right),
        BinaryOp::GreaterEqual => i128::from(left >= right),
        BinaryOp::Equal => i128::from(left == right),
        BinaryOp::NotEqual => i128::from(left != right),
        BinaryOp::BitAnd => left & right,
        BinaryOp::BitXor => left ^ right,
        BinaryOp::BitOr => left | right,
        // Handled by the short-circuiting arms above; reaching here means an operand was not a plain
        // number, which `&&`/`||` do not care about.
        BinaryOp::LogicalAnd => i128::from(left != 0 && right != 0),
        BinaryOp::LogicalOr => i128::from(left != 0 || right != 0),
    };

    Value::Known(value)
}

/// Read an integer literal, C-style: decimal, octal, hexadecimal, binary, with digit separators and
/// an integer suffix.
///
/// Not `str::parse`, because `0x10`, `010`, `0b1010` and `1'000` are all valid and only the first
/// would be recognised. The suffix (`u`, `l`, `ll`, and combinations) is accepted and discarded: a
/// conditional does not care, and refusing to read `1LL` would make a common spelling unevaluable.
pub fn parse_integer(text: &str) -> Option<i128> {
    let cleaned: String = text.chars().filter(|c| *c != '\'').collect();
    let body = cleaned.trim_end_matches(['u', 'U', 'l', 'L']);

    if body.is_empty() {
        return None;
    }

    let (digits, radix) =
        if let Some(rest) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (rest, 16)
        } else if let Some(rest) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            (rest, 2)
        } else if body.len() > 1 && body.starts_with('0') {
            (&body[1..], 8)
        } else {
            (body, 10)
        };

    i128::from_str_radix(digits, radix).ok()
}

/// A recursive-descent reader for a conditional's tokens.
///
/// The precedences are C's, from loosest to tightest, which is why the entry point is the conditional
/// operator and the bottom of the chain is a primary.
struct Parser<'a> {
    tokens: &'a [Token],
    index: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.index)
    }

    fn kind(&self) -> Option<CppTokenKind> {
        self.peek().map(|token| token.kind)
    }

    fn bump(&mut self) -> Option<&'a Token> {
        let token = self.tokens.get(self.index);
        if token.is_some() {
            self.index += 1;
        }
        token
    }

    fn eat(&mut self, kind: CppTokenKind) -> bool {
        if self.kind() == Some(kind) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    /// `a ? b : c`, right-associative.
    fn conditional(&mut self) -> Result<ConditionExpr, EvalError> {
        let condition = self.logical_or()?;
        if !self.eat(CppTokenKind::Question) {
            return Ok(condition);
        }

        let then_value = self.conditional()?;
        if !self.eat(CppTokenKind::Colon) {
            return Err(EvalError::ExpectedOperator {
                found: self.kind().unwrap_or(CppTokenKind::Eof),
            });
        }
        let else_value = self.conditional()?;

        Ok(ConditionExpr::Conditional {
            condition: Box::new(condition),
            then_value: Box::new(then_value),
            else_value: Box::new(else_value),
        })
    }

    fn logical_or(&mut self) -> Result<ConditionExpr, EvalError> {
        let mut left = self.logical_and()?;
        while self.eat(CppTokenKind::LogicalOr) {
            let right = self.logical_and()?;
            left = binary(BinaryOp::LogicalOr, left, right);
        }
        Ok(left)
    }

    fn logical_and(&mut self) -> Result<ConditionExpr, EvalError> {
        let mut left = self.bit_or()?;
        while self.eat(CppTokenKind::LogicalAnd) {
            let right = self.bit_or()?;
            left = binary(BinaryOp::LogicalAnd, left, right);
        }
        Ok(left)
    }

    fn bit_or(&mut self) -> Result<ConditionExpr, EvalError> {
        let mut left = self.bit_xor()?;
        while self.eat(CppTokenKind::Pipe) {
            let right = self.bit_xor()?;
            left = binary(BinaryOp::BitOr, left, right);
        }
        Ok(left)
    }

    fn bit_xor(&mut self) -> Result<ConditionExpr, EvalError> {
        let mut left = self.bit_and()?;
        while self.eat(CppTokenKind::Caret) {
            let right = self.bit_and()?;
            left = binary(BinaryOp::BitXor, left, right);
        }
        Ok(left)
    }

    fn bit_and(&mut self) -> Result<ConditionExpr, EvalError> {
        let mut left = self.equality()?;
        while self.eat(CppTokenKind::Ampersand) {
            let right = self.equality()?;
            left = binary(BinaryOp::BitAnd, left, right);
        }
        Ok(left)
    }

    fn equality(&mut self) -> Result<ConditionExpr, EvalError> {
        let mut left = self.relational()?;
        loop {
            let op = match self.kind() {
                Some(CppTokenKind::Equal) => BinaryOp::Equal,
                Some(CppTokenKind::NotEqual) => BinaryOp::NotEqual,
                _ => break,
            };
            self.index += 1;
            let right = self.relational()?;
            left = binary(op, left, right);
        }
        Ok(left)
    }

    fn relational(&mut self) -> Result<ConditionExpr, EvalError> {
        let mut left = self.shift()?;
        loop {
            let op = match self.kind() {
                Some(CppTokenKind::Less) => BinaryOp::Less,
                Some(CppTokenKind::LessEqual) => BinaryOp::LessEqual,
                Some(CppTokenKind::Greater) => BinaryOp::Greater,
                Some(CppTokenKind::GreaterEqual) => BinaryOp::GreaterEqual,
                _ => break,
            };
            self.index += 1;
            let right = self.shift()?;
            left = binary(op, left, right);
        }
        Ok(left)
    }

    fn shift(&mut self) -> Result<ConditionExpr, EvalError> {
        let mut left = self.additive()?;
        loop {
            let op = match self.kind() {
                Some(CppTokenKind::LeftShift) => BinaryOp::ShiftLeft,
                Some(CppTokenKind::RightShift) => BinaryOp::ShiftRight,
                _ => break,
            };
            self.index += 1;
            let right = self.additive()?;
            left = binary(op, left, right);
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<ConditionExpr, EvalError> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.kind() {
                Some(CppTokenKind::Plus) => BinaryOp::Add,
                Some(CppTokenKind::Minus) => BinaryOp::Subtract,
                _ => break,
            };
            self.index += 1;
            let right = self.multiplicative()?;
            left = binary(op, left, right);
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<ConditionExpr, EvalError> {
        let mut left = self.unary()?;
        loop {
            let op = match self.kind() {
                Some(CppTokenKind::Star) => BinaryOp::Multiply,
                Some(CppTokenKind::Slash) => BinaryOp::Divide,
                Some(CppTokenKind::Percent) => BinaryOp::Remainder,
                _ => break,
            };
            self.index += 1;
            let right = self.unary()?;
            left = binary(op, left, right);
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<ConditionExpr, EvalError> {
        let op = match self.kind() {
            Some(CppTokenKind::Plus) => Some(UnaryOp::Plus),
            Some(CppTokenKind::Minus) => Some(UnaryOp::Minus),
            Some(CppTokenKind::LogicalNot) => Some(UnaryOp::LogicalNot),
            Some(CppTokenKind::Tilde) => Some(UnaryOp::BitNot),
            _ => None,
        };

        if let Some(op) = op {
            self.index += 1;
            let operand = self.unary()?;
            return Ok(ConditionExpr::Unary {
                op,
                operand: Box::new(operand),
            });
        }

        self.primary()
    }

    fn primary(&mut self) -> Result<ConditionExpr, EvalError> {
        let Some(token) = self.peek() else {
            return Err(EvalError::UnexpectedEnd);
        };

        match token.kind {
            CppTokenKind::IntegerLiteral
            | CppTokenKind::CharLiteral
            | CppTokenKind::BoolLiteral => {
                let token = self.bump().expect("checked");
                Ok(ConditionExpr::Number(
                    parse_integer(token.text()).unwrap_or(0),
                ))
            }
            CppTokenKind::LeftParen => {
                self.index += 1;
                let inner = self.conditional()?;
                if !self.eat(CppTokenKind::RightParen) {
                    return Err(EvalError::UnclosedParenthesis);
                }
                Ok(ConditionExpr::Group(Box::new(inner)))
            }
            CppTokenKind::Identifier => {
                // `defined` is an operator only here, and its operand is never expanded.
                if token.text() == "defined" {
                    return self.defined();
                }
                let token = self.bump().expect("checked");
                Ok(ConditionExpr::Identifier(token.text.clone()))
            }
            // `true` and `false` are not keywords in a conditional expression: they are identifiers,
            // and evaluate to `0` unless a macro gives them a value. The lexer produces them as
            // literals, so they are routed back to the identifier case.
            CppTokenKind::TrueKeyword | CppTokenKind::FalseKeyword => {
                let token = self.bump().expect("checked");
                Ok(ConditionExpr::Identifier(token.text.clone()))
            }
            found => Err(EvalError::ExpectedOperand { found }),
        }
    }

    /// `defined X` or `defined(X)`.
    fn defined(&mut self) -> Result<ConditionExpr, EvalError> {
        self.index += 1; // `defined`

        let parenthesised = self.eat(CppTokenKind::LeftParen);

        let Some(token) = self.peek() else {
            return Err(EvalError::ExpectedDefinedName);
        };
        if !token.is_identifier() {
            return Err(EvalError::ExpectedDefinedName);
        }
        let name = self.bump().expect("checked").text.clone();

        if parenthesised && !self.eat(CppTokenKind::RightParen) {
            return Err(EvalError::UnclosedParenthesis);
        }

        Ok(ConditionExpr::Defined(name))
    }
}

fn binary(op: BinaryOp, left: ConditionExpr, right: ConditionExpr) -> ConditionExpr {
    ConditionExpr::Binary {
        op,
        left: Box::new(left),
        right: Box::new(right),
    }
}

/// A condition's extent in the file, for diagnostics.
pub fn condition_range(tokens: &[Token]) -> Option<SourceRange> {
    let first = tokens.first()?;
    let last = tokens.last()?;
    Some(SourceRange::new(
        first.range.start_offset,
        last.range.end_offset() - first.range.start_offset,
    ))
}
