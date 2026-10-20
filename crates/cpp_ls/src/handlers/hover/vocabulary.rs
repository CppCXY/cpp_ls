// The two vocabularies a reader hovers over and the analysis has no declaration for: the language's own keywords
// and Doxygen's commands. Neither is a name, so neither reaches `Session::definition` — and the popup a reader most
// obviously wants on `constexpr` is exactly the one that was empty.
//
// **The text lives in `locales/en.yml`**, which `rust-i18n` embeds at build time through `i18n!("./locales")` — the
// same mechanism the parser's own messages already use (`t!("expected primary expression")`). A translation is
// therefore a second file rather than a second code path, and a key that is missing falls back to the key itself,
// which is what tells [`known`] that there is nothing to say.

/// **What a keyword means**, when this server has something to say about it.
///
/// `None` for a token that is not a keyword, and for one whose key is absent from the table — the second is how a
/// keyword can be added to the lexer without a popup having to be written for it in the same change.
pub fn keyword_hover(token: &cpp_parser::CppSyntaxToken) -> Option<String> {
    let kind = cpp_parser::CppTokenKind::from(token.kind());
    if !cpp_parser::is_keyword(kind) {
        return None;
    }

    // The **spelling**, not the kind's name: `bool` is a `BoolLiteral` in this lexer (`true`/`false` are the
    // literals), and a key derived from the variant would ask about a keyword called `boolliteral`.
    let word = token.text().to_ascii_lowercase();
    known(&format!("keyword.{word}"))
}

/// **What a Doxygen command does**, and what it takes — `@param name description`.
///
/// The name is without its introducer, which is how the doc grammar stores it (`@param` is a command node whose
/// name token is `param`). A command the table does not describe answers `None` rather than a sentence about it
/// being a command: the vocabulary is long, and a generic sentence would be noise on the eighty that are rare.
pub fn command_hover(name: &str) -> Option<String> {
    known(&format!("command.{}", name.to_ascii_lowercase()))
}

/// One key, or `None` when the table has no entry for it.
///
/// `rust-i18n` answers a missing key with the key itself, so "the answer is the key" *is* the absence — checked
/// here once instead of at each caller.
fn known(key: &str) -> Option<String> {
    let text = t!(key);
    (!text.is_empty() && text != key).then(|| text.to_string())
}
