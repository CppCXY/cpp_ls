mod grammar;
mod kind;
mod lexer;
mod parser;
mod parser_error;
mod symbols;
mod syntax;
mod text;

pub use grammar::doc::{DocCommandArgs, DocCommandKind, command_args, command_kind};
pub use kind::*;
pub use lexer::{
    CppLexer, CppTokenData, DocCommentStyle, DocToken, DocTokenKind, LexerConfig, is_block_comment,
    is_doc_whitespace, is_documentation_comment, lex, lex_comment,
};
pub use parser::{Checkpoint, CppParser, EventStreamAudit, MarkEvent, ParserConfig};
pub use parser_error::{CppParseError, CppParseErrorKind};
pub use symbols::{
    BODY_CHAIN_LIMIT, BodyFacts, BodyShape, DefinitionFacts, InForceBody, IncludedMacro, MacroBody,
    MacroBodies, MacroEnvironment, MacroFacts, NoSymbols, NothingAtAll, SymbolKind, SymbolMap,
    SymbolTable, a_body_opens_an_inline_namespace, shape_of_a_body, shape_of_a_body_at,
};
pub use syntax::*;
pub use text::{LineIndex, Reader, SourceRange};

#[macro_use]
extern crate rust_i18n;

rust_i18n::i18n!("./locales", fallback = "en");

pub fn set_locale(locale: &str) {
    rust_i18n::set_locale(locale);
}
