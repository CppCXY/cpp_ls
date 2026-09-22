use crate::{kind::CppTokenKind, parser_error::CppParseError, text::Reader};

use super::{
    char_kind::{is_name_continue, is_name_continue_with_dollar, is_name_start_with_dollar},
    lexer_config::LexerConfig,
    token_data::CppTokenData,
};

pub struct CppLexer<'a> {
    reader: Reader<'a>,
    lexer_config: LexerConfig,
    errors: &'a mut Vec<CppParseError>,
}

impl<'a> CppLexer<'a> {
    pub fn new(
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
            "consteval" => CppTokenKind::ConstevalKeyword,
            "constinit" => CppTokenKind::ConstinitKeyword,
            "decltype" => CppTokenKind::DecltypeKeyword,
            "explicit" => CppTokenKind::ExplicitKeyword,
            "export" => CppTokenKind::ExportKeyword,
            "mutable" => CppTokenKind::MutableKeyword,
            "friend" => CppTokenKind::FriendKeyword,
            "register" => CppTokenKind::RegisterKeyword,
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
                        // Decimal number starting with '.', e.g. `.5`. `.` has already been
                        // consumed, so tell `lex_number` to skip its own first-character step.
                        self.lex_number()
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
            
            // String and character literals, including the `u8`/`u`/`U`/`L` encoding prefixes and
            // the `R` raw-string prefix.
            '"' => self.lex_string_literal(),
            '\'' => self.lex_char_literal(),

            // A backslash starts either a line splice or nothing valid at all.
            '\\' => self.lex_backslash(),

            // Numbers
            '0'..='9' => self.lex_number(),

            // Identifiers, keywords, and the prefixed literals that look like identifiers at the
            // first character (`u8"x"`, `L'c'`, `R"(...)"`).
            ch if is_name_start_with_dollar(ch, self.lexer_config.dollar_in_identifier) => {
                self.lex_identifier_like()
            }
            
            // End of file
            _ if self.reader.is_eof() => CppTokenKind::Eof,
            
            // Unknown character
            _ => {
                self.reader.bump();
                // Still emit a token, so the tree stays lossless, but say so: an unrecognised
                // character is either a typo in the source or a gap in the lexer, and both should
                // be visible rather than silently absorbed into the surrounding construct.
                self.errors.push(CppParseError::syntax_error_from(
                    &format!(
                        "unrecognized character `{}`",
                        self.reader.current_saved_text()
                    ),
                    self.reader.saved_range(),
                ));
                CppTokenKind::Unknown
            }
        }
    }

    /// Lex a backslash: a line splice, the start of a universal character name, or an error.
    ///
    /// Translation phase 2 deletes `\` immediately followed by a newline. That makes the splice
    /// *trivia* for the parser, but it is not ordinary whitespace: a directive such as
    /// `#define FOO \<newline> bar` continues across it, and the preprocessor layer needs to see
    /// the splice to know that.
    fn lex_backslash(&mut self) -> CppTokenKind {
        match self.reader.lookahead(1) {
            '\n' | '\r' => {
                self.reader.bump();
                self.eat_newline();
                CppTokenKind::LineContinuation
            }
            // `\u00e9` is an identifier spelled with a universal character name, which is the only
            // way an identifier can begin with something other than an `XID_Start` character.
            // `lex_identifier_like` records the diagnostic itself when the escape is malformed, so
            // there is exactly one error per bad escape.
            'u' | 'U' => self.lex_identifier_like(),
            _ => {
                self.reader.bump();
                self.errors.push(CppParseError::syntax_error_from(
                    "stray `\\` outside of a string or line splice",
                    self.reader.saved_range(),
                ));
                CppTokenKind::Unknown
            }
        }
    }

    /// Consume the current newline character, including the second half of a `\r\n` or `\n\r` pair.
    fn eat_newline(&mut self) {
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
    }

    /// Lex whitespace characters
    fn lex_whitespace(&mut self) -> CppTokenKind {
        self.reader.eat_while(|ch| ch == ' ' || ch == '\t');
        CppTokenKind::Whitespace
    }

    /// Lex newline characters
    fn lex_newline(&mut self) -> CppTokenKind {
        self.eat_newline();
        CppTokenKind::Newline
    }

    /// Lex block comment /* ... */
    ///
    /// Block comments do **not** nest in C++ (`/* /* */` ends at the first `*/`), unlike the Lua
    /// this lexer was ported from. Treating them as nesting silently swallows everything up to the
    /// next `*/` and reports an "unfinished comment" for perfectly valid code.
    fn lex_block_comment(&mut self) -> CppTokenKind {
        while !self.reader.is_eof() {
            match self.reader.current_char() {
                '*' => {
                    self.reader.bump();
                    if self.reader.current_char() == '/' {
                        self.reader.bump();
                        return CppTokenKind::BlockComment;
                    }
                }
                _ => {
                    self.reader.bump();
                }
            }
        }

        self.errors.push(CppParseError::syntax_error_from(
            "unfinished block comment",
            self.reader.saved_range(),
        ));
        CppTokenKind::BlockComment
    }

    /// Lex something that starts like an identifier: a keyword, a name, or a prefixed literal.
    ///
    /// The four prefixes `u8`, `u`, `U` and `L` are shared between character/string literals and
    /// ordinary identifiers, so `u8"x"` and `u8x` have to be told apart by lookahead. `R` and `LR`
    /// etc. additionally introduce raw strings, where the "string" has completely different
    /// escaping rules.
    /// Lex something that starts like an identifier: a keyword, a name, or a prefixed literal.
    ///
    /// The four prefixes `u8`, `u`, `U` and `L` are shared between character/string literals and
    /// ordinary identifiers, so `u8"x"` and `u8x` have to be told apart by lookahead. `R` and `LR`
    /// etc. additionally introduce raw strings, where the "string" has completely different
    /// escaping rules.
    ///
    /// Also entered for a leading `\`, which can only be a universal character name here.
    fn lex_identifier_like(&mut self) -> CppTokenKind {
        let dollar = self.lexer_config.dollar_in_identifier;

        // The encoding prefixes are not all one character: `u8` is two, and `u8R`/`uR` add the raw
        // marker on top. Getting the length wrong here does not merely mislabel a token, it re-lexes
        // the body of the literal as C++.
        if let Some((prefix_len, quote)) = self.literal_prefix_at(self.reader.current_char()) {
            match quote {
                '"' => {
                    for _ in 0..prefix_len {
                        self.reader.bump();
                    }
                    return self.lex_string_literal();
                }
                '\'' => {
                    for _ in 0..prefix_len {
                        self.reader.bump();
                    }
                    return self.lex_char_literal();
                }
                'R' => {
                    if !self.lexer_config.supports_raw_strings() {
                        self.errors.push(CppParseError::syntax_error_from(
                            "raw string literals require C++11",
                            self.reader.saved_range(),
                        ));
                    }
                    for _ in 0..prefix_len {
                        self.reader.bump();
                    }
                    return self.lex_raw_string_literal();
                }
                _ => {}
            }
        }

        // Universal character names are the only non-`XID` way to begin an identifier. Callers that
        // dispatch on `\` (see `lex_backslash`) land here too, so `saved_range` covers the whole
        // name rather than just the escape.
        if self.reader.current_char() == '\\' && matches!(self.reader.lookahead(1), 'u' | 'U') {
            self.reader.bump(); // `\`
            self.reader.bump(); // `u` or `U`
            if !self.lex_universal_character_name() {
                self.errors.push(CppParseError::syntax_error_from(
                    "invalid universal character name in identifier",
                    self.reader.saved_range(),
                ));
                return CppTokenKind::Unknown;
            }
            while self.lex_identifier_continuation(dollar) {}
            return self.name_to_kind(self.reader.current_saved_text());
        }

        // Plain identifier or keyword.
        self.reader.bump();
        while self.lex_identifier_continuation(dollar) {}

        let name = self.reader.current_saved_text();
        self.name_to_kind(name)
    }

    /// If an encoding prefix for a string/character/raw literal starts here, return its length in
    /// characters and the quote character that follows it.
    ///
    /// Emulating the grammar rather than probing a fixed set is what keeps `u"x"`, `u8"x"`,
    /// `u8R"(x)"`, `LR"(x)"` and `L'c'` all working while leaving `u8x` and `Lvalue` as names.
    fn literal_prefix_at(&self, first: char) -> Option<(usize, char)> {
        // Bare `R"(...)` raw string: no encoding prefix.
        if self.lexer_config.supports_raw_strings()
            && first == 'R'
            && self.reader.lookahead(1) == '"'
        {
            return Some((0, 'R'));
        }

        if !self.lexer_config.string_prefixes {
            return None;
        }

        let prefix_len = match first {
            'U' | 'L' => 1,
            'u' => match self.reader.lookahead(1) {
                '8' => 2,
                'R' if self.reader.lookahead(2) == '"' && self.lexer_config.supports_raw_strings() => {
                    1
                }
                _ => 1,
            },
            _ => return None,
        };

        match self.reader.lookahead(prefix_len) {
            '"' => Some((prefix_len, '"')),
            '\'' => Some((prefix_len, '\'')),
            'R' if self.reader.lookahead(prefix_len + 1) == '"'
                && self.lexer_config.supports_raw_strings() =>
            {
                Some((prefix_len, 'R'))
            }
            _ => None,
        }
    }

    /// Consume one more identifier character, including a universal character name.
    ///
    /// Returns whether anything was consumed. An invalid `\u` escape is reported but still stops
    /// the identifier — a stray backslash must not silently become part of a name.
    fn lex_identifier_continuation(&mut self, dollar_allowed: bool) -> bool {
        let ch = self.reader.current_char();
        if is_name_continue_with_dollar(ch, dollar_allowed) {
            self.reader.bump();
            return true;
        }

        if ch == '\\' && matches!(self.reader.lookahead(1), 'u' | 'U') {
            self.reader.bump(); // `\`
            self.reader.bump(); // `u` or `U`
            if self.lex_universal_character_name() {
                return true;
            }

            self.errors.push(CppParseError::syntax_error_from(
                "invalid universal character name in identifier",
                self.reader.saved_range(),
            ));
            return false;
        }

        false
    }

    /// The `u`/`U` of a universal character name has just been consumed; read its digits and check
    /// that the code point it denotes may appear in an identifier.
    fn lex_universal_character_name(&mut self) -> bool {
        let digits = if self.reader.current_saved_text() == "\\u" {
            4
        } else {
            8
        };

        let mut code: u32 = 0;
        for _ in 0..digits {
            match self.reader.current_char().to_digit(16) {
                Some(value) => {
                    code = code * 16 + value;
                    self.reader.bump();
                }
                None => {
                    // Stop at the first bad digit so the rest of the line still lexes normally.
                    while self.reader.current_saved_text().len()
                        < 2 + digits
                        && is_name_continue(self.reader.current_char())
                    {
                        self.reader.bump();
                    }
                    return false;
                }
            }
        }

        char::from_u32(code).is_some_and(is_name_continue)
    }

    /// Lex a string literal `"..."`, with the opening quote as the current character.
    fn lex_string_literal(&mut self) -> CppTokenKind {
        self.reader.bump(); // consume opening quote

        while !self.reader.is_eof() {
            match self.reader.current_char() {
                '"' => {
                    self.reader.bump(); // consume closing quote
                    return self.finish_literal(CppTokenKind::StringLiteral);
                }
                '\\' => {
                    self.reader.bump(); // consume backslash
                    if !self.reader.is_eof() {
                        self.reader.bump(); // consume escaped character
                    }
                }
                '\n' | '\r' => {
                    // A newline cannot appear in an ordinary literal; report and stop here so the
                    // rest of the line is still lexed normally.
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

    /// Attach a user-defined literal suffix to the literal just lexed, if one follows.
    ///
    /// The suffix must be part of the literal token: `"hello"_s` lexed as a string plus an
    /// identifier makes the parser see two expressions where the language has one.
    fn finish_literal(&mut self, kind: CppTokenKind) -> CppTokenKind {
        if self.reader.current_char() != '_' {
            return kind;
        }

        self.reader.bump(); // `_`
        self.reader
            .eat_while(|ch| is_name_continue_with_dollar(ch, false));
        CppTokenKind::UserDefinedLiteral
    }

    /// Lex a raw string literal `R"delim(...)delim"`.
    ///
    /// The current character is the `R` of the prefix. Raw strings are the one place where the
    /// lexer cannot work character by character: escapes do not apply, and the terminator is
    /// `)delim"` where `delim` is up to 16 characters chosen by the author. Treating `R"(...)"` as
    /// an ordinary string produces a token cut at the first inner quote, which then re-lexes the
    /// rest of the raw string as C++ — the single most destructive lexing mistake possible.
    fn lex_raw_string_literal(&mut self) -> CppTokenKind {
        self.reader.bump(); // `R`
        self.reader.bump(); // `"`

        // Delimiter: at most 16 characters, none of which may be `(`, `)`, whitespace or `\`.
        let mut delimiter = String::new();
        loop {
            let ch = self.reader.current_char();
            if ch == '(' {
                break;
            }
            // `\0` means end of input, so an opening paren is missing.
            if ch == '\\'
                || ch == '\0'
                || ch == ')'
                || ch.is_whitespace()
                || delimiter.len() >= 16
            {
                self.errors.push(CppParseError::syntax_error_from(
                    "malformed raw string delimiter",
                    self.reader.saved_range(),
                ));
                return CppTokenKind::StringLiteral;
            }
            delimiter.push(ch);
            self.reader.bump();
        }

        self.reader.bump(); // `(`

        loop {
            if self.reader.is_eof() {
                self.errors.push(CppParseError::syntax_error_from(
                    "unterminated raw string literal",
                    self.reader.saved_range(),
                ));
                return CppTokenKind::StringLiteral;
            }

            if self.reader.current_char() == ')' {
                // Compare the following characters against `delim"` without consuming them.
                let matches = delimiter
                    .chars()
                    .enumerate()
                    .all(|(offset, expected)| self.reader.lookahead(1 + offset) == expected)
                    && self.reader.lookahead(1 + delimiter.len()) == '"';

                if matches {
                    // Consume `)`, the delimiter and the closing quote.
                    for _ in 0..(2 + delimiter.len()) {
                        self.reader.bump();
                    }
                    return CppTokenKind::StringLiteral;
                }
            }

            self.reader.bump();
        }
    }

    /// Lex character literal `'...'`
    fn lex_char_literal(&mut self) -> CppTokenKind {
        self.reader.bump(); // consume opening quote

        while !self.reader.is_eof() {
            match self.reader.current_char() {
                '\'' => {
                    self.reader.bump(); // consume closing quote
                    return self.finish_literal(CppTokenKind::CharLiteral);
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
    ///
    /// Handles the digit separator `'` (C++14), the binary `0b` prefix (C++14) and user-defined
    /// literal suffixes (C++11), which is why the suffix part has to be decided against the token
    /// kind rather than guessed from a fixed list.
    fn lex_number(&mut self) -> CppTokenKind {
        enum NumberState {
            Int,
            Float,
            Hex,
            HexFloat,
            Binary,
            WithExponent,
        }

        let separators = self.lexer_config.supports_digit_separators();
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
                    if self.lexer_config.language_level < crate::kind::CppLanguageLevel::Cpp14 {
                        self.errors.push(CppParseError::syntax_error_from(
                            "binary literals require C++14",
                            self.reader.saved_range(),
                        ));
                    }
                    self.reader.bump();
                    state = NumberState::Binary;
                }
                '0'..='7' => {
                    // Octal number (continue as normal integer)
                    state = NumberState::Int;
                }
                '.' => {
                    // `0.5` and `0.` — the dot has to be consumed here. Leaving it to the loop does
                    // not work: the `Float` state has no rule for `.`, so the scan would stop with
                    // the dot unconsumed and the caller would read `0` and `.5` as two literals.
                    self.reader.bump();
                    state = NumberState::Float;
                }
                _ => {
                    // Just a zero
                }
            }
        } else if first == '.' {
            // A float starting with a decimal point: `.5`. The dispatcher has already consumed the
            // dot and delegated here, because a `.` is only a number when a digit follows it —
            // otherwise it is member access.
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

            // A digit separator is only a separator *between* digits: `1'000` yes, `1'` no (that is
            // a user-defined literal on `1`). Look ahead rather than treating `'` as part of the
            // number, or `operator''` style suffixes get glued onto the literal.
            if ch == '\'' && separators {
                let next = self.reader.lookahead(1);
                let digit_follows = match state {
                    NumberState::Hex | NumberState::HexFloat => next.is_ascii_hexdigit(),
                    NumberState::Binary => matches!(next, '0' | '1'),
                    _ => next.is_ascii_digit(),
                };
                if digit_follows {
                    self.reader.bump();
                    continue;
                }
                break;
            }

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

        let is_float = matches!(
            state,
            NumberState::Float | NumberState::HexFloat | NumberState::WithExponent
        );

        // Standard suffixes. `u`/`U`/`l`/`L` for integers and `f`/`F`/`l`/`L` for floats. A
        // user-defined suffix starts with `_` and makes the whole thing a `UserDefinedLiteral`,
        // which the parser must not treat as a plain number.
        if is_float {
            if matches!(self.reader.current_char(), 'f' | 'F' | 'l' | 'L') {
                self.reader.bump();
            }
        } else {
            self.reader
                .eat_while(|c| matches!(c, 'u' | 'U' | 'l' | 'L' | 'z' | 'Z'));
        }

        if self.reader.current_char() == '_' {
            self.reader.bump();
            self.reader
                .eat_while(|c| is_name_continue_with_dollar(c, false));
            return CppTokenKind::UserDefinedLiteral;
        }

        if is_float {
            CppTokenKind::FloatingLiteral
        } else {
            CppTokenKind::IntegerLiteral
        }
    }

    /// Lex a header name, as it appears in `#include <iostream>` or `#include "local.h"`.
    ///
    /// This is a separate entry point rather than part of the ordinary sweep, because which tokens
    /// form a header name depends entirely on the enclosing directive: outside one, `<iostream>` is
    /// `Less`, `Identifier`, `Greater` and must stay that way or every template breaks.
    ///
    /// The preprocessor layer calls this with the cursor positioned on the `<` or `"`. If the input
    /// is not actually a well-formed header name, nothing is consumed and `None` is returned, so the
    /// caller can fall back to ordinary tokenization — `#include FOO(x)` and `#include <a> + 1` are
    /// both legal to write and both must keep working.
    pub fn lex_header_name(&mut self) -> Option<CppTokenKind> {
        // The token buffer is reset lazily on the first `lex` call, so an entry point that is not
        // `lex` has to prime the reader itself.
        self.reader.begin_token();

        let opening = self.reader.current_char();
        let terminator = match opening {
            '<' => '>',
            '"' => '"',
            _ => return None,
        };

        // The opening delimiter has not been consumed yet, so `lookahead(1)` is the first character
        // of the header name itself.
        let inner = self.scan_header_name_body(terminator)?;

        // Consume the opening delimiter, the body and the closing delimiter.
        for _ in 0..(inner + 2) {
            self.reader.bump();
        }

        Some(CppTokenKind::HeaderName)
    }

    /// Number of characters between the delimiters, or `None` if the header name is malformed.
    fn scan_header_name_body(&self, terminator: char) -> Option<usize> {
        let mut length = 0usize;

        loop {
            match self.reader.lookahead(length + 1) {
                // Unterminated: decline rather than swallow the rest of the line.
                '\0' | '\n' | '\r' | ';' | '\\' => return None,
                ch if ch == terminator => {
                    return (length > 0).then_some(length);
                }
                _ => length += 1,
            }
        }
    }
}
