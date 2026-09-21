use crate::kind::CppLanguageLevel;

/// Lexer configuration.
///
/// Everything here is a *lexical* setting: it must not change the meaning of a token stream that
/// is already unambiguous, only how much the lexer is willing to accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexerConfig {
    /// Target standard. Gates features that are not in older standards (raw strings in C++11,
    /// digit separators in C++14, `<=>` in C++20).
    pub language_level: CppLanguageLevel,

    /// Accept `$` in identifiers.
    ///
    /// Not standard C++, but GCC, Clang and MSVC all accept it, so it appears in real code
    /// (particularly generated code). Off by default because a strict parser should report it.
    pub dollar_in_identifier: bool,

    /// Recognise `R"(...)"` raw string literals and the `u8`/`u`/`U`/`L` encoding prefixes.
    ///
    /// Raw strings are C++11 and later. This is a separate switch from `language_level` so that a
    /// caller parsing pre-C++11 code can still get the encoding prefixes without raw strings.
    pub string_prefixes: bool,
}

impl LexerConfig {
    pub fn new(language_level: CppLanguageLevel) -> Self {
        LexerConfig {
            language_level,
            dollar_in_identifier: false,
            string_prefixes: language_level >= CppLanguageLevel::Cpp11,
        }
    }

    pub fn with_dollar_in_identifier(mut self, allowed: bool) -> Self {
        self.dollar_in_identifier = allowed;
        self
    }

    pub fn with_string_prefixes(mut self, enabled: bool) -> Self {
        self.string_prefixes = enabled;
        self
    }

    /// Are raw string literals available at this language level?
    pub fn supports_raw_strings(&self) -> bool {
        self.string_prefixes && self.language_level >= CppLanguageLevel::Cpp11
    }

    /// Are digit separators (`1'000'000`) available at this language level?
    pub fn supports_digit_separators(&self) -> bool {
        self.language_level >= CppLanguageLevel::Cpp14
    }
}

impl Default for LexerConfig {
    fn default() -> Self {
        LexerConfig::new(CppLanguageLevel::default_level())
    }
}
