//! **The text of a comment**, which one rule changes: `SpacesInLineCommentPrefix`.
//!
//! clang-format (23.1.5, LLVM) normalises `//using` to `// using` and `//!bang` to `//! bang`, while leaving already
//! spaced text and the decorators alone — measured:

//! ```text
//! //using enum X;   ->  // using enum X;
//! ///doc comment    ->  /// doc comment
//! //!bang           ->  //! bang
//! //:ref            ->  //: ref
//! //  two spaces    ->  //  two spaces      (unchanged: at least one, at most unlimited)
//! //NOLINT          ->  // NOLINT
//! ```
//!
//! The prefix is `//` followed by the run of `/`, `!` and `:` that makes it a doc/reference marker; the option's
//! `Minimum` is the spaces that must follow it, and its `Maximum` (unlimited by default) the most that may.

use std::borrow::Cow;

use crate::config::Style;
use crate::options::StyleOption;

/// The comment's text with the configured spacing after its prefix.
pub fn normalized<'a>(text: &'a str, style: &Style) -> Cow<'a, str> {
    let Some(rest) = text.strip_prefix("//") else {
        // **A block comment is not touched**: `/*block*/` keeps its bytes, and the space before the next token is the
        // spacing rules'.
        return Cow::Borrowed(text);
    };
    let decorators = rest
        .chars()
        .take_while(|character| matches!(character, '/' | '!' | ':'))
        .count();
    let after_prefix = 2 + decorators;
    let minimum = style.count(StyleOption::SpacesInLineCommentPrefixMinimum);
    let maximum = style.number(StyleOption::SpacesInLineCommentPrefixMaximum);
    let body = &text[after_prefix..];
    // **A prefix with nothing after it is a comment with no text**: clang-format leaves `//` as `//`, and adding the
    // minimum space turned every empty comment line into a trailing-space difference (nlohmann json: 2 198 lines in
    // `lexer.hpp` alone).
    if body.is_empty() {
        return Cow::Borrowed(text);
    }
    let spaces = body
        .chars()
        .take_while(|character| *character == ' ')
        .count();
    let wanted = if maximum < 0 {
        spaces.max(minimum)
    } else {
        spaces.clamp(minimum, maximum as usize)
    };
    if wanted == spaces {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + wanted);
    out.push_str(&text[..after_prefix]);
    out.push_str(&" ".repeat(wanted));
    out.push_str(&body[spaces..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn llvm() -> Style {
        Style::default_style()
    }

    fn check(input: &str, expected: &str) {
        assert_eq!(
            normalized(input, &llvm()),
            expected,
            "normalising {input:?}"
        );
    }

    #[test]
    fn one_space_is_inserted_where_there_is_none() {
        check("//using enum X;", "// using enum X;");
        check("//NOLINT", "// NOLINT");
        check("//trailing", "// trailing");
    }

    #[test]
    fn doc_and_reference_markers_keep_their_prefix() {
        check("///doc comment", "/// doc comment");
        check("//!bang", "//! bang");
        check("//:ref", "//: ref");
    }

    #[test]
    fn existing_spacing_is_kept() {
        check("//  two spaces", "//  two spaces");
        check("// already spaced", "// already spaced");
    }

    #[test]
    fn an_empty_comment_stays_empty() {
        // `//` with nothing after it has no text to space: clang-format leaves it alone.
        check("//", "//");
    }

    #[test]
    fn a_block_comment_is_not_touched() {
        check("/*block*/", "/*block*/");
    }
}
