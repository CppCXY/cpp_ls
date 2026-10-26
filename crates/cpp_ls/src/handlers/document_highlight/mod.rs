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
        // **The file's own text, for the cursor and for both answers.** A highlight is a set of ranges in the file the
        // client is showing, and every offset below is an offset in that text — a rendering's would be a different
        // coordinate system (directives resolved, lines merged), which is the defect that made `implementation` answer
        // `null` on the same project.
        let view = session.view_of_the_file(&path)?;
        let held = session.files().held(&view.path)?;
        let offset = offset_at_position(&view, position)?.get();

        let locals = local_references_of(&view.scopes, &view.root);
        let mut found = highlights_of(held, &view.source, offset, &locals);
        // **A member, a global or a function is not a local**, and the local answer for it is empty rather than wrong.
        // The same rule Find References applies is what fills the gap, restricted to this file: one class's member,
        // its declaration here, and its uses here.
        if found.is_empty() {
            found = symbol_highlights(
                session,
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(offset),
            );
        }
        Some(found)
    })
    .await
}

/// **Every occurrence of a symbol in this file** — the answer for a member, a global or a function, where
/// [`highlights_of`] answers for a local.
///
/// The set comes from `references::name_level_offsets`, which is the rule Find References and Rename both use: one
/// class's member, its declaration, and its uses — and not another class's member of the same spelling. The protocol
/// wants a *kind* per range, so the declaration's own name is a `Write` and the rest are `Read`s; which one is a
/// declaration is asked of the same facts that produced the list.
fn symbol_highlights<F: cpp_code_analysis::FileProvider + Clone>(
    session: &cpp_code_analysis::Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
) -> Vec<DocumentHighlight> {
    use cpp_code_analysis::Known;

    if !matches!(session.definitions(view, offset), Known::Yes(_)) {
        return Vec::new();
    }
    let here = cpp_code_analysis::file::paths::normalize_path(&view.path, cfg!(windows));

    let mut found = Vec::new();
    for (file, range) in
        crate::handlers::references::name_level_offsets(session, view, offset, true)
    {
        if cpp_code_analysis::file::paths::normalize_path(&file, cfg!(windows)) != here {
            continue;
        }
        let Some(held) = session.files().held(&file) else {
            continue;
        };
        let (Some(start), Some(end)) = (
            position_in_file(held, range.start_offset),
            position_in_file(held, range.end_offset()),
        ) else {
            continue;
        };
        let kind = if is_a_declaration_here(session, view, offset, &file, range) {
            DocumentHighlightKind::WRITE
        } else {
            DocumentHighlightKind::READ
        };
        found.push(DocumentHighlight {
            range: Range { start, end },
            kind: Some(kind),
        });
    }
    found
}

/// Is `(file, range)` one of the declarations the cursor's name resolves to?
fn is_a_declaration_here<F: cpp_code_analysis::FileProvider + Clone>(
    session: &cpp_code_analysis::Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
    file: &std::path::Path,
    range: cpp_parser::SourceRange,
) -> bool {
    let cpp_code_analysis::Known::Yes(defined) = session.definitions(view, offset) else {
        return false;
    };
    let normalized = cpp_code_analysis::file::paths::normalize_path(file, cfg!(windows));
    defined.found.iter().any(|definition| {
        cpp_code_analysis::file::paths::normalize_path(&definition.file, cfg!(windows))
            == normalized
            && definition.fact.name_range == range
    })
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
#[cfg(test)]
mod a_symbol {
    use std::path::PathBuf;

    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    use super::symbol_highlights;

    const BASE_H: &str = "\
struct First {
    int _value;
    void update();
};
struct Second {
    int _value;
    void update();
};
";

    const USE_CPP: &str = "\
#include \"base.h\"
void First::update() { _value = 1; }
void Second::update() { _value = 2; }
";

    /// **A member is highlighted in this file, and another class's member is not.**
    ///
    /// The handler used to answer `[]` for every non-local, which its module documents as deliberate — but a reader
    /// with the cursor on `_value` sees nothing highlighted, and the answer is a query away: the same rule Find
    /// References applies, restricted to the file the client is showing.
    #[test]
    fn is_highlighted_in_this_file_and_another_classs_is_not() {
        let files = MemoryFiles::new()
            .with_file("/p/base.h", BASE_H)
            .with_file("/p/use.cpp", USE_CPP);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files([PathBuf::from("/p/base.h"), PathBuf::from("/p/use.cpp")]);
        session.index_everything();

        let path = PathBuf::from("/p/use.cpp");
        let held = session.files().held(&path).expect("the file is held");
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        let at = USE_CPP
            .find("_value = 1")
            .expect("the fixture writes it in First::update");

        let found = symbol_highlights(
            &session,
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
        );
        println!(
            "highlights: {:?}",
            found
                .iter()
                .map(|highlight| (highlight.range.start.line, highlight.kind))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            found.len(),
            1,
            "one use in this file, and the declaration is in the header: {found:?}"
        );
        assert_eq!(found[0].range.start.line, 1, "the use in `First::update`");
    }
}
