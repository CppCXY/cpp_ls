//! **A function definition as a span** — what `AllowShortFunctionsOnASingleLine` needs to decide, and what the reflow
//! will need for every construct.
//!
//! clang-format's rule for a short function is measured, not guessed: a definition whose body holds **one statement**
//! and whose whole text fits the column limit becomes one line under `All` (class methods only under `Inline`, never
//! under `None`), and a body with two statements stays broken even when it would fit:

//! ```text
//! void a() { g(); }                 void b() {
//!                                     g();
//!                                     h();
//!                                   }
//! ```
//!
//! The definition is a **token span**: `start` is the first token of the declaration (`void`, `FormatBuilder::…`), and
//! `open`/`close` are the body's braces. The span is found from the tokens rather than from the tree because the
//! parser gives an out-of-line definition no function node at all (measured on `A::A(int x) : x_(x) { … }`: a
//! `Declaration` with the body as a child).

use crate::roles::{BraceKind, Roles};
use crate::tokens::Stream;

/// One function (or lambda) body, with the span of its definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Definition {
    /// The first token of the definition — the return type, or the qualified name.
    pub start: usize,
    /// The body's `{`.
    pub open: usize,
    /// The body's `}`.
    pub close: usize,
    /// **One statement (or none) in the body**: the only shape clang-format will put on a single line.
    pub single_statement: bool,
    /// **Defined inside a class body** — what `AllowShortFunctionsOnASingleLine: Inline` joins and an out-of-line
    /// definition does not.
    pub inside_a_record: bool,
    /// **A block inside the body** (`if`/`for`/a bare block). Such a body can never be written on one line: the inner
    /// block is split by its own rule (`AllowShortBlocksOnASingleLine`), so joining the outer one leaves a break in the
    /// middle — measured, and the reason a joined function must have a flat body.
    pub nested_block: bool,
}

/// Every function and lambda body in the stream, in source order.
pub fn definitions(stream: &Stream<'_>, roles: &Roles) -> Vec<Definition> {
    use cpp_parser::CppTokenKind as K;
    let mut definitions = Vec::new();
    let mut open: Vec<Option<(usize, BraceKind)>> = Vec::new();
    for index in 0..stream.tokens.len() {
        match stream.tokens[index].kind {
            K::LeftBrace => {
                let kind = roles.brace_kind.get(&stream.tokens[index].start).copied();
                open.push(kind.map(|kind| (index, kind)));
            }
            K::RightBrace => {
                let Some(Some((open_index, kind))) = open.pop() else {
                    continue;
                };
                if !matches!(kind, BraceKind::Function | BraceKind::Lambda) {
                    continue;
                }
                definitions.push(Definition {
                    start: definition_start(stream, open_index),
                    open: open_index,
                    close: index,
                    single_statement: single_statement(stream, open_index, index),
                    nested_block: has_a_nested_block(stream, open_index, index),
                    inside_a_record: open.iter().flatten().any(|(_, kind)| {
                        matches!(
                            kind,
                            BraceKind::Class | BraceKind::Struct | BraceKind::Union
                        )
                    }),
                });
            }
            _ => {}
        }
    }
    definitions
}

/// **Where the definition begins**: walk back from the body's `{` to the end of the previous statement or block.
///
/// The walk is token-level and stops at a `;`, a `{` or a `}` at the definition's own bracket depth, at a **blank
/// line**, and at a directive — the four things a definition cannot continue past. Parentheses and brackets are
/// balanced so that a `;` inside `for (;;)` does not end the walk.
fn definition_start(stream: &Stream<'_>, open: usize) -> usize {
    use cpp_parser::CppTokenKind as K;
    let mut index = open;
    let mut depth = 0usize;
    while index > 0 {
        let previous = index - 1;
        // **A blank line or a directive ends the definition**: without this, walking back from a body swallowed the
        // `#include` block above it (no `;` in an include line) and every short function measured as too wide.
        if stream.gaps[index].newlines > 1 {
            break;
        }
        match stream.tokens[previous].kind {
            K::RightParen | K::RightBracket => depth += 1,
            K::LeftParen | K::LeftBracket => depth = depth.saturating_sub(1),
            K::Semicolon | K::LeftBrace | K::RightBrace if depth == 0 => break,
            // **An access specifier is a label, not part of the definition**: without this, walking back from a
            // constructor in a class body crossed `public:` and the join glued the label to the constructor
            // (`public: T(const T &t) … {}`).
            K::Colon
                if depth == 0
                    && previous > 0
                    && matches!(
                        stream.tokens[previous - 1].kind,
                        K::PublicKeyword | K::PrivateKeyword | K::ProtectedKeyword
                    ) =>
            {
                break;
            }
            K::Hash if depth == 0 => break,
            _ => {}
        }
        index = previous;
    }
    index
}

/// **Is there a block inside the body?** — the second reason a body cannot become one line.
fn has_a_nested_block(stream: &Stream<'_>, open: usize, close: usize) -> bool {
    use cpp_parser::CppTokenKind as K;
    stream.tokens[open + 1..close]
        .iter()
        .any(|token| token.kind == K::LeftBrace)
}

/// **Does the body hold at most one statement?**
///
/// clang-format joins `{ g(); }` and refuses `{ g(); h(); }` (measured). The count is token-level: a statement at the
/// body's own depth ends with `;`, and a nested block is a statement of its own — so `{ if (x) { g(); } }` is one
/// statement (`0` semicolons plus one block) and `{ while (x) { a(); } b(); }` is two.
fn single_statement(stream: &Stream<'_>, open: usize, close: usize) -> bool {
    use cpp_parser::CppTokenKind as K;
    let mut depth = 0usize;
    let mut statements = 0usize;
    for token in &stream.tokens[open + 1..close] {
        match token.kind {
            K::LeftParen | K::LeftBracket | K::LeftBrace => {
                if depth == 0 && token.kind == K::LeftBrace {
                    statements += 1;
                }
                depth += 1;
            }
            K::RightParen | K::RightBracket | K::RightBrace => depth = depth.saturating_sub(1),
            K::Semicolon if depth == 0 => statements += 1,
            _ => {}
        }
        if statements > 1 {
            return false;
        }
    }
    true
}

/// **The width of the body alone**, `{ … }` included, as it would be printed on one line: what a lambda's join needs,
/// because clang-format joins a lambda body even when the lambda's signature already spans several lines (measured).
pub fn body_width(
    stream: &Stream<'_>,
    roles: &Roles,
    style: &crate::config::Style,
    definition: &Definition,
) -> usize {
    let mut width = 0;
    for index in definition.open..=definition.close {
        if index > definition.open {
            width += crate::space::between(stream, index, roles, style);
        }
        width += stream.tokens[index].text.chars().count();
    }
    width
}

/// **The width of the definition written on one line**, indented at `indent`.
///
/// `space::between` is the same function the renderer uses, so the answer is the text that would actually be printed —
/// a rule that guessed at spaces once already made a function look wider than it is.
pub fn one_line_width(
    stream: &Stream<'_>,
    roles: &Roles,
    style: &crate::config::Style,
    definition: &Definition,
    indent: usize,
) -> usize {
    let mut width = indent;
    for index in definition.start..=definition.close {
        if index > definition.start {
            width += crate::space::between(stream, index, roles, style);
        }
        width += stream.tokens[index].text.chars().count();
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definitions_of(source: &str) -> Vec<Definition> {
        let stream = crate::tokens::stream(source);
        let roles = crate::roles::of(source);
        definitions(&stream, &roles)
    }

    #[test]
    fn a_definition_spans_from_the_return_type_to_the_closing_brace() {
        let source = "void f() {\n  g();\n}\n";
        let found = definitions_of(source);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start, 0, "the definition starts at `void`");
        assert!(found[0].single_statement);
    }

    #[test]
    fn two_statements_are_not_one() {
        let source = "void f() {\n  g();\n  h();\n}\n";
        assert!(!definitions_of(source)[0].single_statement);
    }

    #[test]
    fn a_nested_block_is_one_statement_and_a_second_one_is_not() {
        assert!(definitions_of("void f() {\n  if (x) {\n    g();\n  }\n}\n")[0].single_statement);
        assert!(
            !definitions_of("void f() {\n  while (x) {\n    g();\n  }\n  h();\n}\n")[0]
                .single_statement
        );
    }

    #[test]
    fn a_class_method_is_inside_a_record() {
        let source = "class C {\n  void m() {\n    g();\n  }\n};\n";
        let found = definitions_of(source);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].inside_a_record);
        assert!(found[0].single_statement);
    }
    #[test]
    fn an_out_of_line_constructor_starts_at_its_qualified_name() {
        let source = "A::A(int x) : x_(x) {\n  init();\n}\n";
        let found = definitions_of(source);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start, 0);
        assert!(found[0].single_statement);
    }
}
