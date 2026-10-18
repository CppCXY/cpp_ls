//! `modernize-use-auto`, from clang-tidy, narrowed to the case that is never wrong: **an iterator declared by its full
//! spelling.**
//!
//! ```cpp
//! for (std::vector<Widget>::iterator it = widgets.begin(); …)   // this check says: auto it = …
//! ```
//!
//! The full spelling is the worst kind of maintenance: rename the container's element type and every declaration of its
//! iterator has to change with it, and the compiler says nothing until the one place that was missed. `auto` is the same
//! type, by the language's own deduction.
//!
//! # How it is found
//!
//! A scan of the text for `::iterator` and `::const_iterator` followed by an identifier — the two spellings every standard
//! container defines, and the only ones this rule claims. The general `modernize-use-auto` (any declaration whose type is
//! written twice) needs the type to be *known*, which the summary does not carry; this narrowed rule needs only the text.

use super::{Check, CheckContext, DiagnosticCode};

pub struct UseAuto;

impl Check for UseAuto {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UseAuto];

    fn check(context: &mut CheckContext<'_>) {
        // **The file, not the reading.** The iterator spelling is a fact about the text its author wrote, and a spelling a
        // macro produced is not a declaration anybody can edit — `add_in_the_file` says the range already is a file offset.
        let source = context.written;
        let protected = context.comments_and_literals();
        for spelling in ["::iterator", "::const_iterator"] {
            let mut from = 0usize;
            while let Some(at) = source[from..].find(spelling) {
                let at = from + at;
                from = at + spelling.len();
                // What follows must be an identifier — `std::vector<int>::iterator it` — and not another `::`.
                if context.in_a_comment_or_literal(at, &protected) {
                    continue;
                }
                let after = &source[at + spelling.len()..];
                let after_trimmed = after.trim_start();
                if after_trimmed.starts_with("::") {
                    continue;
                }
                let Some(first) = after_trimmed.chars().next() else {
                    continue;
                };
                if !(first.is_alphabetic() || first == '_') {
                    continue;
                }
                let name: String = after_trimmed
                    .chars()
                    .take_while(|character| character.is_alphanumeric() || *character == '_')
                    .collect();
                let start = source[..at].rfind(|character: char| !(character.is_alphanumeric() || matches!(character, ':' | '_' | '<' | '>' | ',' | ' ')))
                    .map(|boundary| boundary + 1)
                    .unwrap_or(0);
                context.add_in_the_file(
                    DiagnosticCode::UseAuto,
                    cpp_parser::SourceRange::new(start, at + spelling.len() - start),
                    format!("{spelling} {name}"),
                    format!(
                        "`{name}` is declared by its full iterator spelling — write `auto {name}`, which is the same type \
                         and does not need editing when the container's element type changes"
                    ),
                );
            }
        }
    }
}