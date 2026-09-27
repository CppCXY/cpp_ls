//! # `textDocument/completion` — what can be written at the cursor
//!
//! The two questions a cursor asks, and the query each one goes to:
//!
//! ```text
//! w.|  w.si|  p->|      a member access   → session.member_completions  → the object's type's members
//! one::|  Wid|          a name position   → session.name_completions    → the visible names, innermost first
//! ```
//!
//! **The cursor decides, and the analysis is asked rather than guessed at.** `member_completions` answers
//! `Unknown(UnparsableName)` for a cursor that is not on a member access — that is the documented division between
//! the two queries — so this handler asks the member question first and falls back to the name question on exactly
//! that answer. The other `Unknown`s from the member query (`UnknownType`, `NotDeclaredHere`,
//! `ConditionalCompilation`) mean "this *is* a member access and the type could not be worked out", and the honest
//! answer there is **nothing**: a client that offered the file's global names after a `.` would be offering names
//! that cannot follow it.
//!
//! # What the client gets
//!
//! Every name arrives with the **range it replaces** ([`NameCompletions::name_range`] /
//! [`MemberCompletions::member_range`]) rather than as a bare label, because the two states a completion is asked in
//! are "nothing written yet" and "half a name written", and only the analysis knows which. The list is sorted by
//! **how far out the name was found** (`OfferedName::depth`, `ProjectMember::depth`) — a local before a member of
//! the enclosing class, and that before a file-scope name from an included header — which is C++'s own order of
//! consideration, so a client that displays the list as it arrives displays it in the order a compiler would pick
//! from.
//!
//! **`is_incomplete` is the session's pending work**, and that is what the field is for: while the index still has
//! files to read, a name may be missing for the only reason that its file has not been read yet, so the client is
//! told to ask again as the user types rather than caching an empty list as the truth.

use cpp_code_analysis::{DiskFiles, Known, MemberCompletions, NameCompletions, Session, UnknownReason};
use lsp_types::{
    ClientCapabilities, CompletionItem, CompletionOptions, CompletionParams, CompletionResponse,
    Documentation, MarkupContent, MarkupKind, ServerCapabilities, TextEdit,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::handlers::hover::documentation_text;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{completion_kind, offset_at_position, position_in, uri_to_file_path};

pub async fn on_completion(
    context: ServerContextSnapshot,
    params: CompletionParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<CompletionResponse> {
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;

    // Read in first, under the write lock, so the query itself can be a read — the same boundary every handler has.
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let offset = offset_at_position(&view, position)?;

        Some(completions(session, &view, offset))
    })
    .await
}

/// The completion list for one cursor, from the two queries the analysis answers.
///
/// A plain function rather than a closure inside the handler, so that a test can ask about a cursor without a
/// client, a URI and a position on the wire.
pub fn completions(
    session: &Session<DiskFiles>,
    view: &cpp_code_analysis::FileView,
    offset: usize,
) -> CompletionResponse {
    // The member question first — see the module documentation for why the *answer* decides and not the spelling
    // in front of the cursor.
    match session.member_completions(view, offset) {
        Known::Yes(found) => {
            let items = members(&found, view).collect();
            return CompletionResponse::List(lsp_types::CompletionList {
                is_incomplete: session.pending() > 0,
                items,
            });
        }
        Known::Unknown(UnknownReason::UnparsableName) => {}
        // A member access whose type this analysis cannot work out, or an answer it cannot give yet: nothing is
        // offered, because a name that cannot follow the `.` is worse than an empty popup.
        Known::No | Known::Unknown(_) => {
            return CompletionResponse::List(lsp_types::CompletionList {
                is_incomplete: true,
                items: Vec::new(),
            });
        }
    }

    let Known::Yes(found) = session.name_completions(view, offset) else {
        // `No` and `Unknown` are the same answer here — "not known yet" and "nothing visible" both mean the list
        // is not the truth — so the client is told to ask again.
        return CompletionResponse::List(lsp_types::CompletionList {
            is_incomplete: true,
            items: Vec::new(),
        });
    };

    CompletionResponse::List(lsp_types::CompletionList {
        is_incomplete: session.pending() > 0,
        items: names(&found, view).collect(),
    })
}

/// The members of the accessed type, in the order the analysis found them.
fn members<'a>(
    found: &'a MemberCompletions,
    view: &'a cpp_code_analysis::FileView,
) -> impl Iterator<Item = CompletionItem> + 'a {
    found.members.members.iter().map(move |member| {
        let mut item = item_for(&member.fact, &member.file, member.depth, view, found.member_range);

        // **Which class declares it**, for a member that is inherited — the one thing a reader cannot see from the
        // name, since the derived class body does not mention it.
        if member.depth > 0 {
            item.detail = Some(format!("{} (inherited from {})", name_of(&member.fact), member.declared_in));
        }

        // **A name two bases declare**, which is the ambiguity the list keeps both halves of (see `members_of`):
        // saying so is the honest answer, and a client that shows it saves the reader a compile error.
        if member.ambiguous {
            item.documentation = Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!(
                    "`{}` is declared in more than one base of `{}`, so `{}` does not name one of them.",
                    name_of(&member.fact),
                    found.class,
                    name_of(&member.fact)
                ),
            }));
        }

        item
    })
}

/// The visible names, in the order the analysis found them.
fn names<'a>(
    found: &'a NameCompletions,
    view: &'a cpp_code_analysis::FileView,
) -> impl Iterator<Item = CompletionItem> + 'a {
    found
        .names
        .iter()
        .map(move |offered| item_for(&offered.fact, &offered.file, offered.depth, view, found.name_range))
}

/// One name, as a completion item that **replaces the range the analysis gave**.
///
/// `sort_text` carries the depth, zero-padded, so that a client sorting by it gets C++'s own order — the scope the
/// cursor is in, then each enclosing scope, then the files it includes — instead of the alphabet.
fn item_for(
    fact: &cpp_code_analysis::DeclFact,
    file: &std::path::Path,
    depth: usize,
    view: &cpp_code_analysis::FileView,
    replace: cpp_parser::SourceRange,
) -> CompletionItem {
    CompletionItem {
        label: name_of(fact).to_string(),
        kind: Some(completion_kind(fact.kind)),
        // **Where the declaration is**, for `completionItem/resolve` to find it again — see `identity_of`.
        data: identity_of(fact, file),
        // Where the name lives, when the scope says something the label does not: a member of `ns::Widget` offers
        // `size` with `ns::Widget` beside it.
        detail: fact.scope.clone(),
        sort_text: Some(format!("{depth:03}_{}", name_of(fact))),
        // The range comes back as an edit rather than as `insert_text`, so that the half-written name is
        // *replaced*: `w.si` completed to `size` must not become `w.size` by insertion.
        text_edit: text_edit(view, replace, name_of(fact)),
        ..CompletionItem::default()
    }
}

/// The edit that replaces one range with the chosen name, **if the range is a place in this file**.
///
/// `None` when it is not, which leaves the client to insert the label at the cursor. That happens for an answer
/// built from a *rendering*: a name a macro declared is reported at the invocation, and the invocation is a place
/// in the file, so the range is answerable — but the mapping is the analysis's, and a handler that assumed it would
/// be handing a client an offset into text nobody is looking at.
fn text_edit(
    view: &cpp_code_analysis::FileView,
    replace: cpp_parser::SourceRange,
    new_text: &str,
) -> Option<lsp_types::CompletionTextEdit> {
    Some(lsp_types::CompletionTextEdit::Edit(TextEdit {
        range: lsp_types::Range::new(
            position_in_file_of(view, replace.start_offset)?,
            position_in_file_of(view, replace.end_offset())?,
        ),
        new_text: new_text.to_string(),
    }))
}

fn position_in_file_of(
    view: &cpp_code_analysis::FileView,
    offset: usize,
) -> Option<lsp_types::Position> {
    position_in(&view.source, &view.line_index, offset)
}

fn name_of(fact: &cpp_code_analysis::DeclFact) -> &str {
    &fact.name
}

/// **Where a declaration is**, as the `data` a client echoes back to `completionItem/resolve`.
///
/// The item's own identity, not its rendering: the file and the offset of the declared **name**, which is what
/// every documentation question in this server is asked with (`Session::documentation`). Putting the comment itself
/// in the item would be answering a question the user has not asked yet — for every name in the list ✓.
///
/// `None` when the path cannot be spelled as a string, which leaves an item the client simply does not resolve: an
/// item without documentation is a smaller loss than an item whose documentation is another declaration's.
fn identity_of(fact: &cpp_code_analysis::DeclFact, file: &std::path::Path) -> Option<lsp_types::LSPAny> {
    Some(serde_json::json!({
        "file": file.to_str()?,
        "offset": fact.name_range.start_offset,
    }))
}

/// **`completionItem/resolve`**: the documentation for the one item a client is showing.
///
/// The declaration is found again from the item's own `data` — the file and the name's offset — and its comment is
/// read through the session, exactly as the hover reads it: the same question, the same reading, the same rendering
/// ([`documentation_text`] is shared with the hover and with signature help, so three popups about one declaration
/// cannot disagree about what it says).
///
/// An item this layer cannot re-find (a client that strips `data`, a file deleted since the list was built) comes
/// back **unchanged** rather than as an error: the protocol's own note says a resolve must not fail, and an item
/// without documentation is exactly what the client already has.
pub async fn on_completion_resolve(
    context: ServerContextSnapshot,
    mut item: CompletionItem,
    cancel_token: CancellationToken,
) -> RequestOutcome<CompletionItem> {
    let identity = item.data.clone();
    let file = identity
        .as_ref()
        .and_then(|identity| identity.get("file"))
        .and_then(|value| value.as_str())
        .map(std::path::PathBuf::from);
    let offset = identity
        .as_ref()
        .and_then(|identity| identity.get("offset"))
        .and_then(|value| value.as_u64())
        .map(|value| value as usize);

    // An item with no identity — a client that strips `data`, a hand-written request — is answered with itself:
    // the protocol says a resolve must not fail, and an item without documentation is what the client already has.
    let (Some(file), Some(offset)) = (file, offset) else {
        return RequestOutcome::Ready(item);
    };

    context.analysis().prepare(&file).await;

    let resolved = snapshot_query(context.analysis(), cancel_token, move |session| {
        // The view is of the **declaring** file, which is what makes the lookup below one walk for a declaration in
        // the file being edited and one parse for the first name that comes from a header.
        let view = session.view(&file)?;
        let comment = session.documentation(&view, &file, offset)?;

        Some(documentation_text(&comment))
    })
    .await;

    if let RequestOutcome::Ready(Some(text)) = resolved {
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: text,
        }));
    }

    RequestOutcome::Ready(item)
}

pub struct CompletionCapabilities;

impl RegisterCapabilities for CompletionCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.completion_provider = Some(CompletionOptions {
            // The characters that make a client **ask without the user typing a letter**: the two member
            // operators and the start of a qualified name. `:` alone is enough for `::` — the client sends one
            // request per trigger character, and the second `:` finds the list already asked for.
            trigger_characters: Some(vec![".".to_string(), ">".to_string(), ":".to_string()]),
            // **`completionItem/resolve` is on**, and the round trip is what keeps the list cheap: attaching the
            // documentation to every item would be one lookup per offered name — a hundred tree walks (and, for a
            // name from a header nobody has read, a parse) for a list the user is about to filter by typing —
            // while a resolve asks about the *one* item the client is showing. See `on_completion_resolve`.
            resolve_provider: Some(true),
            ..CompletionOptions::default()
        });
    }
}

#[cfg(test)]
mod tests {
    // The kind mapping has its own tests where it lives now (`crate::util::kind`), beside the outline's — one
    // question, one place, so that the two vocabularies can be read together and cannot drift apart.

    use super::*;

    /// **What a resolve is asked with is the declaration's *place*, not its rendering**: the file and the offset
    /// of the name. That is the whole reason the round trip is cheap — the comment is fetched for the one item a
    /// client shows, rather than for every name in the list.
    #[test]
    fn an_item_carries_where_its_declaration_is() {
        let fact = cpp_code_analysis::DeclFact {
            name: "size".to_string(),
            scope: None,
            local: false,
            kind: cpp_code_analysis::DeclKind::Variable,
            type_of: Some("int".to_string()),
            returns: None,
            bases: Vec::new(),
            range: cpp_parser::SourceRange::new(120, 4),
            name_range: cpp_parser::SourceRange::new(120, 4),
            clean: true,
            guard: cpp_code_analysis::FactGuard::Unconditional,
        };

        let identity = identity_of(&fact, std::path::Path::new("/p/widget.h")).expect("a path spells");

        assert_eq!(identity["file"], serde_json::json!("/p/widget.h"));
        assert_eq!(identity["offset"], serde_json::json!(120));
    }
}
