//! **The renderer: the source with new whitespace, and not one other byte changed.**
//!
//! P0a (`docs/cpp-format.md` §十): the line structure and the indentation stay exactly as the input had them; every
//! gap that is **on one line and holds no comment** is rewritten to the width [`crate::space::between`] decides.
//! Every other gap — a newline, an indentation, a comment, the whitespace around a comment — is copied out of the
//! source verbatim.
//!
//! That asymmetry is the whole safety argument: the token text is copied from the source (never re-serialised), and
//! the only region ever replaced is a run of spaces/tabs that contains no comment and no newline. A file formatted
//! by this pass and then by `clang-format` differs only where the *decisions* differ, which is what the audit
//! measures; a file formatted by this pass can never lose a comment or a token.

use crate::config::Style;
use crate::roles;
use crate::space;
use crate::tokens;

/// Format `source`, keeping its line structure.
pub fn format(source: &str, style: &Style) -> String {
    let stream = tokens::stream(source);
    let roles = roles::of(source);
    let mut out = String::with_capacity(source.len() + source.len() / 16);

    // **A `#` line is copied, not spaced.** Its internal spacing is the preprocessor's (`#  include` is legal,
    // `#include <a/b.h>` is the usual), and `docs/cpp-format.md` leaves `#define` bodies to P4 — a P0 that re-spaced
    // a directive would be a formatter that breaks macros. Measured before this guard: **143** gaps on the LuaParser
    // corpus where `#` and the directive name were pulled apart.
    let mut in_directive = false;

    for index in 0..stream.tokens.len() {
        let gap = &stream.gaps[index];
        let same_line = gap.newlines == 0 && gap.comments.is_empty() && !gap.at_end;
        if same_line && index > 0 && !in_directive {
            out.push_str(&" ".repeat(space::between(&stream, index, &roles, style)));
        } else {
            // Verbatim: the gap's own bytes (newlines, indentation, comments) are not ours to change in this pass.
            out.push_str(source.get(gap.start..gap.end).unwrap_or(""));
        }
        out.push_str(stream.tokens[index].text);

        // A `#` that starts a line opens a directive; the first newline after it closes the directive.
        if stream.tokens[index].kind == cpp_parser::CppTokenKind::Hash
            && (index == 0 || gap.newlines > 0)
        {
            in_directive = true;
        }
        if in_directive && stream.gaps.get(index + 1).is_some_and(|next| next.newlines > 0) {
            in_directive = false;
        }
    }

    if let Some(trailing) = stream.gaps.last() {
        out.push_str(source.get(trailing.start..trailing.end).unwrap_or(""));
    }

    // **The token stream is verified, not assumed.** Removing a space can *merge* two tokens — `> >` becomes `>>`
    // (a right shift), `+ +` becomes `++`, `/ /` becomes a comment — and no amount of care in the spacing rules
    // makes that impossible: it is a property of the lexer, not of the rules. Measured on the corpus before this
    // check existed: one header (`LuaParseError.h`) came out with a different token stream than it went in with.
    //
    // The fallback is the whole file, unchanged. A file that cannot be formatted without changing what it says is
    // a file this tool declines to touch — which is the only honest failure mode for a formatter, and it makes
    // invariant I1 (`docs/cpp-format.md` §七) hold by construction rather than by testing.
    if same_tokens(&stream, &out) {
        out
    } else {
        source.to_string()
    }
}

/// Do `before` and `after` hold the same token sequence (kind and text, in order)?
fn same_tokens(before: &tokens::Stream<'_>, after: &str) -> bool {
    let after = tokens::stream(after);
    before.tokens.len() == after.tokens.len()
        && before
            .tokens
            .iter()
            .zip(&after.tokens)
            .all(|(mine, theirs)| mine.kind == theirs.kind && mine.text == theirs.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn llvm() -> Style {
        Style::default_style()
    }

    #[test]
    fn only_the_spaces_inside_a_line_are_rewritten() {
        let source = "int  x  =  1;\n";
        assert_eq!(format(source, &llvm()), "int x = 1;\n");
    }

    #[test]
    fn comments_and_their_whitespace_are_left_alone() {
        let source = "int  x = 1;  //  keep   me\n";
        assert_eq!(format(source, &llvm()), "int x = 1;  //  keep   me\n");
    }

    #[test]
    fn the_line_structure_and_indentation_are_untouched() {
        let source = "void f() {\n      int  x = 1;\n}\n";
        assert_eq!(format(source, &llvm()), "void f() {\n      int x = 1;\n}\n");
    }

    #[test]
    fn the_token_stream_is_preserved() {
        // The rule the module exists to keep: only whitespace changes.
        let source = "#include <vector>\n\ntemplate <class T>\nT  f(T  x) { return x; }\n";
        let formatted = format(source, &llvm());
        let before: Vec<_> = tokens::stream(source)
            .tokens
            .iter()
            .map(|token| (token.kind, token.text))
            .collect();
        let after: Vec<_> = tokens::stream(&formatted)
            .tokens
            .iter()
            .map(|token| (token.kind, token.text))
            .collect();
        assert_eq!(before, after);
    }

    #[test]
    fn a_braced_initializer_keeps_its_shape() {
        let source = "std::vector<int> v = { 1,2,3 };\n";
        assert_eq!(format(source, &llvm()), "std::vector<int> v = {1, 2, 3};\n");
    }

    #[test]
    fn two_tokens_are_never_glued_into_one() {
        // `> >` must not become `>>`: the tree keeps the two `>` separate, and merging them would change the
        // reading of the file. The formatter declines the file instead of writing something else than it read.
        let source = "std::vector<std::vector<int> > v;\n";
        let formatted = format(source, &llvm());
        let before: Vec<_> = tokens::stream(source).tokens.iter().map(|t| t.text).collect();
        let after: Vec<_> = tokens::stream(&formatted).tokens.iter().map(|t| t.text).collect();
        assert_eq!(before, after);
    }
}
