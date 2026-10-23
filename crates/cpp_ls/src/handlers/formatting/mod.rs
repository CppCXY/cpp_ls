//! # `textDocument/formatting`, `textDocument/rangeFormatting`, `textDocument/willSaveWaitUntil`
//!
//! The formatter is `cpp_formatter`, a crate of its own; this module is the protocol half — read the file's text, hand
//! it over, and answer in `TextEdit`s.
//!
//! # Why the edits are computed here and not by the formatter
//!
//! A formatter that returned "the new text" would make the client replace the whole buffer, which loses the cursor and
//! the selection and makes every keystroke's undo entry the whole file. The edit list is derived by comparing the old
//! and new text line by line ([`edits_between`]): one `TextEdit` per changed run, with unchanged lines outside it.
//!
//! # What each method means here
//!
//! ```text
//! formatting          the whole file, one edit list
//! rangeFormatting     `cpp_formatter::format_range` — the whole file is formatted, and only the lines inside the
//!                     range are taken from it, so an edit can never reach past the range
//! willSaveWaitUntil   the same as `formatting`, answered before the client writes the file
//! ```
//!
//! # On-type formatting is not advertised
//!
//! `textDocument/onTypeFormatting` was wired and then removed: a client only sends it for trigger characters it has been
//! told about *and* considers worth a request, which in practice made it fire unreliably, and the same re-indentation is
//! already covered by formatting on save or on demand. Advertising a capability that does nothing most of the time is
//! worse than not advertising it.

use std::path::Path;

use lsp_types::{
    ClientCapabilities, DocumentFormattingParams, DocumentRangeFormattingParams, OneOf, Position,
    Range, ServerCapabilities, TextEdit,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::uri_to_file_path;

/// Format the whole document.
pub async fn on_formatting(
    context: ServerContextSnapshot,
    params: DocumentFormattingParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<TextEdit>> {
    format_request(context, params.text_document.uri, None, cancel_token).await
}

/// Format the part of the document the selection covers.
pub async fn on_range_formatting(
    context: ServerContextSnapshot,
    params: DocumentRangeFormattingParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<TextEdit>> {
    format_request(context, params.text_document.uri, Some(params.range), cancel_token).await
}

/// Format the document that is about to be written (`textDocument/willSaveWaitUntil`).
///
/// The same answer as `textDocument/formatting`: the client applies it before saving, which is what makes "format on
/// save" work without the editor having to run a format command first.
pub async fn on_will_save_wait_until(
    context: ServerContextSnapshot,
    params: lsp_types::WillSaveTextDocumentParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<TextEdit>> {
    format_request(context, params.text_document.uri, None, cancel_token).await
}

/// The one implementation the methods share: format, diff.
async fn format_request(
    context: ServerContextSnapshot,
    uri: lsp_types::Uri,
    only: Option<Range>,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<TextEdit>> {
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        // **No `session.view` here**: formatting needs the text and the path, not the file's index entry — and a
        // view is exactly what a file that was opened but never parsed does not have yet.
        let text = read_text(session, &path)?;
        // **The same rule the command-line tool uses**: the nearest `.clang-format`, else the project's fallback.
        let style = cpp_formatter::config::style_for_file(&path);
        // **A range formats only its own lines** (`cpp_formatter::format_range`): the library formats the whole file —
        // a fragment cannot know its indentation — and keeps the lines outside the range as they are.
        let formatted = match only {
            Some(range) => cpp_formatter::format_range(
                &text,
                &style,
                range.start.line as usize,
                range.end.line as usize,
            ),
            None => cpp_formatter::format_file(&path, &text, &style),
        };
        Some(edits_between(&text, &formatted))
    })
    .await
}

/// **The edits that turn `old` into `formatted`**: one per run of changed lines.
///
/// Lines are compared as they are, terminator included, so a file whose only change is a line ending still produces an
/// edit for that line. The common prefix and suffix are left out, which is what keeps the edit small enough for a
/// client to preserve the cursor: a formatter that changed one line of a thousand returns one range, not a thousand.
pub fn edits_between(old: &str, formatted: &str) -> Vec<TextEdit> {
    if old == formatted {
        return Vec::new();
    }
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = formatted.split_inclusive('\n').collect();

    let mut prefix = 0usize;
    while prefix < old_lines.len() && prefix < new_lines.len() && old_lines[prefix] == new_lines[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let old_middle = &old_lines[prefix..old_lines.len() - suffix];
    let new_middle: String = new_lines[prefix..new_lines.len() - suffix].concat();
    // The edit covers whole lines, so both ends are a line start: the number of lines before the change, and that
    // number plus the lines the old middle had. No byte offsets and no line index, which is what makes this function
    // testable without a file.
    vec![TextEdit {
        range: Range {
            start: Position { line: prefix as u32, character: 0 },
            end: Position { line: (prefix + old_middle.len()) as u32, character: 0 },
        },
        new_text: new_middle,
    }]
}

/// **The text of a file as the session has it**: the held copy when it has read one, the file on disk otherwise.
///
/// `held` is what a formatting request usually finds (the buffer was opened and the analysis read it); the disk read is
/// the fallback for a file that is open but not yet read, which is the state a client that formats right after
/// `didOpen` can put the server in.
fn read_text(session: &cpp_code_analysis::Session<cpp_code_analysis::DiskFiles>, path: &Path) -> Option<String> {
    if let Some(held) = session.files().held(path) {
        return Some(held.text.to_string());
    }
    std::fs::read_to_string(path).ok()
}

/// The capabilities this module advertises: whole-file formatting and the range form of it.
///
/// The save-time request is advertised by the document-sync options (`will_save_wait_until`), because that is where the
/// protocol puts it; this type only has to say that both formatting entry points exist.
pub struct FormattingCapabilities;

impl RegisterCapabilities for FormattingCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.document_formatting_provider = Some(OneOf::Left(true));
        server_capabilities.document_range_formatting_provider = Some(OneOf::Left(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_changes_so_there_are_no_edits() {
        assert!(edits_between("int x = 1;\n", "int x = 1;\n").is_empty());
    }

    #[test]
    fn one_changed_line_is_one_edit_covering_only_that_line() {
        let edits = edits_between("int  x = 1;\nint y = 2;\n", "int x = 1;\nint y = 2;\n");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].range.start.line, 0);
        assert_eq!(edits[0].range.end.line, 1);
        assert_eq!(edits[0].new_text, "int x = 1;\n");
    }
}
