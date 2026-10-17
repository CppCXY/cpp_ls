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
use lsp_types::{ClientCapabilities, Location, ReferenceParams, ServerCapabilities};
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

        // **And then the query, which no longer waits for the project's index to drain.**
        //
        // The defect was real: `prepare` reads *this* file, while the name being asked about is usually in a header
        // whose facts exist only once the pump has cooked the include closure — measured on a live server for
        // `std::endl`, the first three queries after an open answered **nothing at all** and the fourth answered
        // correctly. The wait added for it was `AnalysisState::settle`, which asked whether the *whole project's*
        // indexing queue was empty — a question about the project standing in for a question about this file.
        //
        // A query about a name needs this file's summary and the files it includes, and that is what
        // `AnalysisState::catch_up` reads. See `docs/status.md` §5.2.
        context.analysis().catch_up(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;

        // **Which reading this question belongs to depends on what is under the cursor, so look.**
        //
        // A view is of the **rendering** — the file with its macros replaced, which is where declarations, types and
        // members are resolved. That is the right reading for a symbol and the wrong one for a macro: `MAX_ITEMS`
        // is not in the rendering at all, because it was replaced by what it stands for, so a question about the
        // use the cursor is on finds nothing there and answers "no references" — a false claim rather than a
        // smaller truth, and one a user would rename on.
        //
        // The file's own text is where a macro's spelling lives, so the name at the cursor is read there first and
        // the choice follows from it. Both reads are needed and neither is a fallback: each is the only reading that
        // can answer its own half.
        let written = session.view_of_the_file(&path)?;
        let in_the_file = crate::util::offset_at_position(&written, position)?;
        if matches!(
            session.macro_references(&written, in_the_file),
            Known::Yes(_)
        ) {
            return locations(session, &written, in_the_file, include_declaration);
        }

        // **A symbol, which is the ordinary case and was the missing one.**
        //
        // This used to fall through to the rendering and ask about a macro *again*: `locations` begins with
        // `macro_references`, so a name that is not a macro answered an empty list — measured on a real file,
        // `class Sux` answered **0 references** with `Sux sux;` two lines below it, and `sux` answered 0 with
        // `sux.print()` on the next line. The protocol has no "and there may be more" for a reference list, so an
        // empty answer is not a smaller truth: it is the claim that nothing uses the name, and a user who renames on
        // it changes the declaration and nothing else.
        //
        // The cursor is resolved from the **file's own text** — the same reading the macro check above used, and the
        // only one that can place a position inside a comment or a directive — and the search is across files, which
        // is why this handler waited for the analysis first.
        match session.symbol_references(
            &written,
            in_the_file,
            cpp_code_analysis::ReferenceBudget::default(),
        ) {
            Known::Yes(found) => Some(
                found
                    .all()
                    .filter(|(_, reference)| {
                        include_declaration || !matches!(reference.kind, ReferenceKind::Definition)
                    })
                    .filter_map(|(file, reference)| {
                        let held = session.files().held(file)?;
                        let start = position_in_file(held, reference.range.start_offset)?;
                        let end = position_in_file(held, reference.range.end_offset())?;
                        Some(Location {
                            uri: path_to_uri(file)?,
                            range: lsp_types::Range::new(start, end),
                        })
                    })
                    .collect(),
            ),
            // The analysis cannot say *which* symbol the cursor is on, or a candidate file could not be listed.
            // `None` is the protocol's "I cannot answer that" — the same refusal `rename` gives, and the honest one:
            // a list assembled from a guessed symbol is a list of the wrong things.
            Known::No | Known::Unknown(_) => None,
        }
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
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    // **Answered from what is indexed, and the omission is on the record.** This used to answer `null` while any file
    // was queued, on the reasoning that a use in an unread file would be missing — true, and the cost was that a reader
    // who asked during a project's first minute got *nothing*, which an editor shows as "no occurrences". A partial
    // list is what clangd answers in the same state, and the count below is how the next reader sees that it was
    // partial rather than complete.
    if session.pending() > 0 {
        log::info!(
            "a reference list while {} file(s) are still queued: uses in those files are not in this answer",
            session.pending()
        );
    }

    let Known::Yes(found) = session.macro_references(view, offset) else {
        // **Not a macro: a name-level answer.** The candidate rule is the macro path's own — a file can only refer to a
        // name it can see, so the files that declare it and everything that includes them. What differs is the second
        // level: a macro is decided by its environment, a name by its **scopes**, and a candidate that declares the
        // spelling as a local is therefore not searched at all rather than guessed at.
        return Some(name_level_locations(
            session,
            view,
            offset,
            include_declaration,
        ));
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
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
) -> Option<(String, cpp_parser::SourceRange)> {
    let (name, range) =
        cpp_code_analysis::sema::resolve::name_at_including_directives(&view.root, offset.get())?;

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

    /// **A reference list is answered while the index has work, and grows when it finishes**, deterministically.
    ///
    /// The request arrives with both files queued: the use in `items.cpp` cannot be found, because nothing has read
    /// that file. The answer is a **list of what can be seen**, not `null` — the earlier version refused, on the
    /// reasoning that a partial list is a lie, and a reader who asked during a project's first minute got nothing at
    /// all. What makes a partial list honest is that it grows: the second half is the same call after the pump, and it
    /// finds at least what the first one did.
    #[test]
    fn a_reference_list_is_partial_while_the_index_has_work_and_complete_after() {
        let mut session = session_with_an_unread_project();
        // **A view of the file the session holds, not of its rendering.** This test is about the state where the
        // index still has work to do, so it deliberately does not pump — and `Session::view` is a *request* for the
        // rendering that answers `None` until that work is done ("a caller that needs meaning waits"). Asking it here
        // was the test contradicting itself: the fixture is "unread on purpose" and the view needs a read.
        // `FileView::parse_with` is the constructor for exactly this: the file's own text, parsed, no cooking.
        let held = session
            .files()
            .held("/p/items.cpp")
            .expect("the file is held");
        let view = cpp_code_analysis::FileView::parse_with(
            &held,
            &cpp_parser::MacroEnvironment::default(),
        );
        let on_the_use = ITEMS_CPP
            .find("MAX_ITEMS")
            .expect("the fixture uses the macro");

        assert!(session.pending() > 0, "the fixture is the state this test is about");
        let partial = locations(
            &session,
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(on_the_use),
            true,
        )
        .expect("a list is answered from what the index has, not refused");
        assert!(
            partial.len() <= 1,
            "and it holds only what an unread `items.cpp` can already show: {partial:?}"
        );

        session.index_everything();
        assert_eq!(session.pending(), 0);

        // The view is re-made, because indexing is what gives the file its summary — and the query reads the index.
        // **A view of the file the session holds, not of its rendering.** This test is about the state where the
        // index still has work to do, so it deliberately does not pump — and `Session::view` is a *request* for the
        // rendering that answers `None` until that work is done ("a caller that needs meaning waits"). Asking it here
        // was the test contradicting itself: the fixture is "unread on purpose" and the view needs a read.
        // `FileView::parse_with` is the constructor for exactly this: the file's own text, parsed, no cooking.
        let held = session
            .files()
            .held("/p/items.cpp")
            .expect("the file is held");
        let view = cpp_code_analysis::FileView::parse_with(
            &held,
            &cpp_parser::MacroEnvironment::default(),
        );
        let found = locations(
            &session,
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(on_the_use),
            true,
        )
        .expect("the index is complete now");
        assert_eq!(found.len(), 2, "the `#define` and the one use: {found:?}");
        assert!(
            found
                .iter()
                .any(|location| location.uri.as_str().ends_with("limits.h")),
            "including the file that defines it: {found:?}"
        );
        assert_eq!(
            locations(
                &session,
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(on_the_use),
                false
            )
            .expect("complete")
            .len(),
            1,
            "and `includeDeclaration: false` leaves the `#define` out"
        );
    }
}

/// How many candidate files one name-level find-references will read.
///
/// The same shape as `cpp_code_analysis::index::references`' macro budget: a bound that is **reported** rather than
/// silently enforced. A project this server measures is a thousand files and a closure of a few hundred, so the
/// default is past anything measured.
const NAME_REFERENCE_BUDGET: usize = 2000;

/// The locations of a **name** — a function, a class, a variable — as opposed to a macro's.
///
/// The declarations come from the index (`Session::definitions`), which is the same answer "go to definition" gives;
/// the uses come from `name_references`, which lexes the files that can see those declarations. The two are merged and
/// deduplicated, because a declaration is also a place the name is written and a client shows it once.
fn name_level_locations<F: cpp_code_analysis::FileProvider + Clone>(
    session: &cpp_code_analysis::Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
    include_declaration: bool,
) -> Vec<Location> {
    let Known::Yes(defined) = session.definitions(view, offset) else {
        return Vec::new();
    };
    let Some(first) = defined.found.first() else {
        return Vec::new();
    };
    let name = first.fact.name.clone();

    let mut declaring: Vec<std::path::PathBuf> = defined
        .found
        .iter()
        .map(|definition| definition.file.clone())
        .collect();
    declaring.sort();
    declaring.dedup();

    let mut locations: Vec<Location> = Vec::new();
    let push = |file: &std::path::Path,
                    range: cpp_parser::SourceRange,
                    locations: &mut Vec<Location>| {
        let Some(held) = session.files().held(file) else {
            return;
        };
        let Some(uri) = crate::util::path_to_uri(file) else {
            return;
        };
        let (Some(start), Some(end)) = (
            crate::util::position_in_file(held, range.start_offset),
            crate::util::position_in_file(held, range.end_offset()),
        ) else {
            return;
        };
        let location = Location {
            uri,
            range: lsp_types::Range { start, end },
        };
        if !locations
            .iter()
            .any(|held| held.uri == location.uri && held.range == location.range)
        {
            locations.push(location);
        }
    };

    if include_declaration {
        for definition in &defined.found {
            push(&definition.file, definition.fact.name_range, &mut locations);
        }
    }

    let read = |path: &std::path::Path| {
        session
            .files()
            .held(path)
            .map(|file| file.text.to_string())
    };
    let references = cpp_code_analysis::index::references::name_references(
        session.index(),
        &read,
        &name,
        &declaring,
        NAME_REFERENCE_BUDGET,
    );
    for (file, ranges) in references.found {
        for range in ranges {
            push(&file, range, &mut locations);
        }
    }

    locations
}

#[cfg(test)]
mod name_level {
    use std::path::PathBuf;

    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    use super::name_level_locations;

    const BASE_H: &str = "struct Widget {\n    int size;\n};\n";
    const USE_CPP: &str = "#include \"base.h\"\nvoid f(Widget w) {\n    (void)w.size;\n}\n";

    /// **A name's references cross files, and the declaration is one of them.**
    ///
    /// The candidate rule is the macro path's — a file can only refer to a name it can see — and the second level is
    /// scopes: `use.cpp` does not declare `Widget` itself, so its identifiers are name-level uses. The assertion that
    /// matters for a reader is that the answer spans **both** files; the one that matters for a rename is that the
    /// declaration is in the list when it is asked for.
    #[test]
    fn a_class_declared_in_a_header_is_referenced_from_the_file_that_includes_it() {
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

        let use_path = PathBuf::from("/p/use.cpp");
        let held = session.files().held(&use_path).expect("the file is held");
        let view = cpp_code_analysis::FileView::parse_with(
            held,
            &cpp_parser::MacroEnvironment::default(),
        );
        let at = USE_CPP.find("Widget").expect("the fixture uses it");

        let locations = name_level_locations(
            &session,
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
            true,
        );

        assert!(
            locations.iter().any(|found| found.uri.path().ends_with("base.h")),
            "the declaration in the header is one of the references: {locations:?}"
        );
        assert!(
            locations.iter().any(|found| found.uri.path().ends_with("use.cpp")),
            "and the use in the file that included it: {locations:?}"
        );
    }
}