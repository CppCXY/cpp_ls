//! # `textDocument/references` — where else this name is written
//!
//! # What this answers, and what it deliberately does not
//!
//! **Macros.** [`Session::macro_references`] walks the four-level ladder the index documents — the candidate files
//! (a file can only use a macro it can see), the text filter (a file whose bytes do not contain the name has no
//! identifier of that name), the lexer (a comment and a string are one token, so a name inside them is not an
//! identifier at all) and then the macro environment, which is the one level that decides by meaning rather than by
//! shape.
//!
//! An ordinary name — a class, a variable, a function — answers `Unknown(NotDeclaredHere)` and this handler turns
//! that into an **empty list**, which is what the protocol has for "nothing here". It is not a missing feature
//! papered over: finding a *name*'s references means asking, for every candidate file, "does the name at this
//! offset resolve to that declaration?", and that needs each candidate's scopes, which is a parse per candidate
//! rather than a lex. The analysis says so in one place (see the module documentation of `index::references`) and
//! this layer passes the answer through rather than inventing one.
//!
//! # Uncertain references are **shown** and not edited
//!
//! A use reached through a conditional `#include` or an `#if` is a place where the name is a macro but not
//! certainly *this* one. A reader asking "where is this used" wants to see it — it is a real occurrence of the
//! spelling, and hiding it would make the list lie about the text — while a **rename** must not touch it, which is
//! [`MacroReferences::rename`]'s own decision rather than this handler's. The protocol has nowhere to put the
//! caveat, so the list carries the occurrences and the rename carries the caution.

use cpp_code_analysis::{Known, ReferenceKind, Session};
use lsp_types::{
    ClientCapabilities, Location, ReferenceParams, ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{path_to_uri, position_in_file, uri_to_file_path};

pub async fn on_references(
    context: ServerContextSnapshot,
    params: ReferenceParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<Location>> {
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let include_declaration = params.context.include_declaration;

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let offset = crate::util::offset_at_position(&view, position)?;

        locations(session, &view, offset, include_declaration)
    })
    .await
}

/// Every place the macro at `offset` is written, as locations.
///
/// # `None` while the index still has work, and why that is the honest answer
///
/// A reference list is a **claim about the whole project**: "these are the places this name is used". The protocol
/// gives it no way to say "and there may be more" — no `isIncomplete`, unlike a completion list — so an answer
/// assembled from a partially read index is not a smaller truth, it is a false one, and a user who renames on the
/// strength of it changes some uses and not others.
///
/// That state is ordinary rather than exotic: a file's summary is dropped the moment it is edited, so any request
/// that arrives between a keystroke and the next drain would be missing that file's own uses. `None` is the
/// protocol's "I cannot answer that", the same refusal `rename` gives for the same reason.
///
/// Generic over the file provider so that a test can build the refused state exactly — a session whose queue has
/// work in it — instead of hoping to catch a real one between two keystrokes.
pub fn locations<F: cpp_code_analysis::FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: usize,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    if session.pending() > 0 {
        log::debug!(
            "no reference list yet: {} file(s) are queued and their uses would be missing",
            session.pending()
        );
        return None;
    }

    let Known::Yes(found) = session.macro_references(view, offset) else {
        // Not a macro, or not a name at all: the protocol's answer for "nothing here" is an empty list, and the
        // difference between the two is the analysis's to explain, not this layer's to guess at.
        return Some(Vec::new());
    };

    let mut locations = Vec::new();

    for file in &found.files {
        // The line index of the file the reference is **in**, which is the only thing that can turn an offset into
        // a position: a reference in a header nobody opened is a reference in a header this session still holds
        // (every file the indexer read, it holds), and a file that is gone cannot be reported at all.
        let Some(held) = session.files().held(&file.file) else {
            continue;
        };

        for reference in &file.references {
            if !include_declaration && matches!(reference.kind, ReferenceKind::Definition) {
                continue;
            }

            let Some(uri) = path_to_uri(&file.file) else {
                continue;
            };
            let (Some(start), Some(end)) = (
                position_in_file(held, reference.range.start_offset),
                position_in_file(held, reference.range.end_offset()),
            ) else {
                continue;
            };

            locations.push(Location {
                uri,
                range: lsp_types::Range::new(start, end),
            });
        }
    }

    // **A file whose uses could not be listed is the same false answer**: the budget stopped before it, or it could
    // not be read. `None` rather than a list with a hole in it.
    if found.not_looked_at > 0 || !found.unreadable.is_empty() {
        log::warn!(
            "no reference list for `{}`: {} candidate file(s) were not looked at and {} could not be read",
            found.name,
            found.not_looked_at,
            found.unreadable.len()
        );
        return None;
    }

    Some(locations)
}

/// **The macro the cursor is on**: its name, and the range the name occupies in this file.
///
/// Shared with `rename`, which has to answer the same question before it may offer to rename anything — and it is
/// answered the **cheap** way (one `Session::macro_definition` lookup, which follows the name to whatever defines it)
/// rather than by running the reference search: a client asks whether a cursor can be renamed on every cursor move,
/// and a search over a few thousand files is not a thing to do per keystroke.
///
/// `name_at_including_directives` rather than `name_at` for the reason that function documents: half the time the
/// name a user asks about is written in a `#define`, which is tokens in a directive rather than a name node the
/// scope walker sees.
pub(super) fn macro_at<F: cpp_code_analysis::FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: usize,
) -> Option<(String, cpp_parser::SourceRange)> {
    let (name, range) =
        cpp_code_analysis::sema::resolve::name_at_including_directives(&view.root, offset)?;

    match session.macro_definition(view, offset) {
        Known::Yes(_) => Some((name, range)),
        // `No` and `Unknown` are the same answer here, and it is the cautious one: a name that is not a macro (or
        // whose macro state is not known yet) cannot be renamed as one.
        Known::No | Known::Unknown(_) => None,
    }
}

pub struct ReferencesCapabilities;

impl RegisterCapabilities for ReferencesCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.references_provider = Some(lsp_types::OneOf::Left(true));
    }
}

#[cfg(test)]
mod tests {
    use super::locations;
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };
    use std::path::PathBuf;

    const LIMITS_H: &str = "#define MAX_ITEMS 64\n";
    const ITEMS_CPP: &str = "#include \"limits.h\"\nint items[MAX_ITEMS];\n";

    fn session_with_an_unread_project() -> Session<MemoryFiles> {
        let files = MemoryFiles::new()
            .with_file("/p/limits.h", LIMITS_H)
            .with_file("/p/items.cpp", ITEMS_CPP);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );

        // A project list, and the text of one file held: the state a request lands in when it arrives between a
        // keystroke and the drain — the file is readable, and the index has not read it yet.
        session.add_project_files([PathBuf::from("/p/limits.h"), PathBuf::from("/p/items.cpp")]);
        session.load("/p/items.cpp");
        session
    }

    /// **A reference list is refused while the index has work**, deterministically rather than by timing.
    ///
    /// The request arrives with both files queued, which is the state that would make the answer a lie: the use in
    /// `items.cpp` cannot be found, because nothing has read that file. The protocol has no `isIncomplete` for this
    /// response, so the honest answer is `null` — and this test builds that state instead of hoping to catch it.
    ///
    /// The second half is the same call once the queue is empty, which is what makes the first half a statement
    /// about *timing* rather than about the query being broken.
    #[test]
    fn a_reference_list_is_refused_while_the_index_has_work() {
        let mut session = session_with_an_unread_project();
        let view = session.view("/p/items.cpp").expect("the file is held");
        let on_the_use = ITEMS_CPP.find("MAX_ITEMS").expect("the fixture uses the macro");

        assert!(session.pending() > 0, "the fixture is the refused state");
        assert_eq!(
            locations(&session, &view, on_the_use, true),
            None,
            "an answer now would be missing whatever the queued files use"
        );

        session.index_everything();
        assert_eq!(session.pending(), 0);

        // The view is re-made, because indexing is what gives the file its summary — and the query reads the index.
        let view = session.view("/p/items.cpp").expect("the file is held");
        let found = locations(&session, &view, on_the_use, true).expect("the index is complete now");
        assert_eq!(
            found.len(),
            2,
            "the `#define` and the one use: {found:?}"
        );
        assert!(
            found
                .iter()
                .any(|location| location.uri.as_str().ends_with("limits.h")),
            "including the file that defines it: {found:?}"
        );
        assert_eq!(
            locations(&session, &view, on_the_use, false)
                .expect("complete")
                .len(),
            1,
            "and `includeDeclaration: false` leaves the `#define` out"
        );
    }
}
