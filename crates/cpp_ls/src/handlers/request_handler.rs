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
    CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
    CodeActionRequest, Completion, DocumentDiagnosticRequest, DocumentHighlightRequest,
    DocumentLinkRequest, DocumentSymbolRequest, FoldingRangeRequest, Formatting, GotoDefinition,
    RangeFormatting, WillSaveWaitUntil,
    GotoImplementation, GotoTypeDefinition, HoverRequest, InlayHintRequest, PrepareRenameRequest, References, Rename,
    Request as LspRequest, ResolveCompletionItem, SelectionRangeRequest, SemanticTokensFullRequest,
    SignatureHelpRequest, WorkspaceSymbolRequest,
};

use crate::context::ServerContext;

use super::{
    call_hierarchy::{
        on_incoming_calls, on_outgoing_calls, on_prepare_call_hierarchy,
    },
    code_action::on_code_action,
    completion::{on_completion, on_completion_resolve},
    definition::on_goto_definition_handler,
    diagnostic::on_pull_document_diagnostic,
    document_highlight::on_document_highlight,
    document_link::on_document_link,
    document_symbol::on_document_symbol,
    formatting::{on_formatting, on_range_formatting, on_will_save_wait_until},
    folding_range::on_folding_range,
    hover::on_hover,
    implementation::on_goto_implementation,
    type_definition::on_goto_type_definition,
    inlay_hint::on_inlay_hint,
    references::on_references,
    rename::{on_prepare_rename, on_rename},
    selection_range::on_selection_range,
    semantic_token::on_semantic_tokens,
    signature_help::on_signature_help,
    workspace_symbol::on_workspace_symbol,
};

macro_rules! dispatch_request {
    ($request:expr, $context:expr, {
        $($req_type:ty => $handler:expr),* $(,)?
    }) => {
        match $request.method.as_str() {
            $(
                <$req_type>::METHOD => {
                    // **A request whose params do not parse is still a request, and the protocol's answer is
                    // `InvalidParams`.** This arm used to fall out of the `if let` with nothing sent when `extract`
                    // failed — no response, no log, and a `dispatch: returned` that looked like success — which a client
                    // can only experience as a request that never got an answer.
                    //
                    // Measured: a `textDocument/prepareCallHierarchy` whose `position` object was nested one level too
                    // deep made the probe wait for ever, and every row after it too, while the server's log showed the
                    // request dispatched and nothing else. The id is cloned first because `extract` consumes the request.
                    let request_id = $request.id.clone();
                    match $request.extract::<<$req_type as LspRequest>::Params>(<$req_type>::METHOD) {
                        Ok((id, params)) => {
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
                        Err(error) => {
                            log::error!(
                                "the params of {} could not be read: {error}",
                                <$req_type>::METHOD
                            );
                            let response = Response::new_err(
                                request_id,
                                lsp_server::ErrorCode::InvalidParams as i32,
                                format!("{error}"),
                            );
                            $context.send(response);
                            return Ok(());
                        }
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
    // **Where the loop is**: a request that never becomes a response either never reaches the dispatcher (the loop is
    // busy with an earlier message) or never leaves it. One line at each end says which — the stall this was written
    // for turned out to be neither, but the two lines are what proved it.
    log::debug!("dispatch: {} entering", req.method);
    dispatch_request!(req, server_context, {
    // The first four are the ones the C++ analysis answers today: a location, a type, the names that can be
    // written at a cursor, and a file's diagnostics. The rest of the table is a one-line row each
    GotoDefinition => on_goto_definition_handler,
    GotoImplementation => on_goto_implementation,
    GotoTypeDefinition => on_goto_type_definition,
    HoverRequest => on_hover,
    CallHierarchyPrepare => on_prepare_call_hierarchy,
    CallHierarchyIncomingCalls => on_incoming_calls,
    CallHierarchyOutgoingCalls => on_outgoing_calls,
    CodeActionRequest => on_code_action,
    Completion => on_completion,
    ResolveCompletionItem => on_completion_resolve,
    DocumentHighlightRequest => on_document_highlight,
    DocumentLinkRequest => on_document_link,
    DocumentSymbolRequest => on_document_symbol,
    Formatting => on_formatting,
    RangeFormatting => on_range_formatting,
    WillSaveWaitUntil => on_will_save_wait_until,
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
    // **The one command this server offers.** A client invokes it to have the CMake configure that would write a
    // compile database run — see the module for why it is a command rather than something the server does on its
    // own, and why it is not shaped per client.
    });
    log::debug!("dispatch: returned");

    Ok(())
}