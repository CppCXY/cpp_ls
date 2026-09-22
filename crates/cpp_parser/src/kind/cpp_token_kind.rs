use core::fmt;

/// C++ Token Kind Enumeration
///
/// This enum defines all token types recognized by the C++ lexer.
/// Tokens are the basic units of syntax analysis, including keywords, operators, literals, etc.
///
/// Note: Only tokens are included here, not syntax structures.
/// Syntax structures are defined in CppSyntaxKind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum CppTokenKind {
    /// Empty token, used for initialization
    None,

    // ========== Keywords ==========

    // Basic keywords
    /// auto - automatic type deduction (C++11)
    AutoKeyword,
    /// break - break out of loop or switch
    BreakKeyword,
    /// case - switch branch
    CaseKeyword,
    /// catch - exception handler
    CatchKeyword,
    /// char - character type
    CharKeyword,
    /// class - class definition
    ClassKeyword,
    /// const - const qualifier
    ConstKeyword,
    /// continue - continue to next loop iteration
    ContinueKeyword,
    /// default - default branch in switch
    DefaultKeyword,
    /// delete - delete dynamic memory
    DeleteKeyword,
    /// do - do-while loop
    DoKeyword,
    /// double - double precision floating point
    DoubleKeyword,
    /// else - else branch
    ElseKeyword,
    /// enum - enumeration
    EnumKeyword,
    /// extern - external linkage
    ExternKeyword,
    /// false - boolean false
    FalseKeyword,
    /// float - single precision floating point
    FloatKeyword,
    /// for - for loop
    ForKeyword,
    /// goto - unconditional jump
    GotoKeyword,
    /// if - conditional statement
    IfKeyword,
    /// inline - inline function
    InlineKeyword,
    /// int - integer type
    IntKeyword,
    /// long - long integer type
    LongKeyword,
    /// new - dynamic memory allocation
    NewKeyword,
    /// operator - operator overloading
    OperatorKeyword,
    /// private - private access
    PrivateKeyword,
    /// protected - protected access
    ProtectedKeyword,
    /// public - public access
    PublicKeyword,
    /// return - function return
    ReturnKeyword,
    /// short - short integer type
    ShortKeyword,
    /// signed - signed type
    SignedKeyword,
    /// sizeof - get size
    SizeofKeyword,
    /// static - static storage
    StaticKeyword,
    /// struct - structure
    StructKeyword,
    /// switch - multi-branch selection
    SwitchKeyword,
    /// this - current object pointer
    ThisKeyword,
    /// throw - throw exception
    ThrowKeyword,
    /// true - boolean true
    TrueKeyword,
    /// try - exception handling
    TryKeyword,
    /// typedef - type definition
    TypedefKeyword,
    /// typeid - type information
    TypeidKeyword,
    /// typename - type name
    TypenameKeyword,
    /// union - union type
    UnionKeyword,
    /// unsigned - unsigned type
    UnsignedKeyword,
    /// using - using declaration/directive
    UsingKeyword,
    /// virtual - virtual function
    VirtualKeyword,
    /// void - void type
    VoidKeyword,
    /// volatile - volatile qualifier
    VolatileKeyword,
    /// while - while loop
    WhileKeyword,

    // C++11 and later keywords
    /// alignas - alignment specifier (C++11)
    AlignasKeyword,
    /// alignof - alignment query (C++11)
    AlignofKeyword,
    /// constexpr - constant expression (C++11)
    ConstexprKeyword,
    /// consteval - immediate function (C++20)
    ConstevalKeyword,
    /// constinit - constant initialization (C++20)
    ConstinitKeyword,
    /// decltype - type deduction (C++11)
    DecltypeKeyword,
    /// explicit - explicit conversion
    ExplicitKeyword,
    /// export - export (deprecated, reintroduced in C++20 for modules)
    ExportKeyword,
    /// mutable - mutable member
    MutableKeyword,
    /// friend - grant access to a non-member
    FriendKeyword,
    /// register - storage class hint (removed in C++17, still accepted by every compiler)
    RegisterKeyword,
    /// namespace - namespace
    NamespaceKeyword,
    /// noexcept - no exception (C++11)
    NoexceptKeyword,
    /// nullptr - null pointer (C++11)
    NullptrKeyword,
    /// static_assert - static assertion (C++11)
    StaticAssertKeyword,
    /// template - template
    TemplateKeyword,
    /// thread_local - thread local storage (C++11)
    ThreadLocalKeyword,

    // C++20 keywords
    /// concept - concept (C++20)
    ConceptKeyword,
    /// requires - constraint (C++20)
    RequiresKeyword,
    /// co_await - coroutine await (C++20)
    CoAwaitKeyword,
    /// co_return - coroutine return (C++20)
    CoReturnKeyword,
    /// co_yield - coroutine yield (C++20)
    CoYieldKeyword,

    // ========== Operators ==========

    // Arithmetic operators
    /// + addition
    Plus,
    /// - subtraction
    Minus,
    /// * multiplication/dereference
    Star,
    /// / division
    Slash,
    /// % modulo
    Percent,

    // Assignment operators
    /// = assignment
    Assign,
    /// += addition assignment
    PlusAssign,
    /// -= subtraction assignment
    MinusAssign,
    /// *= multiplication assignment
    StarAssign,
    /// /= division assignment
    SlashAssign,
    /// %= modulo assignment
    PercentAssign,

    // Comparison operators
    /// == equal
    Equal,
    /// != not equal
    NotEqual,
    /// < less than
    Less,
    /// <= less than or equal
    LessEqual,
    /// > greater than
    Greater,
    /// >= greater than or equal
    GreaterEqual,
    /// <=> three-way comparison (C++20)
    Spaceship,

    // Logical operators
    /// && logical and
    LogicalAnd,
    /// || logical or
    LogicalOr,
    /// ! logical not
    LogicalNot,

    // Bitwise operators
    /// & bitwise and/address-of
    Ampersand,
    /// | bitwise or
    Pipe,
    /// ^ bitwise xor
    Caret,
    /// ~ bitwise not
    Tilde,
    /// << left shift
    LeftShift,
    /// >> right shift
    RightShift,
    /// &= bitwise and assignment
    AmpersandAssign,
    /// |= bitwise or assignment
    PipeAssign,
    /// ^= bitwise xor assignment
    CaretAssign,
    /// <<= left shift assignment
    LeftShiftAssign,
    /// >>= right shift assignment
    RightShiftAssign,

    // Increment/decrement
    /// ++ increment
    PlusPlus,
    /// -- decrement
    MinusMinus,

    // Member access
    /// . member access
    Dot,
    /// -> pointer member access
    Arrow,
    /// .* member pointer access
    DotStar,
    /// ->* pointer to member pointer access
    ArrowStar,

    // Other operators
    /// :: scope resolution
    Scope,
    /// ? conditional operator
    Question,
    /// ?: full conditional operator
    Conditional,

    // ========== Punctuation ==========
    /// ( left parenthesis
    LeftParen,
    /// ) right parenthesis
    RightParen,
    /// { left brace
    LeftBrace,
    /// } right brace
    RightBrace,
    /// [ left bracket
    LeftBracket,
    /// ] right bracket
    RightBracket,
    /// ; semicolon
    Semicolon,
    /// , comma
    Comma,
    /// : colon
    Colon,
    /// ... ellipsis
    Ellipsis,

    // ========== Literals ==========
    /// Integer literal
    /// e.g.: 42, 0x1A, 0777, 0b1010
    IntegerLiteral,

    /// Floating point literal
    /// e.g.: 3.14, 2.5e10, 1.0f
    FloatingLiteral,

    /// Character literal
    /// e.g.: 'a', '\n', L'中'
    CharLiteral,

    /// String literal
    /// e.g.: "hello", L"wide", R"(raw)"
    StringLiteral,

    /// A header name, as it appears after `#include` / `#include_next` / `__has_include`.
    ///
    /// This is a *preprocessor-only* token: outside a directive, `<iostream>` is `Less`,
    /// `Identifier`, `Greater`. The lexer therefore produces it on demand, via
    /// `CppLexer::lex_header_name`, rather than during the ordinary token sweep — `<` and `>` are
    /// far too common to make a guess at, and guessing wrong would wreck every template.
    ///
    /// e.g.: `<iostream>`, `"local.h"`
    HeaderName,

    /// The `bool` type keyword.
    ///
    /// Named `BoolLiteral` rather than `BoolKeyword` for historical reasons, and the name is misleading: this
    /// is the *type*, not a literal. The literals are `true` and `false`, which the lexer produces as
    /// [`TrueKeyword`](Self::TrueKeyword) and [`FalseKeyword`](Self::FalseKeyword). Both readings are accepted
    /// where they are asked for — see `is_type_keyword` and `is_literal` in `syntax::node::token` — so the
    /// kind is recognised either way; only the name is unfortunate.
    BoolLiteral,

    /// Null pointer literal (nullptr is defined as a keyword)
    NullptrLiteral,

    /// User-defined literal (C++11)
    /// e.g.: 42_km, "hello"_s
    UserDefinedLiteral,

    // ========== Identifiers ==========
    /// Ordinary identifier
    /// e.g.: variable, function_name, MyClass
    Identifier,

    // ========== Preprocessor ==========
    /// # preprocessor directive start
    Hash,
    /// ## preprocessor token concatenation
    HashHash,

    // ========== Whitespace and Comments ==========
    /// Whitespace character (space, tab, etc.)
    Whitespace,

    /// Newline character
    Newline,

    /// Single-line comment // ...
    LineComment,

    /// Block comment /* ... */
    BlockComment,

    /// A backslash-newline line splice.
    ///
    /// Phase 2 of translation deletes these, which makes them *trivia* for the parser (they must
    /// not separate tokens) — but they are kept in the tree so the CST stays lossless. Note that
    /// they are not "whitespace" either: `#if FOO \<newline> && BAR` is one directive, so a parser
    /// that treats the splice as a normal newline breaks preprocessor conditionals.
    ///
    /// e.g.: `\` followed by `\n` or `\r\n`
    LineContinuation,

    // ========== Documentation comments ==========
    /// A Doxygen command name inside a comment, without its introducer: the `param` of `@param x`.
    ///
    /// Distinct from [`CppTokenKind::Identifier`] on purpose: inside a comment, `param` is not a name
    /// that could be declared, and a consumer walking the tree for identifiers must not find it.
    DocCommandName,

    /// The running text of a documentation comment.
    DocText,

    /// Structural punctuation inside a comment: `[`, `]`, `,`, `(`, `)`, `.`, `:`, `=`, `::`.
    ///
    /// One kind for all of them, because the C++ grammar must treat every one as trivia — a comment
    /// containing `)` must never close a C++ parameter list. The doc layer tells them apart by
    /// looking at the text, which it can do because it is reading the comment rather than the code.
    DocTrivia,

    // ========== Special Tokens ==========
    /// End of file
    Eof,

    /// Unknown character - unrecognized by the lexer
    Unknown,

    /// Error token - for error recovery
    Error,
}

impl fmt::Display for CppTokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // 关键字
            Self::AutoKeyword => write!(f, "auto"),
            Self::BreakKeyword => write!(f, "break"),
            Self::CaseKeyword => write!(f, "case"),
            Self::CatchKeyword => write!(f, "catch"),
            Self::CharKeyword => write!(f, "char"),
            Self::ClassKeyword => write!(f, "class"),
            Self::ConstKeyword => write!(f, "const"),
            Self::ContinueKeyword => write!(f, "continue"),
            Self::DefaultKeyword => write!(f, "default"),
            Self::DeleteKeyword => write!(f, "delete"),
            Self::DoKeyword => write!(f, "do"),
            Self::DoubleKeyword => write!(f, "double"),
            Self::ElseKeyword => write!(f, "else"),
            Self::EnumKeyword => write!(f, "enum"),
            Self::ExternKeyword => write!(f, "extern"),
            Self::FalseKeyword => write!(f, "false"),
            Self::FloatKeyword => write!(f, "float"),
            Self::ForKeyword => write!(f, "for"),
            Self::GotoKeyword => write!(f, "goto"),
            Self::IfKeyword => write!(f, "if"),
            Self::InlineKeyword => write!(f, "inline"),
            Self::IntKeyword => write!(f, "int"),
            Self::LongKeyword => write!(f, "long"),
            Self::NewKeyword => write!(f, "new"),
            Self::OperatorKeyword => write!(f, "operator"),
            Self::PrivateKeyword => write!(f, "private"),
            Self::ProtectedKeyword => write!(f, "protected"),
            Self::PublicKeyword => write!(f, "public"),
            Self::ReturnKeyword => write!(f, "return"),
            Self::ShortKeyword => write!(f, "short"),
            Self::SignedKeyword => write!(f, "signed"),
            Self::SizeofKeyword => write!(f, "sizeof"),
            Self::StaticKeyword => write!(f, "static"),
            Self::StructKeyword => write!(f, "struct"),
            Self::SwitchKeyword => write!(f, "switch"),
            Self::ThisKeyword => write!(f, "this"),
            Self::ThrowKeyword => write!(f, "throw"),
            Self::TrueKeyword => write!(f, "true"),
            Self::TryKeyword => write!(f, "try"),
            Self::TypedefKeyword => write!(f, "typedef"),
            Self::TypeidKeyword => write!(f, "typeid"),
            Self::TypenameKeyword => write!(f, "typename"),
            Self::UnionKeyword => write!(f, "union"),
            Self::UnsignedKeyword => write!(f, "unsigned"),
            Self::UsingKeyword => write!(f, "using"),
            Self::VirtualKeyword => write!(f, "virtual"),
            Self::VoidKeyword => write!(f, "void"),
            Self::VolatileKeyword => write!(f, "volatile"),
            Self::WhileKeyword => write!(f, "while"),

            // C++11及后续标准关键字
            Self::AlignasKeyword => write!(f, "alignas"),
            Self::AlignofKeyword => write!(f, "alignof"),
            Self::ConstexprKeyword => write!(f, "constexpr"),
            Self::DecltypeKeyword => write!(f, "decltype"),
            Self::ExplicitKeyword => write!(f, "explicit"),
            Self::ExportKeyword => write!(f, "export"),
            Self::MutableKeyword => write!(f, "mutable"),
            Self::FriendKeyword => write!(f, "friend"),
            Self::RegisterKeyword => write!(f, "register"),
            Self::NamespaceKeyword => write!(f, "namespace"),
            Self::NoexceptKeyword => write!(f, "noexcept"),
            Self::NullptrKeyword => write!(f, "nullptr"),
            Self::StaticAssertKeyword => write!(f, "static_assert"),
            Self::TemplateKeyword => write!(f, "template"),
            Self::ThreadLocalKeyword => write!(f, "thread_local"),
            Self::ConstevalKeyword => write!(f, "consteval"),
            Self::ConstinitKeyword => write!(f, "constinit"),

            // C++20关键字
            Self::ConceptKeyword => write!(f, "concept"),
            Self::RequiresKeyword => write!(f, "requires"),
            Self::CoAwaitKeyword => write!(f, "co_await"),
            Self::CoReturnKeyword => write!(f, "co_return"),
            Self::CoYieldKeyword => write!(f, "co_yield"),

            // 操作符
            Self::Plus => write!(f, "+"),
            Self::Minus => write!(f, "-"),
            Self::Star => write!(f, "*"),
            Self::Slash => write!(f, "/"),
            Self::Percent => write!(f, "%"),
            Self::Assign => write!(f, "="),
            Self::PlusAssign => write!(f, "+="),
            Self::MinusAssign => write!(f, "-="),
            Self::StarAssign => write!(f, "*="),
            Self::SlashAssign => write!(f, "/="),
            Self::PercentAssign => write!(f, "%="),
            Self::Equal => write!(f, "=="),
            Self::NotEqual => write!(f, "!="),
            Self::Less => write!(f, "<"),
            Self::LessEqual => write!(f, "<="),
            Self::Greater => write!(f, ">"),
            Self::GreaterEqual => write!(f, ">="),
            Self::Spaceship => write!(f, "<=>"),
            Self::LogicalAnd => write!(f, "&&"),
            Self::LogicalOr => write!(f, "||"),
            Self::LogicalNot => write!(f, "!"),
            Self::Ampersand => write!(f, "&"),
            Self::Pipe => write!(f, "|"),
            Self::Caret => write!(f, "^"),
            Self::Tilde => write!(f, "~"),
            Self::LeftShift => write!(f, "<<"),
            Self::RightShift => write!(f, ">>"),
            Self::AmpersandAssign => write!(f, "&="),
            Self::PipeAssign => write!(f, "|="),
            Self::CaretAssign => write!(f, "^="),
            Self::LeftShiftAssign => write!(f, "<<="),
            Self::RightShiftAssign => write!(f, ">>="),
            Self::PlusPlus => write!(f, "++"),
            Self::MinusMinus => write!(f, "--"),
            Self::Dot => write!(f, "."),
            Self::Arrow => write!(f, "->"),
            Self::DotStar => write!(f, ".*"),
            Self::ArrowStar => write!(f, "->*"),
            Self::Scope => write!(f, "::"),
            Self::Question => write!(f, "?"),
            Self::Conditional => write!(f, "?:"),

            // 标点符号
            Self::LeftParen => write!(f, "("),
            Self::RightParen => write!(f, ")"),
            Self::LeftBrace => write!(f, "{{"),
            Self::RightBrace => write!(f, "}}"),
            Self::LeftBracket => write!(f, "["),
            Self::RightBracket => write!(f, "]"),
            Self::Semicolon => write!(f, ";"),
            Self::Comma => write!(f, ","),
            Self::Colon => write!(f, ":"),
            Self::Ellipsis => write!(f, "..."),

            // 预处理器
            Self::Hash => write!(f, "#"),
            Self::HashHash => write!(f, "##"),

            // 字面量与预处理器扩展
            Self::HeaderName => write!(f, "header-name"),
            Self::LineContinuation => write!(f, "\\"),
            Self::UserDefinedLiteral => write!(f, "user-defined literal"),
            Self::IntegerLiteral => write!(f, "integer literal"),
            Self::FloatingLiteral => write!(f, "floating literal"),
            Self::StringLiteral => write!(f, "string literal"),
            Self::CharLiteral => write!(f, "character literal"),
            Self::BoolLiteral => write!(f, "boolean literal"),
            Self::NullptrLiteral => write!(f, "nullptr"),
            Self::Identifier => write!(f, "identifier"),
            Self::Whitespace => write!(f, "whitespace"),
            Self::Newline => write!(f, "newline"),
            Self::LineComment => write!(f, "line comment"),
            Self::BlockComment => write!(f, "block comment"),
            Self::DocCommandName => write!(f, "documentation command"),
            Self::DocText => write!(f, "documentation text"),
            Self::DocTrivia => write!(f, "comment punctuation"),
            Self::Eof => write!(f, "end of file"),
            Self::Unknown => write!(f, "unknown"),
            Self::Error => write!(f, "error"),
            Self::None => write!(f, "none"),
        }
    }
}
