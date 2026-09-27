//! # `textDocument/rename` and `textDocument/prepareRename` — renaming a macro, and **refusing** everything else
//!
//! # What can be renamed, and why the answer is "macros" rather than "names"
//!
//! A macro's name is a **textual** thing: there is one table in the preprocessor, and a name in it is replaced
//! wherever it appears. So "every place this name is used" is answerable — that is what
//! [`crate::handlers::references`] does — and an edit at each of those places is exact.
//!
//! An ordinary name is not answerable that way: whether two occurrences are the same `Widget` is a question about
//! scopes, and answering it means parsing every candidate file. So this handler **refuses** an ordinary name
//! rather than renaming the occurrences it happens to see: a rename that edits half of a name's uses is a broken
//! build written into the user's files, which is the one outcome an editor must not produce. The refusal is the
//! protocol's own (`null`), which a client shows as "this cannot be renamed here" and offers no edit box for.
//!
//! # Four ways to refuse, and each one is a real state
//!
//! * the cursor is not on a macro's name (`prepareRename` says so too, so a client does not even offer the box);
//! * the new name is not a single identifier — checked by **lexing** it with the same lexer that reads the files,
//!   because "is this a name" is the lexer's question and a hand-rolled character test would be a second, worse
//!   answer to it;
//! * the search was **incomplete**: the budget stopped before some candidates, or a candidate could not be read.
//!   A partial list of a macro's uses is exactly the half-rename above, so it is refused with the same finality;
//! * a file with an edit in it has no line index — a file deleted between the index and the query. Its edits
//!   cannot be spelled as ranges, and dropping them silently would be the partial rename again.
//!
//! # What is *not* refused: an uncertain reference
//!
//! A use behind a conditional `#include` or an `#if` might be another macro's. [`MacroReferences::rename`] leaves
//! those alone **by design** — editing code the user did not ask about is worse than leaving it — and this handler
//! passes that decision through. It logs how many were left, because the protocol's `WorkspaceEdit` has nowhere to
//! put the caveat, and a rename that silently skipped two occurrences would be a claim the user cannot check.

use std::collections::HashMap;

use cpp_code_analysis::{Known, Session};
use lsp_types::{
    ClientCapabilities, OneOf, PrepareRenameResponse, RenameOptions, RenameParams, ServerCapabilities,
    TextDocumentPositionParams, TextEdit, WorkspaceEdit,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use super::references::macro_at;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{path_to_uri, position_in_file, uri_to_file_path};

/// `textDocument/prepareRename` — the name a client may put a rename box around.
///
/// The range is the macro name itself, which is what the box starts with. `None` is "nothing here can be renamed",
/// and the client is expected to hide the feature for that cursor — which is why this answer is the same cheap one
/// the rename itself starts from.
pub async fn on_prepare_rename(
    context: ServerContextSnapshot,
    params: TextDocumentPositionParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<PrepareRenameResponse> {
    let uri = params.text_document.uri;
    let position = params.position;

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let offset = crate::util::offset_at_position(&view, position)?;
        let (_, name_range) = macro_at(session, &view, offset)?;

        Some(PrepareRenameResponse::Range(lsp_types::Range::new(
            position_in_file(session.files().held(&view.path)?, name_range.start_offset)?,
            position_in_file(session.files().held(&view.path)?, name_range.end_offset())?,
        )))
    })
    .await
}

pub async fn on_rename(
    context: ServerContextSnapshot,
    params: RenameParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<WorkspaceEdit> {
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let new_name = params.new_name;

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let offset = crate::util::offset_at_position(&view, position)?;

        workspace_edit_of_renaming_a_macro(session, &view, offset, &new_name)
    })
    .await
}

/// The edits that rename the macro at `offset`, or `None` for any of the refusals the module documents.
///
/// Generic over the file provider for the same reason [`crate::handlers::references::locations`] is: a test can
/// build the refused state exactly — a session with work queued — rather than hoping to catch one.
pub fn workspace_edit_of_renaming_a_macro<F: cpp_code_analysis::FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: usize,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    // (1) A macro's name, or nothing.
    macro_at(session, view, offset)?;

    // (2) A single identifier, by the lexer's own reading of it.
    if !is_one_identifier(new_name) {
        log::debug!("refusing a rename to {new_name:?}: not a single identifier");
        return None;
    }

    // (3) **A complete search**, and the same condition `references` refuses on: a file whose summary was dropped by
    // an edit is not a candidate, so its uses would be missing from the list — and a rename that edits some of a
    // macro's uses and leaves the rest is a broken build written into the user's files.
    if session.pending() > 0 {
        log::debug!(
            "refusing to rename: {} file(s) are queued, so the search would be partial",
            session.pending()
        );
        return None;
    }

    let Known::Yes(found) = session.macro_references(view, offset) else {
        return None;
    };

    // (4) A search that stopped early is not a search.
    if found.not_looked_at > 0 || !found.unreadable.is_empty() {
        log::warn!(
            "refusing to rename `{}`: {} candidate file(s) were not looked at and {} could not be read",
            found.name,
            found.not_looked_at,
            found.unreadable.len()
        );
        return None;
    }

    // (5) Every file with an edit in it has to be spellable as ranges.
    let mut changes: HashMap<lsp_types::Uri, Vec<TextEdit>> = HashMap::new();

    for edit in found.rename(new_name) {
        let Some(held) = session.files().held(&edit.file) else {
            log::warn!(
                "refusing to rename `{}`: {} is not held, so its edits cannot be placed",
                found.name,
                edit.file.display()
            );
            return None;
        };
        let (Some(uri), Some(start), Some(end)) = (
            path_to_uri(&edit.file),
            position_in_file(held, edit.range.start_offset),
            position_in_file(held, edit.range.end_offset()),
        ) else {
            return None;
        };

        changes.entry(uri).or_default().push(TextEdit {
            range: lsp_types::Range::new(start, end),
            new_text: edit.replacement.clone(),
        });
    }

    // The occurrences a rename deliberately leaves alone are **said out loud** somewhere: the protocol has no place
    // for a note on a `WorkspaceEdit`, so it goes to the log, where a user reporting "it missed one" can find it.
    if found.uncertain() > 0 {
        log::info!(
            "renaming `{}`: {} occurrence(s) reached through a conditional include or an `#if` were left alone",
            found.name,
            found.uncertain()
        );
    }

    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}

/// Is `name` exactly one identifier, as the lexer reads it?
///
/// The same lexer the files go through, so a name this accepts is a name the analysis would have read as one token
/// — including the dialect's own rules (`$` in an identifier, where the product accepts it). Trivia is allowed
/// around it and nothing else is: `Widget ` is a name, `Widget x` is not, and `""` is not.
fn is_one_identifier(name: &str) -> bool {
    let (tokens, _) = cpp_parser::lex(name, &cpp_parser::LexerConfig::default());

    let mut significant = tokens.iter().filter(|token| {
        !cpp_parser::is_trivia(token.kind) && token.kind != cpp_parser::CppTokenKind::Eof
    });

    let first = significant.next();
    matches!(first, Some(token) if token.kind == cpp_parser::CppTokenKind::Identifier)
        && significant.next().is_none()
}

pub struct RenameCapabilities;

impl RegisterCapabilities for RenameCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.rename_provider = Some(OneOf::Right(RenameOptions {
            // The box appears only where a rename is possible: the client asks this handler first, and an answer of
            // `None` is how the feature is hidden for every other cursor.
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::{is_one_identifier, workspace_edit_of_renaming_a_macro};
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };
    use std::path::PathBuf;

    const LIMITS_H: &str = "#define MAX_ITEMS 64\n";
    const ITEMS_CPP: &str = "#include \"limits.h\"\nint items[MAX_ITEMS];\n";

    /// **"Is this a name" is the lexer's question**, so the check is a lexing rather than a character test — and the
    /// cases below are the ones a hand-rolled test gets wrong: a keyword is an identifier to a lexer that has no
    /// keyword table in the way, a digit-first word is not an identifier at all, and trivia around a name is not
    /// part of it.
    #[test]
    fn a_new_name_is_one_identifier_and_nothing_else() {
        for name in ["Widget", "_STD_BEGIN", "MAX_ENTRIES", "x", "Widget ", " Widget"] {
            assert!(is_one_identifier(name), "{name:?} is a name");
        }

        for name in [
            "",
            " ",
            "two words",
            "MAX ENTRIES",
            "1abc",
            "a+b",
            "\"quoted\"",
            "std::string",
            "a;",
        ] {
            assert!(!is_one_identifier(name), "{name:?} is not a name");
        }
    }

    /// **A rename is refused while the index has work**, which is the guard that keeps a rename from editing some
    /// of a macro's uses and leaving the rest — the one outcome an editor must not produce.
    ///
    /// The state is built rather than waited for: a project list with both files queued and nothing read. The second
    /// half is the same call once the queue is empty, so that the first half is a statement about *timing* rather
    /// than about a rename that never works.
    #[test]
    fn a_rename_is_refused_while_the_index_has_work() {
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

        session.add_project_files([PathBuf::from("/p/limits.h"), PathBuf::from("/p/items.cpp")]);
        session.load("/p/items.cpp");

        let view = session.view("/p/items.cpp").expect("the file is held");
        let on_the_use = ITEMS_CPP.find("MAX_ITEMS").expect("the fixture uses the macro");

        assert!(session.pending() > 0, "the fixture is the refused state");
        assert!(
            workspace_edit_of_renaming_a_macro(&session, &view, on_the_use, "MAX_ENTRIES").is_none(),
            "a rename now would edit the uses the index knows and skip the ones it has not read"
        );

        session.index_everything();
        let view = session.view("/p/items.cpp").expect("the file is held");
        let edit = workspace_edit_of_renaming_a_macro(&session, &view, on_the_use, "MAX_ENTRIES")
            .expect("the index is complete now");
        let changes = edit.changes.expect("a change map");
        assert_eq!(changes.len(), 2, "the definer and the user: {changes:?}");
        assert!(
            changes
                .values()
                .all(|edits| edits.iter().all(|edit| edit.new_text == "MAX_ENTRIES")),
            "every edit writes the new name: {changes:?}"
        );
    }
}
