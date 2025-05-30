/// C++ Operator Kind Definitions
///
/// This module defines unary and binary operators for C++,
/// along with their precedence and associativity.

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum UnaryOperator {
    /// Logical NOT: !expr
    Not,
    /// Bitwise NOT: ~expr
    BitNot,
    /// Unary plus: +expr
    Plus,
    /// Unary minus: -expr
    Minus,
    /// Prefix increment: ++expr
    PreIncrement,
    /// Prefix decrement: --expr
    PreDecrement,
    /// Dereference: *expr
    Deref,
    /// Address-of: &expr
    AddressOf,
    /// Postfix increment: expr++
    PostIncrement,
    /// Postfix decrement: expr--
    PostDecrement,
    /// Sizeof: sizeof(expr)
    Sizeof,
    /// Typeid: typeid(expr)
    Typeid,
    /// New: new Type
    New,
    /// Delete: delete expr
    Delete,
    /// No operation (placeholder)
    Nop,
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum BinaryOperator {
    /// Addition: a + b
    Add,
    /// Subtraction: a - b
    Sub,
    /// Multiplication: a * b
    Mul,
    /// Division: a / b
    Div,
    /// Modulo: a % b
    Mod,
    /// Assignment: a = b
    Assign,
    /// Addition assignment: a += b
    AddAssign,
    /// Subtraction assignment: a -= b
    SubAssign,
    /// Multiplication assignment: a *= b
    MulAssign,
    /// Division assignment: a /= b
    DivAssign,
    /// Modulo assignment: a %= b
    ModAssign,
    /// Bitwise AND: a & b
    BitAnd,
    /// Bitwise OR: a | b
    BitOr,
    /// Bitwise XOR: a ^ b
    BitXor,
    /// Bitwise AND assignment: a &= b
    BitAndAssign,
    /// Bitwise OR assignment: a |= b
    BitOrAssign,
    /// Bitwise XOR assignment: a ^= b
    BitXorAssign,
    /// Left shift: a << b
    Shl,
    /// Right shift: a >> b
    Shr,
    /// Left shift assignment: a <<= b
    ShlAssign,
    /// Right shift assignment: a >>= b
    ShrAssign,
    /// Logical AND: a && b
    LogicalAnd,
    /// Logical OR: a || b
    LogicalOr,
    /// Logical XOR (not in C++, but for completeness)
    LogicalXor,
    /// Equal to: a == b
    Eq,
    /// Not equal to: a != b
    Neq,
    /// Less than: a < b
    Lt,
    /// Less than or equal to: a <= b
    Le,
    /// Greater than: a > b
    Gt,
    /// Greater than or equal to: a >= b
    Ge,
    /// Three-way comparison: a <=> b (C++20)
    Spaceship,
    /// Member access: a.b
    MemberAccess,
    /// Pointer member access: a->b
    PtrMemberAccess,
    /// Scope resolution: a::b
    Scope,
    /// Array subscript: a[b]
    Subscript,
    /// Function call: a(b)
    Call,
    /// Comma: a, b
    Comma,
    /// Conditional: a ? b : c (handled as ternary, but can be listed)
    Conditional,
    /// No operation (placeholder)
    Nop,
}

/// Operator precedence and associativity for C++
/// Lower number means lower precedence
#[derive(Debug, Clone, Copy)]
pub struct OperatorPrecedence {
    pub precedence: u8,
    pub right_associative: bool,
}

pub const UNARY_PRECEDENCE: u8 = 3; // Example: unary +, -, !, ~

pub const BINARY_PRECEDENCE: &[(BinaryOperator, OperatorPrecedence)] = &[
    // Precedence and associativity based on C++ standard
    (BinaryOperator::Scope, OperatorPrecedence { precedence: 1, right_associative: false }),
    (BinaryOperator::MemberAccess, OperatorPrecedence { precedence: 2, right_associative: false }),
    (BinaryOperator::PtrMemberAccess, OperatorPrecedence { precedence: 2, right_associative: false }),
    (BinaryOperator::Call, OperatorPrecedence { precedence: 2, right_associative: false }),
    (BinaryOperator::Subscript, OperatorPrecedence { precedence: 2, right_associative: false }),
    (BinaryOperator::Mul, OperatorPrecedence { precedence: 5, right_associative: false }),
    (BinaryOperator::Div, OperatorPrecedence { precedence: 5, right_associative: false }),
    (BinaryOperator::Mod, OperatorPrecedence { precedence: 5, right_associative: false }),
    (BinaryOperator::Add, OperatorPrecedence { precedence: 6, right_associative: false }),
    (BinaryOperator::Sub, OperatorPrecedence { precedence: 6, right_associative: false }),
    (BinaryOperator::Shl, OperatorPrecedence { precedence: 7, right_associative: false }),
    (BinaryOperator::Shr, OperatorPrecedence { precedence: 7, right_associative: false }),
    (BinaryOperator::Lt, OperatorPrecedence { precedence: 8, right_associative: false }),
    (BinaryOperator::Le, OperatorPrecedence { precedence: 8, right_associative: false }),
    (BinaryOperator::Gt, OperatorPrecedence { precedence: 8, right_associative: false }),
    (BinaryOperator::Ge, OperatorPrecedence { precedence: 8, right_associative: false }),
    (BinaryOperator::Eq, OperatorPrecedence { precedence: 9, right_associative: false }),
    (BinaryOperator::Neq, OperatorPrecedence { precedence: 9, right_associative: false }),
    (BinaryOperator::BitAnd, OperatorPrecedence { precedence: 10, right_associative: false }),
    (BinaryOperator::BitXor, OperatorPrecedence { precedence: 11, right_associative: false }),
    (BinaryOperator::BitOr, OperatorPrecedence { precedence: 12, right_associative: false }),
    (BinaryOperator::LogicalAnd, OperatorPrecedence { precedence: 13, right_associative: false }),
    (BinaryOperator::LogicalOr, OperatorPrecedence { precedence: 14, right_associative: false }),
    (BinaryOperator::Conditional, OperatorPrecedence { precedence: 15, right_associative: true }),
    (BinaryOperator::Assign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (BinaryOperator::AddAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (BinaryOperator::SubAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (BinaryOperator::MulAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (BinaryOperator::DivAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (BinaryOperator::ModAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (BinaryOperator::BitAndAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (BinaryOperator::BitOrAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (BinaryOperator::BitXorAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (BinaryOperator::ShlAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (BinaryOperator::ShrAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (BinaryOperator::Comma, OperatorPrecedence { precedence: 17, right_associative: false }),
    (BinaryOperator::Spaceship, OperatorPrecedence { precedence: 9, right_associative: false }),
];
