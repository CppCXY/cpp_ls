use rowan::NodeCache;

use crate::{
    kind::{CppLanguageLevel, Dialect},
    lexer::LexerConfig,
    symbols::{MacroFacts, SymbolTable},
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
    /// **The macros the file's includes define**, when the caller knows them.
    ///
    /// The seam this type was always documented to have and never did. Without it the grammar has one way to
    /// decide that a name is a macro — its **spelling** ([`crate::grammar`]'s `written_like_a_macro`) — and a
    /// spelling cannot separate two shapes that differ in what the name is *for* rather than in how it is written.
    /// The measurement that says so is on `the_name_before_is_a_macro` in the expression grammar: a rule keyed on
    /// the spelling took MSVC's `<xmemory>` from 582 declarations covering the file to 153 covering a fifth, and
    /// left `<chrono>` at a hundredth of itself, because `_STD declval<_Alloc&>()` and `_STD _Convert_size<size_type>(…)`
    /// are spelled identically and are not the same construct.
    ///
    /// `None` is "nobody says", and a grammar rule that asks must then fall back to the spelling it used before
    /// this field existed — see [`ParserConfig::is_a_macro_at`].
    macro_facts: Option<&'cache dyn MacroFacts>,
    /// **Where one file's text ends and the next begins**, as token offsets into the text being parsed.
    ///
    /// A scope may not cross a file boundary. A header that opens a `namespace` and never closes it — which is a
    /// real idiom, and MSVC's `/analyze` headers do it — would otherwise hold **every file spliced after it**, and
    /// the declarations of those files would be filed under a scope that is not theirs. This is the parser's half
    /// of a rule the analysis layer used to enforce by quarantining files; that repair was deleted, and
    /// [`crate::ParserConfig::with_file_boundaries`] is what was supposed to replace it.
    ///
    /// Empty for a parse of one file's own text, where there is no boundary to respect.
    file_boundaries: Vec<usize>,
}

impl<'cache> ParserConfig<'cache> {
    pub fn new(level: CppLanguageLevel, node_cache: Option<&'cache mut NodeCache>) -> Self {
        Self {
            level,
            dialect: Dialect::default(),
            lexer_config: LexerConfig::new(level),
            node_cache,
            symbol_table: None,
            macro_facts: None,
            file_boundaries: Vec::new(),
        }
    }

    /// **Tell the parser where one file's text ends and the next begins.**
    ///
    /// A scope may not cross a file boundary: the `{` a header left open is closed at the end of that header,
    /// rather than paired with a `}` another file wrote or leaving the next file's declarations inside it. The
    /// offsets are the first token of each file **after the first**, which is what
    /// `cpp_code_analysis::RenderedUnit::file_boundaries` answers for a stitched unit.
    ///
    /// # Why the grammar, and not the event stream
    ///
    /// Measured before this existed, by closing the open nodes in the mark-event stream at each boundary: the tree
    /// shape does not move — `TranslationUnit → NamespaceDecl → CompoundStat → Declaration` either way — because
    /// the holder is a `CompoundStat` whose `{` the **grammar** paired with the `}` at the end of the file. The
    /// leak happens while the tokens are read, so it has to be refused there.
    pub fn with_file_boundaries(mut self, boundaries: Vec<usize>) -> Self {
        self.file_boundaries = boundaries;
        self
    }

    /// The boundaries in force, in ascending order — see [`ParserConfig::with_file_boundaries`].
    pub fn file_boundaries(&self) -> &[usize] {
        &self.file_boundaries
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

    /// **Parse with the macros the file's includes define.** See the field's note.
    ///
    /// The caller that has an include closure — the index, and a session's view of an open file — is the only one
    /// that can supply this, and it is the difference between a grammar rule *guessing* that `_STD` is a macro from
    /// its spelling and *knowing* it.
    pub fn with_macros_from_includes(mut self, macro_facts: &'cache dyn MacroFacts) -> Self {
        self.macro_facts = Some(macro_facts);
        self
    }

    /// The macros in force, if the caller supplied any.
    pub fn macro_facts(&self) -> Option<&'cache dyn MacroFacts> {
        self.macro_facts
    }

    /// **Is `name` a macro at `at`?** — the question every grammar rule that used to read a spelling is asking.
    ///
    /// Three answers, and the third is the one that keeps this honest: `Some(true)` and `Some(false)` are the
    /// environment speaking, and **`None` is nobody speaking** — which a caller must read as "use the spelling",
    /// never as "not a macro". [`crate::NothingAtAll`] and an absent environment both answer `None` for every name,
    /// so a parse without one behaves exactly as it did before this method existed.
    pub fn is_a_macro_at(&self, name: &str, at: usize) -> Option<bool> {
        let facts = self.macro_facts?;
        Some(
            facts.is_a_macro_at(name, at)
                || facts.body_text_in_force(name).is_some()
                || facts.body_text_of(name, at).is_some(),
        )
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
            macro_facts: None,
            file_boundaries: Vec::new(),
        }
    }
}
