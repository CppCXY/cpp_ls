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

        assert!(
            session.pending() > 0,
            "the fixture is the state this test is about"
        );
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
///
/// # A member of one class is not a member of another, and this function is where that is decided
///
/// The lexer answers *every* occurrence of the spelling, because a spelling is all a lexer can see: asking for
/// `_diagnosticStyle` on a real project answered three classes' members and eighteen uses, and a reader looking at
/// `CodeFormat::_diagnosticStyle` was shown `LuaCodeFormat`'s and `FormatState`'s as well. That is a list of the
/// **wrong entities** — a rename built on it would rewrite another class's member.
///
/// So the cursor's own class decides which declaration is meant, and an occurrence is dropped when it is *provably*
/// another class's:
///
/// ```text
/// the cursor is inside a class body     → the declaration whose `scope` is that class is the one meant
/// an occurrence is a bare name inside   → dropped: unqualified lookup in a member function finds its own
///   a member of a *different* class       class's member before anything else, so it cannot be this one
/// an occurrence has a qualifier         → kept: `x.`, `p->`, `Type::` — which class or object the receiver is
///   (`.`, `->`, `::`)                     is a type question this list does not answer, and keeping it is the
///                                         conservative direction for a reader who is looking, not renaming
/// no enclosing class at all             → kept, for the same reason
/// ```
///
/// The declarations are filtered by the same rule, which is what keeps another class's *declaration* out of the list.
pub fn name_level_offsets<F: cpp_code_analysis::FileProvider + Clone>(
    session: &cpp_code_analysis::Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
    include_declaration: bool,
) -> Vec<(std::path::PathBuf, cpp_parser::SourceRange)> {
    let Known::Yes(defined) = session.definitions(view, offset) else {
        return Vec::new();
    };
    let Some(first) = defined.found.first() else {
        return Vec::new();
    };
    let name = first.fact.name.clone();

    // **The class the cursor is in**, which is what says *which* of the declarations is meant.
    let owner = the_class_the_cursor_is_in(&view.scopes, &view.root, offset.get());
    let defined = narrowed_to_the_cursors_class(view, offset.get(), defined);
    let aimed: Vec<&cpp_code_analysis::ProjectDefinition> = defined.found.iter().collect();

    let mut declaring: Vec<std::path::PathBuf> = aimed
        .iter()
        .map(|definition| definition.file.clone())
        .collect();
    declaring.sort();
    declaring.dedup();


    let mut found: Vec<(std::path::PathBuf, cpp_parser::SourceRange)> = Vec::new();
    let push = |file: &std::path::Path,
                range: cpp_parser::SourceRange,
                found: &mut Vec<(std::path::PathBuf, cpp_parser::SourceRange)>| {
        let key = (file.to_path_buf(), range);
        if !found.contains(&key) {
            found.push(key);
        }
    };

    if include_declaration {
        for definition in &aimed {
            push(&definition.file, definition.fact.name_range, &mut found);
        }
    }

    let read =
        |path: &std::path::Path| session.files().held(path).map(|file| file.text.to_string());
    let references = cpp_code_analysis::index::references::name_references(
        session.index(),
        &read,
        &name,
        &declaring,
        NAME_REFERENCE_BUDGET,
    );
    // **One parse per candidate file, and only if it has an occurrence.** The scopes are what says which class an
    // occurrence is inside, and a file with no occurrence of the name is never parsed.
    let mut parsed: std::collections::HashMap<std::path::PathBuf, Option<cpp_code_analysis::FileView>> =
        std::collections::HashMap::new();
    for (file, ranges) in references.found {
        let scopes = match &owner {
            Some(class) => {
                let view = parsed.entry(file.clone()).or_insert_with(|| {
                    session.files().held(&file).map(|held| {
                        cpp_code_analysis::FileView::parse_with(
                            held,
                            &cpp_parser::MacroEnvironment::default(),
                        )
                    })
                });
                match view.as_ref() {
                    Some(view) => Some((class, view)),
                    None => None,
                }
            }
            None => None,
        };
        for range in ranges {
            if let Some((class, view)) = &scopes
                && an_occurrence_of_another_class(session, view, range.start_offset, class)
            {
                continue;
            }
            push(&file, range, &mut found);
        }
    }

    found
}

/// [`name_level_offsets`], as protocol positions.
fn name_level_locations<F: cpp_code_analysis::FileProvider + Clone>(
    session: &cpp_code_analysis::Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
    include_declaration: bool,
) -> Vec<Location> {
    name_level_offsets(session, view, offset, include_declaration)
        .into_iter()
        .filter_map(|(file, range)| {
            let uri = crate::util::path_to_uri(&file)?;
            let (Some(start), Some(end)) = (
                position_of(session, &file, range.start_offset),
                position_of(session, &file, range.end_offset()),
            ) else {
                return None;
            };
            Some(Location {
                uri,
                range: lsp_types::Range { start, end },
            })
        })
        .collect()
}
/// **The position of a byte in a file, whether or not this session is holding it.**
///
/// The VFS holds what has been read into *this* run, and a header the index read from its **disk cache** is not in it
/// — which is why a member's declaration in its header was missing from every reference list. Reading such a file
/// here is one `read_to_string` for a list a user asked for, and it is the only way the declaration can be shown.
///
/// Split out of the `push` closure so that the disk path is testable on its own: a session over `MemoryFiles` holds
/// every file it knows, and the case that matters is precisely the file it does not know.
fn position_of<F: cpp_code_analysis::FileProvider + Clone>(
    session: &cpp_code_analysis::Session<F>,
    file: &std::path::Path,
    offset: usize,
) -> Option<lsp_types::Position> {
    if let Some(held) = session.files().held(file) {
        return crate::util::position_in(&held.text, &held.line_index, offset);
    }
    let text = std::fs::read_to_string(file).ok()?;
    let index = cpp_parser::LineIndex::parse(&text);
    crate::util::position_in(&text, &index, offset)
}
/// The **innermost class** whose body contains `offset` — by name, which is what a `DeclFact::scope` carries.
///
/// # The two ways a position is "inside a class", and only one of them is a scope
///
/// A member function defined **inside** its class body has a class scope around the position, and the scope tree says
/// so. A member function defined **outside** it — `void CodeFormat::UpdateCodeStyle() { … }` in a `.cpp`, which is
/// where most code a reader asks about lives — has no class scope at all: the position is inside a *function* at file
/// scope, and the class exists only in the declarator's text (`CodeFormat::UpdateCodeStyle`).
///
/// Both are answered here, because the second is the ordinary case in a real project: without it every reference
/// query in a `.cpp` had no class to compare against and nothing was filtered.
pub fn the_class_the_cursor_is_in(
    scopes: &cpp_code_analysis::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    offset: usize,
) -> Option<String> {
    if let Some(id) = scopes.scope_at(offset)
        && let Some(class) = scopes.scope_chain(id).into_iter().find_map(|id| {
            let scope = scopes.scope(id)?;
            (scope.kind == cpp_code_analysis::ScopeKind::Class)
                .then(|| scope.name.as_ref().map(|name| name.to_string()))
                .flatten()
        })
    {
        return Some(class);
    }
    // **The class a qualified definition names.** `void CodeFormat::UpdateCodeStyle(…)` is a `FunctionDef` whose
    // declarator reads `CodeFormat::UpdateCodeStyle`, so the last `::` segment before the parameter list is the class.
    let token = cpp_parser::token_at(root, offset)?;
    // **A definition parses as a `Declaration` with a body**, not as a `FunctionDef` — measured by printing the
    // ancestors of a name inside one: `IdentifierExpr, BinaryExpr, ExpressionStat, CompoundStat, Declaration,
    // TranslationUnit`. Both kinds are accepted because a function defined *inside* a class body is the other shape.
    let definition = token.parent_ancestors().find(|node| {
        matches!(
            cpp_parser::CppSyntaxKind::from(node.kind()),
            cpp_parser::CppSyntaxKind::FunctionDef | cpp_parser::CppSyntaxKind::Declaration
        )
    })?;
    // **The declaration's own text, up to its parameter list.** The `InitDeclarator` of a definition holds only
    // `()` — measured: `void First::update() { … }` has children `[DeclSpecifierSeq, InitDeclarator, CompoundStat]`
    // and the declarator's text is `"()"` — so the qualified spelling lives in the declaration, and the first `(`
    // ends the signature.
    let text = definition.text().to_string();
    let before_the_parameters = text.split('(').next()?.trim();
    let (before_the_name, _) = before_the_parameters.rsplit_once("::")?;
    // **The last identifier before the name is the class**, not the whole left side: `void First::update` splits
    // into `void First` and `update`, and `void First` is not a class spelling. `ns::C::f` gives `C`, because a
    // `DeclFact::scope` carries the last segment and the class itself is the last word of what is left.
    before_the_name
        .rsplit(|character: char| !(character.is_alphanumeric() || character == '_'))
        .next()
        .map(str::trim)
        .filter(|class| !class.is_empty())
        .map(str::to_string)
}

/// **The declarations the cursor's own class is about**, when the cursor is inside a class and the name is one of its
/// members — the rule [`name_level_locations`] and Go to Definition both need.
///
/// Reported twice from the same screenshot: Go to Definition on `_diagnosticStyle` listed three classes' members, and
/// Find References listed the same three twice over. A member of one class is not a member of another, and the cursor's
/// enclosing class is what says which one is meant.
///
/// A name the class does **not** declare itself is left alone: lookups reached further out (a base's member, a
/// namespace's name), and the whole list is the honest answer then.
pub(crate) fn narrowed_to_the_cursors_class(
    view: &cpp_code_analysis::FileView,
    offset: usize,
    found: cpp_code_analysis::ProjectDefinitions,
) -> cpp_code_analysis::ProjectDefinitions {
    let Some(class) = the_class_the_cursor_is_in(&view.scopes, &view.root, offset) else {
        return found;
    };
    let inside: Vec<cpp_code_analysis::ProjectDefinition> = found
        .found
        .iter()
        .filter(|definition| definition.fact.scope.as_deref() == Some(class.as_str()))
        .cloned()
        .collect();
    if inside.is_empty() {
        return found;
    }
    let mut narrowed = found;
    narrowed.found = inside;
    narrowed
}
/// **Is this occurrence provably another class's member?** — the filter [`name_level_locations`] applies.
///
/// Two shapes, and they are answered by different questions because the language answers them differently:
///
/// ```text
/// a bare name       `_value` inside `Second::update` — unqualified lookup finds that class's member first, so a
///                   different enclosing class proves it is not this one
/// a qualified name  `other._value` / `p->_value` / `Type::_value` — the *receiver's type* is what decides, and
///                   `Session::type_at` is the same inference every other query uses
/// ```
///
/// A receiver whose type cannot be worked out keeps the occurrence: a list a reader cannot rename from is a
/// smaller problem than a list that hides a use.
fn an_occurrence_of_another_class<F: cpp_code_analysis::FileProvider + Clone>(
    session: &cpp_code_analysis::Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: usize,
    owner: &str,
) -> bool {
    use cpp_code_analysis::Known;

    // **Qualified**: the member access the occurrence is in names it, and its object's type is the answer.

    if let Some(access) = cpp_code_analysis::sema::resolve::member_access_at(&view.root, offset)
        && access.member_range.start_offset == offset
    {
        let at = cpp_code_analysis::file::coordinates::ReadingOffset::new(
            usize::from(access.object.text_range().start()),
        );
        return match session.type_at(view, at) {
            Known::Yes(found) => cpp_code_analysis::sema::types::parse_type_spelling(
                &found.type_of,
            )
            .class_name()
            .is_some_and(|class| {
                crate::handlers::implementation::last_segment_of(class) != owner
            }),
            _ => false,
        };
    }

    // **Bare**: the enclosing class decides, and a class that is not this one means the name is not this member.
    match the_class_the_cursor_is_in(&view.scopes, &view.root, offset) {
        Some(class) => class != owner,
        None => false,
    }
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
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        let at = USE_CPP.find("Widget").expect("the fixture uses it");

        let locations = name_level_locations(
            &session,
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
            true,
        );

        assert!(
            locations
                .iter()
                .any(|found| found.uri.path().ends_with("base.h")),
            "the declaration in the header is one of the references: {locations:?}"
        );
        assert!(
            locations
                .iter()
                .any(|found| found.uri.path().ends_with("use.cpp")),
            "and the use in the file that included it: {locations:?}"
        );
    }
}
#[cfg(test)]
mod a_member_of_one_class {
    use std::path::PathBuf;

    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    use super::name_level_locations;

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

    /// **One class's member is not another class's**, and the declaration in the header is one of its references.
    ///
    /// Measured from a reader's screenshot: Find References on `_diagnosticStyle` answered three classes' members and
    /// eighteen uses, all by spelling, and no header at all. The spelling is all a lexer can see, so the filter is a
    /// scope question — and for a `.cpp` the class is not a scope but the prefix of a qualified definition
    /// (`void First::update()`), which is the case this test pins.
    #[test]
    fn is_not_another_classs_member_and_the_header_declaration_counts() {
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

        let path = PathBuf::from("/p/use.cpp");
        let held = session.files().held(&path).expect("the file is held");
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        // The cursor on `_value` **inside `First::update`** — the member the reader asked about.
        let at = USE_CPP
            .find("_value = 1")
            .expect("the fixture writes the member in First::update");

        let locations = name_level_locations(
            &session,
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
            true,
        );
        println!(
            "references: {:?}",
            locations
                .iter()
                .map(|found| (found.uri.path().to_string(), found.range.start.line))
                .collect::<Vec<_>>()
        );

        assert!(
            locations
                .iter()
                .any(|found| found.uri.path().ends_with("base.h")),
            "the declaration in the header is a reference: {locations:?}"
        );
        assert_eq!(
            locations
                .iter()
                .filter(|found| found.uri.path().ends_with("use.cpp"))
                .count(),
            1,
            "one use — `First::update`'s — and not `Second`'s: {locations:?}"
        );
    }
}
#[cfg(test)]
mod a_file_the_session_does_not_hold {
    use super::position_of;
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    /// **A declaration in a file the VFS never read still gets a position.**
    ///
    /// This is the real defect behind "查找引用没有查找到头文件": the index knows the declaration is in
    /// `codeformat.h`, and the request answered nothing for it because the VFS holds only what has been read into this
    /// run — a header the index read *from its disk cache* is not in it. The fix reads the file; this test makes the
    /// file exist on disk and *not* in the session, which is the state that used to answer `None`.
    #[test]
    fn is_positioned_even_though_the_vfs_has_never_seen_it() {
        let root = std::env::temp_dir().join("cpp_ls-references-disk");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let header = root.join("only_on_disk.h");
        std::fs::write(&header, "struct Widget {\n    int size;\n};\n").expect("the fixture");

        let files = MemoryFiles::new();
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let session = Session::with_config(
            &root,
            providers,
            WatchFilter::new(&root),
            CompilerConfig::default(),
        );
        assert!(
            session.files().held(&header).is_none(),
            "the fixture is the state this test is about"
        );

        // The `size` of line 2, counted from zero: its position is line 1, character 8.
        let at = "struct Widget {\n    int ".len();
        let found = position_of(&session, &header, at).expect("the file is read for its line index");
        println!("{header:?} at {at} → {found:?}");
        assert_eq!(found.line, 1);
        assert_eq!(found.character, 8);
    }
}
#[cfg(test)]
mod a_definition_in_a_class {
    use std::path::PathBuf;

    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    /// **Go to Definition on a member answers its own class's declaration, not every class's.**
    ///
    /// The first screenshot of this report: three `_diagnosticStyle`s, from `CodeFormat`, `FormatState` and
    /// `DiagnosticBuilder`, offered for a cursor inside one of them. `definitions` answers by name — it has to, a name
    /// is what a lookup is — so the narrowing belongs to the caller that knows where the cursor is.
    #[test]
    fn is_the_class_the_cursor_is_in() {
        let base = "\
struct First {
    int _value;
    void update();
};
struct Second {
    int _value;
    void update();
};
";
        let use_cpp = "\
#include \"base.h\"
void First::update() { _value = 1; }
";
        let files = MemoryFiles::new()
            .with_file("/p/base.h", base)
            .with_file("/p/use.cpp", use_cpp);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files([PathBuf::from("/p/base.h"), PathBuf::from("/p/use.cpp")]);
        session.index_everything();

        let path = PathBuf::from("/p/use.cpp");
        let held = session.files().held(&path).expect("the file is held");
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        let at = use_cpp.find("_value").expect("the fixture uses it");

        let cpp_code_analysis::Known::Yes(found) = session.definitions(
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
        ) else {
            panic!("`_value` is declared");
        };
        println!(
            "before: {:?}",
            found
                .found
                .iter()
                .map(|definition| definition.fact.qualified_name())
                .collect::<Vec<_>>()
        );
        assert_eq!(found.found.len(), 2, "the fixture is two classes' members");

        let narrowed = super::narrowed_to_the_cursors_class(&view, at, found);
        let names: Vec<String> = narrowed
            .found
            .iter()
            .map(|definition| definition.fact.qualified_name())
            .collect();
        println!("after: {names:?}");
        assert_eq!(
            names,
            ["First::_value"],
            "the cursor is in `First::update`, so that is the declaration meant"
        );
    }
}
#[cfg(test)]
mod a_receiver_of_another_class {
    use std::path::PathBuf;

    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    use super::name_level_locations;

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
void Second::update() {
    First first;
    first._value = 2;    // First's member, reached through a First
    _value = 3;          // Second's member
}
";

    /// **A qualified use is the receiver's**, so `first._value` belongs to `First` even from `Second`'s member.
    ///
    /// This is the half the bare-name rule cannot answer: the occurrence is inside `Second::update`, which would make
    /// every unqualified `_value` there `Second`'s — and it is, except the one behind `.`. The receiver's type decides,
    /// with the same inference every other query uses.
    #[test]
    fn is_the_receivers_and_a_bare_name_is_the_enclosing_classs() {
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

        let path = PathBuf::from("/p/use.cpp");
        let held = session.files().held(&path).expect("the file is held");
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());

        // **From `First::update`**: its own use, the receiver-qualified one, and not `Second`'s.
        let at = USE_CPP
            .find("_value = 1")
            .expect("the fixture writes it in First::update");
        let found = name_level_locations(
            &session,
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
            true,
        );
        let lines: Vec<u32> = found
            .iter()
            .filter(|location| location.uri.path().ends_with("use.cpp"))
            .map(|location| location.range.start.line)
            .collect();
        println!("lines in use.cpp for First::_value: {lines:?}");
        assert_eq!(
            lines,
            [1, 4],
            "the bare use in `First::update` and the receiver-qualified one — not `Second`'s own"
        );
    }
}