//! # `textDocument/formatting`, `textDocument/rangeFormatting`, `textDocument/willSaveWaitUntil`
//!
//! **The formatter is whatever `[format] formatter` names**: this repository's own `cpp_formatter` (a library call),
//! or a program — `clang-format` by default, by name or by path (a child process). [`formatter`] is where that choice
//! and the process live; this module is the protocol half — read the file's text, hand it over, answer in `TextEdit`s
//! — and the two formatters answer the same shape, so nothing below has to know which one ran.
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
//! rangeFormatting     the selected lines only: `cpp_formatter::format_range` for the built-in formatter, and
//!                     `clang-format --lines` for a program — an edit can never reach past the range
//! willSaveWaitUntil   the same as `formatting`, answered before the client writes the file
//! ```
//!
//! # On-type formatting is not advertised
//!
//! `textDocument/onTypeFormatting` was wired and then removed: a client only sends it for trigger characters it has been
//! told about *and* considers worth a request, which in practice made it fire unreliably, and the same re-indentation is
//! already covered by formatting on save or on demand. Advertising a capability that does nothing most of the time is
//! worse than not advertising it.
//!
//! # The hover's macro expansion keeps the built-in formatter
//!
//! Hovering a macro lays its body out with [`cpp_formatter::reflow`], chosen in `cpp_code_analysis` and **not** by
//! this module. It stays on the built-in formatter whatever `[format] formatter` says: a card is produced inside the
//! server, and one child process per hover would be a request the editor never asked for. [`formatter`]'s own docs
//! say so where the choice is made.

use std::path::Path;

use lsp_types::{
    ClientCapabilities, DocumentFormattingParams, DocumentRangeFormattingParams, OneOf, Position,
    Range, ServerCapabilities, TextEdit,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::uri_to_file_path;

pub mod formatter;

use formatter::{Formatter, format_with_clang_format};

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
    format_request(
        context,
        params.text_document.uri,
        Some(params.range),
        cancel_token,
    )
    .await
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

/// The one implementation the methods share: read the text and the configured formatter, format, diff.
async fn format_request(
    context: ServerContextSnapshot,
    uri: lsp_types::Uri,
    only: Option<Range>,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<TextEdit>> {
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    // **The text and the choice, in one snapshot**: both are answers about a file's session, and the formatting itself
    // is not — it happens below, off the snapshot, because a whole-file format is 100 ms of work and a child process
    // is longer than any query thread should be held for.
    let asked = cancel_token.clone();
    let read = snapshot_query(context.analysis(), asked, move |session| {
        let path = uri_to_file_path(&uri)?;
        // **No `session.view` here**: formatting needs the text and the path, not the file's index entry — and a view
        // is exactly what a file that was opened but never parsed does not have yet.
        let text = read_text(session, &path)?;
        let formatter = Formatter::of(session.project_config().config.format.formatter());
        Some((path, text, formatter))
    })
    .await;
    // `snapshot_query` has already turned the closure's `None` into a `Missing` answer — a file with no text has
    // nothing to format, which is not a formatted empty file.
    let (path, text, chosen) = match read {
        RequestOutcome::Ready(value) => value,
        RequestOutcome::Missing => return RequestOutcome::Missing,
        RequestOutcome::Cancelled(source) => return RequestOutcome::Cancelled(source),
        RequestOutcome::Failed { code, message } => {
            return RequestOutcome::Failed { code, message };
        }
    };

    let formatted = match &chosen {
        Formatter::Builtin => match the_built_in_answer(path.clone(), text.clone(), only).await {
            Ok(text) => text,
            Err(error) => return error,
        },
        Formatter::Program(program) => {
            let lines = only.map(formatter::lines_of);
            match format_with_clang_format(program, &path, &text, lines, &cancel_token).await {
                // **A range keeps to its lines even when the program did not.** See
                // [`formatter::keep_only_the_lines`] for the measurement: `clang-format --lines=1:1` answers the
                // neighbouring line reindented too.
                Ok(formatted) => match lines {
                    Some((first, last)) => {
                        formatter::keep_only_the_lines(first, last, &text, &formatted)
                    }
                    None => formatted,
                },
                // **A formatter that is not there is a warning, not an error**: the built-in one answers the request,
                // so format-on-save keeps working, and the reader is told once what happened and how to fix either
                // side of it. A refusal — a program that ran and said no — is logged with its own words, and the
                // file is still formatted by the built-in one for the same reason.
                Err(failure) => {
                    let said = failure.said(program);
                    log::warn!("{said}; formatting with the built-in cpp_formatter instead");
                    if formatter::warn_once(program) {
                        context.client().show_message(lsp_types::ShowMessageParams {
                            typ: lsp_types::MessageType::WARNING,
                            message: format!(
                                "format.formatter names {program:?}: {said}. The built-in cpp_formatter is used \
                                 instead — install clang-format, or set formatter = \"cpp_formatter\" in \
                                 .cppls.toml to choose it deliberately."
                            ),
                        });
                    }
                    match the_built_in_answer(path.clone(), text.clone(), only).await {
                        Ok(text) => text,
                        Err(error) => return error,
                    }
                }
            }
        }
    };

    RequestOutcome::Ready(edits_between(&text, &formatted))
}

/// **The built-in formatter's answer, off the async workers** — the style lookup reads `.clang-format` from disk and
/// the passes walk the whole file, so this belongs on a blocking thread.
///
/// Shared by the two callers that want it: a project that named `cpp_formatter`, and one whose program is not there.
async fn the_built_in_answer(
    path: std::path::PathBuf,
    text: String,
    only: Option<Range>,
) -> Result<String, RequestOutcome<Vec<TextEdit>>> {
    let lines = only.map(|range| (range.start.line as usize, range.end.line as usize));
    match tokio::task::spawn_blocking(move || format_with_the_builtin(&path, &text, lines)).await {
        Ok(formatted) => Ok(formatted),
        Err(error) => Err(RequestOutcome::Failed {
            code: lsp_server::ErrorCode::InternalError,
            message: format!("the built-in formatter did not finish: {error}"),
        }),
    }
}

/// **The built-in formatter**, which is a library call and knows nothing about the protocol.
fn format_with_the_builtin(path: &Path, text: &str, lines: Option<(usize, usize)>) -> String {
    // **The same rule the command-line tool uses**: the nearest `.clang-format`, else the project's fallback.
    let style = cpp_formatter::config::style_for_file(path);
    match lines {
        Some((first, last)) => cpp_formatter::format_range(text, &style, first, last),
        None => cpp_formatter::format_file(path, text, &style),
    }
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
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
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
            start: Position {
                line: prefix as u32,
                character: 0,
            },
            end: Position {
                line: (prefix + old_middle.len()) as u32,
                character: 0,
            },
        },
        new_text: new_middle,
    }]
}

/// **The text of a file as the session has it**: the held copy when it has read one, the file on disk otherwise.
///
/// `held` is what a formatting request usually finds (the buffer was opened and the analysis read it); the disk read is
/// the fallback for a file that is open but not yet read, which is the state a client that formats right after
/// `didOpen` can put the server in.
fn read_text(
    session: &cpp_code_analysis::Session<cpp_code_analysis::DiskFiles>,
    path: &Path,
) -> Option<String> {
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
