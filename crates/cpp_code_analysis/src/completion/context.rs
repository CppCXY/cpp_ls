//! **What the cursor is in the middle of writing** — the question every other part of completion is asked about.
//!
//! A completion is not one list. The four things a reader asks for when they press the key are the members of an
//! object, the names in a scope, the name of a directive, and the name of a header — and a list built without
//! asking which of the four this is has to be a union of all of them, which is what "everything is offered, and
//! almost nothing of it is what I wanted" means. This module is the reading that decides.
//!
//! ```text
//! w.|            Member          the object's type's members
//! ns::Wid|       Qualified       the names written directly in `ns`
//! loc|           Name            every name visible from here
//! #inc|          DirectiveName   the directives this file may write
//! #include <vec| Include         a header the search path can find
//! // a note|     Nothing         prose is not code
//! ```
//!
//! # Why the whole context is read rather than the token before the cursor
//!
//! The two states a completion is asked in are "nothing written yet" and "half a name written", and the *token*
//! before the cursor tells them apart only by accident: after `return ` and after `return x` the previous
//! significant token is `return` and `x`, but after `w.` and after `w.si` it is `.` in one case and the same
//! identifier in the other as an ordinary name would give. What decides is the **node** — a member access with an
//! unwritten member, a name node with a qualifier — which is why this reads the tree, and why the reading lives
//! here rather than being re-derived by each consumer.
//!
//! # What is deliberately not here
//!
//! * No filter on *semantic* validity beyond the shape: `int|` inside an expression still offers the class names,
//!   because C++ writes a type name as a value (a constructor call, a functional cast) and a list that hid them
//!   would be wrong in the common case to be tidier in the rare one. The ordering is where that judgement lives
//!   (see [`crate::completion`]).
//! * No template-argument bookkeeping: inside `vector<|` the answer is the names in scope, which is exactly what
//!   an unqualified name position means, and the closing `>` is the client's business.

use cpp_parser::{CppSyntaxKind, CppSyntaxNode, SourceRange};

use crate::sema::resolve::{self, MemberAccess, NamePosition};

/// What the cursor is writing.
///
/// Not `PartialEq`: the member variant holds the object expression, which is a syntax node rather than a value.
/// A consumer that has to tell two contexts apart matches on the variant, which is the only question it can
/// answer honestly anyway.
#[derive(Debug, Clone)]
pub enum CompletionContext {
    /// After a `.` or `->`: the members of the object's type.
    Member(Box<MemberAccess>),
    /// Inside or at the end of a `::`-qualified name: the names written directly in that scope.
    Qualified(NamePosition),
    /// An ordinary name position, including a cursor where nothing is written yet.
    Name(NamePosition),
    /// Inside a `#` directive's **name** — `#inc` — where the answer is the directives themselves.
    DirectiveName { written: String, range: SourceRange },
    /// After `#include`, inside the header name.
    Include {
        /// The header name written so far: `vec` for `#include <vec`.
        written: String,
        /// Which delimiter was opened, if one was: `'<'`, `'"'`, or `None` for `#include vec`.
        delimiter: Option<char>,
        /// Whether the name was already closed (`#include <vector>|`), in which case there is nothing to complete.
        closed: bool,
        range: SourceRange,
    },
    /// A macro name in a `#define`/`#ifdef`/`#ifndef`/`#undef` line, or any other directive position where a name
    /// is the answer.
    DirectiveArgument { written: String, range: SourceRange },
    /// Prose, or a position where nothing can be written.
    Nothing,
}

/// The context at `offset`, read from the tree.
///
/// The order of the questions is load-bearing and is the order of specificity: a directive owns everything inside
/// it, a member access owns the name after its operator, a qualified name owns its last segment, and only what is
/// left over is an ordinary name position.
pub fn context_at(root: &CppSyntaxNode, offset: usize) -> CompletionContext {
    // **A comment or a literal is prose.** Asked first because everything below would otherwise answer: a `w.`
    // inside a comment is a member access as far as the tree is concerned.
    if inside_a_comment_or_a_literal(root, offset) {
        return CompletionContext::Nothing;
    }

    if let Some(directive) = directive_at(root, offset) {
        return directive;
    }

    // The member question before the name question, because a member access **is** a name node in this grammar —
    // `w.size` is an `IndexExpr` holding a `NameExpr` — and a reader that asked the name question first would
    // answer "every name in scope" after a `.`.
    if let Some(access) = resolve::member_access_at(root, offset) {
        return CompletionContext::Member(Box::new(access));
    }

    match resolve::name_position_at(root, offset) {
        Some(position) if position.scope.is_empty() => CompletionContext::Name(position),
        Some(position) => CompletionContext::Qualified(position),
        None => CompletionContext::Nothing,
    }
}

/// The directive context at `offset`, when the cursor is inside a `PreprocessorDirective`.
///
/// Read from the directive's own **text** rather than from its tokens, and the reason is that a directive being
/// typed is not a directive the grammar recognises: `#inc` is a `#` followed by an identifier the parser has no
/// rule for, and a reading built on the parsed shape would answer nothing at the one keystroke that asks. The text
/// from the `#` to the cursor is what a preprocessor would see, so that is what this splits.
fn directive_at(root: &CppSyntaxNode, offset: usize) -> Option<CompletionContext> {
    let node = node_ancestor_of_kind(root, offset, CppSyntaxKind::PreprocessorDirective)?;
    let range = node.text_range();
    let start = usize::from(range.start());
    let end = usize::from(range.end());

    // The text from the `#` to the cursor. `get` rather than a slice, so a cursor that is not on a character
    // boundary (possible in a directive holding a non-ASCII identifier) answers `Nothing` instead of panicking.
    let text = node.text().to_string();
    let cut = offset.clamp(start, end).saturating_sub(start);
    let Some(before) = text.get(..cut) else {
        return Some(CompletionContext::Nothing);
    };

    // `#` and the optional space before the name: the answer is the name of the directive itself.
    let after_hash = before.strip_prefix('#')?;
    let bare = after_hash.trim_start();

    // Nothing after the `#` at all, or a partial word that no space has ended yet: the directive's own name.
    // The one case this is not the answer is a **complete** name followed by a space — `#include |` — which is the
    // argument, and `include` is the only argument that has a vocabulary worth offering. `#define |` and
    // `#ifdef |` take an arbitrary name, which is the ordinary name position rather than a directive one.
    let name_ends_at = bare.find(char::is_whitespace);
    let (name, rest) = match name_ends_at {
        Some(at) => (&bare[..at], Some(bare[at..].trim_start())),
        None => (bare, None),
    };

    let complete = name_ends_at.is_some();

    if !complete {
        // The spelling is being typed, and the replacement covers what of it is written.
        let written_at = start + (before.len() - name.len());
        return Some(CompletionContext::DirectiveName {
            written: name.to_string(),
            range: SourceRange::new(written_at, name.len()),
        });
    }

    // A complete name followed by only whitespace — `#include ` with the cursor right after the space — has an
    // argument whose first character has not been typed, and `#define` is the same shape with an ordinary name.
    let Some(rest) = rest else {
        return Some(CompletionContext::DirectiveArgument {
            written: String::new(),
            range: SourceRange::new(offset, 0),
        });
    };

    match name {
        "include" | "include_next" | "if" | "elif" | "ifdef" | "ifndef" | "undef" | "define" | "pragma"
        | "line" | "error" | "warning" | "assert" | "unassert" => {
            Some(header_or_name(name, rest, start, before.len(), offset))
        }
        // Every other directive — `#else`, `#endif`, the null directive — takes no argument, and a list there is
        // a list of names that cannot follow it.
        _ => Some(CompletionContext::Nothing),
    }
}

/// What follows a directive's name: a header name for `#include`, and an ordinary name for the rest.
fn header_or_name(
    directive: &str,
    rest: &str,
    directive_start: usize,
    written_before_cursor: usize,
    offset: usize,
) -> CompletionContext {
    // Where the argument starts in the file: the directive's `#`, plus what was written before the argument, plus
    // the whitespace between them.
    let argument_at = directive_start + written_before_cursor - rest.len();

    // **An unterminated argument is not a header name.** `#include vec|` is a directive whose argument has not
    // begun — a compiler reads the delimiter, not the spelling — so the answer there is the names in scope (a
    // macro) rather than every header on the search path whose name contains `vec`, which on a real toolchain is
    // hundreds of items offered for a word the user is not writing.
    if directive != "include" && directive != "include_next" {
        let (written, range) = segment_written(rest, argument_at, offset);
        return CompletionContext::DirectiveArgument { written, range };
    }

    let delimiter = rest.chars().next().filter(|first| *first == '<' || *first == '"');

    if delimiter.is_none() {
        let (written, range) = segment_written(rest, argument_at, offset);
        return CompletionContext::DirectiveArgument { written, range };
    }

    let inner = &rest[1..];

    // The name is closed when the matching delimiter is already there: `#include <vector>` with the cursor at the
    // end has nothing to complete, and offering headers there would insert a second name.
    let closer = match delimiter {
        Some('<') => Some('>'),
        Some('"') => Some('"'),
        _ => None,
    };
    let closed = closer.is_some_and(|closer| inner.contains(closer));

    // The prefix is what lies **before the cursor**, which for a cursor in the middle of a header name
    // (`#include <vec|tor>`) is the part that filters while the range covers the whole spelling.
    let (written, range) = segment_written(inner, argument_at + 1, offset);

    CompletionContext::Include {
        written,
        delimiter,
        closed,
        range,
    }
}

/// The name written at `rest`, split into what is before the cursor and the range that replaces it.
///
/// The same rule [`NamePosition`] applies to a name segment, for the same reason: what filters is the part before
/// the cursor, and what a client replaces is the whole token — a cursor in the middle of `ve|ctor` filters by `ve`
/// and replaces `vector`.
fn segment_written(rest: &str, at: usize, offset: usize) -> (String, SourceRange) {
    // The token the cursor is in: up to the first character that cannot be part of a name.
    let end_of_token = rest
        .find(|character: char| {
            !(character.is_alphanumeric() || character == '_' || character == '.' || character == '/'
                || character == '-')
        })
        .unwrap_or(rest.len());

    let token = &rest[..end_of_token];
    let cursor_in_token = offset.saturating_sub(at).min(token.len());

    (
        token[..cursor_in_token].to_string(),
        SourceRange::new(at, token.len()),
    )
}

/// The innermost ancestor of the token at `offset` with this kind.
fn node_ancestor_of_kind(
    root: &CppSyntaxNode,
    offset: usize,
    kind: CppSyntaxKind,
) -> Option<CppSyntaxNode> {
    let token = cpp_parser::token_at(root, offset);
    match token {
        Some(token) => token
            .parent_ancestors()
            .find(|node| CppSyntaxKind::from(node.kind()) == kind),
        None => root
            .ancestors()
            .find(|node| CppSyntaxKind::from(node.kind()) == kind),
    }
}

/// Is this token — or the comment it belongs to — prose rather than code?
///
/// # The boundary, which is the whole of the subtlety
///
/// `cpp_parser::token_at` is **right-biased**: an offset at a token's end is the *next* token's start, and past the
/// last token it is `None`. So `// a note|` with the newline still to come lands on the newline and is prose, while
/// the same cursor at the end of the file — `// a note|` and nothing after — lands on nothing at all and would be
/// read as code. The strict `start < offset < end` test below is what makes the two agree: an offset that merely
/// *touches* a comment's end is not inside it, and a cursor on a line of its own after a comment is a name
/// position like any other.
///
/// A **doc comment** is the exception the ancestor walk exists for: its text is re-lexed into finer tokens by the
/// documentation layer, so a cursor inside `/// a note` is not on a `LineComment` token at all — the question has
/// to be asked of the ancestors as well.
fn inside_a_comment_or_a_literal(root: &CppSyntaxNode, offset: usize) -> bool {
    let Some(token) = cpp_parser::token_at(root, offset) else {
        return false;
    };

    if matches!(
        cpp_parser::CppTokenKind::from(token.kind()),
        cpp_parser::CppTokenKind::LineComment
            | cpp_parser::CppTokenKind::BlockComment
            | cpp_parser::CppTokenKind::StringLiteral
            | cpp_parser::CppTokenKind::CharLiteral
    ) {
        let range = cpp_parser::source_range(token.text_range());
        return range.start_offset < offset && offset < range.end_offset();
    }

    token
        .parent_ancestors()
        .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocComment)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The context at a cursor written as `|` in the fixture — one marker per fixture, and the offset is where
    /// the marker is once it has been removed.
    fn context_in(source: &str) -> (CompletionContext, usize, String) {
        let offset = source.find('|').expect("the fixture marks the cursor");
        let text = source.replace('|', "");
        let root = cpp_parser::CppParser::parse(&text, cpp_parser::ParserConfig::default())
            .get_red_root();

        (context_at(&root, offset), offset, text)
    }

    #[test]
    fn a_member_access_is_a_member_context() {
        let (context, ..) = context_in("void f() { Widget w; w.| }\n");
        assert!(
            matches!(context, CompletionContext::Member(_)),
            "{context:?}"
        );
    }

    #[test]
    fn a_qualified_name_is_a_qualified_context() {
        let (context, ..) = context_in("void f() { ns::| }\n");
        match context {
            CompletionContext::Qualified(position) => assert_eq!(position.scope, "ns"),
            other => panic!("a `::` names a scope: {other:?}"),
        }
    }

    #[test]
    fn a_bare_cursor_is_a_name_context() {
        let (context, ..) = context_in("void f() { int x = | }\n");
        match context {
            CompletionContext::Name(position) => {
                assert!(position.scope.is_empty(), "no qualifier was written");
            }
            other => panic!("a cursor in an expression is a name position: {other:?}"),
        }
    }

    #[test]
    fn a_comment_is_not_code() {
        let (context, ..) = context_in("// a note |here\nint x;\n");
        assert!(matches!(context, CompletionContext::Nothing), "{context:?}");
    }

    /// The other side of the same boundary: a cursor at the **end** of a comment is at the start of what comes
    /// next, which is code. `#include <vector> // the header|` with the newline still to come is the case this
    /// exists for — the newline is not trivia to a right-biased token lookup.
    #[test]
    fn the_end_of_a_comment_is_not_inside_it() {
        let (context, ..) = context_in("int a; // a note|\n");
        assert!(
            matches!(context, CompletionContext::Name(_)),
            "the position after a comment is a name position: {context:?}"
        );
    }

    /// A `///` comment is re-lexed by the documentation layer, so its text is not one token — the ancestor walk is
    /// what catches a cursor inside it.
    #[test]
    fn a_doc_comment_is_not_code_either() {
        let (context, ..) = context_in("/// a note |here\nint x;\n");
        assert!(matches!(context, CompletionContext::Nothing), "{context:?}");
    }

    #[test]
    fn a_string_literal_is_not_code() {
        let (context, ..) = context_in("const char* s = \"hello |\";\n");
        assert!(matches!(context, CompletionContext::Nothing), "{context:?}");
    }

    #[test]
    fn a_hash_is_the_directives_own_name() {
        let (context, ..) = context_in("int x;\n#|\n");
        match context {
            CompletionContext::DirectiveName { written, .. } => assert!(written.is_empty()),
            other => panic!("a lone `#` asks for a directive: {other:?}"),
        }
    }

    #[test]
    fn a_partial_directive_name_is_replaced_not_extended() {
        let (context, offset, _) = context_in("int x;\n#inc|\n");
        match context {
            CompletionContext::DirectiveName { written, range } => {
                assert_eq!(written, "inc", "what has been typed filters the list");
                assert_eq!(range.start_offset, offset - 3, "and the edit replaces it");
                assert_eq!(range.length, 3);
            }
            other => panic!("`#inc` is the directive's own name: {other:?}"),
        }
    }

    #[test]
    fn an_include_asks_for_a_header() {
        let (context, ..) = context_in("#include <vec|\n");
        match context {
            CompletionContext::Include {
                written,
                delimiter,
                closed,
                ..
            } => {
                assert_eq!(written, "vec");
                assert_eq!(delimiter, Some('<'));
                assert!(!closed);
            }
            other => panic!("after `#include` the answer is headers: {other:?}"),
        }
    }

    #[test]
    fn a_closed_include_has_nothing_to_complete() {
        let (context, ..) = context_in("#include <vector>|\n");
        match context {
            CompletionContext::Include { closed, .. } => assert!(closed),
            other => panic!("{other:?}"),
        }
    }

    /// **A directive's argument with no delimiter is not a header** — a compiler reads `<` or `"`, not the
    /// spelling, so `#include vec|` is a directive whose argument is an ordinary name (a macro somebody wrote) and
    /// not a request for every header on the search path whose name contains `vec`.
    #[test]
    fn an_include_without_a_delimiter_is_an_ordinary_name_position() {
        let (context, ..) = context_in("#include vec|\n");
        match context {
            CompletionContext::DirectiveArgument { written, .. } => assert_eq!(written, "vec"),
            other => panic!("no delimiter was written: {other:?}"),
        }
    }

    #[test]
    fn an_include_with_the_cursor_after_the_delimiter_has_an_empty_prefix() {
        let (context, ..) = context_in("#include <|\n");
        match context {
            CompletionContext::Include {
                written, delimiter, ..
            } => {
                assert!(written.is_empty(), "nothing is written yet");
                assert_eq!(delimiter, Some('<'));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_endif_takes_no_argument() {
        let (context, ..) = context_in("#endif |\n");
        assert!(matches!(context, CompletionContext::Nothing), "{context:?}");
    }

    #[test]
    fn a_define_argument_is_an_ordinary_name_position() {
        let (context, ..) = context_in("#define |\n");
        assert!(
            matches!(context, CompletionContext::DirectiveArgument { .. }),
            "{context:?}"
        );
    }
}
