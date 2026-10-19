//! # `textDocument/documentHighlight` — the other places in this file the name under the cursor is used
//!
//! The analysis already answers the hard part: [`local_references_of`] resolves every identifier in a file to the
//! **declaration** it names, by C++'s own lookup (innermost scope outward, shadowing respected) rather than by spelling.
//! That is why this handler is a translation and not a search: the question "where else is this local used" is answered
//! once for the whole file, and the cursor only selects which answer to show.
//!
//! # What is highlighted, and as what
//!
//! ```text
//! the declaration's own name   DocumentHighlightKind::Write   it is where the value is introduced
//! every use                    DocumentHighlightKind::Read
//! ```
//!
//! # Why nothing is highlighted for a project-wide name
//!
//! [`local_references_of`] is about **locals**: a member, a global or a macro is resolved through the index rather than
//! through one file's scopes, and the answer would be a different query. A client asks for a highlight on every cursor
//! move, so the honest behaviour for a name this file cannot resolve alone is an empty list rather than a partial one.

use cpp_code_analysis::VfsFile;
use cpp_code_analysis::sema::declarations::{LocalReferences, local_references_of};
use lsp_types::{
    ClientCapabilities, DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams, OneOf,
    Range, ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{offset_at_position, position_in_file, uri_to_file_path};

pub async fn on_document_highlight(
    context: ServerContextSnapshot,
    params: DocumentHighlightParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<DocumentHighlight>> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let held = session.files().held(&view.path)?;
        let offset = view
            .file_offset_of(offset_at_position(&view, position)?)?
            .get();

        let locals = local_references_of(&view.scopes, &view.root);
        Some(highlights_of(held, &view.source, offset, &locals))
    })
    .await
}

/// Every occurrence of the local the cursor is on — or nothing when the cursor is not on one.
///
/// A free function because the decisions are worth testing without a client: **which** declaration the cursor selects
/// (the one it is the name of, or a use of), and the extent of each highlight (the identifier, not a zero-width point).
pub fn highlights_of(
    file: &VfsFile,
    source: &str,
    offset: usize,
    locals: &[LocalReferences],
) -> Vec<DocumentHighlight> {
    let Some(entry) = locals.iter().find(|entry| {
        entry.declared_at as usize == offset || entry.uses.iter().any(|at| *at as usize == offset)
    }) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    push_highlight(
        file,
        source,
        entry.declared_at as usize,
        DocumentHighlightKind::WRITE,
        &mut found,
    );
    for at in &entry.uses {
        push_highlight(
            file,
            source,
            *at as usize,
            DocumentHighlightKind::READ,
            &mut found,
        );
    }
    found
}

/// One highlight, over the **identifier** that starts at `offset`.
fn push_highlight(
    file: &VfsFile,
    source: &str,
    offset: usize,
    kind: DocumentHighlightKind,
    found: &mut Vec<DocumentHighlight>,
) {
    let Some(text) = source.get(offset..) else {
        return;
    };
    let length = text
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        .count();
    let (Some(start), Some(end)) = (
        position_in_file(file, offset),
        position_in_file(file, offset + length),
    ) else {
        return;
    };
    found.push(DocumentHighlight {
        range: Range { start, end },
        kind: Some(kind),
    });
}

pub struct DocumentHighlightCapabilities;

impl RegisterCapabilities for DocumentHighlightCapabilities {
    fn register_capabilities(server: &mut ServerCapabilities, _client: &ClientCapabilities) {
        server.document_highlight_provider = Some(OneOf::Left(true));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    use super::*;

    /// **The declaration is a write and the uses are reads, and the cursor may be on either.**
    ///
    /// The resolution itself is the analysis's — this test is about the two things the handler decides: which entry a
    /// cursor selects (a use selects its declaration's entry, not a same-spelled neighbour), and the extent of each
    /// highlight (the identifier, which is what a client underlines).
    #[test]
    fn a_local_used_twice_is_one_write_and_two_reads() {
        let text = "void f() {\n    int value = 0;\n    value = value + 1;\n}\n";
        let files = MemoryFiles::new().with_file("/p/a.cpp", text);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.load("/p/a.cpp");

        let path = PathBuf::from("/p/a.cpp");
        let held = session.files().held(&path).expect("the file is held");
        // The view without a project: the file's own text, parsed and scoped, which is what a highlight of a local
        // needs — no include closure is involved in resolving a local.
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        let locals = local_references_of(&view.scopes, &view.root);

        // The cursor on the first use of `value` on the third line: a use selects its declaration's entry.
        let on_a_use = text.find("value = value").expect("the fixture uses it");
        let found = highlights_of(held, &view.source, on_a_use, &locals);
        assert_eq!(found.len(), 3, "the declaration and two uses: {found:?}");
        assert_eq!(found[0].kind, Some(DocumentHighlightKind::WRITE));
        assert!(
            found[1..]
                .iter()
                .all(|highlight| highlight.kind == Some(DocumentHighlightKind::READ)),
            "the uses are reads: {found:?}"
        );
        // The highlight covers the identifier, not a zero-width point.
        assert_eq!(
            found[0].range.start.character, 8,
            "`int value`: the name starts at column 8"
        );
        assert_eq!(
            found[0].range.end.character, 13,
            "`value` is five characters"
        );

        // A cursor on nothing is no highlight at all, which is what a client expects on every blank position.
        assert!(highlights_of(held, &view.source, 0, &locals).is_empty());
    }
}
