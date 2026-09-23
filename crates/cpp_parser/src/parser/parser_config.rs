use rowan::NodeCache;

use crate::{kind::CppLanguageLevel, lexer::LexerConfig, symbols::SymbolTable};

pub struct ParserConfig<'cache> {
    pub level: CppLanguageLevel,
    lexer_config: LexerConfig,
    node_cache: Option<&'cache mut NodeCache>,
    symbol_table: Option<&'cache dyn SymbolTable>,
}

impl<'cache> ParserConfig<'cache> {
    pub fn new(level: CppLanguageLevel, node_cache: Option<&'cache mut NodeCache>) -> Self {
        Self {
            level,
            lexer_config: LexerConfig::new(level),
            node_cache,
            symbol_table: None,
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

    /// Parse with an **external symbol table**: what the caller has already resolved about names this file
    /// cannot see. See [`crate::symbols`] for the contract and for the order the evidence is consulted in — the
    /// table is a *preference*, not a requirement, and parsing without one is the same as parsing with
    /// [`crate::NoSymbols`].
    pub fn with_symbol_table(mut self, symbol_table: &'cache dyn SymbolTable) -> Self {
        self.symbol_table = Some(symbol_table);
        self
    }

    /// The table in force, if the caller supplied one.
    ///
    /// `None` is the ordinary case — a buffer parsed with no context at all — and every caller in the grammar must
    /// treat it as "no evidence from outside", never as "nothing is a type".
    pub fn symbol_table(&self) -> Option<&'cache dyn SymbolTable> {
        self.symbol_table
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
            symbol_table: None,
        }
    }
}
