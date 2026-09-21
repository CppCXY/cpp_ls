mod grammar;
mod kind;
mod lexer;
mod parser;
mod parser_error;
mod syntax;
mod text;

pub use kind::*;
pub use lexer::{CppLexer, LexerConfig};
pub use parser::{Checkpoint, CppParser, EventStreamAudit, MarkEvent, ParserConfig};
pub use parser_error::{CppParseError, CppParseErrorKind};
pub use syntax::*;
pub use text::{LineIndex, Reader, SourceRange};

#[macro_use]
extern crate rust_i18n;

rust_i18n::i18n!("./locales", fallback = "en");

pub fn set_locale(locale: &str) {
    rust_i18n::set_locale(locale);
}
