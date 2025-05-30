use crate::{kind::CppTokenKind, parser_error::CppParseError, text::Reader};

use super::{is_name_continue, is_name_start, lexer_config::LexerConfig, token_data::CppTokenData};

pub struct CppLexer<'a> {
    reader: Reader<'a>,
    lexer_config: LexerConfig,
    errors: &'a mut Vec<CppParseError>,
}

impl CppLexer<'_> {
    pub fn new<'a>(
        text: &'a str,
        lexer_config: LexerConfig,
        errors: &'a mut Vec<CppParseError>,
    ) -> CppLexer<'a> {
        CppLexer {
            reader: Reader::new(text),
            lexer_config,
            errors,
        }
    }

    pub fn tokenize(&mut self) -> Vec<CppTokenData> {
        let mut tokens = vec![];

        while !self.reader.is_eof() {
            let kind = self.lex();
            if kind == CppTokenKind::Eof {
                break;
            }

            tokens.push(CppTokenData::new(kind, self.reader.saved_range()));
        }

        tokens
    }

    /// Convert identifier to keyword token if it matches a C++ keyword
    fn name_to_kind(&self, name: &str) -> CppTokenKind {
        match name {
            // Basic keywords
            "auto" => CppTokenKind::AutoKeyword,
            "break" => CppTokenKind::BreakKeyword,
            "case" => CppTokenKind::CaseKeyword,
            "catch" => CppTokenKind::CatchKeyword,
            "char" => CppTokenKind::CharKeyword,
            "class" => CppTokenKind::ClassKeyword,
            "const" => CppTokenKind::ConstKeyword,
            "continue" => CppTokenKind::ContinueKeyword,
            "default" => CppTokenKind::DefaultKeyword,
            "delete" => CppTokenKind::DeleteKeyword,
            "do" => CppTokenKind::DoKeyword,
            "double" => CppTokenKind::DoubleKeyword,
            "else" => CppTokenKind::ElseKeyword,
            "enum" => CppTokenKind::EnumKeyword,
            "extern" => CppTokenKind::ExternKeyword,
            "false" => CppTokenKind::FalseKeyword,
            "float" => CppTokenKind::FloatKeyword,
            "for" => CppTokenKind::ForKeyword,
            "goto" => CppTokenKind::GotoKeyword,
            "if" => CppTokenKind::IfKeyword,
            "inline" => CppTokenKind::InlineKeyword,
            "int" => CppTokenKind::IntKeyword,
            "long" => CppTokenKind::LongKeyword,
            "new" => CppTokenKind::NewKeyword,
            "operator" => CppTokenKind::OperatorKeyword,
            "private" => CppTokenKind::PrivateKeyword,
            "protected" => CppTokenKind::ProtectedKeyword,
            "public" => CppTokenKind::PublicKeyword,
            "return" => CppTokenKind::ReturnKeyword,
            "short" => CppTokenKind::ShortKeyword,
            "signed" => CppTokenKind::SignedKeyword,
            "sizeof" => CppTokenKind::SizeofKeyword,
            "static" => CppTokenKind::StaticKeyword,
            "struct" => CppTokenKind::StructKeyword,
            "switch" => CppTokenKind::SwitchKeyword,
            "this" => CppTokenKind::ThisKeyword,
            "throw" => CppTokenKind::ThrowKeyword,
            "true" => CppTokenKind::TrueKeyword,
            "try" => CppTokenKind::TryKeyword,
            "typedef" => CppTokenKind::TypedefKeyword,
            "typeid" => CppTokenKind::TypeidKeyword,
            "typename" => CppTokenKind::TypenameKeyword,
            "union" => CppTokenKind::UnionKeyword,
            "unsigned" => CppTokenKind::UnsignedKeyword,
            "using" => CppTokenKind::UsingKeyword,
            "virtual" => CppTokenKind::VirtualKeyword,
            "void" => CppTokenKind::VoidKeyword,
            "volatile" => CppTokenKind::VolatileKeyword,
            "while" => CppTokenKind::WhileKeyword,
            
            // C++11 and later keywords
            "alignas" => CppTokenKind::AlignasKeyword,
            "alignof" => CppTokenKind::AlignofKeyword,
            "constexpr" => CppTokenKind::ConstexprKeyword,
            "decltype" => CppTokenKind::DecltypeKeyword,
            "explicit" => CppTokenKind::ExplicitKeyword,
            "export" => CppTokenKind::ExportKeyword,
            "mutable" => CppTokenKind::MutableKeyword,
            "namespace" => CppTokenKind::NamespaceKeyword,
            "noexcept" => CppTokenKind::NoexceptKeyword,
            "nullptr" => CppTokenKind::NullptrKeyword,
            "static_assert" => CppTokenKind::StaticAssertKeyword,
            "template" => CppTokenKind::TemplateKeyword,
            "thread_local" => CppTokenKind::ThreadLocalKeyword,
            
            // C++20 keywords
            "concept" => CppTokenKind::ConceptKeyword,
            "requires" => CppTokenKind::RequiresKeyword,
            "co_await" => CppTokenKind::CoAwaitKeyword,
            "co_return" => CppTokenKind::CoReturnKeyword,
            "co_yield" => CppTokenKind::CoYieldKeyword,
            
            // Not a keyword, return as identifier
            _ => CppTokenKind::Identifier,
        }
    }

    /// Main lexing function - tokenizes the next token from the input
    fn lex(&mut self) -> CppTokenKind {
        self.reader.reset_buff();

        match self.reader.current_char() {
            // Whitespace
            '\n' | '\r' => self.lex_newline(),
            ' ' | '\t' => self.lex_whitespace(),
            
            // Single character tokens
            '(' => {
                self.reader.bump();
                CppTokenKind::LeftParen
            }
            ')' => {
                self.reader.bump();
                CppTokenKind::RightParen
            }
            '{' => {
                self.reader.bump();
                CppTokenKind::LeftBrace
            }
            '}' => {
                self.reader.bump();
                CppTokenKind::RightBrace
            }
            '[' => {
                self.reader.bump();
                CppTokenKind::LeftBracket
            }
            ']' => {
                self.reader.bump();
                CppTokenKind::RightBracket
            }
            ';' => {
                self.reader.bump();
                CppTokenKind::Semicolon
            }
            ',' => {
                self.reader.bump();
                CppTokenKind::Comma
            }
            '~' => {
                self.reader.bump();
                CppTokenKind::Tilde
            }
            '?' => {
                self.reader.bump();
                CppTokenKind::Question
            }
            
            // Operators that can be single or multi-character
            '+' => {
                self.reader.bump();
                match self.reader.current_char() {
                    '=' => {
                        self.reader.bump();
                        CppTokenKind::PlusAssign
                    }
                    '+' => {
                        self.reader.bump();
                        CppTokenKind::PlusPlus
                    }
                    _ => CppTokenKind::Plus,
                }
            }
            '-' => {
                self.reader.bump();
                match self.reader.current_char() {
                    '=' => {
                        self.reader.bump();
                        CppTokenKind::MinusAssign
                    }
                    '-' => {
                        self.reader.bump();
                        CppTokenKind::MinusMinus
                    }
                    '>' => {
                        self.reader.bump();
                        if self.reader.current_char() == '*' {
                            self.reader.bump();
                            CppTokenKind::ArrowStar
                        } else {
                            CppTokenKind::Arrow
                        }
                    }
                    _ => CppTokenKind::Minus,
                }
            }
            '*' => {
                self.reader.bump();
                if self.reader.current_char() == '=' {
                    self.reader.bump();
                    CppTokenKind::StarAssign
                } else {
                    CppTokenKind::Star
                }
            }
            '/' => {
                self.reader.bump();
                match self.reader.current_char() {
                    '=' => {
                        self.reader.bump();
                        CppTokenKind::SlashAssign
                    }
                    '/' => {
                        // Single-line comment
                        self.reader.bump();
                        self.reader.eat_while(|ch| ch != '\n' && ch != '\r');
                        CppTokenKind::LineComment
                    }
                    '*' => {
                        // Block comment
                        self.reader.bump();
                        self.lex_block_comment()
                    }
                    _ => CppTokenKind::Slash,
                }
            }
            '%' => {
                self.reader.bump();
                if self.reader.current_char() == '=' {
                    self.reader.bump();
                    CppTokenKind::PercentAssign
                } else {
                    CppTokenKind::Percent
                }
            }
            '=' => {
                self.reader.bump();
                if self.reader.current_char() == '=' {
                    self.reader.bump();
                    CppTokenKind::Equal
                } else {
                    CppTokenKind::Assign
                }
            }
            '!' => {
                self.reader.bump();
                if self.reader.current_char() == '=' {
                    self.reader.bump();
                    CppTokenKind::NotEqual
                } else {
                    CppTokenKind::LogicalNot
                }
            }
            '<' => {
                self.reader.bump();
                match self.reader.current_char() {
                    '=' => {
                        self.reader.bump();
                        if self.reader.current_char() == '>' {
                            self.reader.bump();
                            CppTokenKind::Spaceship
                        } else {
                            CppTokenKind::LessEqual
                        }
                    }
                    '<' => {
                        self.reader.bump();
                        if self.reader.current_char() == '=' {
                            self.reader.bump();
                            CppTokenKind::LeftShiftAssign
                        } else {
                            CppTokenKind::LeftShift
                        }
                    }
                    _ => CppTokenKind::Less,
                }
            }
            '>' => {
                self.reader.bump();
                match self.reader.current_char() {
                    '=' => {
                        self.reader.bump();
                        CppTokenKind::GreaterEqual
                    }
                    '>' => {
                        self.reader.bump();
                        if self.reader.current_char() == '=' {
                            self.reader.bump();
                            CppTokenKind::RightShiftAssign
                        } else {
                            CppTokenKind::RightShift
                        }
                    }
                    _ => CppTokenKind::Greater,
                }
            }
            '&' => {
                self.reader.bump();
                match self.reader.current_char() {
                    '&' => {
                        self.reader.bump();
                        CppTokenKind::LogicalAnd
                    }
                    '=' => {
                        self.reader.bump();
                        CppTokenKind::AmpersandAssign
                    }
                    _ => CppTokenKind::Ampersand,
                }
            }
            '|' => {
                self.reader.bump();
                match self.reader.current_char() {
                    '|' => {
                        self.reader.bump();
                        CppTokenKind::LogicalOr
                    }
                    '=' => {
                        self.reader.bump();
                        CppTokenKind::PipeAssign
                    }
                    _ => CppTokenKind::Pipe,
                }
            }
            '^' => {
                self.reader.bump();
                if self.reader.current_char() == '=' {
                    self.reader.bump();
                    CppTokenKind::CaretAssign
                } else {
                    CppTokenKind::Caret
                }
            }
            ':' => {
                self.reader.bump();
                if self.reader.current_char() == ':' {
                    self.reader.bump();
                    CppTokenKind::Scope
                } else {
                    CppTokenKind::Colon
                }
            }
            '.' => {
                self.reader.bump();
                match self.reader.current_char() {
                    '.' => {
                        self.reader.bump();
                        if self.reader.current_char() == '.' {
                            self.reader.bump();
                            CppTokenKind::Ellipsis
                        } else {
                            // Not a valid token, but we handle it gracefully
                            CppTokenKind::Unknown
                        }
                    }
                    '*' => {
                        self.reader.bump();
                        CppTokenKind::DotStar
                    }
                    '0'..='9' => {
                        // Decimal number starting with '.'
                        // We need to restart number parsing from the '.'
                        return self.lex_number();
                    }
                    _ => CppTokenKind::Dot,
                }
            }
            '#' => {
                self.reader.bump();
                if self.reader.current_char() == '#' {
                    self.reader.bump();
                    CppTokenKind::HashHash
                } else {
                    CppTokenKind::Hash
                }
            }
            
            // String literals
            '"' => self.lex_string_literal(),
            '\'' => self.lex_char_literal(),
            
            // Numbers
            '0'..='9' => self.lex_number(),
            
            // Identifiers and keywords
            ch if is_name_start(ch) => {
                self.reader.bump();
                self.reader.eat_while(is_name_continue);
                let name = self.reader.current_saved_text();
                self.name_to_kind(name)
            }
            
            // End of file
            _ if self.reader.is_eof() => CppTokenKind::Eof,
            
            // Unknown character
            _ => {
                self.reader.bump();
                CppTokenKind::Unknown
            }
        }
    }

    /// Lex whitespace characters
    fn lex_whitespace(&mut self) -> CppTokenKind {
        self.reader.eat_while(|ch| ch == ' ' || ch == '\t');
        CppTokenKind::Whitespace
    }

    /// Lex newline characters
    fn lex_newline(&mut self) -> CppTokenKind {
        match self.reader.current_char() {
            '\n' => {
                self.reader.bump();
                if self.reader.current_char() == '\r' {
                    self.reader.bump();
                }
            }
            '\r' => {
                self.reader.bump();
                if self.reader.current_char() == '\n' {
                    self.reader.bump();
                }
            }
            _ => {}
        }
        CppTokenKind::Newline
    }

    /// Lex block comment /* ... */
    fn lex_block_comment(&mut self) -> CppTokenKind {
        let mut depth = 1;
        while !self.reader.is_eof() && depth > 0 {
            match self.reader.current_char() {
                '*' => {
                    self.reader.bump();
                    if self.reader.current_char() == '/' {
                        self.reader.bump();
                        depth -= 1;
                    }
                }
                '/' => {
                    self.reader.bump();
                    if self.reader.current_char() == '*' {
                        self.reader.bump();
                        depth += 1; // Nested comment
                    }
                }
                _ => {
                    self.reader.bump();
                }
            }
        }

        if depth > 0 {
            self.errors.push(CppParseError::syntax_error_from(
                "unfinished block comment",
                self.reader.saved_range(),
            ));
        }

        CppTokenKind::BlockComment
    }

    /// Lex string literal "..."
    fn lex_string_literal(&mut self) -> CppTokenKind {
        self.reader.bump(); // consume opening quote
        
        while !self.reader.is_eof() {
            match self.reader.current_char() {
                '"' => {
                    self.reader.bump(); // consume closing quote
                    return CppTokenKind::StringLiteral;
                }
                '\\' => {
                    self.reader.bump(); // consume backslash
                    if !self.reader.is_eof() {
                        self.reader.bump(); // consume escaped character
                    }
                }
                '\n' | '\r' => {
                    // Unterminated string
                    self.errors.push(CppParseError::syntax_error_from(
                        "unterminated string literal",
                        self.reader.saved_range(),
                    ));
                    return CppTokenKind::StringLiteral;
                }
                _ => {
                    self.reader.bump();
                }
            }
        }

        // Reached EOF without finding closing quote
        self.errors.push(CppParseError::syntax_error_from(
            "unterminated string literal",
            self.reader.saved_range(),
        ));
        CppTokenKind::StringLiteral
    }

    /// Lex character literal '...'
    fn lex_char_literal(&mut self) -> CppTokenKind {
        self.reader.bump(); // consume opening quote
        
        while !self.reader.is_eof() {
            match self.reader.current_char() {
                '\'' => {
                    self.reader.bump(); // consume closing quote
                    return CppTokenKind::CharLiteral;
                }
                '\\' => {
                    self.reader.bump(); // consume backslash
                    if !self.reader.is_eof() {
                        self.reader.bump(); // consume escaped character
                    }
                }
                '\n' | '\r' => {
                    // Unterminated character literal
                    self.errors.push(CppParseError::syntax_error_from(
                        "unterminated character literal",
                        self.reader.saved_range(),
                    ));
                    return CppTokenKind::CharLiteral;
                }
                _ => {
                    self.reader.bump();
                }
            }
        }

        // Reached EOF without finding closing quote
        self.errors.push(CppParseError::syntax_error_from(
            "unterminated character literal",
            self.reader.saved_range(),
        ));
        CppTokenKind::CharLiteral
    }

    /// Lex numeric literals (integers, floats, hex, binary, etc.)
    fn lex_number(&mut self) -> CppTokenKind {
        enum NumberState {
            Int,
            Float,
            Hex,
            HexFloat,
            Binary,
            WithExponent,
        }

        let mut state = NumberState::Int;
        let first = self.reader.current_char();
        
        // Handle special number prefixes
        if first == '0' {
            self.reader.bump();
            match self.reader.current_char() {
                'x' | 'X' => {
                    self.reader.bump();
                    state = NumberState::Hex;
                }
                'b' | 'B' => {
                    self.reader.bump();
                    state = NumberState::Binary;
                }
                '0'..='7' => {
                    // Octal number (continue as normal integer)
                    state = NumberState::Int;
                }
                '.' => {
                    state = NumberState::Float;
                }
                _ => {
                    // Just a zero
                }
            }
        } else if first == '.' {
            // Float starting with decimal point
            self.reader.bump();
            state = NumberState::Float;
        } else {
            // Regular decimal number
            self.reader.bump();
        }

        // Continue reading digits based on state
        loop {
            if self.reader.is_eof() {
                break;
            }
            
            let ch = self.reader.current_char();
            let should_continue = match (&state, ch) {
                (NumberState::Int, '0'..='9') => true,
                (NumberState::Int, '.') => {
                    state = NumberState::Float;
                    true
                }
                (NumberState::Int, 'e' | 'E') => {
                    self.reader.bump();
                    if matches!(self.reader.current_char(), '+' | '-') {
                        self.reader.bump();
                    }
                    state = NumberState::WithExponent;
                    continue; // Don't bump again
                }
                (NumberState::Float, '0'..='9') => true,
                (NumberState::Float, 'e' | 'E') => {
                    self.reader.bump();
                    if matches!(self.reader.current_char(), '+' | '-') {
                        self.reader.bump();
                    }
                    state = NumberState::WithExponent;
                    continue;
                }
                (NumberState::Hex, '0'..='9' | 'a'..='f' | 'A'..='F') => true,
                (NumberState::Hex, '.') => {
                    state = NumberState::HexFloat;
                    true
                }
                (NumberState::Hex, 'p' | 'P') => {
                    self.reader.bump();
                    if matches!(self.reader.current_char(), '+' | '-') {
                        self.reader.bump();
                    }
                    state = NumberState::WithExponent;
                    continue;
                }
                (NumberState::HexFloat, '0'..='9' | 'a'..='f' | 'A'..='F') => true,
                (NumberState::HexFloat, 'p' | 'P') => {
                    self.reader.bump();
                    if matches!(self.reader.current_char(), '+' | '-') {
                        self.reader.bump();
                    }
                    state = NumberState::WithExponent;
                    continue;
                }
                (NumberState::Binary, '0' | '1') => true,
                (NumberState::WithExponent, '0'..='9') => true,
                _ => false,
            };

            if should_continue {
                self.reader.bump();
            } else {
                break;
            }
        }

        // Handle suffixes
        if !self.reader.is_eof() {
            match state {
                NumberState::Int | NumberState::Hex | NumberState::Binary => {
                    // Integer suffixes: u, U, l, L, ul, UL, etc.
                    self.reader.eat_while(|c| matches!(c, 'u' | 'U' | 'l' | 'L'));
                }
                NumberState::Float | NumberState::HexFloat | NumberState::WithExponent => {
                    // Float suffixes: f, F, l, L
                    if matches!(self.reader.current_char(), 'f' | 'F' | 'l' | 'L') {
                        self.reader.bump();
                    }
                }
            }
        }

        // Return appropriate token type
        match state {
            NumberState::Float | NumberState::HexFloat | NumberState::WithExponent => {
                CppTokenKind::FloatingLiteral
            }
            _ => CppTokenKind::IntegerLiteral,
        }
    }
}
