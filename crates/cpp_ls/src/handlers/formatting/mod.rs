//! # `textDocument/formatting`, `textDocument/rangeFormatting`, `textDocument/onTypeFormatting`
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
//! rangeFormatting     the same list, filtered to the edits that touch the requested range
//! onTypeFormatting    the same list, filtered to the edit that touches the line being typed on
//! ```
//!
//! Filtering rather than re-formatting a fragment is deliberate: `cpp_formatter` needs the whole file to know a
//! statement's indentation, and a fragment formatted alone comes out indented from column zero. A client that formats a
//! selection therefore gets the edits the whole-file pass would have made inside it, and nothing else moves.

use std::path::Path;

use lsp_types::{
    ClientCapabilities, DocumentFormattingParams, DocumentOnTypeFormattingOptions,
    DocumentOnTypeFormattingParams, DocumentRangeFormattingParams, OneOf, Position, Range,
    ServerCapabilities, TextEdit,
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

/// Format the line that was just typed on (`}` and `;` are the triggers this server advertises).
pub async fn on_type_formatting(
    context: ServerContextSnapshot,
    params: DocumentOnTypeFormattingParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<TextEdit>> {
    let line = params.text_document_position.position.line;
    let range = Range {
        start: Position { line, character: 0 },
        end: Position { line: line + 1, character: 0 },
    };
    format_request(context, params.text_document_position.text_document.uri, Some(range), cancel_token)
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

/// The one implementation the three methods share: format, diff, filter.
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
        // **No `session.view` here**: formatting needs the text and the path, not the file`s index entry — and a
        // view is exactly what a file that was opened but never parsed does not have yet.
        let text = read_text(session, &path)?;
        // **The same rule the command-line tool uses**: the nearest `.clang-format`, else the default style.
        let style = cpp_formatter::config::style_for_file(&path);
        let formatted = cpp_formatter::format_file(&path, &text, &style);
        let edits = edits_between(&text, &formatted);
        let edits: Vec<TextEdit> = match only {
            Some(range) => edits.into_iter().filter(|edit| touches(edit, range)).collect(),
            None => edits,
        };
        Some(edits)
    })
    .await
}

/// **The edits that turn `file`'s text into `formatted`**: one per run of changed lines.
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
    // number plus the lines the old middle had. No byte offsets and no line index, which is what makes this
    // function testable without a file.
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
fn touches(edit: &TextEdit, range: Range) -> bool {
    edit.range.start.line <= range.end.line && range.start.line <= edit.range.end.line
}

pub struct FormattingCapabilities;

impl RegisterCapabilities for FormattingCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.document_formatting_provider = Some(OneOf::Left(true));
        server_capabilities.document_range_formatting_provider = Some(OneOf::Left(true));
        // **The two characters a C++ file is worth re-indenting after**: `}` closes a block, `;` ends a statement,
        // and both change what the next line's indentation should be.
        server_capabilities.document_on_type_formatting_provider = Some(DocumentOnTypeFormattingOptions {
            first_trigger_character: "}".to_string(),
            more_trigger_character: Some(vec![";".to_string()]),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_code_analysis::{CompilerConfig, MemoryFiles, Session};

    fn held(text: &str) -> cpp_code_analysis::VfsFile {
        use cpp_code_analysis::{OpenDocuments, SessionFiles, WatchFilter};
        let files = MemoryFiles::new().with_file("/p/a.cpp", text);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
        session.files().held(std::path::Path::new("/p/a.cpp")).expect("the file is held").clone()
    }
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

    #[test]
    fn an_edit_that_spans_a_range_touches_it() {
        let edit = TextEdit {
            range: Range {
                start: Position { line: 2, character: 0 },
                end: Position { line: 9, character: 0 },
            },
            new_text: String::new(),
        };
        let asked = Range {
            start: Position { line: 5, character: 0 },
            end: Position { line: 6, character: 0 },
        };
        assert!(touches(&edit, asked));
        let outside = Range {
            start: Position { line: 20, character: 0 },
            end: Position { line: 21, character: 0 },
        };
        assert!(!touches(&edit, outside));
    }
}