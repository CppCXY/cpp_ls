use rowan::NodeCache;

use crate::{
    kind::{CppLanguageLevel, Dialect},
    lexer::LexerConfig,
    symbols::{MacroEnvironment, SymbolTable},
};

pub struct ParserConfig<'cache> {
    pub level: CppLanguageLevel,
    /// Which compiler's own reserved spellings mean what — see [`Dialect`].
    ///
    /// A separate question from `level`, and the reason is written on [`Dialect`]: `-std=gnu++20` is C++20 *and*
    /// GNU's spellings at once, which a single "level" cannot say. The analysis layer sets this from the
    /// toolchain's own predefined macros (`__GNUC__` / `_MSC_VER`).
    pub dialect: Dialect,
    lexer_config: LexerConfig,
    node_cache: Option<&'cache mut NodeCache>,
    symbol_table: Option<&'cache dyn SymbolTable>,
    /// What the file's **includes** contribute, each entry in force from its own offset — see
    /// [`MacroEnvironment`].
    macros_from_includes: Option<&'cache MacroEnvironment>,
}

impl<'cache> ParserConfig<'cache> {
    pub fn new(level: CppLanguageLevel, node_cache: Option<&'cache mut NodeCache>) -> Self {
        Self {
            level,
            dialect: Dialect::default(),
            lexer_config: LexerConfig::new(level),
            node_cache,
            symbol_table: None,
            macros_from_includes: None,
        }
    }

    pub fn lexer_config(&self) -> LexerConfig {
        self.lexer_config
    }

    /// Parse for a **target compiler**: what its reserved spellings mean. See [`Dialect`].
    pub fn with_dialect(mut self, dialect: Dialect) -> Self {
        self.dialect = dialect;
        self
    }

    pub fn dialect(&self) -> Dialect {
        self.dialect
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

    /// Parse with what the file's **includes** contribute, each macro in force from the offset its `#include`
    /// ended at. See [`MacroEnvironment`]: the evidence is *positional*, which is what a flat table failed to be.
    pub fn with_macros_from_includes(mut self, macros: &'cache MacroEnvironment) -> Self {
        self.macros_from_includes = Some(macros);
        self
    }

    /// What the includes contribute, if the caller supplied it.
    ///
    /// `None` is the ordinary case for a buffer parsed on its own, and every caller must read it as no evidence
    /// from outside — never as nothing is a macro.
    pub fn macros_from_includes(&self) -> Option<&'cache MacroEnvironment> {
        self.macros_from_includes
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
            dialect: Dialect::default(),
            lexer_config: LexerConfig::default(),
            node_cache: None,
            symbol_table: None,
            macros_from_includes: None,
        }
    }
}
