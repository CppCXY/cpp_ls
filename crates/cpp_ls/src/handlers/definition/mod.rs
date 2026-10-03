//! # `textDocument/definition` — where the name at the cursor is declared
//!
//! The port template for a **pull** handler: resolve the position, ask the analysis session, turn `Known` into
//! `RequestOutcome`.
//!
//! ```text
//! uri + position ──> path + offset ──> session.view(path) ──> session.definitions(&view, offset)
//!                          │                                        │
//!                          │                                        └─ Known<ProjectDefinitions> { found, conditional }
//!                          └─ util::offset_at_position                    │
//!                                                                        └─ Yes → one location, or **all of them**
//!                                                                           No / Unknown → Missing (null)
//! ```
//!
//! **`Known` is why this handler does not guess.** `No` means "there is no such declaration" and `Unknown` means
//! "the index cannot say yet" — both answer `null`, because a client that jumps to a wrong location is worse off
//! than one that gets no location at all.
//!
//! # Why several locations, and when
//!
//! One name can cover several declarations, and the protocol has a shape for that: `Location[]` is what a client
//! shows as a peek list. `find` in `std::basic_string` is one **overload set** (seventeen declarations in MSVC's
//! library), `std::cin` is written twice in `<iostream>`, and neither is an ambiguity a reader wants to be told
//! about — they are the answer. The analysis side says which lists are worth sending
//! ([`cpp_code_analysis::ProjectIndex::definitions`]): one namespace collapses to one entry, because a namespace
//! is one entity however many files reopen it, and `std` alone has fifty-eight declarations in the index.
//!
//! A single declaration is still sent as `Scalar`, which is the shape every client has handled since before lists
//! existed — and the shape this handler sent before it could answer with more than one.
//!
//! # The range is the name, in the *declaring* file
//!
//! [`DeclFact::name_range`] is the fact's own offsets, so the range is computed by reading the declaring file
//! through the session (the buffer when it is open — a declaration in an unsaved buffer is at the offsets the user
//! can see) and mapping them onto a line and column. When that file cannot be read at all, the answer is still the
//! file, with a zero-width range at its start: a jump that lands at the top of the right file is worth having, and
//! the position of a name the analysis cannot see is not something this layer can invent.
//!
//! # The fourth answer: an `#include` points at a file
//!
//! `#include <vector>` **declares nothing** — no scope declares it, the index has no fact by that name, and the
//! macro table has no entry for it — so every question above answers "nothing found" for a line whose whole purpose
//! is to name a file. It is asked first ([`cpp_code_analysis::Session::header_at`]) because the shape at the cursor
//! decides which question applies, and there is no position at which both can be true.

use cpp_code_analysis::{DiskFiles, Known, Session};
use lsp_types::{
    ClientCapabilities, GotoDefinitionParams, GotoDefinitionResponse, Location, OneOf, Range,
    ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{offset_at_position, path_to_uri, position_in_file, uri_to_file_path};

pub async fn on_goto_definition_handler(
    context: ServerContextSnapshot,
    params: GotoDefinitionParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<GotoDefinitionResponse> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    // The file is read in first, under the write lock, so that the query below can be a read: a file the client has
    // open is already held by `didOpen`, and this covers the request that arrives about one that is not.
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;

        // **The module interface units this file imports.** A name that arrives through `import std;` is declared in
        // a file the project never scanned — `<VC>/Tools/MSVC/<version>/modules/std.ixx` is outside it — so a
        // go-to-definition on `std::string` in a file that says `import std;` has nothing to point at until that file
        // is read. `false`: this handler has no edit to catch up on, so the read does it itself.
        crate::handlers::read_the_modules(&context, &path, false).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let offset = offset_at_position(&view, position)?;

        // **A header name before a declaration**, because the two cannot both be here and only one of them can
        // answer: `#include <vector>` declares nothing, so the declaration question below would answer "nothing
        // found" for a name that has a perfectly good file behind it. Asked first rather than as a fallback for
        // the same reason a member query is asked before a name query — the *shape* at the cursor decides which
        // question applies, and the shape here is a directive.
        //
        // **Asked of the file's own text, not of the rendering.** A rendering has the directives *resolved*: the
        // `#include` line is gone from it, because what it stood for is written out in its place. So the one
        // position a reader most obviously wants to jump from — the header they wrote — is not in the reading the
        // rest of this handler works in, and asking there finds nothing.
        if let Some(written) = session.view_of_the_file(&path) {
            if let Some(in_the_file) = offset_at_position(&written, position) {
                if let Known::Yes(header) = session.header_at(&written, in_the_file) {
                    // A header this session cannot hold — the file is on disk but the VFS refused it — answers
                    // `None` rather than a location with no range, which is the same "nothing to go to" the reader
                    // had before.
                    return location_of_a_file(session, &header).map(GotoDefinitionResponse::Scalar);
                }
            }
        }

        let Known::Yes(found) = session.definitions(&view, offset) else {
            return None;
        };

        let mut locations: Vec<Location> = Vec::new();
        for declaration in &found.found {
            let Some(uri) = path_to_uri(&declaration.file) else {
                continue;
            };
            let range = name_range(session, &declaration.file, &declaration.fact).unwrap_or_default();
            locations.push(Location { uri, range });
        }

        match locations.len() {
            0 => None,
            1 => Some(GotoDefinitionResponse::Scalar(locations.remove(0))),
            _ => Some(GotoDefinitionResponse::Array(locations)),
        }
    })
    .await
}

/// **A jump to a file rather than to a name in it** — what `#include <vector>` points at.
///
/// The range is the **whole first line** rather than a point at the top of the file, and the difference is what a
/// client does with it: an editor that receives a scalar location *selects* the range, and a zero-width range at
/// offset zero selects nothing and scrolls to a column that may not be where anything is. The file itself is the
/// answer — a header has nothing to highlight — so the range says "here, from the beginning" and no more.
///
/// `None` when the resolved path cannot be spelled as a URI or is not in the VFS, which leaves the client with the
/// same "no definition" it had before rather than with a URI it cannot open.
fn location_of_a_file(
    session: &Session<DiskFiles>,
    header: &cpp_code_analysis::HeaderTarget,
) -> Option<Location> {
    let uri = path_to_uri(&header.resolved)?;
    let file = session.files().held(&header.resolved)?;

    let end_of_the_first_line = file
        .text
        .find('\n')
        .map_or_else(|| file.text.len(), |newline| newline + 1);
    let end = position_in_file(file, end_of_the_first_line)?;
    Some(Location {
        uri,
        range: Range::new(lsp_types::Position::new(0, 0), end),
    })
}

/// The declaration's name, as a range in the file that declares it.
fn name_range(
    session: &cpp_code_analysis::Session<cpp_code_analysis::DiskFiles>,
    file: &std::path::Path,
    fact: &cpp_code_analysis::DeclFact,
) -> Option<Range> {
    // The declaring file, from the VFS: its text and **its line index**, which is the pair the entry was made with.
    // No parse — a name's line and column do not need a tree — and no second read of a header this session is
    // already holding.
    let declaring = session.files().held(file)?;

    Some(Range::new(
        position_in_file(declaring, fact.name_range.start_offset)?,
        position_in_file(declaring, fact.name_range.end_offset())?,
    ))
}

pub struct DefinitionCapabilities;

impl RegisterCapabilities for DefinitionCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.definition_provider = Some(OneOf::Left(true));
    }
}



