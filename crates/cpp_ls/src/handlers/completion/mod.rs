//! # `textDocument/completion` — what can be written at the cursor
//!
//! This handler is thin on purpose: **the whole of the decision is in the analysis layer**
//! ([`cpp_code_analysis::completion`]), and what is left here is the wire. That boundary is the point of the
//! module: "which question is this cursor asking, and in what order should the answer be shown" is a question
//! about C++ and about the index, not about LSP, and a version of it written against `CompletionItem` is a version
//! that cannot be tested without a client.
//!
//! ```text
//! the request          position → offset → session.completions(view, offset)   (analysis)
//! the answer           CompletionItem → lsp_types::CompletionItem             (here)
//! ```
//!
//! # What the client gets
//!
//! * **The range it replaces** ([`cpp_code_analysis::CompletionSet::replace`]), not a bare label. The two states a
//!   completion is asked in are "nothing written yet" and "half a name written", and only the analysis knows which:
//!   `w.si` completed to `size` must not become `w.size` by insertion.
//! * **A `sortText` that is the order the analysis produced**, so that a client sorting the list itself does not
//!   undo the ranking — a local before a keyword before a name from `<cstdio>` is the whole feature, and a client
//!   that re-sorted alphabetically would throw it away.
//! * **Snippets, when the client says it can insert them.** The bodies are the analysis's; whether to send them at
//!   all is a capability of the client, and a client that cannot interpolate `${1:condition}` would insert the
//!   placeholder text literally.
//! * **`completionItem/resolve` is on**, and each item carries only its declaration's **position** — the file and
//!   the offset of the name. The documentation is fetched for the one item the user is looking at, rather than for
//!   every name in the list.
//!
//! # `is_incomplete` is two different things, and this is where they are told apart
//!
//! The protocol has one flag for two claims that a client reacts to in opposite ways:
//!
//! * **the analysis still has files to read** ([`Session::pending`]) — a name may be missing for the only reason
//!   that nothing has read the file it is in yet, so the client should **ask again** as the user types;
//! * **the budget cut this list** ([`CompletionSet::truncated`]) — there are more names than one message can carry.
//!
//! Both are set, and the second one is the correction of a mistake this file made for a long time: it reasoned that
//! a capped list "would return the same list for ever", so it cleared the flag. That is true only for a client that
//! re-asks with the **same** prefix, and no client does: the prefix is applied in the index **where the names are
//! still borrowed** (`declarations_in_scope`), so the next keystroke runs a *narrower query* rather than filtering a
//! list — and it is exact, because a query with `str` written collects the names beginning with `str` and nothing
//! else. Measured on a file that includes `<cstdio>`, `<iostream>`, `<optional>`, `<string>` and `<format>`:
//! completion after `std::` collects **2 146** declarations and sends the best **200**, `string` and `basic_string`
//! among those it does not send, and with the flag cleared the client filtered those 200 rows locally for ever —
//! typing `std::str` showed *nothing*, while the same query asked of the server answers `string` first.
//!
//! The cost of setting it is one request per keystroke while a big namespace is being typed through, which is the
//! request a client sends anyway when it does not know the answer.

use cpp_code_analysis::{CompletionItem as AnalysisItem, DiskFiles, ItemKind, Session};
use lsp_types::{
    ClientCapabilities, CompletionItem, CompletionItemKind, CompletionItemLabelDetails,
    CompletionOptions, CompletionParams, CompletionResponse, CompletionTextEdit, Documentation,
    InsertTextFormat, MarkupContent, MarkupKind, ServerCapabilities, TextEdit,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::handlers::hover::documentation_text;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{offset_at_position, position_at_offset, uri_to_file_path};

pub async fn on_completion(
    context: ServerContextSnapshot,
    params: CompletionParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<CompletionResponse> {
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let snippets = snippets_of(context.lsp_features().supports_snippets());

    // **Every request, whether or not anything goes wrong with it.** The report is "补全时有时无" — sometimes there,
    // sometimes not — and a log that records only the empty answers cannot tell "the client asked and got nothing"
    // from "the client never asked", which have nothing in common as fixes. This line and the one at the answer are
    // the pair that separates them.

    // Read in first, under the write lock, so the query itself can be a read — the same boundary every handler has.
    //
    // **Two steps, and the second is the one that makes the answer about the text the user sees.** `prepare` puts the
    // file in the table; `catch_up` makes its **summary** current, because an edit replaces the text *and* drops the
    // summary, and the summary is rebuilt later by the pump. A completion asked inside that window gets the new text
    // with the old facts: `full.` cannot find the type of `full`, the member query declines, and the client is shown
    // the names in scope — a list of things that cannot follow a `.`. Measured on a user's file: the popup after
    // `myName.firstName.` was `printf`, `full`, `sum`, `main` and the keywords.
    //
    // It is one parse of **one file**, and nothing at all when there is no edit waiting — see [`Session::catch_up`],
    // which also says why waiting for the pump would have been the wrong fix (the pump reads the file's whole
    // include closure, which for a file that includes `<string>` is thousands of headers).
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;

        let read = path.clone();
        context
            .analysis()
            .update_session(move |session| session.catch_up(&read))
            .await;

        // …and the module interface units this file imports, which the index cannot name a file for until something
        // reads them: `import std;` is `<VC>/Tools/MSVC/<version>/modules/std.ixx`, a file **outside the project**,
        // and until it is read the completion after `std::` is empty and `std::string` resolves to nothing. See
        // [`crate::handlers::read_the_modules`] for the measurement and for why the summary has to be current first.
        crate::handlers::read_the_modules(&context, &path, true).await;
    }

    // **A completion waits too, and for the same measured reason.**
    //
    // The list is built to degrade rather than lie — a name whose declaration has not been read is skipped — so an
    // index that is still filling gives fewer items and never wrong ones. Measured on a report: opening a file
    // showed a popup of **nothing but keywords** while hover worked, which is precisely what "fewer" looks like when
    // there is nothing to be less than. The list says `is_incomplete`, so a client does ask again — but only when
    // the user types, and the first look at a file deserves the same answer as the second.
    //
    // The budget is deliberately the same short one the hints use: a settled session pays one lock acquisition, and
    // a session that is still reading gives its answer after two seconds rather than never.
    context
        .analysis()
        .settle(Some(&cancel_token), std::time::Duration::from_millis(2000))
        .await;

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        // **The file's own tokens, and not the compiler's rendering.**
        //
        // A completion is about what a reader is **typing**, and that is the text in front of them. The rendering is
        // the parser's input, built for resolving macros and types — and it is built by *deleting the line breaks*,
        // so a file that reads as twenty lines becomes one. A cursor reader cannot survive that: measured on a live
        // server, a plain identifier inside a function was read as a **qualified name in scope `std`**, because with
        // everything on one line the recovery glued the `std::` of `std::string name;` to the `local_` being typed.
        // The answer was `std`'s two hundred members while the variables in the function and the file's own globals
        // were missing — the wrong scope, in a list that looks like a full one.
        //
        // **The offset and the view are taken together, from that one reading.** Resolving the position through
        // `view_and_offset_at` — which prefers the rendering — and then querying the file's own view is the exact
        // mixing this replaced, and it was, for one build: measured, the same client position resolved to offset 53
        // before an edit and 26 after it, because the first came from one reading and the answer from the other.
        let view = session.view_of_the_file(&path)?;
        let offset = offset_at_position(&view, position)?;

        let found = session.completions(&view, offset);

        log_a_member_that_produced_no_members(
            session,
            &view,
            offset,
            (position.line, position.character),
            &found.items,
        );

        Some(CompletionResponse::List(lsp_types::CompletionList {
            // See the module documentation: pending work means "a file may not have been read yet", a capped list
            // means "there are more names than fitted, and the next keystroke asks a narrower question".
            // **An empty list is never final**, and this is the third case — added after a report that named it
            // exactly: *"the first time I type `s` I get `std` and the rest; I delete it and type `s` again and
            // there is no completion at all"*. `isIncomplete: false` tells the client the list stands for the whole
            // prefix, so a final **empty** answer is cached as "nothing here" and the next keystroke that deserves
            // an answer is never asked. Every reason the list can be empty is temporary: the file's summary was just
            // dropped by an edit, a declaration it needs has not been read yet, the cursor is mid-word in a way this
            // layer cannot see. None of them is a fact about the prefix.
            is_incomplete: found.items.is_empty() || session.pending() > 0 || found.truncated,
            items: found
                .items
                .iter()
                .enumerate()
                .map(|(rank, item)| item_for(item, rank, &view, found.replace, snippets))
                .collect(),
        }))
    })
    .await
}

/// **Why a `.` produced no members** — the one diagnosis in this handler worth a log line.
///
/// A `.` whose answer is the names in scope is the shape a user reported as "补全明显错误": the popup is full of
/// things that cannot follow the operator, and nothing a client sees says whether the *type* could not be worked
/// out, the class could not be found, or the cursor was never read as a member access at all. Those three have
/// different fixes and the same symptom, so the reason is worth recording — but only for an answer with **no
/// members in it**, which is the one shape that needs explaining.
///
/// **What it reports, and why each part is here.** The protocol position the client sent *and* the byte it became,
/// because a completion is asked about a position rather than about a place in a file and the conversion between
/// the two is the first thing to check; the character the byte landed on, because that is what turns "character 8"
/// into "the `2` of `full2`" without anybody counting columns; the line itself; and the sentence
/// [`cpp_code_analysis::why_no_members`] produces, which comes from the same reading the list came from.
///
/// Measured on this feature: a report of a `.` that offered nothing was a cursor **one column** from the operator
/// the user was looking at, three separate times — and every one of them was invisible in a log that recorded only
/// the byte.
fn log_a_member_that_produced_no_members(
    session: &Session<DiskFiles>,
    view: &cpp_code_analysis::FileView,
    offset: usize,
    asked: (u32, u32),
    items: &[AnalysisItem],
) {
    let has_members = items
        .iter()
        .any(|item| matches!(item.kind, ItemKind::Method | ItemKind::Field));

    if has_members {
        return;
    }

    let line_body = {
        // The text of the line the offset is on, so that a failure report says *where* as well as *why*.
        let before = &view.source[..offset.min(view.source.len())];
        let start = before.rfind('\n').map_or(0, |at| at + 1);
        let end = view.source[start..]
            .find('\n')
            .map_or(view.source.len(), |at| start + at);
        view.source[start..end].to_string()
    };

    // **The character the offset landed on**, which is the half of the report the protocol's own numbers do not
    // give: a client sends a line and a character, the two become a byte, and a reader comparing "asked line 10
    // character 8" with a line of text has to count columns to know whether the two agree. Measured on a live
    // server: that arithmetic is where three separate reports of "the completion is wrong here" turned out to be
    // about a cursor one column away from the operator the user was looking at.
    let landed_on = view
        .source
        .get(offset..)
        .and_then(|rest| rest.chars().next())
        .map_or("the end of the file".to_string(), |found| {
            format!("{found:?}")
        });

    log::info!(
        "completion at {}:{offset} (asked line {} character {}, on {landed_on}) — the line is {line_body:?} — \
             the index holds {} files with {} still queued — has no members: {}",
        view.path.display(),
        asked.0,
        asked.1,
        session.index().len(),
        session.pending(),
        cpp_code_analysis::why_no_members(
            session.index(),
            &view.scopes,
            &view.root,
            &view.path,
            offset
        )
    );
}

/// Can the client interpolate a snippet body?
///
/// A parameter rather than a lookup inside [`completions`], because that function is the one a test calls without
/// a client: the capability belongs to the request, and the *rendering* of an item is a pure function of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snippets {
    Yes,
    No,
}

/// A client's answer, as the enum above.
fn snippets_of(supported: bool) -> Snippets {
    if supported { Snippets::Yes } else { Snippets::No }
}

/// One analysis item, as the protocol's item.
///
/// # Why `sortText` and not `sortText` alone
///
/// The rank is written as a zero-padded number, because `sortText` is a **string** and a client compares strings:
/// `"10"` sorts before `"9"`, which would put the tenth item above the ninth. Four digits is more than the budget
/// can produce and still reads as a number in a log.
fn item_for(
    item: &AnalysisItem,
    rank: usize,
    view: &cpp_code_analysis::FileView,
    replace: cpp_parser::SourceRange,
    snippets: Snippets,
) -> CompletionItem {
    let insert_as_a_snippet = item.snippet && snippets == Snippets::Yes;

    CompletionItem {
        label: item.label.clone(),
        // The kind and the detail are separate fields of the protocol on purpose, and 3.17 lets a client draw them
        // as one line — which is what the analysis's `detail` was written for.
        label_details: item
            .detail
            .as_ref()
            .map(|detail| CompletionItemLabelDetails {
                detail: Some(format!("  {detail}")),
                description: None,
            }),
        kind: Some(completion_kind(item.kind)),
        // **Where the declaration is**, for `completionItem/resolve` to find it again — see `identity_of`.
        data: item
            .identity
            .as_ref()
            .and_then(|(file, offset)| identity_of(file, *offset)),
        detail: item.detail.clone(),
        // The order the analysis produced, carried through as the client's own sort key.
        sort_text: Some(format!("{rank:04}_{}", item.label)),
        // Only when it differs from the label: for a qualified listing the client filters on `std::string` while
        // showing `string`, which is what makes the *ordering* and the *filtering* agree.
        filter_text: item.filter.clone(),
        insert_text_format: insert_as_a_snippet.then_some(InsertTextFormat::SNIPPET),
        text_edit: text_edit(view, replace, item, insert_as_a_snippet),
        ..CompletionItem::default()
    }
}

/// The edit that replaces one range with the chosen item, **if the range is a place in this file**.
///
/// `None` when it is not — a range the analysis built from a *rendering*, or a byte offset that is not on this
/// text's line index — which leaves the client to insert the label at the cursor. A handler that assumed the
/// mapping would be handing a client an offset into text nobody is looking at.
fn text_edit(
    view: &cpp_code_analysis::FileView,
    replace: cpp_parser::SourceRange,
    item: &AnalysisItem,
    insert_as_a_snippet: bool,
) -> Option<CompletionTextEdit> {
    let new_text = if insert_as_a_snippet {
        item.insert.as_str()
    } else {
        // A snippet the client cannot interpolate is inserted as its **label**: the alternative is placeholder
        // syntax in the file.
        item.label.as_str()
    };

    Some(CompletionTextEdit::Edit(TextEdit {
        range: lsp_types::Range::new(
            position_at_offset(view, replace.start_offset)?,
            position_at_offset(view, replace.end_offset())?,
        ),
        new_text: new_text.to_string(),
    }))
}

/// **Where a declaration is**, as the `data` a client echoes back to `completionItem/resolve`.
///
/// The item's own identity, not its rendering: the file and the offset of the declared **name**, which is what
/// every documentation question in this server is asked with (`Session::documentation`). Putting the comment itself
/// in the item would be answering a question the user has not asked yet — for every name in the list.
fn identity_of(file: &std::path::Path, offset: usize) -> Option<lsp_types::LSPAny> {
    Some(serde_json::json!({
        "file": file.to_str()?,
        "offset": offset,
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

    // An item with no identity — a keyword, a snippet, a header, or a client that strips `data` — is answered with
    // itself: the protocol says a resolve must not fail, and an item without documentation is what the client
    // already has.
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

/// The protocol's icon for an item kind.
///
/// The mapping is the analysis's vocabulary, not the protocol's — see [`ItemKind`] — and it lives here because
/// this is the only place that knows both. `EnumMember` and `Field` are separate kinds rather than both `FIELD`
/// because a client draws them differently, and a member *function* is a `METHOD` rather than a `FUNCTION` for
/// the same reason.
fn completion_kind(kind: ItemKind) -> CompletionItemKind {
    match kind {
        ItemKind::Variable => CompletionItemKind::VARIABLE,
        ItemKind::Function => CompletionItemKind::FUNCTION,
        ItemKind::Method => CompletionItemKind::METHOD,
        ItemKind::Field => CompletionItemKind::FIELD,
        ItemKind::Class => CompletionItemKind::CLASS,
        ItemKind::Enum => CompletionItemKind::ENUM,
        ItemKind::EnumMember => CompletionItemKind::ENUM_MEMBER,
        ItemKind::Namespace => CompletionItemKind::MODULE,
        ItemKind::Macro => CompletionItemKind::CONSTANT,
        ItemKind::TypeParameter => CompletionItemKind::TYPE_PARAMETER,
        ItemKind::Keyword => CompletionItemKind::KEYWORD,
        ItemKind::Snippet => CompletionItemKind::SNIPPET,
        ItemKind::Header => CompletionItemKind::FILE,
    }
}

/// What this handler tells a client it can do.
pub struct CompletionCapabilities;

impl RegisterCapabilities for CompletionCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.completion_provider = Some(CompletionOptions {
            // **The characters that make a client ask without the user typing a letter**, and the list is short on
            // purpose: a trigger character costs a request on every occurrence in every file, and the ones that are
            // not here are the ones that occur inside ordinary expressions.
            //
            // ```text
            // .    a member access            the request the feature exists for
            // >    the `>` of `->`            requested after `a->` and after `a > b`; the second is one filtered
            //                                 request whose answer is the members of the wrong thing — cheap, and the
            //                                 alternative is not completing `->` at all
            // :    the first `:` of `::`      the second `:` finds the list already asked for
            // #    a directive                what a `#include` / `#define` / `#if` is written with
            // <    a header name              `#include <` is the one place a `<` really does open a name
            // "    a header name, quoted      `#include "` likewise
            // ```
            //
            // **Not `/`**, which is the one that was tried and taken back out: `#include <sys/` is the case it was
            // for, and it fires inside every division and every comment in the file. The round trip a reader pays
            // for that is one more keystroke, and the round trip everybody else pays is a request per `/`.
            trigger_characters: Some(vec![
                ".".to_string(),
                ">".to_string(),
                ":".to_string(),
                "#".to_string(),
                "<".to_string(),
                "\"".to_string(),
            ]),
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
    use super::*;

    /// **What a resolve is asked with is the declaration's *place*, not its rendering**: the file and the offset
    /// of the name. That is the whole reason the round trip is cheap — the comment is fetched for the one item a
    /// client shows, rather than for every name in the list.
    #[test]
    fn an_item_carries_where_its_declaration_is() {
        let identity = identity_of(std::path::Path::new("/p/widget.h"), 120).expect("a path spells");

        assert_eq!(identity["file"], serde_json::json!("/p/widget.h"));
        assert_eq!(identity["offset"], serde_json::json!(120));
    }

    /// Every kind the analysis distinguishes has an icon, and the match is exhaustive on purpose: a kind added to
    /// [`ItemKind`] without a row here would not compile, where a `_` arm would quietly hand a client the wrong
    /// icon.
    #[test]
    fn every_item_kind_has_an_icon() {
        assert_eq!(completion_kind(ItemKind::Variable), CompletionItemKind::VARIABLE);
        assert_eq!(completion_kind(ItemKind::Function), CompletionItemKind::FUNCTION);
        assert_eq!(completion_kind(ItemKind::Method), CompletionItemKind::METHOD);
        assert_eq!(completion_kind(ItemKind::Field), CompletionItemKind::FIELD);
        assert_eq!(completion_kind(ItemKind::Class), CompletionItemKind::CLASS);
        assert_eq!(completion_kind(ItemKind::Enum), CompletionItemKind::ENUM);
        assert_eq!(completion_kind(ItemKind::EnumMember), CompletionItemKind::ENUM_MEMBER);
        assert_eq!(completion_kind(ItemKind::Namespace), CompletionItemKind::MODULE);
        assert_eq!(completion_kind(ItemKind::Macro), CompletionItemKind::CONSTANT);
        assert_eq!(completion_kind(ItemKind::TypeParameter), CompletionItemKind::TYPE_PARAMETER);
        assert_eq!(completion_kind(ItemKind::Keyword), CompletionItemKind::KEYWORD);
        assert_eq!(completion_kind(ItemKind::Snippet), CompletionItemKind::SNIPPET);
        assert_eq!(completion_kind(ItemKind::Header), CompletionItemKind::FILE);
    }
}
