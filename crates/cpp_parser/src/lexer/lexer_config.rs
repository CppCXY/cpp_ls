use crate::kind::CppLanguageLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexerConfig {
    pub language_level: CppLanguageLevel,
}

impl Default for LexerConfig {
    fn default() -> Self {
        LexerConfig {
            language_level: CppLanguageLevel::Cpp23,
        }
    }
}
