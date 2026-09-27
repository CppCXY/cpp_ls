//! # Request dispatch — method name to handler
//!
//! One macro and one table. Every row is a **type** from `lsp_types::request` plus the handler's name, and the
//! macro does the five things each request needs in the same order:
//!
//! ```text
//! 1. match the method string against <Req>::METHOD      (the client's spelling, not a string literal here)
//! 2. extract::<Req::Params>()                           (a params type that does not fit is not an error the
//!                                                        client sees: the row is skipped and the request falls
//!                                                        through to "handler not found")
//! 3. context.snapshot()                                 (a handle to the services, cloned per request)
//! 4. remember how many notifications the client has sent (the bound step 5 applies up to)
//! 5. context.task(id, |cancel_token| handler(…))         (spawned, so the message loop keeps reading; the
//!                                                        token is what `$/cancelRequest` cancels)
//! ```
//!
//! **A handler never touches the connection.** It returns `RequestOutcome<T>` and `ServerContext::task` turns
//! that into the response — `Ready` → result, `Missing` → `null`, `Cancelled` → the cancel error, a panic →
//! an internal error. That is why every row below looks the same and why adding one is a one-line edit.
//!
//! # Why a request applies the notification queue before its handler runs
//!
//! Because the queue is invisible to the *client*, and it must be invisible to the request too. The handlers only
//! enqueue (so the loop reading from the client never waits for a parse), which left one hole: a `didChange`
//! followed immediately by a completion was served **before** the change had been applied, so the analysis held
//! older text, the cursor's position fell past the end of that text's line, and the answer was `null` — or worse,
//! a completion of the wrong kind. Measured on this server: a burst of 24 changes followed by one completion
//! answered with the file's global names instead of the object's members.
//!
//! So step 4 reads [`crate::context::UpdateInbox::queued`] on the message loop's own task — where the order is the
//! client's — and the spawned task applies the queue up to that number before handing the snapshot to the handler.
//! Everything the client sent before the request is applied; a change that arrives after it is not, which is what
//! the client's own `$/cancelRequest` is about. See [`crate::context::update_queue`] for the whole argument.

use std::error::Error;

use log::error;
use lsp_server::{Request, Response};
use lsp_types::request::{
    Completion, DocumentDiagnosticRequest, DocumentSymbolRequest, FoldingRangeRequest,
    GotoDefinition, HoverRequest, InlayHintRequest, PrepareRenameRequest, References, Rename,
    Request as LspRequest, ResolveCompletionItem, SelectionRangeRequest,
    SemanticTokensFullRequest,
    SignatureHelpRequest,
    WorkspaceSymbolRequest,
};

use crate::context::ServerContext;

use super::{
    completion::{on_completion, on_completion_resolve},
    definition::on_goto_definition_handler,
    diagnostic::on_pull_document_diagnostic,
    document_symbol::on_document_symbol,
    folding_range::on_folding_range,
    hover::on_hover,
    inlay_hint::on_inlay_hint,
    references::on_references,
    selection_range::on_selection_range,
    semantic_token::on_semantic_tokens,
    signature_help::on_signature_help,
    rename::{on_prepare_rename, on_rename},
    workspace_symbol::on_workspace_symbol,
};

macro_rules! dispatch_request {
    ($request:expr, $context:expr, {
        $($req_type:ty => $handler:expr),* $(,)?
    }) => {
        match $request.method.as_str() {
            $(
                <$req_type>::METHOD => {
                    if let Ok((id, params)) = $request.extract::<<$req_type as LspRequest>::Params>(<$req_type>::METHOD) {
                        let snapshot = $context.snapshot();
                        // **How much of the client's stream this request is entitled to see.** Read here, on the
                        // task that has just read the messages in order, and applied inside the request's own task.
                        let upto = snapshot.inbox().queued();
                        $context.task(id.clone(), move |cancel_token| {
                            let snapshot = snapshot.clone();
                            async move {
                                crate::context::catch_up_upto(&snapshot, upto).await;
                                $handler(snapshot, params, cancel_token).await
                            }
                        }).await;
                        return Ok(());
                    }
                }
            )*
            method => {
                error!("handler not found for request: {}", method);
                let response = Response::new_err(
                    $request.id.clone(),
                    lsp_server::ErrorCode::MethodNotFound as i32,
                    "handler not found".to_string(),
                );
                $context.send(response);
            }
        }
    };
}

pub async fn on_request_handler(
    req: Request,
    server_context: &mut ServerContext,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    dispatch_request!(req, server_context, {
        // The first four are the ones the C++ analysis answers today: a location, a type, the names that can be
        // written at a cursor, and a file's diagnostics. The rest of the table is a one-line row each
        GotoDefinition => on_goto_definition_handler,
        HoverRequest => on_hover,
        Completion => on_completion,
        ResolveCompletionItem => on_completion_resolve,
        DocumentSymbolRequest => on_document_symbol,
        FoldingRangeRequest => on_folding_range,
        InlayHintRequest => on_inlay_hint,
        References => on_references,
        SelectionRangeRequest => on_selection_range,
        SemanticTokensFullRequest => on_semantic_tokens,
        SignatureHelpRequest => on_signature_help,
        PrepareRenameRequest => on_prepare_rename,
        Rename => on_rename,
        WorkspaceSymbolRequest => on_workspace_symbol,
        DocumentDiagnosticRequest => on_pull_document_diagnostic,
    });

    Ok(())
}
