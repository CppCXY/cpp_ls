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
