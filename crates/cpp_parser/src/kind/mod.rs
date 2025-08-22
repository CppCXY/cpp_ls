mod cpp_language_level;
mod cpp_operator_kind;
mod cpp_syntax_kind;
mod cpp_token_kind;

pub use cpp_language_level::CppLanguageLevel;
pub use cpp_operator_kind::{CppBinaryOperator, CppUnaryOperator, UNARY_PRECEDENCE, BINARY_PRECEDENCE};
pub use cpp_syntax_kind::CppSyntaxKind;
pub use cpp_token_kind::CppTokenKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum CppKind {
    Syntax(CppSyntaxKind),
    Token(CppTokenKind),
}

impl From<CppSyntaxKind> for CppKind {
    fn from(kind: CppSyntaxKind) -> Self {
        CppKind::Syntax(kind)
    }
}

impl From<CppTokenKind> for CppKind {
    fn from(kind: CppTokenKind) -> Self {
        CppKind::Token(kind)
    }
}

impl From<CppKind> for CppSyntaxKind {
    fn from(val: CppKind) -> Self {
        match val {
            CppKind::Syntax(kind) => kind,
            _ => CppSyntaxKind::None,
        }
    }
}

impl From<CppKind> for CppTokenKind {
    fn from(val: CppKind) -> Self {
        match val {
            CppKind::Token(kind) => kind,
            _ => CppTokenKind::None,
        }
    }
}

impl CppKind {
    pub fn is_syntax(self) -> bool {
        matches!(self, CppKind::Syntax(_))
    }

    pub fn is_token(self) -> bool {
        matches!(self, CppKind::Token(_))
    }

    pub fn get_raw(self) -> u16 {
        match self {
            CppKind::Syntax(kind) => kind as u16 | 0x8000,
            CppKind::Token(kind) => kind as u16,
        }
    }

    pub fn from_raw(raw: u16) -> CppKind {
        if raw & 0x8000 != 0 {
            CppKind::Syntax(unsafe { std::mem::transmute(raw & 0x7FFF) })
        } else {
            CppKind::Token(unsafe { std::mem::transmute(raw) })
        }
    }
}

#[derive(Debug)]
pub struct PriorityTable {
    pub left: i32,
    pub right: i32,
}

#[derive(Debug, PartialEq)]
pub enum CppOpKind {
    None,
    Unary(CppUnaryOperator),
    Binary(CppBinaryOperator),
}

impl From<CppUnaryOperator> for CppOpKind {
    fn from(op: CppUnaryOperator) -> Self {
        CppOpKind::Unary(op)
    }
}

impl From<CppBinaryOperator> for CppOpKind {
    fn from(op: CppBinaryOperator) -> Self {
        CppOpKind::Binary(op)
    }
}
