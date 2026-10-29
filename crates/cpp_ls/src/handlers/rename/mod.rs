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
    ClientCapabilities, OneOf, PrepareRenameResponse, RenameOptions, RenameParams,
    ServerCapabilities, TextDocumentPositionParams, TextEdit, WorkspaceEdit,
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

        // **And then the query, which no longer waits for the project's index to drain.**
        //
        // The wait was `AnalysisState::settle`, asking whether the *whole project's* indexing queue
        // was empty — because `prepare` reads *this* file while the name being renamed is usually
        // declared in a header whose facts arrive later. The question that matters is this file's
        // own closure, and `AnalysisState::catch_up` is what reads it.
        context.analysis().catch_up(&path).await;
    }

    // **A refused rename says why.** The gate below is right — a partial search would write half a rename into the
    // user's files — but the protocol's `null` is what an editor shows as "this cannot be renamed here", and during a
    // project's first minute that is the wrong sentence. The message is sent before the decision is taken, because the
    // decision cannot carry it: `RequestOutcome` has `Ready`, `Cancelled` and `Missing`, and no error.
    let still_reading = context
        .analysis()
        .with_snapshot(|session| session.pending())
        .unwrap_or(0);
    if still_reading > 0 {
        context.client().show_message(lsp_types::ShowMessageParams {
            typ: lsp_types::MessageType::INFO,
            message: format!(
                "cpp_ls is still reading the project ({still_reading} file(s) left), so a rename would miss uses. \
                 The editor offers it again when the index has caught up."
            ),
        });
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view_of_the_file(&path)?;
        let offset = crate::util::offset_at_position(&view, position)?;
        // **A macro first, then a symbol.** The two paths ask the same question of different layers: a macro lives in
        // the preprocessor's table, a symbol in the index. A cursor is on one or the other, never both, so the order
        // does not change an answer; it only decides which layer is asked first.
        let (name, range) = match macro_at(session, &view, offset) {
            Some(found) => found,
            None => symbol_at(session, &view, offset)?,
        };
        let held = session.files().held(&view.path)?;

        Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: lsp_types::Range::new(
                position_in_file(held, range.start_offset)?,
                position_in_file(held, range.end_offset())?,
            ),
            placeholder: name,
        })
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
        let view = session.view_of_the_file(&path)?;
        let offset = crate::util::offset_at_position(&view, position)?;

        workspace_edit_of_renaming_a_macro(session, &view, offset, &new_name)
            .or_else(|| workspace_edit_of_renaming_a_symbol(session, &view, offset, &new_name))
    })
    .await
}

/// **The symbol at the cursor** — its spelling and the range it occupies in *this* file, or `None` for a cursor that
/// is not on a name the index can resolve.
///
/// The range comes from the file's own text (`name_at_including_directives`), because that is the text the client's
/// box is drawn over; `definitions` is asked only *whether* the name is a symbol, and its answer may well be a
/// declaration in a header — which is not where the cursor is.
pub fn symbol_at<F: cpp_code_analysis::FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
) -> Option<(String, cpp_parser::SourceRange)> {
    let Known::Yes(_) = session.definitions(view, offset) else {
        return None;
    };
    let (name, range) =
        cpp_code_analysis::sema::resolve::name_at_including_directives(&view.root, offset.get())?;

    // A name that is written in a `#define` is a macro's, and the macro path answers it first; refusing it here keeps
    // the two paths from disagreeing about what the cursor is.
    Some((name, range))
}

/// The edits that rename the **symbol** at `offset` — the same list Find References answers, written as an edit.
///
/// The rule is [`crate::handlers::references::name_level_offsets`] and nothing else: a member of one class, its
/// declaration in the header and its uses, and not another class's member of the same spelling. The refusals are the
/// macro path's own, for the same reason and with the same finality:
///
/// ```text
/// not one identifier     a rename cannot write `Widget x` into a declaration
/// index has work         the search would be partial, and half a rename is a broken build
/// outside the project    a header the toolchain owns is not the user's to edit
/// ```
pub fn workspace_edit_of_renaming_a_symbol<F: cpp_code_analysis::FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    // (1) A symbol, and a name to write.
    let (name, name_range) = symbol_at(session, view, offset)?;
    if !is_one_identifier(new_name) {
        log::debug!("refusing a rename to {new_name:?}: not a single identifier");
        return None;
    }

    // (2) A complete search — the same gate the macro path states, and for the same reason.
    if session.pending() > 0 {
        log::debug!(
            "refusing to rename `{name}`: {} file(s) are queued, so the search would be partial",
            session.pending()
        );
        return None;
    }

    // **A local is only ever renamed inside its own file.** A local name is not visible through an include, and its
    // spelling appears in every file — so the cross-file search below would edit `i` in files that have nothing to do
    // with this one. The per-file answer already exists (`local_references_of`, which the highlight handler uses), and
    // this is the same distinction `document_highlight` makes between a local and a symbol.
    // **A local is renamed inside this file.** A local name is not visible through an include and its spelling appears
    // everywhere, so the cross-file search below would edit `i` in files that have nothing to do with this one. The
    // per-file answer is `local_references_of`, and an unused local has no entry there — the function records a *use*
    // against its declaration — which is not a reason to refuse: renaming the declaration alone is the whole edit.
    let defined = match session.definitions(view, offset) {
        Known::Yes(defined) => defined,
        _ => return None,
    };
    if defined.found.iter().any(|definition| definition.fact.local) {
        let locals =
            cpp_code_analysis::sema::declarations::local_references_of(&view.scopes, &view.root);
        // **The cursor's own name starts where the entry says**, not where the client's column does: a cursor lands
        // *inside* an identifier (`more` at column 9 is its second letter), while `local_references_of` records the
        // first byte of the name. Comparing the two raw offsets answers "no such local" for every cursor that is not
        // exactly on the first letter.
        let (_, at_the_cursor) = cpp_code_analysis::sema::resolve::name_at_including_directives(
            &view.root,
            offset.get(),
        )?;

        let uses: Vec<u32> = locals
            .iter()
            .find(|entry| {
                entry.declared_at as usize == at_the_cursor.start_offset
                    || entry
                        .uses
                        .iter()
                        .any(|at| *at as usize == at_the_cursor.start_offset)
            })
            .map(|entry| entry.uses.clone())
            .unwrap_or_default();
        // **The declaration first, then the uses** — an unused local is one edit, which is the whole name.
        let mut ranges: Vec<u32> = vec![at_the_cursor.start_offset as u32];
        ranges.extend(uses);
        let mut changes: HashMap<lsp_types::Uri, Vec<TextEdit>> = HashMap::new();
        let held = session.files().held(&view.path)?;
        let uri = path_to_uri(&view.path)?;
        for at in ranges {
            // The name's own extent at that offset — a local's use is an identifier, and the declaration's range is
            // the identifier in its declarator.
            let Some((_, range)) = cpp_code_analysis::sema::resolve::name_at_including_directives(
                &view.root,
                at as usize,
            ) else {
                continue;
            };
            let (Some(start), Some(end)) = (
                position_in_file(held, range.start_offset),
                position_in_file(held, range.end_offset()),
            ) else {
                return None;
            };
            changes.entry(uri.clone()).or_default().push(TextEdit {
                range: lsp_types::Range::new(start, end),
                new_text: new_name.to_string(),
            });
        }
        return Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        });
    }

    let found = crate::handlers::references::name_level_offsets(session, view, offset, true);
    if found.is_empty() {
        log::debug!("refusing to rename `{name}`: no declaration was found for it");
        return None;
    }

    // (3) **Never outside the project**, exactly as the macro path decides it.
    if let Some(outside) = found
        .iter()
        .map(|(file, _)| file)
        .find(|file| !crate::util::is_inside(file, session.root()))
    {
        log::warn!(
            "refusing to rename `{name}`: {} is outside the project",
            outside.display()
        );
        return None;
    }

    // (4) Every file with an edit in it has to be spellable as ranges.
    let mut changes: HashMap<lsp_types::Uri, Vec<TextEdit>> = HashMap::new();
    for (file, range) in found {
        let Some(held) = session.files().held(&file) else {
            log::warn!(
                "refusing to rename `{name}`: {} is not held, so its edits cannot be placed",
                file.display()
            );
            return None;
        };
        let (Some(uri), Some(start), Some(end)) = (
            path_to_uri(&file),
            position_in_file(held, range.start_offset),
            position_in_file(held, range.end_offset()),
        ) else {
            return None;
        };
        changes.entry(uri).or_default().push(TextEdit {
            range: lsp_types::Range::new(start, end),
            new_text: new_name.to_string(),
        });
    }

    // The cursor's own name is in the list by construction (the declaration or the use it is on); if the search
    // somehow answered none of them, the edit would be a rename of nothing and is refused above by `found.is_empty`.
    let _ = name_range;

    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}
/// The edits that rename the macro at `offset`, or `None` for any of the refusals the module documents.
///
/// Generic over the file provider for the same reason [`crate::handlers::references::locations`] is: a test can
/// build the refused state exactly — a session with work queued — rather than hoping to catch one.
pub fn workspace_edit_of_renaming_a_macro<F: cpp_code_analysis::FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
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

    // (5) **Never outside the project.** A rename edits the user's files; a build system's headers are not the user's
    // files. Renaming `_STD_BEGIN` or a macro the toolchain owns would rewrite a header this server must not touch, and
    // the answer is `null` rather than a partial edit — a rename that changes some of a macro's uses and not the rest
    // is a broken build written into someone else's tree.
    //
    // The root is the session's own, so this is the same boundary the file scan uses: anything the project's
    // `compile_commands.json` reaches *through* an include is readable, and only what lies under the root is writable.
    if let Some(outside) = found
        .files
        .iter()
        .map(|file| &file.file)
        .find(|file| !crate::util::is_inside(file, session.root()))
    {
        log::warn!(
            "refusing to rename `{}`: {} is outside the project",
            found.name,
            outside.display()
        );
        return None;
    }

    // (6) Every file with an edit in it has to be spellable as ranges.
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
        for name in [
            "Widget",
            "_STD_BEGIN",
            "MAX_ENTRIES",
            "x",
            "Widget ",
            " Widget",
        ] {
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

        // **A view of the file the session holds, not of its rendering.** This test is about the state where the
        // index still has work to do, so it deliberately does not pump — and `Session::view` is a *request* for the
        // rendering that answers `None` until that work is done ("a caller that needs meaning waits"). Asking it here
        // was the test contradicting itself: the fixture is "unread on purpose" and the view needs a read.
        // `FileView::parse_with` is the constructor for exactly this: the file's own text, parsed, no cooking.
        let held = session
            .files()
            .held("/p/items.cpp")
            .expect("the file is held");
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        let on_the_use = ITEMS_CPP
            .find("MAX_ITEMS")
            .expect("the fixture uses the macro");

        assert!(session.pending() > 0, "the fixture is the refused state");
        assert!(
            workspace_edit_of_renaming_a_macro(
                &session,
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(on_the_use),
                "MAX_ENTRIES"
            )
            .is_none(),
            "a rename now would edit the uses the index knows and skip the ones it has not read"
        );

        session.index_everything();
        // **A view of the file the session holds, not of its rendering.** This test is about the state where the
        // index still has work to do, so it deliberately does not pump — and `Session::view` is a *request* for the
        // rendering that answers `None` until that work is done ("a caller that needs meaning waits"). Asking it here
        // was the test contradicting itself: the fixture is "unread on purpose" and the view needs a read.
        // `FileView::parse_with` is the constructor for exactly this: the file's own text, parsed, no cooking.
        let held = session
            .files()
            .held("/p/items.cpp")
            .expect("the file is held");
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        let edit = workspace_edit_of_renaming_a_macro(
            &session,
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(on_the_use),
            "MAX_ENTRIES",
        )
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

#[cfg(test)]
mod inside {
    use std::path::Path;

    use crate::util::is_inside;

    /// **What "inside the project" means, stated once and tested on the awkward cases.**
    ///
    /// The two sides of the comparison come from different places — the index's file list and the session's own root —
    /// so they spell separators differently and, on Windows, may differ in case. A raw `starts_with` answers "outside"
    /// for a header the user just wrote, which would refuse every rename in a project whose root was typed with a
    /// different case than the build system used. That is the bug this test exists for.
    #[test]
    fn the_project_boundary_is_compared_the_way_paths_are() {
        let root = Path::new("/p");
        assert!(is_inside(Path::new("/p/a.cpp"), root));
        assert!(is_inside(Path::new("/p/src/deep/a.cpp"), root));

        // A sibling that shares the prefix is **outside**: `/project2` is not `/project`.
        assert!(!is_inside(Path::new("/project2/a.cpp"), root));
        assert!(!is_inside(Path::new("/a.cpp"), root));
        assert!(
            !is_inside(Path::new("/p"), Path::new("/p/src")),
            "the root is not inside a child"
        );

        // Separators, both ways.
        assert!(is_inside(Path::new(r"\p\a.cpp"), root));
        assert!(is_inside(Path::new("/p/a.cpp"), Path::new(r"\p")));

        // And on Windows the comparison is case-insensitive, because the filesystem is.
        if cfg!(windows) {
            assert!(is_inside(Path::new("/P/A.CPP"), root));
        }
    }
}
#[cfg(test)]
mod a_symbol {
    use std::path::PathBuf;

    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    use super::{symbol_at, workspace_edit_of_renaming_a_symbol};

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

    fn session() -> (Session<MemoryFiles>, PathBuf) {
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
        (session, PathBuf::from("/p/use.cpp"))
    }

    /// **A member rename edits its own class and nothing else** — the promise the refusal in the module's
    /// documentation makes, now kept by a rule instead of by refusing: the cursor's class decides which member is
    /// meant, exactly as Find References does.
    ///
    /// Reported from a reader's session: Find References on `_diagnosticStyle` answered three classes' members, so a
    /// rename built on it would have rewritten two other classes. The same list is what this test checks, in the one
    /// place where a wrong answer edits the user's files.
    #[test]
    fn is_renamed_in_its_own_class_and_the_declaration_follows() {
        let (session, path) = session();
        let held = session.files().held(&path).expect("the file is held");
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        let at = USE_CPP
            .find("_value = 1")
            .expect("the fixture writes the member in First::update");

        let (name, range) = symbol_at(
            &session,
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
        )
        .expect("a member is a symbol");
        assert_eq!(name, "_value");
        assert_eq!(&USE_CPP[range.start_offset..range.end_offset()], "_value");

        let edit = workspace_edit_of_renaming_a_symbol(
            &session,
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
            "_renamed",
        )
        .expect("an indexed project can rename a member");
        let changes = edit.changes.expect("a change map");
        let total: usize = changes.values().map(|edits| edits.len()).sum();
        println!(
            "edits: {:?}",
            changes
                .iter()
                .map(|(uri, edits)| (uri.path().to_string(), edits.len()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            changes.len(),
            2,
            "the header's declaration and the source's use: {changes:?}"
        );
        assert_eq!(
            total, 2,
            "and not `Second`'s declaration or its use: {changes:?}"
        );
        assert!(
            changes
                .values()
                .flatten()
                .all(|edit| edit.new_text == "_renamed"),
            "every edit writes the new name"
        );
    }
}
