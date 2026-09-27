//! # `textDocument/foldingRange` — the regions of a file a client may hide
//!
//! The simplest handler here, and the only one that reads **nothing but the file**: no index, no session state, no
//! other file. A fold is a fact about this text — where a brace pair starts and ends, where a comment block runs,
//! where an `#if` closes — so [`Session::folding_ranges`] answers from the buffer's own tokens, and an unsaved edit
//! is folded exactly as the user typed it.
//!
//! # Lines are this layer's business
//!
//! The analysis answers in **offsets** ([`cpp_code_analysis::folding::Fold::range`]), like every other query in this
//! crate, and the protocol asks in **lines**. The conversion is one pass through the VFS's line index — the same
//! index every position in this server comes from — and it is also where the one thing folding must never do is
//! caught: a range whose ends land on the same line hides nothing, and a client that receives one may draw a fold
//! marker against a single line. The analysis already refuses to produce those (it asks whether the region's text
//! contains a newline), and this layer checks again because it is the layer that knows what a line *is*.
//!
//! # The three kinds the protocol has
//!
//! `Code` is the absence of a kind: the protocol says a client treats a fold with no kind as code, and it is what
//! the brace pairs are. `Comment`, `Imports` and `Region` are named, and they are the three a C++ reader uses most:
//! hide the licence header, hide the includes, hide the branch that is not their platform's.

use cpp_code_analysis::folding::FoldKind;
use lsp_types::{
    ClientCapabilities, FoldingRange, FoldingRangeKind, FoldingRangeParams, FoldingRangeProviderCapability,
    ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{position_in_file, uri_to_file_path};

pub async fn on_folding_range(
    context: ServerContextSnapshot,
    params: FoldingRangeParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<FoldingRange>> {
    let uri = params.text_document.uri;

    // Read in first, under the write lock: folding needs the file's text, and a file nobody has read has none.
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let held = session.files().held(&view.path)?;

        Some(
            session
                .folding_ranges(&view)
                .into_iter()
                .filter_map(|fold| folding_range_of(held, fold))
                .collect(),
        )
    })
    .await
}

/// One fold, as the protocol's line range — or `None` when it hides nothing.
///
/// A function of its own, rather than a closure inside the handler, because the two things it decides are the two
/// worth testing directly: which **line** each end is on, and whether the range is worth sending at all.
pub fn folding_range_of(
    file: &cpp_code_analysis::VfsFile,
    fold: cpp_code_analysis::folding::Fold,
) -> Option<FoldingRange> {
    let start = position_in_file(file, fold.range.start_offset)?;
    // **The end is the last character of the region, not the first of the next line**: a client folds the lines
    // from `start_line` to `end_line` inclusive, so an end taken one past the region would hide the line after it.
    let end = position_in_file(file, fold.range.end_offset().saturating_sub(1))?;

    // One line hides nothing, and the protocol's own note says a client may treat such a range as no fold at all.
    // The analysis does not produce one; this is the layer that knows what a line is, so it checks.
    if end.line <= start.line {
        return None;
    }

    Some(FoldingRange {
        start_line: start.line,
        start_character: None,
        end_line: end.line,
        end_character: None,
        kind: kind_of(fold.kind),
        collapsed_text: None,
    })
}

/// The analysis's kind for the protocol's.
///
/// `Code` is `None` rather than a made-up kind: the protocol defines a fold with no kind as code, and inventing a
/// value for the commonest case would be a claim about what a client should draw.
fn kind_of(kind: FoldKind) -> Option<FoldingRangeKind> {
    match kind {
        FoldKind::Code => None,
        FoldKind::Comment => Some(FoldingRangeKind::Comment),
        FoldKind::Region => Some(FoldingRangeKind::Region),
        FoldKind::Imports => Some(FoldingRangeKind::Imports),
    }
}

pub struct FoldingRangeCapabilities;

impl RegisterCapabilities for FoldingRangeCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.folding_range_provider =
            Some(FoldingRangeProviderCapability::Simple(true));
    }
}

#[cfg(test)]
mod tests {
    use super::folding_range_of;
    use cpp_code_analysis::folding::{Fold, FoldKind};
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };
    use std::path::PathBuf;

    /// A file held by a session, so that its line index is the one the VFS built — the same one every position in
    /// this server comes from.
    fn a_held_file(text: &str) -> (Session<MemoryFiles>, PathBuf) {
        let files = MemoryFiles::new().with_file("/p/a.cpp", text);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.load("/p/a.cpp");
        (session, PathBuf::from("/p/a.cpp"))
    }

    /// **The end line is the last line of the region**, not the line after it, and a range that hides nothing is
    /// not sent.
    ///
    /// Both are the conversion's own decisions rather than the analysis's, which is why they are tested here: the
    /// analysis promises offsets whose text contains a newline, and only this layer knows what a line is.
    #[test]
    fn a_fold_becomes_the_lines_it_hides() {
        let text = "one\ntwo\nthree\nfour\n";
        let (session, path) = a_held_file(text);
        let file = session.files().held(&path).expect("the file is held");

        // Lines 1..3 — `two` through `four`: the end offset is just past `four`, and the last character of the
        // region is on line 3.
        let fold = Fold {
            range: cpp_parser::SourceRange::new(4, text.len() - 5),
            kind: FoldKind::Code,
        };
        let converted = folding_range_of(file, fold).expect("three lines are worth folding");
        assert_eq!(converted.start_line, 1);
        assert_eq!(converted.end_line, 3, "not 4, which would hide nothing of `four`");

        // One line is not a fold: the protocol says a client may ignore such a range, and a server that sent one
        // would be asking it to decide.
        let one_line = Fold {
            range: cpp_parser::SourceRange::new(0, 3),
            kind: FoldKind::Code,
        };
        assert_eq!(folding_range_of(file, one_line), None);

        // And an empty range — a fold whose end is at its start — is not one either.
        let empty = Fold {
            range: cpp_parser::SourceRange::new(4, 0),
            kind: FoldKind::Code,
        };
        assert_eq!(folding_range_of(file, empty), None);
    }
}
