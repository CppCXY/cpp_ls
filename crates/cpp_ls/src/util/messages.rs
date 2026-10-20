//! **The language this server speaks** — one place that decides what `locale` means, and one place that applies it.
//!
//! Three layers can name a language, and they are read in this order:
//!
//! ```text
//! locale = "zh-CN"  in .cppls.toml   the project's own choice — most specific, so it wins
//! "locale": "zh-cn" in `initialize`  the client's setting (VS Code's display language)
//! "en"                                the default, and the fallback for a key a translation does not carry
//! ```
//!
//! Applied through [`cpp_parser::set_locale`], which forwards to `rust-i18n`'s **process-wide** current locale —
//! so the parser's syntax errors, the analysis's diagnostics and this crate's own messages all change together. That
//! is the point of putting the call here rather than in each crate: a language is a property of the session, not of a
//! module.

/// **Use `locale` for every message from now on**, normalised to the tag the translation files are named with.
///
/// The tag is normalised because clients spell it three ways — `zh-cn` (the protocol's own example), `zh_CN`, and
/// `zh-CN` — while a file is one name. Region and language are taken as written, case-corrected: `zh_cn` → `zh-CN`.
pub fn set_locale(locale: &str) {
    // The tag is normalised by `cpp_parser::set_locale`, which is also the function that applies it to the
    // process-wide locale — one rule, one place, and this crate does not need a second copy of it.
    cpp_parser::set_locale(locale);
    log::info!("messages will be written in `{}`", cpp_parser::normalise_locale(locale));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The three spellings a client sends are one tag**, which is what makes a file per language enough.
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
            // A language nobody has translated is passed through: `rust-i18n` falls back to English, which is a
            // better answer than silently claiming the request was honoured.
            ("ja", "ja"),
            ("", "en"),
            ("  ", "en"),
        ] {
            assert_eq!(cpp_parser::normalise_locale(written), expected, "{written:?}");
        }
    }
}
