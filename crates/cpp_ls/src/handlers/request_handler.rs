//! # Request dispatch — method name to handler
//!
//! One macro and one table. Every row is a **type** from `lsp_types::request` plus the handler's name, and the
//! macro does the four things each request needs in the same order:
//!
//! ```text
//! 1. match the method string against <Req>::METHOD      (the client's spelling, not a string literal here)
//! 2. extract::<Req::Params>()                           (a params type that does not fit is not an error the
//!                                                        client sees: the row is skipped and the request falls
//!                                                        through to "handler not found")
//! 3. context.snapshot()                                 (a handle to the services, cloned per request)
//! 4. context.task(id, |cancel_token| handler(…))         (spawned, so the message loop keeps reading; the
//!                                                        token is what `$/cancelRequest` cancels)
//! ```
//!
//! **A handler never touches the connection.** It returns `RequestOutcome<T>` and `ServerContext::task` turns
//! that into the response — `Ready` → result, `Missing` → `null`, `Cancelled` → the cancel error, a panic →
//! an internal error. That is why every row below looks the same and why adding one is a one-line edit.

use std::error::Error;

use log::error;
use lsp_server::{Request, Response};
use lsp_types::request::{
    DocumentDiagnosticRequest, GotoDefinition, HoverRequest, Request as LspRequest,
};

use crate::context::ServerContext;

use super::{
    definition::on_goto_definition_handler,
    diagnostic::on_pull_document_diagnostic,
    hover::on_hover,
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
                        $context.task(id.clone(), |cancel_token| async move {
                            $handler(snapshot, params, cancel_token).await
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
        // The first three are the ones the C++ analysis answers today: a location, a type, and a file's
        // diagnostics. The rest of the table is a one-line row each — see `docs/ls-architecture.md`.
        GotoDefinition => on_goto_definition_handler,
        HoverRequest => on_hover,
        DocumentDiagnosticRequest => on_pull_document_diagnostic,
    });

    Ok(())
}
