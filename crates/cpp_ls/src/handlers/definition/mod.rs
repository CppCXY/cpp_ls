//! # `textDocument/definition` — where the name at the cursor is declared
//!
//! The port template for a **pull** handler: resolve the position, ask the analysis session, turn `Known` into
//! `RequestOutcome`. Compare the Lua original (git history of this crate) — the shape is identical and only the
//! query changed, which is the whole point of the layer split in `docs/ls-architecture.md`.
//!
//! ```text
//! uri + position ──> path + offset ──> session.view(path) ──> session.definition(&view, offset)
//!                          │                                        │
//!                          │                                        └─ Known<ProjectDefinition> { file, fact }
//!                          └─ FileView::offset_at(line, column)         │
//!                                                                       └─ Yes → Location { uri, range }
//!                                                                          No / Unknown → Missing (null)
//! ```
//!
//! **`Known` is why this handler does not guess.** `No` means "there is no declaration here" and `Unknown`
//! means "the index cannot say yet" — both answer `null`, because a client that gets a wrong location is worse
//! off than one that gets none (`docs/index-design.md`, the same rule the analysis layer follows).

use cpp_code_analysis::Known;
use lsp_types::{
    ClientCapabilities, GotoDefinitionParams, GotoDefinitionResponse, Location, OneOf, Range,
    ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{path_to_uri, uri_to_file_path};

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
        let offset = view.offset_at(position.line as usize, position.character as usize)?;

        // TODO(port): `DeclFact` carries the declaration's own token range; until the analysis layer hands it out
        // as a range (or as offsets we can map back through the *declaring* file's text), the answer is the
        // declaring file with a zero-width range — honest, and obviously incomplete.
        match session.definition(&view, offset) {
            Known::Yes(found) => {
                let uri = path_to_uri(&found.file)?;
                Some(GotoDefinitionResponse::Scalar(Location {
                    uri,
                    range: Range::default(),
                }))
            }
            Known::No | Known::Unknown(_) => None,
        }
    })
    .await
}

pub struct DefinitionCapabilities;

impl RegisterCapabilities for DefinitionCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.definition_provider = Some(OneOf::Left(true));
    }
}
