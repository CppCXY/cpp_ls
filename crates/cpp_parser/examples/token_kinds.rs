//! **What a token's kind really is** — the measurement that this layer needed and did not take.
//!
//! `sema/check/` was written comparing `CppSyntaxKind::from(node.kind())` against node kinds and, for tokens, a
//! flat `match` against `CppTokenKind` variants. Two checks then went **quiet** — compiled, ran, reported nothing —
//! and the reason is the shape of this API rather than the logic: rowan's kind for this language is neither of the
//! two enums but the **wrapper** around them, so a comparison is only meaningful once the wrapper has been
//! unwrapped, and a `match` that never fires is not a type error.
//!
//! This prints, for a source string, every token of one node with both texts of its kind, so that a check can be
//! written against what the parser actually produces.
//!
//! ```text
//! usage: token_kinds                       # the default source below
//!        token_kinds "if (x = 1) { }"      # any source
//! ```

use cpp_parser::{CppKind, CppParser, CppTokenKind, ParserConfig};

const DEFAULT_SOURCE: &str = "int main() { int x = 0; if (x = 1) { return x; } return 0; }\n";

fn main() {
    let source = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_SOURCE.to_string());
    let tree = CppParser::parse(&source, ParserConfig::default());

    println!("--- every token, in order ---");
    for element in tree.get_red_root().descendants_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        let wrapper = token.kind();
        println!(
            "{:>4}  text {:?}\n      wrapper  {:?}\n      CppKind  {:?}",
            usize::from(token.text_range().start()),
            token.text().to_string(),
            wrapper,
            CppKind::from(wrapper),
        );
    }

    println!("\n--- the same tokens, classified ---");
    for element in tree.get_red_root().descendants_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        let text = token.text().to_string();
        if !matches!(text.as_str(), "=" | "==" | "(" | ")" | "&&") {
            continue;
        }
        let as_a_token_kind = CppTokenKind::from(CppKind::from(token.kind()));
        println!(
            "{text:?}: CppTokenKind::{as_a_token_kind:?}  is_assign={}  is_equals_equals={}",
            as_a_token_kind == CppTokenKind::Assign,
            as_a_token_kind == CppTokenKind::Equal,
        );
    }
}
