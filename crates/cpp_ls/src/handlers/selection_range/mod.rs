//! # `textDocument/selectionRange` — the rungs an editor walks when a selection expands
//!
//! One request, many cursors: a client that supports multi-cursor asks about several positions at once, and each
//! answer is a **chain** — the word under the cursor, then each construct containing it, out to the whole file. The
//! chain is [`FileView::selection_chain`]'s, read from the file's own tree: the analysis answers in offsets and this
//! layer turns them into positions, which is the same division as every other handler here.
//!
//! # The protocol's shape, built inside out
//!
//! A `SelectionRange` points at its **parent**, so the analysis's innermost-first chain is reversed while it is
//! built: the last rung (the file) is the outermost object and has no parent, and each step inward becomes its
//! child. Building it the other way — walking the chain outward and trying to attach children — would need a
//! mutable walk of a structure that is naturally built from the outside in.
//!
//! # What is *not* answered
//!
//! A position past the end of the file, or in a file this session cannot read: the request answers `null` rather
//! than a chain of one empty range. A client asking "what can I select here" about somewhere there is no here is
//! asking about a file that is not the one it thinks it is.

use lsp_types::{
    ClientCapabilities, SelectionRange, SelectionRangeParams, SelectionRangeProviderCapability,
    ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{file_offset_at_position, position_in_file, uri_to_file_path};

pub async fn on_selection_range(
    context: ServerContextSnapshot,
    params: SelectionRangeParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<SelectionRange>> {
    let uri = params.text_document.uri;
    let positions = params.positions;

    // Read in first, under the write lock: the chain is read from the file's own tree, and a file nobody has read
    // has none.
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        // **The file's own text, for the cursor and for the walk.** A selection is what the reader is about to see
        // highlighted, and `Session::selection_chain` walks the written text — while the position below used to be
        // converted through the **rendering**, which resolves directives and merges lines, so it named a different
        // place (the same defect `docs/lsp-status.md` P1 ③ records for `implementation`).
        let view = session.view_of_the_file(&path)?;
        let held = session.files().held(&view.path)?;

        let mut answered = Vec::with_capacity(positions.len());
        for position in positions {
            // **The file's offset, and then the query that lives in the file's coordinates.** A selection is what the
            // reader is about to see highlighted, so it is asked of the text they are looking at — `Session::
            // selection_chain` parses the written text and walks that — and no translation is needed on either side of
            // it. This handler used to map the cursor *into* the rendering and every rung *out* of it, and the two
            // mappings are what produced the defects this module's notes describe: a chain one line too high, and a
            // rung that did not contain the cursor it was built for (a cursor in whitespace maps forward to the next
            // token, so a walk of the rendering starts after the cursor).
            let offset = file_offset_at_position(&view, position)?;
            let chain = session.selection_chain(&view, offset);

            answered.push(chain_of(held, &chain)?);
        }

        Some(answered)
    })
    .await
}

/// One chain, as the protocol's nested ranges — the innermost first, each pointing at the one that contains it.
///
/// A function of its own so that the *shape* is testable without a client: the analysis promises a chain of offsets
/// that strictly grow, and this is where it becomes a structure a client walks outward.
pub fn chain_of(
    file: &cpp_code_analysis::VfsFile,
    chain: &[cpp_parser::SourceRange],
) -> Option<SelectionRange> {
    let mut outer: Option<SelectionRange> = None;

    // **From the outermost in**: the last rung has no parent, and each step inward becomes its child. Reversing here
    // rather than building outward is what keeps every range's parent the range that contains it.
    for range in chain.iter().rev() {
        let start = position_in_file(file, range.start_offset)?;
        let end = position_in_file(file, range.end_offset())?;

        outer = Some(SelectionRange {
            range: lsp_types::Range::new(start, end),
            parent: outer.map(Box::new),
        });
    }

    outer
}

pub struct SelectionRangeCapabilities;

impl RegisterCapabilities for SelectionRangeCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.selection_range_provider =
            Some(SelectionRangeProviderCapability::Simple(true));
    }
}

#[cfg(test)]
mod tests {
    use super::chain_of;
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };
    use lsp_types::SelectionRange;

    /// The rungs of an answer, from the client's point of view: the innermost range first, then each parent.
    fn rungs(range: &SelectionRange) -> Vec<(u32, u32)> {
        let mut found = vec![(range.range.start.line, range.range.end.line)];
        let mut current = range.parent.as_deref();

        while let Some(parent) = current {
            found.push((parent.range.start.line, parent.range.end.line));
            current = parent.parent.as_deref();
        }

        found
    }

    /// **The chain is nested the way the protocol walks it**: innermost first in the object, each range pointing at
    /// the one containing it, and the outermost with no parent.
    #[test]
    fn a_chain_becomes_nested_ranges() {
        let source = "int f() {\n    return 1;\n}\n";
        let files = MemoryFiles::new().with_file("/p/a.cpp", source);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.load("/p/a.cpp");
        // The file's own view, built from what the VFS holds — see the note in `handlers::references`: a rendering is
        // asked for and waits for the cook, and this fixture is a plain file with nothing to cook.
        let held = session.files().held("/p/a.cpp").expect("the file is held");
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        let file = session.files().held("/p/a.cpp").expect("held");

        let chain = view.selection_chain(cpp_code_analysis::file::coordinates::ReadingOffset::new(
            source.find('1').expect("the fixture has one"),
        ));
        assert!(chain.len() >= 3, "the fixture has several rungs: {chain:?}");

        let nested = chain_of(file, &chain).expect("every rung is a place in the file");
        // **The statement's rung is one line, and it used to be two.** `return 1;` has the node's range end at its `;`
        // now, where the trailing newline — layout, not part of the statement — used to be glued inside it. The old
        // expectation, `(1, 2)`, was the newline's line; the range a client gets is the statement a reader would select
        // by double-clicking, which is this one.
        assert_eq!(
            rungs(&nested),
            vec![(1, 1), (1, 1), (0, 3), (0, 3)],
            "the token, the statement, the function, and the file: {nested:?}"
        );

        // The parent chain really is a chain: each range contains its child.
        let mut current = Some(&nested);
        while let Some(range) = current {
            if let Some(parent) = range.parent.as_deref() {
                assert!(
                    parent.range.start <= range.range.start && range.range.end <= parent.range.end,
                    "a parent has to contain its child: {range:?} inside {parent:?}"
                );
            }
            current = range.parent.as_deref();
        }

        // An empty chain — a file with no tree at all — is no answer rather than an empty one.
        assert_eq!(chain_of(file, &[]), None);
    }
}
