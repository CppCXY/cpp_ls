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

/// **What a CUDA word means** — `None` unless the project is a CUDA project.
///
/// These are not keywords to this lexer (see `cpp_code_analysis::cuda`): `__global__` is a legal variable name in
/// C++, so the language server cannot answer about it from the token alone. What makes it a qualifier is the
/// project's family — `__CUDACC__`, an `nvcc` toolchain, or `[compile.cuda]` in `.cppls.toml` — and in a project that
/// is not CUDA the word is a name like any other, which the queries above have already tried.
///
/// The long text is the `cuda` section of the locale file; a word whose key is missing still gets the one-line
/// description the completion list shows, so a key added late is a thin popup rather than none.
pub fn cuda_hover(token: &cpp_parser::CppSyntaxToken, cuda: bool) -> Option<String> {
    if !cuda {
        return None;
    }

    let word = cpp_code_analysis::cuda::word(token.text())?;
    Some(known(word.hover_key).unwrap_or_else(|| word.detail.to_string()))
}

/// One key, or `None` when the table has no entry for it.
///
/// `rust-i18n` answers a missing key with the key itself, so "the answer is the key" *is* the absence — checked
/// here once instead of at each caller.
fn known(key: &str) -> Option<String> {
    let text = t!(key);
    (!text.is_empty() && text != key).then(|| text.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The token of the first occurrence of `needle` in a CUDA parse of `source`.
    fn token_of(source: &str, needle: &str, cuda: bool) -> cpp_parser::CppSyntaxToken {
        let tree = cpp_parser::CppParser::parse(
            source,
            cpp_parser::ParserConfig::default().with_cuda(cuda),
        );
        let at = source.find(needle).expect("the fixture names it") + needle.len() / 2;
        cpp_parser::token_at(&tree.get_red_root(), at).expect("a token at the offset")
    }

    /// **Every word in the table has a popup in every locale**, because a missing key is a popup that says only the
    /// one-line description — the failure this test exists to make loud.
    ///
    /// The check is on the **files**, not on `t!`: the current locale is one value for the process, and a test that
    /// switched it would race every other test that reads a message. Reading the two files as text is enough, because
    /// the keys under `cuda:` are flat — the assertion is that a line `  <key>:` exists.
    #[test]
    fn every_cuda_word_has_its_popup_in_every_locale() {
        const EN: &str = include_str!("../../../locales/en.yml");
        const ZH: &str = include_str!("../../../locales/zh-CN.yml");

        for word in cpp_code_analysis::cuda::words() {
            let leaf = word
                .hover_key
                .strip_prefix("cuda.")
                .expect("a key under the cuda section");
            let line = format!("{leaf}:");
            for (name, source) in [("en", EN), ("zh-CN", ZH)] {
                assert!(
                    source
                        .lines()
                        .any(|line_of| line_of.trim_start().starts_with(&line)),
                    "{} has no `{leaf}` entry in {name}.yml",
                    word.name
                );
            }
        }
    }

    /// **A CUDA word has a popup only in a CUDA project**, and the popup names the word it describes.
    ///
    /// The family is the switch, not the token: `__global__` is an identifier, so a file read as C++ gets the ordinary
    /// answers (nothing at all for a word that declares nothing), which is what a project that has never seen `nvcc`
    /// should get.
    #[test]
    fn a_qualifier_is_answered_only_where_it_is_a_qualifier() {
        let source = "__global__ void kernel() { }\n";
        let as_cuda = token_of(source, "__global__", true);
        let text = cuda_hover(&as_cuda, true).expect("a CUDA project has an answer");
        assert!(
            text.contains("__global__"),
            "the popup names the word: {text}"
        );
        assert!(text.contains("kernel"), "and says what it is: {text}");

        assert!(
            cuda_hover(&as_cuda, false).is_none(),
            "the same token in a project that is not CUDA is just a name"
        );
    }

    /// **A built-in variable and a vector type are answered too** — `threadIdx` even when no toolkit is installed,
    /// which is the state of a machine without CUDA: nothing declares it, and the reader still deserves an answer.
    #[test]
    fn the_toolkits_own_names_are_answered() {
        let source = "void f() { unsigned i = threadIdx.x; float4 v; }\n";
        let builtin = token_of(source, "threadIdx", true);
        let text = cuda_hover(&builtin, true).expect("`threadIdx` has an answer");
        assert!(text.contains("threadIdx"), "{text}");

        let vector = token_of(source, "float4", true);
        let text = cuda_hover(&vector, true).expect("`float4` has an answer");
        assert!(text.contains("float4"), "{text}");

        // A name that is not CUDA's vocabulary is not answered, and neither is a spelling with different case.
        let other = token_of("void f() { int value = 1; }\n", "value", true);
        assert!(cuda_hover(&other, true).is_none());
        let mistyped = token_of("void f() { int ThreadIdx = 1; }\n", "ThreadIdx", true);
        assert!(cuda_hover(&mistyped, true).is_none(), "case matters");
    }
}
