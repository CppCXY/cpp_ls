use rowan::NodeCache;

use crate::{kind::CppLanguageLevel, lexer::LexerConfig};

pub struct ParserConfig<'cache> {
    pub level: CppLanguageLevel,
    lexer_config: LexerConfig,
    node_cache: Option<&'cache mut NodeCache>,
}

impl<'cache> ParserConfig<'cache> {
    pub fn new(level: CppLanguageLevel, node_cache: Option<&'cache mut NodeCache>) -> Self {
        Self {
            level,
            lexer_config: LexerConfig {
                language_level: level,
            },
            node_cache,
        }
    }

    pub fn lexer_config(&self) -> LexerConfig {
        self.lexer_config
    }

    pub fn node_cache(&mut self) -> Option<&mut NodeCache> {
        self.node_cache.as_deref_mut()
    }
}

impl Default for ParserConfig<'_> {
    fn default() -> Self {
        Self {
            level: CppLanguageLevel::Cpp23,
            lexer_config: LexerConfig {
                language_level: CppLanguageLevel::Cpp23,
            },
            node_cache: None,
        }
    }
}
