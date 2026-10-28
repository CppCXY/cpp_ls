//! **The completion inside a format string** — the closed vocabulary of the grammar under the cursor.
//!
//! ```cpp
//! printf("%|", x)          d i o u x X e E f F g G a A c s p n m   — each with what it means
//! printf("%5.|", x)        the same list, after the precision
//! std::format("{:|}", x)   the presentation types, and the flags at the start of a spec
//! scanf("%|", &n)          the conversions, and what each one reads
//! ```
//!
//! # Why the answer here replaces the language's own list
//!
//! Inside a format string there are no names, no types and no keywords: `"%5."` is followed by a conversion letter
//! and by nothing else. So when the cursor is inside a format string this module answers **instead of** the ordinary
//! completion, including when its own list is empty — a list of the names in scope inside `"%5."` is a list of things
//! that cannot be written there.
//!
//! # What the two layers are
//!
//! The vocabulary and the "may this go here" test are [`cpp_code_analysis::format::completions_at`] — the same
//! module that decides which bytes are a piece, so the list cannot offer a character the scanner would not read as a
//! conversion. This file turns that into `CompletionItem`s: the item's kind, the text edit that **replaces** the
//! character under the cursor, and the filter text.

use cpp_code_analysis::VfsFile;
use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::format::completions_at;
use cpp_code_analysis::{DiskFiles, FileView, Session};
use lsp_types::{CompletionItem, CompletionItemKind, Position, Range, TextEdit};

/// **The format completions at `in_the_file`**, or `None` when the cursor is not inside a format string at all.
///
/// `Some(Vec::new())` is a real answer and a different one from `None`: it means "this cursor is inside a format
/// string, and nothing may go here" — which must **not** fall through to the language's own list.
pub fn format_string_complete(
    session: &Session<DiskFiles>,
    view: &FileView,
    file: &VfsFile,
    in_the_file: FileOffset,
) -> Option<Vec<CompletionItem>> {
    // The offsets are the file's, so a rendering cannot answer — see [`super`].
    if view.written_text().is_some() {
        return None;
    }
    let config = super::config_for(session)?;

    let literals = session.format_literals(view, &config);
    let literal = literals
        .iter()
        .find(|literal| literal.range.contain(in_the_file.get()))?;

    let text = view
        .source
        .get(literal.range.start_offset..literal.range.end_offset())?;
    let at = in_the_file.get() - literal.range.start_offset;
    let found = completions_at(text, literal.kind, at);

    // **The edit is in file coordinates**, which is what a client applies: the piece's offsets are inside the literal,
    // and the literal's start is the offset to add. The protocol wants positions, so the file's line index turns them
    // into a range — the same index `position_in_file` uses everywhere else.
    let start_of = |offset: usize| position_of(file, literal.range.start_offset + offset);
    Some(
        found
            .into_iter()
            .map(|completion| {
                let range = match (start_of(completion.start), start_of(completion.end)) {
                    (Some(start), Some(end)) => Some(Range::new(start, end)),
                    _ => None,
                };
                CompletionItem {
                    label: completion.label.clone(),
                    kind: Some(CompletionItemKind::ENUM_MEMBER),
                    detail: Some(completion.detail),
                    filter_text: Some(completion.label.clone()),
                    text_edit: range.map(|range| {
                        lsp_types::CompletionTextEdit::Edit(TextEdit::new(
                            range,
                            completion.label.clone(),
                        ))
                    }),
                    ..CompletionItem::default()
                }
            })
            .collect(),
    )
}

/// A byte offset in the file, as a protocol position.
fn position_of(file: &VfsFile, offset: usize) -> Option<Position> {
    if offset > file.text.len() {
        return None;
    }
    crate::util::position_in_file(file, offset)
}
