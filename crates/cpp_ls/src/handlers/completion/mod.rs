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

use crate::handlers;
use cpp_code_analysis::file::coordinates::ReadingOffset;
use cpp_code_analysis::{CompletionItem as AnalysisItem, DiskFiles, ItemKind, Session, detail_of};
use lsp_types::{
    ClientCapabilities, CompletionItem, CompletionItemKind, CompletionItemLabelDetails,
    CompletionOptions, CompletionParams, CompletionResponse, CompletionTextEdit, Documentation,
    InsertTextFormat, MarkupContent, MarkupKind, ServerCapabilities, TextEdit,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::handlers::hover::documentation_text;
use crate::util::{file_offset_at_position, position_at_offset, uri_to_file_path};

pub async fn on_completion(
    context: ServerContextSnapshot,
    params: CompletionParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<CompletionResponse> {
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let snippets = snippets_of(context.lsp_features().supports_snippets());
    let requested_path = uri_to_file_path(&uri);

    // **No request-side catch-up.** The update queue's writer owns the index impact of an edit — it calls
    // `Session::catch_up` and `cook` for the changed files before the barrier releases — and `didOpen` does the same
    // for a newly opened buffer. A completion that drained nine unread includes here would repeat that work on the
    // keystroke path, without a cancellation token, and could hold the session's write lock after the client has
    // already superseded the request. The first answer may be provisional; the writer and the pump make it current.

    // **The reading a semantic answer comes from.** The file's own view keeps the positions and the prefix; the
    // rendering owns bindings, types and members. Asking for the current buffer's rendering here is what makes a
    // macro-declared local (`kvec_t(int64_t) ids`) visible on the **first** completion after an edit, instead of
    // only when some other request happened to have left a rendering warm.
    // **No readiness check and no timeout.** The update queue applies an edit and its index impact under the
    // session's write lock; a read lock acquired here is therefore a lock acquired after that work, which is the
    // barrier. If a query needs a fact, the writer owed it before releasing.
    let from_the_rendering = match requested_path.as_ref() {
        Some(path) => {
            context
                .analysis()
                .completion_from_a_rendering(path, position)
                .await
        }
        None => None,
    };

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view_of_the_file(&path)?;
        let offset = file_offset_at_position(&view, position)?;

        // **Inside a format string the vocabulary is the grammar's, not the language's.**
        //
        // `printf("%5.` is followed by a conversion letter and by nothing else; a list of the names in scope there is
        // a list of things that cannot be written. So the format list replaces the ordinary one — *including when it
        // is empty*, which is why this answers `Some(empty)` rather than `None`: falling through would offer the very
        // list this branch exists to suppress.
        //
        // Read first, before the index is asked anything, because it needs no index: one token walk of the file.
        if let Some(held) = session.files().held(&view.path)
            && let Some(items) =
                handlers::format::complete::format_string_complete(session, &view, held, offset)
        {
            return Some(CompletionResponse::List(lsp_types::CompletionList {
                // **Complete**, unlike the language's own list: the grammar's vocabulary is closed, so a client may
                // filter it without asking again. An empty one says "nothing may go here", which is a fact and not a
                // temporary state.
                is_incomplete: false,
                items,
            }));
        }

        let found = from_the_rendering.unwrap_or_else(|| session.completions(&view, offset));
        log_a_member_that_produced_no_members(
            session,
            &view,
            offset.get(),
            (position.line, position.character),
            &found.items,
        );

        // **A list that cannot match what was typed is not a complete answer.** The flag is the protocol's one
        // way of saying "ask again"; clearing it unconditionally is what makes a client that trusts it show
        // **nothing** for every keystroke until some later answer happens to match the prefix, with the server idle
        // in between (reported from an editor: typing `k` in `channel_all_info` showed no list until `kvec`).
        //
        // Three reasons, one flag, and each is honest here:
        //
        // * `found.incomplete` — the analysis itself is provisional (the rendering of this text is not read yet, or
        //   a class it needs is not in the index). See `CompletionSet::incomplete`.
        // * **nothing in the list starts with the typed prefix** — the answer is not about this word. That happens
        //   when the list was built for a *different* reading of the file (a stale rendering, a cursor at the edge of
        //   a token), and a client that filters it shows an empty menu.
        // * **no prefix at all.** The answer to "what can be written here" is not the answer to "what word am I
        //   typing", and the difference matters because a client caches a *complete* answer and reuses it while the
        //   reader keeps typing: measured from the same report, an empty-prefix answer (`1000` names at a blank line)
        //   was cached by blink.cmp and **no request was sent at all** for `k`, `kv`, `kve` — the server's own log of
        //   that session holds one completion request, at `kvec`. Marked provisional, the empty-prefix answer is
        //   re-asked the moment a character is typed, which is exactly when its answer changes.
        //
        // `truncated` is deliberately **not** part of this: a capped list is re-asking for the same capped list,
        // which is the case `CompletionSet::truncated` documents.
        let answers_the_prefix = found.prefix.is_empty()
            || found.items.iter().any(|item| {
                item.label.starts_with(&found.prefix)
                    || item
                        .filter
                        .as_deref()
                        .is_some_and(|filter| filter.starts_with(&found.prefix))
            });
        let is_incomplete = found.incomplete || found.prefix.is_empty() || !answers_the_prefix;

        Some(CompletionResponse::List(lsp_types::CompletionList {
            // **Never ask the client to keep re-querying a list it can filter.** `isIncomplete: true` makes the
            // frontend treat the answer as a provisional page: it filters/re-requests instead of showing the names
            // it already has, and on a big file that is every keystroke. The server answers with the names it knows;
            // the next keystroke is a new request anyway — unless the answer is provisional or about another word,
            // which is what the computation above distinguishes.
            is_incomplete,
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

    log::debug!(
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
            // `offset` is the rendering's throughout this report — the lines above slice `view.source` with it — and
            // the query reads `view.root`, so it is the same number in the same numbering.
            ReadingOffset::new(offset),
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
    if supported {
        Snippets::Yes
    } else {
        Snippets::No
    }
}

/// One analysis item, as the protocol's item.
///
/// # The short type line
///
/// `labelDetails.detail` is the one field the protocol says a client may draw **beside the label** without resolving
/// the item. A member list is exactly where that matters: `capacity`, `items` and `size` are indistinguishable until
/// the reader knows two are counts and one is a pointer, and a reader looking at the popup has not selected any row to
/// resolve. The analysis has the one-line type already (`CompletionItem::detail`), so it goes in both places:
///
/// * `labelDetails.detail` for the list a client can draw immediately;
/// * `detail` for clients that read that field from the list response.
///
/// The **full declaration and documentation** stay behind `completionItem/resolve`: a parameter list per item is the
/// largest part of a several-hundred-item answer, and resolve is sent for the one item the user is looking at. The
/// distinction is the short type (one line, cheap) against the documentation (a parse, one item).
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
        kind: Some(completion_kind(item.kind)),
        // **The type line beside the name.** It is the one piece of information that tells two members apart in
        // the list — `items` is a pointer, `size` is a count — and it was computed by the analysis and then dropped
        // here: a member list rendered as three bare names. The handler sends it verbatim; a client that wants the
        // full declaration asks `completionItem/resolve`, which is a different question.
        detail: item.detail.clone(),
        label_details: item
            .detail
            .as_ref()
            .map(|detail| CompletionItemLabelDetails {
                detail: Some(format!("  {detail}")),
                description: None,
            }),
        // **Where the declaration is**, for `completionItem/resolve` to find it again — see `identity_of`. This is
        // what makes the deferral above possible: the detail is not sent, and the *identity* that produces it is.
        data: item
            .identity
            .as_ref()
            .and_then(|(file, offset)| identity_of(file, *offset)),
        // The order the analysis produced, carried through as the client's own sort key.
        sort_text: Some(format!("{rank:04}_{}", item.label)),
        // Only when it differs from the label: for a qualified listing the client filters on `std::string` while
        // showing `string`, which is what makes the *ordering* and the *filtering* agree.
        //
        // **Kept even though the server filters**, because this is what the client highlights against: the analysis
        // matched `std::string` while a reader typed `str`, and a client told only the label would bold the wrong
        // letters — or none.
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
        item.insert.clone()
    } else if item.insert.starts_with("->") {
        // **A rewritten operator is part of the insertion, not part of the placeholders.** The analysis answers
        // `->size` when the reader typed `.` on a pointer, and a client that cannot interpolate snippets still has to
        // get the operator: the alternative is `$0` in the file, which is why the label is normally what is inserted.
        format!("->{}", item.label)
    } else {
        // A snippet the client cannot interpolate is inserted as its **label**: the alternative is placeholder
        // syntax in the file.
        item.label.clone()
    };

    Some(CompletionTextEdit::Edit(TextEdit {
        range: lsp_types::Range::new(
            position_at_offset(view, ReadingOffset::new(replace.start_offset))?,
            position_at_offset(view, ReadingOffset::new(replace.end_offset()))?,
        ),
        new_text,
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

    // **Resolve is a request about one declaration, and that declaration's file may be exactly the one the pump
    // has not reached yet.** A completion list can be built from the buffer's own reading while the project index
    // is still filling; the item then has an identity whose summary is absent, `declaration_in` answers `None`, and
    // the item comes back unchanged for ever — the client has no reason to ask again. Prepare that one file's
    // summary and rendering first, the same way a diagnostic pull does; resolve is sent for one item, and its
    // contract is allowed to wait for that item's declaration.
    let _ = context.analysis().prepare_for_a_diagnostic(&file).await;

    let looked_up = file.clone();
    // **The label, for the lookup that has to match a declaration the identity offset only points near.**
    let label = item.label.clone();
    let resolved = snapshot_query(context.analysis(), cancel_token, move |session| {
        // **The detail the list did not send**, found from the identity the list did send. This is the whole reason
        // `item_for` leaves the two detail fields empty: a parameter list per item is the largest part of a
        // completion answer and a client draws a few dozen rows of it. See the note there.
        // **The fact, and the offset it is actually at.** An item built from a *binding* — a member of a class
        // declared in the file being edited — carries the declarator`s start as its identity (the `int` of
        // `int size;`), while the summary files the same declaration at its **name**, so an exact match on the
        // identity offset found nothing and the item came back without its documentation. Measured: `w.` on a
        // `Widget` in the edited file offered `size` with `offset: 20`, the fact is at 60, and resolve answered
        // unchanged. The label is what the client echoed, and it is what selects between two declarations of one
        // name; the identity offset says which of them, so the first at or after it is the one.
        let fact = declaration_in(session, &looked_up, offset, &label);
        let detail = fact.as_ref().and_then(detail_of);
        let at = fact
            .as_ref()
            .map(|fact| fact.name_range.start_offset)
            .unwrap_or(offset);

        // The view is of the **declaring** file, which is what makes the lookup below one walk for a declaration in
        // the file being edited and one parse for the first name that comes from a header.
        let documentation = session
            .view(&file)
            .and_then(|view| session.documentation(&view, &looked_up, at))
            .map(|comment| documentation_text(&comment));
        Some((detail, documentation.flatten()))
    })
    .await;

    if let RequestOutcome::Ready((detail, documentation)) = resolved {
        // **The resolve replaces the short type with the full declaration** — a function's return type and parameter
        // list, a class's bases — and adds its documentation. The list already carried the short line; this is the
        // one item the reader selected.
        item.label_details = detail.as_ref().map(|detail| CompletionItemLabelDetails {
            detail: Some(format!("  {detail}")),
            description: None,
        });
        item.detail = detail;

        if let Some(text) = documentation {
            item.documentation = Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: text,
            }));
        }
    }

    RequestOutcome::Ready(item)
}

/// **The declaration an item's identity names**, read back out of the file it was declared in.
///
/// `offset` is `DeclFact::name_range`'s start, which is what [`identity_of`] was given — so the fact is found by the
/// position of its own name and confirmed by the label the client echoed, because two declarations can share an
/// offset in a *recovered* parse and only one of them is the item the user is looking at.
///
/// `None` for an item that has no declaration behind it — a keyword, a snippet, a header — and the caller then
/// sends no detail, which is the item minus a hint rather than a wrong hint.
/// **The declaration an item`s identity names.**
///
/// `offset` is the identity the list sent — a **binding`s** declarator start for a member of a class declared in
/// the file being edited (`int size;` answers the `int`), and a fact`s name for one that came from the summary.
/// The two are the same declaration written at two offsets, so the exact match is tried first and then the first
/// fact of that **name** at or after it: the label is what the client echoed, and within one file it is what
/// selects between two declarations of one name.
fn declaration_in(
    session: &cpp_code_analysis::Session<DiskFiles>,
    file: &std::path::Path,
    offset: usize,
    label: &str,
) -> Option<cpp_code_analysis::DeclFact> {
    let summary = session.index().summary(file)?;
    if let Some(exact) = summary
        .declarations
        .iter()
        .find(|fact| fact.name_range.start_offset == offset)
    {
        return Some(exact.clone());
    }
    summary
        .declarations
        .iter()
        .filter(|fact| fact.name.as_str() == label && fact.name_range.start_offset >= offset)
        .min_by_key(|fact| fact.name_range.start_offset)
        .cloned()
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
        let identity =
            identity_of(std::path::Path::new("/p/widget.h"), 120).expect("a path spells");

        assert_eq!(identity["file"], serde_json::json!("/p/widget.h"));
        assert_eq!(identity["offset"], serde_json::json!(120));
    }

    /// Every kind the analysis distinguishes has an icon, and the match is exhaustive on purpose: a kind added to
    /// [`ItemKind`] without a row here would not compile, where a `_` arm would quietly hand a client the wrong
    /// icon.
    #[test]
    fn every_item_kind_has_an_icon() {
        assert_eq!(
            completion_kind(ItemKind::Variable),
            CompletionItemKind::VARIABLE
        );
        assert_eq!(
            completion_kind(ItemKind::Function),
            CompletionItemKind::FUNCTION
        );
        assert_eq!(
            completion_kind(ItemKind::Method),
            CompletionItemKind::METHOD
        );
        assert_eq!(completion_kind(ItemKind::Field), CompletionItemKind::FIELD);
        assert_eq!(completion_kind(ItemKind::Class), CompletionItemKind::CLASS);
        assert_eq!(completion_kind(ItemKind::Enum), CompletionItemKind::ENUM);
        assert_eq!(
            completion_kind(ItemKind::EnumMember),
            CompletionItemKind::ENUM_MEMBER
        );
        assert_eq!(
            completion_kind(ItemKind::Namespace),
            CompletionItemKind::MODULE
        );
        assert_eq!(
            completion_kind(ItemKind::Macro),
            CompletionItemKind::CONSTANT
        );
        assert_eq!(
            completion_kind(ItemKind::TypeParameter),
            CompletionItemKind::TYPE_PARAMETER
        );
        assert_eq!(
            completion_kind(ItemKind::Keyword),
            CompletionItemKind::KEYWORD
        );
        assert_eq!(
            completion_kind(ItemKind::Snippet),
            CompletionItemKind::SNIPPET
        );
        assert_eq!(completion_kind(ItemKind::Header), CompletionItemKind::FILE);
    }
}
