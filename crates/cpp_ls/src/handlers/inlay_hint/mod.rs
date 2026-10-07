//! # `textDocument/inlayHint` — the parameter names an editor draws inside the code
//!
//! ```text
//! scale(3, 0.5)        what the file says
//! scale(count: 3, factor: 0.5)   what the reader sees, with two hints drawn between the tokens
//! ```
//!
//! A hint is text the editor *draws*, not text in the file, and that is what makes the bar for one high: it sits
//! in the middle of the line the user is reading, and nothing distinguishes a wrong hint from the code around it.
//! So the analysis answers with names it read off the callee's own declaration ([`Session::inlay_hints`]), and this
//! layer renders them — `count:` with a space after it — and refuses to invent anything the analysis did not say.
//!
//! # What is *not* hinted
//!
//! Types. `auto n = count();` would read better as `auto n: int = …`, and this server cannot write that `int`: the
//! analysis has no type system, nothing deduces `auto`, and a `std::vector<int>` is not an instantiated type here.
//! A type hint that was wrong would be a lie printed into the user's code, which is worse than the hint being
//! missing — see the `inlay` module for the two shapes that are answerable and the boundary between them.
//!
//! # Offsets in, positions out
//!
//! The analysis answers in **byte offsets**, like every other query, and the protocol asks in lines and columns.
//! The conversion is the VFS's line index — the same one every position in this server comes from — and it happens
//! here, at the edge, where the wire format lives.
//!
//! The requested range arrives the same way and is converted in the other direction. It is the client's *visible*
//! area rather than a promise: a range whose end is past the end of the file is ordinary (the viewport can extend
//! past the last line), so the ends are clamped rather than refused, and a start past the end simply selects no
//! calls.

use cpp_code_analysis::{FileView, ParameterHint, VfsFile};
use lsp_types::{
    ClientCapabilities, InlayHint, InlayHintKind, InlayHintLabel, InlayHintParams,
    InlayHintServerCapabilities, OneOf, ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{offset_at_position, position_in_file, uri_to_file_path};

pub async fn on_inlay_hint(
    context: ServerContextSnapshot,
    params: InlayHintParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<InlayHint>> {
    let uri = params.text_document.uri;
    let requested = params.range;

    // Read in first, under the write lock: a hint needs the file's text, and a file nobody has read has none. The
    // *callee's* file is not prepared here — the analysis reaches it through the session, which reads it on demand.
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;

        // **And this file's own reading, because the wait this replaced was doing that too.**
        //
        // The wait was `AnalysisState::settle`, and what it bought was not only "the project's queue is empty": an
        // edit **drops this file's summary** (`Session::buffer_changed`), the pump rebuilds it later, and a request
        // that arrives in between reads a file whose declarations are not in the index — so a call in it resolves to
        // nothing and the hint is missing. Inlay hints carry no "incomplete" flag, so an empty answer here stands
        // until something else moves the editor.
        //
        // `catch_up` is that, as a question about **this file**: one parse of it when an edit is waiting, and
        // nothing at all when there is not. Handlers that reach the index through a name — `hover`, `definition`,
        // `signature_help` — get it from `read_the_modules`; the ones that ask about a *position* say it here.
        context.analysis().catch_up(&path).await;
    }

    // **A hint no longer waits for the project's index to drain.**
    //
    // It did, and the reason is worth keeping: a hint's parameter names come from the callee's declaration, so an
    // answer built while the index is filling can be **empty** — and inlay hints carry no "incomplete" flag, so a
    // client has no reason to ask again. The wait was `AnalysisState::settle`, which asked whether the *whole
    // project's* queue was empty with a two-second budget.
    //
    // The question that matters is about this file, and it is answered above: `prepare` puts the file's text in
    // reach, `catch_up` puts its declarations in the index, `read_the_modules` reads what it imports. See
    // `docs/incremental-edits.md` §1 for the argument and [`AnalysisState::catch_up`] for what replaced the wait.

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        // **The file's own tokens, deliberately, and this is not the fallback it looks like.**
        //
        // The analysis does not need this file's macros to name a call's parameters: the names come from the
        // *callee's* declaration, which is in another file and unaffected by anything typed here. Measured on a
        // 31-line file with four `#include`s: the same question yields **4 hints on either reading**, so asking for
        // the rendering would buy nothing and cost a render plus a parse of the file at every keystroke.
        //
        // It also keeps the two coordinate systems apart by construction. A hint's offset comes back in *this*
        // view's coordinates and is placed with the file's line index — which is the same thing exactly when the
        // view is the file's own tokens. Asking for a rendering here would silently move every hint by however many
        // lines the directives had taken out, which is the mistake the hover handler had.
        let view = session.view_of_the_file(&path)?;
        let held = session.files().held(&view.path)?;

        // **Answered from what is known, rather than withheld until everything is.**
        //
        // This used to return `None` while any file was queued, on the reasoning that a callee in a file nobody has
        // read yet resolves to nothing. That reasoning is right about the *cause* and wrong about the *remedy*: an
        // unresolved callee is **skipped** by `parameter_hints`, so an index still filling gives fewer hints and
        // never a wrong one — the same trade [`Session::classified_names`] makes in its own words.
        //
        // Withholding instead made the feature disappear. An edit re-queues the file, and the client asks for hints
        // immediately afterwards: measured, the queue is non-empty for **~48 ms** after a keystroke, and a person
        // typing is inside that window at every stroke. So the hints vanished exactly while they were being written
        // — which is what the report said, in those words. What makes the incomplete answer repairable is the
        // refresh the pump sends when the queue drains, not a refusal here.
        let hints = session.inlay_hints(&view, visible_range(&view, requested)?);

        Some(
            hints
                .into_iter()
                .filter_map(|hint| inlay_hint_of(held, hint))
                .collect(),
        )
    })
    .await
}

/// The client's visible range, as offsets in this file.
///
/// Clamped rather than refused at the ends: a viewport can extend past the last line, and a client that asked
/// about the end of a file is asking about whatever is there. An end that cannot be placed is the end of the file,
/// and a start that cannot be placed is the end of the file too — so nothing is selected, which is the true answer
/// for a range that begins past what the file holds.
fn visible_range(
    view: &FileView,
    range: lsp_types::Range,
) -> Option<cpp_parser::SourceRange> {
    let len = view.source.len();
    let start = offset_at_position(view, range.start).unwrap_or(len);
    let end = offset_at_position(view, range.end).unwrap_or(len);

    (start <= end).then(|| cpp_parser::SourceRange::new(start, end - start))
}

/// One hint, as the protocol's position and label — or `None` when the position cannot be placed.
///
/// A function of its own rather than a closure in the handler, because the two decisions worth testing directly are
/// here: what the label says, and that a position the file cannot place produces **no** hint rather than one at a
/// guessed place. No tooltip is set: the analysis answers with a name and a position, and a tooltip saying
/// "parameter" would be a sentence with nothing in it.
pub fn inlay_hint_of(file: &VfsFile, hint: ParameterHint) -> Option<InlayHint> {
    let position = position_in_file(file, hint.offset)?;

    Some(InlayHint {
        position,
        // The colon is this layer's punctuation: the analysis says which parameter the argument lands in, and the
        // wire format decides how that is written.
        label: InlayHintLabel::String(format!("{}:", hint.name)),
        kind: Some(InlayHintKind::PARAMETER),
        text_edits: None,
        tooltip: None,
        padding_left: None,
        // The hint is drawn *in front of* the argument, so the space belongs after it: `count: 3`.
        padding_right: Some(true),
        data: None,
    })
}

/// Nothing is resolved lazily: every hint this server sends is complete, so the client is told there is no
/// `inlayHint/resolve` to call.
pub struct InlayHintCapabilities;

impl RegisterCapabilities for InlayHintCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.inlay_hint_provider = Some(OneOf::Right(
            InlayHintServerCapabilities::Options(lsp_types::InlayHintOptions {
                resolve_provider: Some(false),
                ..Default::default()
            }),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::inlay_hint_of;
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, ParameterHint, Session, SessionFiles,
        WatchFilter,
    };

    /// A file held by a session, so that its line index is the one the VFS built — the same one every position in
    /// this server comes from.
    fn a_held_file(text: &str) -> (Session<MemoryFiles>, std::path::PathBuf) {
        let files = MemoryFiles::new().with_file("/p/a.cpp", text);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.load("/p/a.cpp");
        (session, std::path::PathBuf::from("/p/a.cpp"))
    }

    /// The label is the name and a colon, the kind is `Parameter`, and the padding is on the side the argument is
    /// on; a position the file cannot place produces nothing at all.
    #[test]
    fn a_parameter_hint_becomes_a_labelled_hint_at_the_argument() {
        let text = "int x = scale(3);\n";
        let (session, path) = a_held_file(text);
        let file = session.files().held(&path).expect("the file is held");

        let hint = inlay_hint_of(
            file,
            ParameterHint {
                offset: text.find('3').expect("the fixture"),
                name: "count".to_string(),
            },
        )
        .expect("the offset is in the file");

        assert_eq!(hint.position.line, 0);
        assert_eq!(hint.position.character, 14, "the hint sits on the argument");
        match &hint.label {
            lsp_types::InlayHintLabel::String(text) => assert_eq!(text, "count:"),
            other => panic!("a plain label was expected, not label parts: {other:?}"),
        }
        assert_eq!(hint.kind, Some(lsp_types::InlayHintKind::PARAMETER));
        assert_eq!(hint.padding_right, Some(true), "the space goes after the colon");

        // A hint past the end of the file is dropped rather than placed at the end: a name drawn in the wrong
        // place is worse than no name.
        assert!(
            inlay_hint_of(
                file,
                ParameterHint {
                    offset: text.len() + 10,
                    name: "count".to_string(),
                }
            )
            .is_none()
        );
    }
}
