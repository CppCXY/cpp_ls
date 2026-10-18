use crate::features::LanguageConfig;
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
    /// **The syntax this lexer accepts**, as a set of standards and vendors — see [`crate::features`].
    ///
    /// `language_level` is kept for the callers that still take one; this is the field the lexer's own branches read, and
    /// [`LexerConfig::new`] derives it so a caller that has not migrated gets the level's meaning rather than nothing.
    pub language: LanguageConfig,

    /// Accept `$` in identifiers.
    ///
    /// Not standard C++, but GCC, Clang and MSVC all accept it in their default modes, so real code
    /// contains it — and not only generated code: the Windows SDK's own SAL headers spell macro
    /// names as `__$allowed_on_return` (`specstrings_strict.h`), so a lexer that refuses `$` does
    /// not merely report one character, it **loses the definition** and every use of it.
    ///
    /// **On by default**, which is the whole of the trade-off: the standard's answer is that `$` is
    /// not a name character, and the two readings differ visibly — refusing it yields
    /// `unrecognized character` at the `$` and splits `__$allowed_on_return` into `__`, `$`,
    /// `allowed_on_return`, three tokens that no macro table can match. A file that compiles is the
    /// file we were asked to read, so the permissive reading is the default and the strict one is
    /// one call away ([`LexerConfig::with_dollar_in_identifier`]`(false)`), exactly as a caller
    /// reaches for `-fno-dollars-in-identifiers`.
    pub dollar_in_identifier: bool,

    /// Recognise `R"(...)"` raw string literals and the `u8`/`u`/`U`/`L` encoding prefixes.
    ///
    /// Raw strings are C++11 and later. This is a separate switch from `language_level` so that a
    /// caller parsing pre-C++11 code can still get the encoding prefixes without raw strings.
    pub string_prefixes: bool,
}

impl LexerConfig {
    pub fn with_language(mut self, language: LanguageConfig) -> Self {
        self.language = language;
        self
    }

    /// The syntax this lexer accepts.
    pub fn language(&self) -> LanguageConfig {
        self.language
    }

    pub fn new(language_level: CppLanguageLevel) -> Self {
        // **The standards from the level, and every vendor** — see the field's own note: that is what this lexer did
        // before the migration, so the migration changes no parse.
        let language = LanguageConfig::from_language_level(language_level).with_all_vendors();
        LexerConfig {
            language_level,
            // **The level's standards, and every vendor.** That is what this lexer did before the migration — its keyword
            // table was unconditional, so `__typeof__` and `__declspec` were recognised whatever the level said — and
            // keeping it means the migration changes no parse. A caller that knows its compiler narrows this with
            // `with_language`.
            language,
            // See the field: every mainstream compiler accepts it in its default mode, and a corpus
            // that *uses* it is the argument. The strict reading is `with_dollar_in_identifier(false)`.
            dollar_in_identifier: true,
            string_prefixes: language.support(crate::features::Feature::RawStrings),
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
        // **Through the feature set, and nothing else.** `R"(…)` is C++11 and not C's, and a lexer configured for C must
        // refuse it whatever its old `language_level` says — the conflation `features` exists to remove. The first
        // version of this function kept the level test *beside* the feature test, and a `Cpp23` level with a C
        // configuration answered "yes"; one authority or none.
        self.string_prefixes && self.language.support(crate::features::Feature::RawStrings)
    }

    /// Are digit separators (`1'000'000`) available at this language level?
    pub fn supports_digit_separators(&self) -> bool {
        self.language.support(crate::features::Feature::DigitSeparators)
    }
}
impl Default for LexerConfig {
    fn default() -> Self {
        LexerConfig::new(CppLanguageLevel::default_level())
    }
}
