//! **What a documentation comment refers to** — the two spellings a reader writes, read once.
//!
//! A comment refers to the program in two ways, and until this module existed each consumer knew one of them:
//!
//! ```cpp
//! /// See [`Widget::grow`] for the rule.        a symbol reference
//! /// @see Widget::grow                          a command that takes a reference
//! ```
//!
//! Both name a symbol; the first is a *span* found by scanning, the second is a command's first argument. Everything
//! above this layer — the colour, the hover card, the jump, the completion — needs the same answer to the same
//! question, so the question is asked here and every caller shares it. A second copy of the rule would be a second
//! answer to disagree with.
//!
//! The reader itself lives in `cpp_parser` ([`cpp_parser::doc_links`], [`cpp_parser::doc_references`]): finding the
//! spans is a question about a comment's text, and that is the parser's business. This module decides *which* of the
//! two a cursor is in, and where a documentation comment begins and ends.

use cpp_parser::SourceRange;

/// A symbol a documentation comment names: the span it is written in, and the spelling as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocTarget {
    /// The span of the *target* — the text between a link's backticks, or the argument of `@see` — in the file.
    pub range: SourceRange,
    /// The spelling, as written: `Widget::grow`, `std::vector<int>`, `widget.h`.
    pub target: String,
}

impl DocTarget {
    /// The spelling with the punctuation a *reference* may add, removed: `grow()` and `MAX!` name `grow` and `MAX`.
    ///
    /// Rust's links write `()` to say "this is a call" and `!` to say "this is a macro", and a reader writing C++
    /// prose does the same. Neither is part of the name, and a lookup that kept them would resolve nothing.
    pub fn spelled_for_a_lookup(&self) -> &str {
        self.target
            .trim_end_matches("()")
            .trim_end_matches('!')
            .trim()
    }
}

/// The reference the cursor at `offset` is on, if it is on one.
///
/// A link is asked first: it is the narrower span, and a link written *inside* a command's argument would otherwise be
/// answered by the wider one.
pub fn target_at(text: &str, offset: usize) -> Option<DocTarget> {
    let whole = SourceRange::new(0, text.len());
    let holds = |range: &SourceRange| range.start_offset <= offset && offset <= range.end_offset();

    if let Some(link) = cpp_parser::doc_links(text, whole)
        .into_iter()
        .find(|link| holds(&link.range))
    {
        return Some(DocTarget {
            range: link.range,
            target: link.target,
        });
    }

    cpp_parser::doc_references(text, whole)
        .into_iter()
        .find(|reference| holds(&reference.range))
        .map(|reference| DocTarget {
            range: reference.range,
            target: reference.target,
        })
}

/// Every reference in the text, links and command arguments together, in the order they are written.
pub fn targets(text: &str) -> Vec<DocTarget> {
    let whole = SourceRange::new(0, text.len());
    let mut found: Vec<DocTarget> = cpp_parser::doc_links(text, whole)
        .into_iter()
        .map(|link| DocTarget {
            range: link.range,
            target: link.target,
        })
        .chain(
            cpp_parser::doc_references(text, whole)
                .into_iter()
                .map(|reference| DocTarget {
                    range: reference.range,
                    target: reference.target,
                }),
        )
        .collect();
    found.sort_by_key(|target| (target.range.start_offset, target.range.end_offset()));
    found
}

/// **Is this offset inside a documentation comment?** — the gate both a reference and a completion have to pass.
///
/// A documentation comment is `///` or `//!` on its own line, and `/**`-`/*!` … `*/` for a block; a `//` or `/*` is a
/// comment and not documentation, which is Doxygen's own rule. Asked of the *text* rather than of the lexer's tokens,
/// because a backtick is not a C++ token: `/// See [`Wid` lexes as a comment followed by an unrecognised character
/// and an identifier, and a token-based test answered "code" for the middle of a comment.
pub fn is_a_documentation_comment(text: &str, offset: usize) -> bool {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line_start = before.rfind('\n').map_or(0, |at| at + 1);
    let trimmed = text[line_start..].trim_start();

    if trimmed.starts_with("///") || trimmed.starts_with("//!") || trimmed.starts_with("/**") {
        return true;
    }

    // A `/** … */` block's *middle* lines do not begin with the marker, so the block itself is the question: the
    // nearest opener is the one that matters, and it counts only if nothing closed it first.
    match (before.rfind("/*"), before.rfind("*/")) {
        (Some(opened), closed) => {
            let still_open = closed.is_none_or(|closed| closed < opened);
            still_open
                && (before[opened..].starts_with("/**") || before[opened..].starts_with("/*!"))
        }
        _ => false,
    }
}

/// **Every `@code` … `@endcode` body in `text`**, as spans of the file — **one span per line, comment marker removed**.
///
/// The block's *interior* only: the delimiters are commands and are drawn as such, while the body is an example of
/// code — which is what a highlighter and a completion treat differently from prose.
///
/// # Why the marker is stripped here rather than by every caller
///
/// A code block is written *inside* a comment, so each of its lines begins with the comment's own marker —
/// `/// int main() { return 0; }`, or ` *` in a `/** */` block. A caller that lexed the raw line would see a comment
/// and produce no tokens at all, which is exactly what the first version of the highlighter did. Removing the marker
/// is a fact about comments, which is this module's business, and what comes back is the code, in file coordinates.
pub fn code_blocks(text: &str) -> Vec<SourceRange> {
    let mut spans = Vec::new();
    let mut at = 0usize;

    while let Some(open) = text[at..]
        .find("@code")
        .map(|offset| at + offset)
        .or_else(|| text[at..].find("\\code").map(|offset| at + offset))
    {
        let opens_at = open + "@code".len();
        // The body starts after the rest of the `@code` line — `@code{.cpp}` is a spelling too, and either way what
        // is written after the command on its own line is still the command's argument rather than the example.
        let start = text[opens_at..]
            .find('\n')
            .map_or(text.len(), |offset| opens_at + offset + 1);
        let close = text[start..]
            .find("@endcode")
            .map(|offset| start + offset)
            .or_else(|| text[start..].find("\\endcode").map(|offset| start + offset));
        let end = close.unwrap_or(text.len());

        let mut line_start = start;
        for line in text[start..end].split_inclusive('\n') {
            let indentation = line.len() - line.trim_start().len();
            let body = line.trim_start();
            let marker = if let Some(rest) = body
                .strip_prefix("///")
                .or_else(|| body.strip_prefix("//!"))
                .or_else(|| body.strip_prefix("//"))
                .or_else(|| body.strip_prefix('*'))
            {
                body.len() - rest.len()
            } else {
                0
            };
            let content = body[marker..].trim_end_matches(['\r', '\n']);
            let content = content.strip_prefix(' ').unwrap_or(content);
            if !content.trim().is_empty() {
                spans.push(SourceRange::new(
                    line_start + indentation + marker,
                    content.len(),
                ));
            }
            line_start += line.len();
        }

        match close {
            Some(end) => at = end + "@endcode".len(),
            None => break,
        }
    }

    spans
}

/// **Is the offset inside an `@code` … `@endcode` block?** — where a comment is an example of code rather than prose.
///
/// The commands nest by *line*, which is how Doxygen writes them: `@code` opens on one `///` line and `@endcode`
/// closes several lines later, and each of those lines is a separate comment as far as the C++ lexer is concerned.
/// So the question is which of the two came last before the cursor, not whether the cursor's own line is a comment.
pub fn inside_a_code_block(text: &str, offset: usize) -> bool {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let opened = before.rfind("@code").or_else(|| before.rfind("\\code"));
    let closed = before
        .rfind("@endcode")
        .or_else(|| before.rfind("\\endcode"));

    match (opened, closed) {
        (Some(opened), Some(closed)) => opened > closed,
        (Some(_), None) => true,
        _ => false,
    }
}
