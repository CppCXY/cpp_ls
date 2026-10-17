//! # `textDocument/documentLink` — where an `#include` points
//!
//! A document link is the protocol's way of saying "this span is a reference to something outside the buffer, and the
//! editor may make it clickable". For C++ there is exactly one such span that every reader wants: the header name in
//! `#include "widget.h"`. The analysis already resolved it — [`IncludeFact::resolved`] is the answer of the same search
//! the compiler would make — so this handler is a translation from an offset to a line and from a path to a URL, and
//! nothing else.
//!
//! # What is not offered
//!
//! A link for an include that **did not resolve**. The protocol allows a link with no target (a client may then ask the
//! server to resolve it later), and a server that sent one anyway would be drawing an underline that leads nowhere. The
//! unresolved include is already reported as a `include-not-found` diagnostic, which is the honest place for it.
//!
//! # Why no `resolve` provider
//!
//! Because everything a link needs is known when the line is read: the range from the fact and the target from the
//! resolution. A `resolve` round trip would be a second request to learn what the first answer already held.

use lsp_types::{
    ClientCapabilities, DocumentLink, DocumentLinkOptions, DocumentLinkParams, ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{path_to_uri, position_in_file, uri_to_file_path};

pub async fn on_document_link(
    context: ServerContextSnapshot,
    params: DocumentLinkParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<DocumentLink>> {
    let uri = params.text_document.uri;

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let held = session.files().held(&view.path)?;
        let summary = session.index().summary(&view.path)?;

        Some(
            summary
                .includes
                .iter()
                .filter_map(|include| document_link_of(held, include))
                .collect(),
        )
    })
    .await
}

/// One `#include`, as the protocol's link — or `None` when it leads nowhere.
///
/// A function of its own because the two decisions it makes are worth testing without a client: **which** span the link
/// covers (the header name, not the `#include` keyword), and whether the link is worth sending at all (an unresolved
/// include is not).
pub fn document_link_of(
    file: &cpp_code_analysis::VfsFile,
    include: &cpp_code_analysis::IncludeFact,
) -> Option<DocumentLink> {
    let resolved = include.resolved.as_ref()?;
    let start = position_in_file(file, include.range.start_offset)?;
    let end = position_in_file(file, include.range.end_offset())?;

    Some(DocumentLink {
        range: lsp_types::Range { start, end },
        target: path_to_uri(resolved),
        tooltip: Some(include.spelling.clone()),
        data: None,
    })
}

pub struct DocumentLinkCapabilities;

impl RegisterCapabilities for DocumentLinkCapabilities {
    fn register_capabilities(server: &mut ServerCapabilities, _client: &ClientCapabilities) {
        server.document_link_provider = Some(DocumentLinkOptions {
            resolve_provider: Some(false),
            work_done_progress_options: Default::default(),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cpp_code_analysis::{
        CompilerConfig, IncludeFact, IncludeForm, MemoryFiles, OpenDocuments, Session, SessionFiles,
        WatchFilter,
    };

    use super::*;

    /// A session holding `a.cpp`, which includes one header that exists and — in the other assertion below — one that
    /// does not. The facts are built here rather than indexed, because the resolution is the analysis's answer and
    /// this test is about the translation: which span a link covers, and whether an unresolved include gets one.
    fn a_held_file() -> (Session<MemoryFiles>, PathBuf, String) {
        let text = "#include \"widget.h\"\n#include <missing.h>\n";
        let files = MemoryFiles::new()
            .with_file("/p/a.cpp", text)
            .with_file("/p/widget.h", "struct Widget {};\n");
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.load("/p/a.cpp");
        (session, PathBuf::from("/p/a.cpp"), text.to_string())
    }

    fn an_include(spelling: &str, resolved: Option<PathBuf>, at: usize) -> IncludeFact {
        IncludeFact {
            form: IncludeForm::Quote,
            spelling: spelling.to_string(),
            resolved,
            is_next: false,
            range: cpp_parser::SourceRange::new(at, "#include \"widget.h\"".len()),
            guard: cpp_code_analysis::FactGuard::Unconditional,
        }
    }

    /// **The link is the directive, and only an include that resolved gets one.**
    ///
    /// The second half is the decision worth pinning: the protocol allows a link with no target, and a server that
    /// sent one would be drawing an underline that leads nowhere. The unresolved include is a diagnostic's business,
    /// and it is not a link's.
    #[test]
    fn a_resolved_include_becomes_a_link_and_an_unresolved_one_does_not() {
        let (session, path, text) = a_held_file();
        let file = session.files().held(&path).expect("the file is held");
        let first = text.find("#include").expect("the fixture includes one");

        let link = document_link_of(
            file,
            &an_include("widget.h", Some(PathBuf::from("/p/widget.h")), first),
        )
        .expect("a resolved include is a link");
        assert_eq!(link.range.start.line, 0);
        assert_eq!(link.tooltip.as_deref(), Some("widget.h"));
        let target = link.target.as_ref().expect("the target is the resolved file");
        assert!(
            target.path().ends_with("widget.h"),
            "the link points at the header the search found: {target:?}"
        );

        let unresolved = document_link_of(file, &an_include("missing.h", None, first + 22));
        assert!(
            unresolved.is_none(),
            "an include that did not resolve leads nowhere and is not a link"
        );
    }
}