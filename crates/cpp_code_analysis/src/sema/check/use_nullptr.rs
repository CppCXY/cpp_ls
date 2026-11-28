//! `modernize-use-nullptr`, from clang-tidy: **`NULL` is a macro for `0`, and `0` is an `int`.**
//!
//! ```cpp
//! Widget* w = NULL;                        // this check says: nullptr
//! void f(Widget*);
//! f(NULL);                                 // …and picks the wrong overload where one exists
//! ```
//!
//! The bug the rule prevents is not stylistic: with `void f(int)` beside `void f(Widget*)`, `f(NULL)` calls the `int`
//! overload, because the macro expands to `0`. C++11's `nullptr` has the pointer type and does not.
//!
//! # Why the file's text rather than the tree
//!
//! Because the spelling is the whole evidence, and **the tree does not keep it**: `NULL` is a macro, so the moment the
//! header that defines it has been read the cooked tree holds `0` and nothing in it says a reader wrote `NULL`. That
//! made the finding depend on *what else the run had read* — measured on the corpus, `use-nullptr` was reported for
//! `findings/style.cpp` checked on its own and lost as soon as `clean/basics.cpp`, which includes `<cstddef>`, was in
//! the same run; `cpp_check --index`, which reads the whole project first, lost it every time. Reading the written text
//! answers the question the rule actually asks, and [`CheckContext::comments_and_literals`] is the same list
//! `cstyle-cast` scans with, so a comment or a string that says `NULL` is not a use.
//!
//! What is skipped: **a directive**, because `#define NULL 0`, `#ifndef NULL` and `#undef NULL` are the macro's own
//! spelling rather than a place a reader used it.

use super::{Check, CheckContext, DiagnosticCode};

pub struct UseNullptr;

/// `NULL` — four characters, and the rule's whole subject.
const NULL: &str = "NULL";

impl Check for UseNullptr {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UseNullptr];

    fn check(context: &mut CheckContext<'_>) {
        let protected = context.comments_and_literals();
        let text = context.written;
        let mut from = 0usize;
        while let Some(found) = text[from..].find(NULL) {
            let start = from + found;
            from = start + NULL.len();
            // A word, not a fragment of a longer one — `NULLABLE` and `MY_NULL` are other names.
            if text[..start]
                .chars()
                .next_back()
                .is_some_and(is_a_name_character)
                || text[from..].chars().next().is_some_and(is_a_name_character)
            {
                continue;
            }
            if context.in_a_comment_or_literal(start, &protected) {
                continue;
            }
            if in_a_directive(text, start) {
                continue;
            }
            context.add_in_the_file(
                DiagnosticCode::UseNullptr,
                cpp_parser::SourceRange::new(start, NULL.len()),
                NULL.to_string(),
                "`NULL` is `0` in every standard header, so this picks an `int` overload where one exists — write `nullptr`, which has the pointer type.".to_string(),
            );
        }
    }
}

/// Is this the kind of character a name is written with?
fn is_a_name_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// **Is the offset on a preprocessor directive's line?** — the line's first non-space character is `#`.
fn in_a_directive(text: &str, offset: usize) -> bool {
    let line_start = text[..offset].rfind('\n').map_or(0, |at| at + 1);
    text[line_start..offset].trim_start().starts_with('#')
}
