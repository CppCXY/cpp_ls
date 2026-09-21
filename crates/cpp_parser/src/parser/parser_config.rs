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
            lexer_config: LexerConfig::new(level),
            node_cache,
        }
    }

    pub fn lexer_config(&self) -> LexerConfig {
        self.lexer_config
    }

    /// Replace the lexer settings. Used by the parser to re-lex a header name, and by callers that
    /// need a non-default dialect (for example `$` in identifiers).
    pub fn with_lexer_config(mut self, lexer_config: LexerConfig) -> Self {
        self.lexer_config = lexer_config;
        self
    }

    pub fn node_cache(&mut self) -> Option<&mut NodeCache> {
        self.node_cache.as_deref_mut()
    }
}

/// Not derived: `level` and `lexer_config` have to agree, and the derived version would silently
/// pair an explicit level with a stale lexer configuration whenever either default changes.
#[allow(clippy::derivable_impls)]
impl Default for ParserConfig<'_> {
    fn default() -> Self {
        Self {
            level: CppLanguageLevel::default_level(),
            lexer_config: LexerConfig::default(),
            node_cache: None,
        }
    }
}
