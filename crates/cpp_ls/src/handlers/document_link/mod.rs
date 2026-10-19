//! # `textDocument/documentLink` — where an `#include` points
//!
//! A document link is the protocol's way of saying "this span is a reference to something outside the buffer, and the
//! editor may make it clickable". For C++ there is exactly one such span that every reader wants: the header name in
//! `#include "widget.h"`. The analysis already resolved it — [`IncludeFact::resolved`] is the answer of the same search
//! the compiler would make — so this handler is a translation from an offset to a line and from a path to a URL, and
//! nothing else.
//!
//! # The link is the **path**, and that is not what the fact records
//!
//! An [`IncludeFact::range`] is the **directive** — `#include <vector>  // the container` and its trailing comment
//! included — which is the fact's answer to *where is this `#include`*: it is what matches a cursor on the line to
//! the fact that says where the line points ([`cpp_code_analysis::Session::header_at`]). A link is the other shape:
//! a clickable region, which should be the file's name and nothing else. Sending the directive made every link in a
//! real file cover its whole line, which reads as an underline under a comment the reader wrote for themselves. So
//! the name is read back out of the **file's own text**
//! ([`cpp_code_analysis::sema::resolve::header_name_in`]), and the directive's range is only the fallback for the
//! shape that has no name in it (`#include HEADER`, whose target is a macro rather than a path).
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

use cpp_code_analysis::sema::resolve::header_name_in;

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
        // **The file's own reading, not its rendering.** A rendering has the directives resolved: the `#include`
        // line is gone from it, with the included file's content written in its place — so the one span a link is
        // about does not exist in the tree this handler can see, and the facts would have no text to be found in.
        // `view_of_the_file` parses the text the reader wrote, which is where every directive is.
        let view = session.view_of_the_file(&path)?;
        let held = session.files().held(&view.path)?;
        let summary = session.index().summary(&view.path)?;

        let headers = summary.includes.iter().filter_map(|include| {
            document_link_of(held, include, header_name_in(&view.root, include.range))
        });

        // **And the modules it imports.** `import math;` names a file exactly as `#include "math.cppm"` would, and
        // the analysis already knows which one — through the index once the interface unit has been read, and
        // through the naming convention before that. The link covers the **module name**, not the declaration.
        let modules = session
            .module_imports_of(&view)
            .into_iter()
            .filter_map(move |import| module_link_of(held, &import));

        Some(headers.chain(modules).collect())
    })
    .await
}

/// One `#include`, as the protocol's link — or `None` when it leads nowhere.
///
/// A function of its own because the two decisions it makes are worth testing without a client: **which** span the link
/// covers (the header name, not the `#include` keyword), and whether the link is worth sending at all (an unresolved
/// include is not).
///
/// `name` is the span of the path inside the directive, when the tree has one; the directive's own range is the
/// fallback, because a link over too much text is still a link to the right file while no link at all is a line the
/// reader can no longer click.
pub fn document_link_of(
    file: &cpp_code_analysis::VfsFile,
    include: &cpp_code_analysis::IncludeFact,
    name: Option<cpp_parser::SourceRange>,
) -> Option<DocumentLink> {
    let resolved = include.resolved.as_ref()?;
    let covered = name.unwrap_or(include.range);
    let start = position_in_file(file, covered.start_offset)?;
    let end = position_in_file(file, covered.end_offset())?;

    Some(DocumentLink {
        range: lsp_types::Range { start, end },
        target: path_to_uri(resolved),
        tooltip: Some(include.spelling.clone()),
        data: None,
    })
}

/// **One `import m;`, as the protocol's link** — the module's counterpart of [`document_link_of`].
///
/// A module name is a reference to a file exactly as a header name is, and the analysis has already resolved it
/// ([`cpp_code_analysis::Session::module_imports_of`]). Two decisions, the same two: the span is the **name** the
/// reader wrote, and an import that reached no file gets no link.
pub fn module_link_of(
    file: &cpp_code_analysis::VfsFile,
    import: &cpp_code_analysis::ModuleImport,
) -> Option<DocumentLink> {
    let resolved = import.resolved.as_ref()?;
    let start = position_in_file(file, import.range.start_offset)?;
    let end = position_in_file(file, import.range.end_offset())?;

    Some(DocumentLink {
        range: lsp_types::Range { start, end },
        target: path_to_uri(resolved),
        tooltip: Some(format!("import {}", import.name)),
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
        CompilerConfig, IncludeFact, IncludeForm, MemoryFiles, OpenDocuments, Session,
        SessionFiles, WatchFilter,
    };

    use super::*;

    /// A session holding `a.cpp`, whose two includes are the two shapes that matter: one that exists — with a
    /// trailing comment after it, which is what a real file writes — and one that does not. The summary is built
    /// from the file (the facts and their ranges are the analysis's own), and the view is of the **file's** text,
    /// which is where the header names are.
    fn a_held_file() -> (Session<MemoryFiles>, PathBuf, String) {
        let text = "#include \"widget.h\" // the widget\n#include <missing.h>\n";
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
        session.add_project_files([PathBuf::from("/p/a.cpp")]);
        session.load("/p/a.cpp");
        session.index_everything();
        (session, PathBuf::from("/p/a.cpp"), text.to_string())
    }

    fn an_include(
        spelling: &str,
        resolved: Option<PathBuf>,
        range: cpp_parser::SourceRange,
    ) -> IncludeFact {
        IncludeFact {
            form: IncludeForm::Quote,
            spelling: spelling.to_string(),
            resolved,
            is_next: false,
            range,
            guard: cpp_code_analysis::FactGuard::Unconditional,
        }
    }

    /// **The link covers the path, and only an include that resolved gets one.**
    ///
    /// Reported from an editor: every link covered its whole line, so the underline ran through the comment the
    /// reader had written beside the include. The range is the **header name**, which is the clickable thing; the
    /// rest of the line is the reader's prose.
    ///
    /// The second half is the other decision worth pinning: the protocol allows a link with no target, and a server
    /// that sent one would be drawing an underline that leads nowhere. The unresolved include is a diagnostic's
    /// business, and it is not a link's.
    #[test]
    fn a_resolved_include_becomes_a_link_that_covers_the_path() {
        let (session, path, text) = a_held_file();
        let view = session
            .view_of_the_file(&path)
            .expect("the file's own reading is held");
        let file = session.files().held(&path).expect("the file is held");
        let summary = session.index().summary(&path).expect("the file is indexed");

        let links: Vec<DocumentLink> = summary
            .includes
            .iter()
            .filter_map(|include| {
                document_link_of(file, include, header_name_in(&view.root, include.range))
            })
            .collect();

        // `#include "widget.h" // the widget` — the name is `widget.h`, at columns 10 to 18. The trailing comment
        // starts at 20, and the directive's range covers it: that is the bug this asserts is gone.
        assert_eq!(links.len(), 1, "only the include that resolved is a link");
        let link = &links[0];
        assert_eq!(link.range.start, lsp_types::Position::new(0, 10));
        assert_eq!(
            link.range.end,
            lsp_types::Position::new(0, 18),
            "the link is the path, not the directive and not the comment after it"
        );
        assert!(
            text[10..18].eq("widget.h"),
            "the span is the name as the file writes it"
        );
        assert_eq!(link.tooltip.as_deref(), Some("widget.h"));
        let target = link
            .target
            .as_ref()
            .expect("the target is the resolved file");
        assert!(
            target.path().ends_with("widget.h"),
            "the link points at the header the search found: {target:?}"
        );

        // **The unresolved include is not a link**, asked through the same path.
        let unresolved = summary
            .includes
            .iter()
            .find(|include| include.spelling == "missing.h")
            .expect("the fixture includes one");
        assert!(
            document_link_of(
                file,
                unresolved,
                header_name_in(&view.root, unresolved.range),
            )
            .is_none(),
            "an include that did not resolve leads nowhere and is not a link"
        );
    }

    /// **An `import` is a link to the file that declares the module.**
    ///
    /// A module name is a reference to a file exactly as a header name is, and it had none: `import math;` in an
    /// editor was plain text while `#include "math.cppm"` beside it was clickable. The span is the **module name**,
    /// for the same reason the include's is the path — a link is a clickable region, and the rest of the line is the
    /// reader's syntax.
    #[test]
    fn an_import_becomes_a_link_that_covers_the_module_name() {
        let text = "import math;\nint x = 0;\n";
        let files = MemoryFiles::new().with_file("/p/main.cpp", text).with_file(
            "/p/math.cppm",
            "export module math;\nexport int add(int, int);\n",
        );
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files([PathBuf::from("/p/main.cpp")]);
        session.index_everything();

        let view = session
            .view_of_the_file("/p/main.cpp")
            .expect("the file's own reading is held");
        let file = session
            .files()
            .held("/p/main.cpp")
            .expect("the file is held");

        let imports = session.module_imports_of(&view);
        assert_eq!(imports.len(), 1, "the file imports one module: {imports:?}");
        assert_eq!(imports[0].name, "math");

        let link = module_link_of(file, &imports[0]).expect("a resolved import is a link");
        assert_eq!(link.range.start, lsp_types::Position::new(0, 7));
        assert_eq!(
            link.range.end,
            lsp_types::Position::new(0, 11),
            "the link is `math`, not the declaration"
        );
        assert_eq!(link.tooltip.as_deref(), Some("import math"));
        let target = link.target.as_ref().expect("the target is the unit");
        assert!(
            target.path().ends_with("math.cppm"),
            "and it points at the interface unit: {target:?}"
        );
    }

    /// **A directive with no header name in it still gets a link over the directive.**
    ///
    /// `#include HEADER` is a macro include — the target is a name, and the tree has no `HeaderName` token for it —
    /// so the fallback is the directive's own range. A link over too much text is a line the reader can still
    /// click; no link at all is a line they cannot, which is the worse of the two.
    #[test]
    fn a_directive_with_no_name_in_it_falls_back_to_its_own_range() {
        let (session, path, _) = a_held_file();
        let file = session.files().held(&path).expect("the file is held");
        let directive = cpp_parser::SourceRange::new(0, "#include \"widget.h\"".len());

        let link = document_link_of(
            file,
            &an_include("widget.h", Some(PathBuf::from("/p/widget.h")), directive),
            None,
        )
        .expect("a resolved include is a link");

        assert_eq!(link.range.start, lsp_types::Position::new(0, 0));
        assert_eq!(
            link.range.end,
            lsp_types::Position::new(0, 19),
            "the fallback is the directive the fact recorded"
        );
    }
}
