//! # `textDocument/documentSymbol` — the file's own structure
//!
//! The one handler whose query is deliberately **not** the cooked reading. Every other feature asks what a compiler
//! makes of the file; an outline asks what the file **is**, and the two answers differ in both directions:
//!
//! ```text
//! #if 0                                in the outline (the reader is editing it), never compiled
//! struct Never { int x; };
//! #endif
//! DECLARE_HANDLE(HWND);                not in the outline: the file writes a call, not a declaration
//! ```
//!
//! So the tree comes from the file's own declarations ([`cpp_code_analysis::FileSummary::outline`]) and the nesting
//! from their scopes — a class's members under the class, a namespace's contents under the namespace.
//!
//! # Two ranges per symbol, and the client needs both
//!
//! `range` is the **whole declaration** and `selection_range` is the **name**: a client folds or highlights with
//! the first and puts the cursor, the breadcrumb and the rename box on the second. The protocol requires the second
//! to be inside the first, which is a property of the facts rather than something this layer arranges — a
//! declaration's name is written inside it — but a fact read out of a *recovered* parse can have a range that does
//! not hold, so the pair is checked here and the name's range is used for both when it does not. A client that
//! rejected an outline because one entry's ranges disagreed would show nothing at all.
//!
//! # Freshness, which for this feature is the whole game
//!
//! An outline is refreshed as the user types, and a file's summary is dropped the moment it is edited. That is why
//! [`cpp_code_analysis::Session::outline`] falls back to the buffer's own parse: the alternative is a panel that
//! blinks empty between a keystroke and the next drain.

use cpp_code_analysis::{FileView, OutlineSymbol};
use lsp_types::{
    ClientCapabilities, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, OneOf, Range,
    ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{position_in, symbol_kind, uri_to_file_path};

pub async fn on_document_symbol(
    context: ServerContextSnapshot,
    params: DocumentSymbolParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<DocumentSymbolResponse> {
    let uri = params.text_document.uri;

    // Read in first, under the write lock: an outline of a file nobody has read would be an empty outline, which is
    // a wrong answer rather than a missing one.
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let outline = session.outline(&view);

        Some(DocumentSymbolResponse::Nested(symbols(&view, &outline)))
    })
    .await
}

/// The outline, as the protocol's nested symbols.
///
/// A plain function rather than a closure in the handler, so that a test can build one from a view without a
/// client, a URI and a `params` value. The session does not appear because it is not needed: the facts have already
/// been read into the outline ([`Session::outline`]), and everything below is a conversion.
pub fn symbols(view: &FileView, outline: &[OutlineSymbol]) -> Vec<DocumentSymbol> {
    outline
        .iter()
        .map(|symbol| document_symbol(view, symbol))
        .collect()
}

fn document_symbol(view: &FileView, symbol: &OutlineSymbol) -> DocumentSymbol {
    let fact = &symbol.fact;
    let whole = range_in_file(view, fact.range);
    let name = range_in_file(view, fact.name_range);

    // **The protocol's one invariant about this pair**: `selection_range` is inside `range`. Asking for the name's
    // range twice when it is not keeps the entry (its kind and its nesting are still right) instead of handing a
    // client a symbol it may reject.
    let (range, selection_range) = match (whole, name) {
        (Some(whole), Some(name)) if name.start >= whole.start && name.end <= whole.end => {
            (whole, name)
        }
        (_, Some(name)) => (name, name),
        (Some(whole), None) => (whole, whole),
        (None, None) => (Range::default(), Range::default()),
    };

    // The schema this crate builds against still **requires** `deprecated`, and `tags` is the modern spelling of the
    // same idea — so the field is written once, empty, and the deprecation is allowed here rather than left as a
    // warning: leaving it out does not compile, and a server that has nothing to deprecate sets neither.
    #[allow(deprecated)]
    DocumentSymbol {
        name: fact.name.clone(),
        // What the label does not say: the scope a member was written in (`ns::Widget`).
        detail: fact.scope.clone(),
        kind: symbol_kind(fact.kind),
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: Some(
            symbol
                .children
                .iter()
                .map(|child| document_symbol(view, child))
                .collect(),
        ),
    }
}

/// A source range as a place in the file, through the line index the view already holds.
fn range_in_file(view: &FileView, range: cpp_parser::SourceRange) -> Option<Range> {
    Some(Range::new(
        position_in(&view.source, &view.line_index, range.start_offset)?,
        position_in(&view.source, &view.line_index, range.end_offset())?,
    ))
}

pub struct DocumentSymbolCapabilities;

impl RegisterCapabilities for DocumentSymbolCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.document_symbol_provider = Some(OneOf::Left(true));
    }
}
