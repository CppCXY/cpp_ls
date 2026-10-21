mod grammar;
mod kind;
mod lexer;
mod parser;
mod parser_error;
mod symbols;
mod syntax;
mod text;

pub use grammar::doc::{
    DocCommandArgs, DocCommandKind, DocCommandSpec, command_args, command_kind, every_command,
};
pub use kind::*;
pub use lexer::{
    CppLexer, CppTokenData, DocCommentStyle, DocToken, DocTokenKind, LexerConfig, is_block_comment,
    is_doc_whitespace, is_documentation_comment, lex, lex_comment,
};
pub use parser::{Checkpoint, CppParser, EventStreamAudit, MarkEvent, ParsedPrefix, ParserConfig};
pub use parser_error::{CppParseError, CppParseErrorKind};
pub use symbols::{
    BODY_CHAIN_LIMIT, BodyFacts, BodyShape, DefinitionFacts, InForceBody, IncludedMacro,
    MacroBodies, MacroBody, MacroEnvironment, MacroFacts, NoSymbols, NothingAtAll, SymbolKind,
    SymbolMap, SymbolTable, a_body_opens_an_inline_namespace, shape_of_a_body, shape_of_a_body_at,
};
pub use syntax::*;
pub use text::{LineIndex, Reader, SourceRange};

#[macro_use]
extern crate rust_i18n;

rust_i18n::i18n!("./locales", fallback = "en");

/// **Use `locale` for every message from now on** — the parser's, the analysis's and the server's.
///
/// `rust-i18n`'s current locale is **process-wide**, so this one call changes every `t!` in every crate that loaded
/// a table — which is the point: a language is a property of the session, not of a module.
///
/// The tag is normalised first, because a client spells it three ways — `zh-cn` (the protocol's own example),
/// `zh_CN`, `zh-CN` — while a translation file has one name. See [`normalise_locale`].
pub fn set_locale(locale: &str) {
    rust_i18n::set_locale(&normalise_locale(locale));
}

/// **The tag a locale file is named with** — `zh-cn`/`zh_CN`/`ZH-CN` all answer `zh-CN`.
///
/// Only the *shape* of a language tag is handled (a language, then an optional region): anything else is passed
/// through, and `rust-i18n` then falls back to English — the right answer for a language nobody has translated yet,
/// and better than silently pretending the request was honoured.
pub fn normalise_locale(locale: &str) -> String {
    let trimmed = locale.trim().replace('_', "-");
    let mut parts = trimmed.split('-');

    let Some(language) = parts.next() else {
        return String::from("en");
    };
    if language.is_empty() {
        return String::from("en");
    }

    let mut normalised = language.to_ascii_lowercase();
    if let Some(region) = parts.next()
        && !region.is_empty()
    {
        normalised.push('-');
        normalised.push_str(&region.to_ascii_uppercase());
    }

    normalised
}

#[cfg(test)]
mod locale_tests {
    use super::normalise_locale;

    /// **The spellings a client sends are one tag**, which is what makes one file per language enough.
    #[test]
    fn a_locale_tag_is_normalised_to_the_name_of_its_file() {
        for (written, expected) in [
            ("zh-CN", "zh-CN"),
            ("zh-cn", "zh-CN"),
            ("zh_CN", "zh-CN"),
            ("ZH-CN", "zh-CN"),
            ("en", "en"),
            ("en-US", "en-US"),
            ("pt_BR", "pt-BR"),
            ("ja", "ja"),
            ("", "en"),
            ("  ", "en"),
        ] {
            assert_eq!(normalise_locale(written), expected, "{written:?}");
        }
    }
}

/// **The syntax a parse accepts**, as a set of standards and vendors — see [eatures].
pub use kind::features;
