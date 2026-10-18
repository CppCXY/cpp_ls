//! `bugprone-macro-parentheses`, from clang-tidy: **a macro's replacement list needs its own parentheses.**
//!
//! ```cpp
//! #define TWICE(x) x * 2        // this check says: ((x) * 2)
//! int n = TWICE(1 + 2);         // …because this is `1 + 2 * 2`, which is 5, and the author meant 6
//! ```
//!
//! The rule exists because a macro is a **text substitution**, not a function: whatever expression the caller passes is
//! spliced in without precedence, and the compiler then applies the operators in the order the text happens to have. Every
//! C and C++ style guide says the same thing, which is how one knows the bug is real rather than a taste.
//!
//! # How it is found
//!
//! Over a `#define` whose replacement list is not already parenthesised and contains an operator that binds looser than a
//! postfix expression. A macro with a single token, a braced initialiser, or a call is not this rule's shape, and a
//! replacement list that starts with `(` is already the answer.

use super::{Check, CheckContext, DiagnosticCode};

pub struct MacroParentheses;

impl Check for MacroParentheses {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::MacroParentheses];

    fn check(context: &mut CheckContext<'_>) {
        // **The macro facts, not a scan of the source.** `context.source` is the *reading* — the text after preprocessing,
        // where a macro use is already expanded and the `#define` is gone (measured: a file that begins with
        // `#define TWICE(x) x * 2` reaches a check as `"int f(int n) { return n + 1*2; }"`). The facts carry the
        // replacement list's own range, and `context.written` is the file as its author wrote it.
        let bodies: Vec<(String, usize, usize)> = context
            .summary
            .macros
            .iter()
            .filter_map(|fact| {
                let range = fact.body_range?;
                // The body is sliced again below, where the borrow of `context` is not held; here it only proves the range
                // is inside the file.
                context.written.get(range.start_offset..range.end_offset())?;
                Some((fact.name.clone(), range.start_offset, range.end_offset()))
            })
            .collect();

        for (name, start, end) in bodies {
            let body = context.written.get(start..end).unwrap_or("").trim();
            // A body that is one token, a braced initialiser, a call, or already parenthesised is not this rule's shape.
            if body.is_empty()
                || body.starts_with('(')
                || body.starts_with('{')
                || body.starts_with("do")
                && !has_a_loose_operator(body)
            {
                continue;
            }
            if !has_a_loose_operator(body) {
                continue;
            }
            context.add(
                DiagnosticCode::MacroParentheses,
                cpp_parser::SourceRange::new(start, end - start),
                name.clone(),
                format!(
                    "`{name}`'s replacement list is not parenthesised, so a caller's expression is spliced in without its \
                     precedence — write `#define {name}(…) (…)`"
                ),
            );
        }
    }
}

/// Does the replacement list contain an operator that binds looser than a primary expression, outside any nested
/// parentheses? A leading `-` or `!` is unary, and `->`, `>>` and `::` are not binary operators.
fn has_a_loose_operator(body: &str) -> bool {
    let mut depth = 0i32;
    for (index, character) in body.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '+' | '-' | '*' | '/' | '%' | '&' | '|' | '^' | '?' | '<' | '>' | '=' | '!' if depth == 0 => {
                let before = body[..index].chars().last();
                let after = body[index + 1..].chars().next();
                let is_unary = before.is_none()
                    || before.is_some_and(|previous| {
                        matches!(previous, '(' | '[' | '{' | ',' | '=' | '+' | '-' | '*' | '/' | '!' | '?' | ':')
                    });
                let is_a_suffix = matches!(
                    (character, after),
                    ('-', Some('>')) | ('>', Some('>')) | (':', Some(':'))
                );
                if !is_unary && !is_a_suffix {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}