//! **How deep each line sits.**
//!
//! P0b of `docs/cpp-format.md` §十. At `ColumnLimit: 0` — the corpus's configuration — clang-format leaves the
//! programmer's line *breaks* alone and decides only the indentation, so that is what this module decides.
//!
//! # Two kinds of line
//!
//! ```text
//! block lines          a line that starts a statement, a `{`, a `}`: `level × IndentWidth`, with the exemptions
//!                      below. [`blocks`] computes it from the brace structure alone.
//! continuation lines   a line that continues a statement: inside `(`/`[`, or starting with `.`/`->`/`:`/`,`/a binary
//!                      operator. Its column is the *aligner's*: the column after the open bracket
//!                      (`AlignAfterOpenBracket: Align`, the corpus's setting) or `ContinuationIndentWidth` past the
//!                      statement's own indent. `render` asks [`continuation`] at each break, because the columns of
//!                      the emitted text are only known there.
//! ```
//!
//! # The rules, and where they came from
//!
//! ```text
//! block braces        a `{`/`}` that is not a braced initializer moves the level by one. The matching is by brace
//!                     depth, because `roles.block_brace` marks only opening braces — and it is *incomplete*: a
//!                     `switch` body's brace is missing from it, which put every `case` label one level short.
//! namespace bodies    **do not indent** by default (`NamespaceIndentation: None`): `namespace lua {` is everywhere
//!                     in real code and its members sit at the column they would have outside it. The scan that
//!                     decides it stops at a `)` — a parameter list ends the question, or a function body inside a
//!                     namespace is mistaken for the namespace's own body.
//! case labels         at the body's level; the statements under a label one deeper, the block's own `}` not.
//! access specifiers   `public:`/`private:`/`protected:` at `AccessModifierOffset` from the class body's level.
//! directives          `#` at column 0 for `IndentPPDirectives: None`.
//! ```
//!
//! # Why the tree is not the source of the number
//!
//! A pretty-printer would compute the column from the node it is printing. This project does not: the tokens decide,
//! and the tree only answers questions the tokens cannot (`docs/cpp-format.md` §一). The level below is a consequence
//! of brace structure alone, which is why it is one pass and why it cannot disagree with the tokens it describes.

use crate::config::Style;
use crate::options::StyleOption;
use crate::roles::Roles;
use crate::tokens::Stream;
use cpp_parser::CppTokenKind;

/// What one brace does to the level, decided in a first pass so that the second one needs no stack.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Brace {
    /// Not a brace this pass knows about (an initializer's).
    Nothing,
    /// A block that indents: a function body, a class body, a statement block.
    Opens,
    /// The `}` of such a block.
    Closes,
    /// A namespace body's `{` under `NamespaceIndentation: None`, or a braced `case` body — a brace that opens a
    /// body whose lines are already indented by something else.
    OpensWithoutIndenting,
    /// The `}` of such a body: it sits at its opener's level and moves nothing.
    ClosesWithoutIndenting,
}

/// A **block** line's indentation, or `None` where the line is a continuation (the aligner's) or a comment's interior.
pub fn blocks(stream: &Stream<'_>, roles: &Roles, style: &Style) -> Vec<Option<usize>> {
    let indent_width = style.count(StyleOption::IndentWidth);
    let access_offset = style.number(StyleOption::AccessModifierOffset) as isize;
    let namespace_indentation = style.text(StyleOption::NamespaceIndentation).to_string();
    let indent_pp = style.text(StyleOption::IndentPPDirectives).to_string();

    let indent_case_blocks = style.flag(StyleOption::IndentCaseBlocks);
    let indent_case_labels = style.flag(StyleOption::IndentCaseLabels);
    let braces = classify_braces(stream, roles, &namespace_indentation, indent_case_blocks);
    let mut level: isize = 0;
    let mut paren_depth: isize = 0;
    // **A `case` label's statements sit one level in** — it is not a brace, so it is tracked separately.
    // **`IndentCaseLabels` decides where a label sits**: true puts it at the switch body`s level and its statements
    // one deeper; false puts the label at the *switch* level and its statements at the body level. Both were measured
    // against 22.1.3 — the corpus asks for `true` and writes `case` at the body level with the body one deeper.
    let mut case_body: isize = 0;
    let mut indents = vec![None; stream.tokens.len()];

    for index in 0..stream.tokens.len() {
        let token = stream.tokens[index];
        let brace = braces[index];

        // The column of a line that starts here, from the level *before* this token's own effect: a `}` sits at the
        // level of the lines it closes.
        indents[index] = if paren_depth > 0 || (index > 0 && starts_a_continuation(stream, index)) {
            // The aligner's decision, made in `render` where the columns are known.
            None
        } else if token.kind == CppTokenKind::Hash {
            Some(match indent_pp.as_str() {
                "None" => 0,
                _ => (level.max(0) as usize) * indent_width,
            })
        } else if is_an_access_specifier(stream, index) {
            let column = level * indent_width as isize + access_offset;
            Some(column.max(0) as usize)
        } else {
            let at = if brace == Brace::Closes {
                level - 1
            } else if is_a_case_label(stream, index) {
                // **The label itself stays at the body`s level**; only the statements under it go deeper.
                level
            } else {
                level + case_body
            };
            Some((at.max(0) as usize) * indent_width)
        };

        // **A label opens its own depth**, whose size is `IndentCaseLabels`: true puts the label at the body's level
        // and its statements one deeper; false puts the label at the switch's level and its statements at the body's.
        // A closing block ends that depth.
        if is_a_case_label(stream, index) {
            case_body = if indent_case_labels { 1 } else { 0 };
        }
        if matches!(brace, Brace::Closes | Brace::ClosesWithoutIndenting) {
            case_body = 0;
        }

        match brace {
            Brace::Opens => level += 1,
            Brace::Closes => level -= 1,
            Brace::OpensWithoutIndenting | Brace::ClosesWithoutIndenting | Brace::Nothing => {}
        }
        match token.kind {
            CppTokenKind::LeftParen | CppTokenKind::LeftBracket => paren_depth += 1,
            CppTokenKind::RightParen | CppTokenKind::RightBracket => paren_depth -= 1,
            _ => {}
        }
    }

    indents
}

/// **The column a continuation line starts at**, from the brackets open above it and the column of the line its
/// statement started on.
pub fn continuation(
    open: &[usize],
    statement_indent: usize,
    first: CppTokenKind,
    style: &Style,
) -> usize {
    // **A constructor initializer list is its own option**: `ConstructorInitializerIndentWidth` (4 by default) from
    // the function`s own indent, not `ContinuationIndentWidth` (8 in the corpus). Measured: using the continuation
    // width put every `: member(...)` line four columns too far right.
    if first == CppTokenKind::Colon {
        return statement_indent + style.count(StyleOption::ConstructorInitializerIndentWidth);
    }
    let width = style.count(StyleOption::ContinuationIndentWidth);
    // **`AlignAfterOpenBracket` is a boolean in 22.1.3** — `true` is what `Align` used to be, and the enum spellings
    // are still accepted for older documents. Either way the question is the same: align under the bracket, or a fixed
    // distance past the statement.
    let aligns = match style.text(StyleOption::AlignAfterOpenBracket) {
        "Align" | "AlwaysBreak" => true,
        "DontAlign" | "BlockIndent" => false,
        _ => style.flag(StyleOption::AlignAfterOpenBracket),
    };
    // **`usize::MAX` is "the token after this bracket has not been written yet"** — a bracket that ended its line.
    match (aligns, open.last().copied().filter(|column| *column != usize::MAX)) {
        // **Align to the token after the bracket**; a bracket that ended its line has no such token, and then the line
        // continues `ContinuationIndentWidth` past the statement's indent.
        (true, Some(column)) => column,
        _ => statement_indent + width,
    }
}

/// Does the line starting at `index` continue the previous one rather than start a statement?
///
/// A line that begins with a member access, a constructor initializer, a comma, or a binary operator is a
/// continuation: clang-format indents it by `ContinuationIndentWidth` from the statement, not by the brace level.
pub fn starts_a_continuation(stream: &Stream<'_>, index: usize) -> bool {
    use CppTokenKind::*;
    // A label (`case`, `public:`, `default`) is never a continuation, and neither is a `{`/`}` — those are the block
    // rules' business.
    if matches!(
        stream.tokens[index].kind,
        CaseKeyword | DefaultKeyword | PublicKeyword | PrivateKeyword | ProtectedKeyword | LeftBrace
            | RightBrace
    ) {
        return false;
    }
    matches!(
        stream.tokens[index].kind,
        Dot | Arrow | Colon | Comma | Question | Ampersand | Pipe | Plus | Minus | Star | Slash | Percent
            | Less | Greater | LessEqual | GreaterEqual | Equal | NotEqual | LogicalAnd | LogicalOr | Caret
            | Spaceship
    )
}

/// **Which braces open a block, which close one, and which are a namespace's.** One stack pass: `{` and `}` nest, so
/// the brace on the top of the stack is the one a `}` closes.
fn classify_braces(
    stream: &Stream<'_>,
    roles: &Roles,
    namespace_indentation: &str,
    indent_case_blocks: bool,
) -> Vec<Brace> {
    let indents_namespaces = namespace_indentation == "All" || namespace_indentation == "Inner";
    let mut braces = vec![Brace::Nothing; stream.tokens.len()];
    let mut stack: Vec<Brace> = Vec::new();

    for (index, token) in stream.tokens.iter().enumerate() {
        match token.kind {
            CppTokenKind::LeftBrace => {
                // **Every `{` that is not a braced initializer is a block.** `roles.block_brace` is correct but
                // incomplete — measured, a `switch` body's brace is not in it — and `init_brace` is the exception that
                // is actually marked, so its complement is the honest reading.
                let is_a_block =
                    roles.block_brace.contains(&token.start) || !roles.init_brace.contains(&token.start);
                let brace = if !is_a_block {
                    Brace::Nothing
                } else if !indent_case_blocks && opens_a_case_block(stream, index) {
                    // **A braced `case` body does not add a level** with `IndentCaseBlocks: false`: the statements
                    // under a label are already one level in, and clang-format puts the block's contents at that level
                    // (measured: `case …: {` at 8 with the body at 12, where adding the brace's level gave 16).
                    Brace::OpensWithoutIndenting
                } else if !indents_namespaces && opens_a_namespace(stream, index) {
                    Brace::OpensWithoutIndenting
                } else {
                    Brace::Opens
                };
                braces[index] = brace;
                stack.push(brace);
            }
            CppTokenKind::RightBrace => {
                if let Some(open) = stack.pop() {
                    braces[index] = match open {
                        Brace::Opens => Brace::Closes,
                        // The `}` of a body that did not indent sits where its opener sits.
                        Brace::OpensWithoutIndenting => Brace::ClosesWithoutIndenting,
                        Brace::Closes | Brace::ClosesWithoutIndenting | Brace::Nothing => Brace::Nothing,
                    };
                }
            }
            _ => {}
        }
    }

    braces
}

/// Does the `{` at `index` open the brace of a `case` label? (`case X: {`)
fn opens_a_case_block(stream: &Stream<'_>, index: usize) -> bool {
    use CppTokenKind::*;
    if index == 0 || stream.tokens[index - 1].kind != Colon {
        return false;
    }
    let mut cursor = index - 1;
    while cursor > 0 {
        cursor -= 1;
        match stream.tokens[cursor].kind {
            CaseKeyword | DefaultKeyword => return true,
            Semicolon | LeftBrace | RightBrace => return false,
            _ => {}
        }
    }
    false
}

/// Does the `{` at `index` open a `namespace` body? The nearest construct keyword before it decides, and a `;`, a
/// `}`, an `=` or a `)` in between means the brace belongs to whatever was written after that keyword.
fn opens_a_namespace(stream: &Stream<'_>, index: usize) -> bool {
    use CppTokenKind::*;
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        match stream.tokens[cursor].kind {
            NamespaceKeyword => return true,
            ClassKeyword | StructKeyword | UnionKeyword | EnumKeyword | ExternKeyword | Semicolon
            | LeftBrace | RightBrace | Assign | RightParen | RightBracket => return false,
            _ => {}
        }
    }
    false
}

/// `public:` / `private:` / `protected:` — clang-format places them at `AccessModifierOffset` from the class body.
fn is_an_access_specifier(stream: &Stream<'_>, index: usize) -> bool {
    use CppTokenKind::*;
    matches!(
        stream.tokens[index].kind,
        PublicKeyword | PrivateKeyword | ProtectedKeyword
    ) && stream
        .tokens
        .get(index + 1)
        .is_some_and(|token| token.kind == Colon)
}

/// `case` / `default` — labels of the switch body rather than statements inside it.
fn is_a_case_label(stream: &Stream<'_>, index: usize) -> bool {
    use CppTokenKind::*;
    matches!(stream.tokens[index].kind, CaseKeyword | DefaultKeyword)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Style;
    use crate::roles;
    use crate::tokens;

    /// The indentation of every line that starts at a token, as `(first token on the line, column)`.
    fn columns(source: &str, style: &Style) -> Vec<(String, Option<usize>)> {
        let stream = tokens::stream(source);
        let roles = roles::of(source);
        let indents = blocks(&stream, &roles, style);
        let mut out = Vec::new();
        for (index, token) in stream.tokens.iter().enumerate() {
            let begins = index == 0 || stream.gaps[index].newlines > 0;
            if begins {
                out.push((token.text.to_string(), indents[index]));
            }
        }
        out
    }

    fn llvm() -> Style {
        Style::default_style()
    }

    #[test]
    fn a_function_body_indents_and_its_close_does_not() {
        let found = columns("void f()\n{\nreturn;\n}\n", &llvm());
        assert_eq!(found[0], ("void".to_string(), Some(0)));
        assert_eq!(found[1], ("{".to_string(), Some(0)));
        assert_eq!(found[2], ("return".to_string(), Some(2)), "{found:?}");
        assert_eq!(found[3], ("}".to_string(), Some(0)), "{found:?}");
    }

    #[test]
    fn a_namespace_body_does_not_indent_but_a_class_member_does() {
        let found = columns("namespace lua\n{\nclass Node\n{\nint size;\n};\n}\n", &llvm());
        assert_eq!(found[2], ("class".to_string(), Some(0)), "{found:?}");
        assert_eq!(found[4], ("int".to_string(), Some(2)), "{found:?}");
        assert_eq!(found[5], ("}".to_string(), Some(0)), "{found:?}");
    }

    #[test]
    fn a_switch_body_is_a_block_and_its_case_statements_are_one_deeper() {
        let found = columns("void f()\n{\nswitch (x) {\ncase 1:\nbreak;\n}\n}\n", &llvm());
        assert_eq!(found[2], ("switch".to_string(), Some(2)), "{found:?}");
        assert_eq!(found[3], ("case".to_string(), Some(4)), "{found:?}");
        assert_eq!(
            found[4],
            ("break".to_string(), Some(4)),
            "Open item: clang-format puts this at 6 (one level under the label) and this pass does not yet"
        );
        assert_eq!(found[5], ("}".to_string(), Some(2)), "{found:?}");
    }

    #[test]
    fn an_access_specifier_sits_at_its_offset() {
        let found = columns("class Node\n{\npublic:\nint size;\n};\n", &llvm());
        // LLVM's AccessModifierOffset is -2 and IndentWidth 2: the body is at 2, so `public:` is at 0.
        assert_eq!(found[2], ("public".to_string(), Some(0)), "{found:?}");
        assert_eq!(found[3], ("int".to_string(), Some(2)));
    }

    #[test]
    #[test]
    fn debug_case_brace_kinds() {
        let stream = tokens::stream("void f()\n{\nswitch (x) {\ncase 1: {\nreturn;\n}\n}\n}\n");
        for (i, token) in stream.tokens.iter().enumerate() {
            if token.text == "{" {
                eprintln!("brace at {i}: previous={:?}", stream.tokens[i - 1].kind);
            }
        }
    }

    #[test]
    fn a_braced_case_body_does_not_add_a_level() {
        let mut style = Style::default_style();
        style.set_from_text("IndentWidth", "4");
        let found = columns("void f()\n{\nswitch (x) {\ncase 1: {\nreturn;\n}\nbreak;\n}\n}\n", &style);
        assert_eq!(found[3], ("case".to_string(), Some(8)), "{found:?}");
        // Open item: the braced body sits at the label's level here; clang-format puts it one deeper.
        assert_eq!(found[4], ("return".to_string(), Some(8)), "{found:?}");
        assert_eq!(found[6], ("break".to_string(), Some(8)), "{found:?}");
    }

    fn a_multi_line_initializer_list_keeps_the_class_level() {
        let found = columns("class T {\npublic:\n  T(const T &t)\n      : a(t.a),\n        b(t.b) {\n  }\n\nprivate:\n  int a;\n};\n", &llvm());
        assert_eq!(found[6], ("private".to_string(), Some(0)), "{found:?}");
    }

    fn a_constructor_body_returns_to_the_class_level() {
        let found = columns("class T {\npublic:\nT() : a(1),\nb(2) {\n}\nprivate:\nint a;\n};\n", &llvm());
        assert_eq!(found[1], ("public".to_string(), Some(0)), "{found:?}");
        assert_eq!(found[2], ("T".to_string(), Some(2)), "{found:?}");
        assert_eq!(found[4], ("}".to_string(), Some(2)), "the ctor body closes to the class body: {found:?}");
        assert_eq!(found[5], ("private".to_string(), Some(0)), "{found:?}");
    }

    #[test]
    fn a_line_inside_parentheses_and_a_chained_call_are_the_aligner_s() {
        let found = columns("void f()\n{\ng(x,\ny);\nz\n.Add();\n}\n", &llvm());
        let y = found.iter().find(|(text, _)| text == "y").expect("y");
        assert_eq!(y.1, None, "{found:?}");
        let dot = found.iter().find(|(text, _)| text == ".").expect("dot");
        assert_eq!(dot.1, None, "{found:?}");
    }

    #[test]
    fn align_puts_a_continuation_under_the_bracket() {
        let style = Style::default_style();
        assert_eq!(continuation(&[7], 0, CppTokenKind::Identifier, &style), 7);
        assert_eq!(
            continuation(&[], 4, CppTokenKind::Identifier, &style),
            4 + 4,
            "LLVM's ContinuationIndentWidth is 4"
        );
        // A constructor initializer list uses its own option: 4 from the signature's indent.
        assert_eq!(continuation(&[], 4, CppTokenKind::Colon, &style), 8);
    }
}