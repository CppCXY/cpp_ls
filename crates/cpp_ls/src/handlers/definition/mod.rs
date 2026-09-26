//! # `textDocument/definition` — where the name at the cursor is declared
//!
//! The port template for a **pull** handler: resolve the position, ask the analysis session, turn `Known` into
//! `RequestOutcome`.
//!
//! ```text
//! uri + position ──> path + offset ──> session.view(path) ──> session.definition(&view, offset)
//!                          │                                        │
//!                          │                                        └─ Known<ProjectDefinition> { file, fact }
//!                          └─ util::offset_at_position                    │
//!                                                                        └─ Yes → Location { uri, range of the name }
//!                                                                           No / Unknown → Missing (null)
//! ```
//!
//! **`Known` is why this handler does not guess.** `No` means "there is no such declaration" and `Unknown` means
//! "the index cannot say yet" — both answer `null`, because a client that jumps to a wrong location is worse off
//! than one that gets no location at all (`docs/index-design.md`, the same rule the analysis layer follows).
//!
//! # The range is the name, in the *declaring* file
//!
//! [`DeclFact::name_range`] is the fact's own offsets, so the range is computed by reading the declaring file
//! through the session (the buffer when it is open — a declaration in an unsaved buffer is at the offsets the user
//! can see) and mapping them onto a line and column. When that file cannot be read at all, the answer is still the
//! file, with a zero-width range at its start: a jump that lands at the top of the right file is worth having, and
//! the position of a name the analysis cannot see is not something this layer can invent.

use cpp_code_analysis::Known;
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

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let offset = offset_at_position(&view, position)?;

        let Known::Yes(found) = session.definition(&view, offset) else {
            return None;
        };

        let uri = path_to_uri(&found.file)?;
        let range = name_range(session, &found.file, &found.fact).unwrap_or_default();

        Some(GotoDefinitionResponse::Scalar(Location { uri, range }))
    })
    .await
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
    let declaring = session.files().file(file)?;

    Some(Range::new(
        position_in_file(&declaring, fact.name_range.start_offset)?,
        position_in_file(&declaring, fact.name_range.end_offset())?,
    ))
}

pub struct DefinitionCapabilities;

impl RegisterCapabilities for DefinitionCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.definition_provider = Some(OneOf::Left(true));
    }
}

