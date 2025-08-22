/// C++ Operator Kind Definitions
///
/// This module defines unary and binary operators for C++,
/// along with their precedence and associativity.

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum CppUnaryOperator {
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
pub enum CppBinaryOperator {
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

pub const BINARY_PRECEDENCE: &[(CppBinaryOperator, OperatorPrecedence)] = &[
    // Precedence and associativity based on C++ standard
    (CppBinaryOperator::Scope, OperatorPrecedence { precedence: 1, right_associative: false }),
    (CppBinaryOperator::MemberAccess, OperatorPrecedence { precedence: 2, right_associative: false }),
    (CppBinaryOperator::PtrMemberAccess, OperatorPrecedence { precedence: 2, right_associative: false }),
    (CppBinaryOperator::Call, OperatorPrecedence { precedence: 2, right_associative: false }),
    (CppBinaryOperator::Subscript, OperatorPrecedence { precedence: 2, right_associative: false }),
    (CppBinaryOperator::Mul, OperatorPrecedence { precedence: 5, right_associative: false }),
    (CppBinaryOperator::Div, OperatorPrecedence { precedence: 5, right_associative: false }),
    (CppBinaryOperator::Mod, OperatorPrecedence { precedence: 5, right_associative: false }),
    (CppBinaryOperator::Add, OperatorPrecedence { precedence: 6, right_associative: false }),
    (CppBinaryOperator::Sub, OperatorPrecedence { precedence: 6, right_associative: false }),
    (CppBinaryOperator::Shl, OperatorPrecedence { precedence: 7, right_associative: false }),
    (CppBinaryOperator::Shr, OperatorPrecedence { precedence: 7, right_associative: false }),
    (CppBinaryOperator::Lt, OperatorPrecedence { precedence: 8, right_associative: false }),
    (CppBinaryOperator::Le, OperatorPrecedence { precedence: 8, right_associative: false }),
    (CppBinaryOperator::Gt, OperatorPrecedence { precedence: 8, right_associative: false }),
    (CppBinaryOperator::Ge, OperatorPrecedence { precedence: 8, right_associative: false }),
    (CppBinaryOperator::Eq, OperatorPrecedence { precedence: 9, right_associative: false }),
    (CppBinaryOperator::Neq, OperatorPrecedence { precedence: 9, right_associative: false }),
    (CppBinaryOperator::BitAnd, OperatorPrecedence { precedence: 10, right_associative: false }),
    (CppBinaryOperator::BitXor, OperatorPrecedence { precedence: 11, right_associative: false }),
    (CppBinaryOperator::BitOr, OperatorPrecedence { precedence: 12, right_associative: false }),
    (CppBinaryOperator::LogicalAnd, OperatorPrecedence { precedence: 13, right_associative: false }),
    (CppBinaryOperator::LogicalOr, OperatorPrecedence { precedence: 14, right_associative: false }),
    (CppBinaryOperator::Conditional, OperatorPrecedence { precedence: 15, right_associative: true }),
    (CppBinaryOperator::Assign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (CppBinaryOperator::AddAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (CppBinaryOperator::SubAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (CppBinaryOperator::MulAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (CppBinaryOperator::DivAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (CppBinaryOperator::ModAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (CppBinaryOperator::BitAndAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (CppBinaryOperator::BitOrAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (CppBinaryOperator::BitXorAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (CppBinaryOperator::ShlAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (CppBinaryOperator::ShrAssign, OperatorPrecedence { precedence: 16, right_associative: true }),
    (CppBinaryOperator::Comma, OperatorPrecedence { precedence: 17, right_associative: false }),
    (CppBinaryOperator::Spaceship, OperatorPrecedence { precedence: 9, right_associative: false }),
];
