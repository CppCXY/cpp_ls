use std::error::Error;

use log::warn;
use lsp_server::Notification;
use lsp_types::{
    CancelParams, DidChangeConfigurationParams, NumberOrString,
    notification::{
        Cancel, DidChangeConfiguration, DidChangeTextDocument, DidChangeWatchedFiles,
        DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
        Notification as LspNotification, SetTrace,
    },
};

use crate::context::ServerContext;
use crate::context::UpdateEvent;

use super::{
    configuration,
    text_document::{
        on_did_change_text_document, on_did_change_watched_files, on_did_close_document,
        on_did_open_text_document, on_did_save_text_document, on_set_trace,
    },
};

/// **Read a notification's params, or say why not.**
///
/// The dispatcher used to drop a malformed one silently (`if let Ok(params)` with no `else`), so a client bug — or a
/// field this server reads and the client does not send — left the analysis a document behind with nothing in the log to
/// say why (`docs/worklog.md` P2-6).
///
/// It takes the notification **by value** because `Notification::extract` consumes it, and it reads the URI out of the
/// raw params *before* extracting, which is the only place it exists once extraction has failed. The params themselves
/// are never logged: a `didChange` carries whole buffers.
fn or_report<P: serde::de::DeserializeOwned>(notification: lsp_server::Notification) -> Option<P> {
    let method = notification.method.clone();
    let uri = notification
        .params
        .get("textDocument")
        .and_then(|document| document.get("uri"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<no textDocument.uri>")
        .to_string();

    match notification.extract::<P>(&method) {
        Ok(params) => Some(params),
        Err(error) => {
            warn!("dropping a malformed {method} notification for {uri}: {error}");
            None
        }
    }
}

macro_rules! dispatch_notification {
    ($notification:expr, $context:expr, {
        sync: { $($sync_notif:ty => $sync_handler:expr),* $(,)? }
        async: { $($async_notif:ty => $async_handler:expr),* $(,)? }
    }) => {
        match $notification.method.as_str() {
            Cancel::METHOD => {
                if let Some(params) = or_report::<CancelParams>($notification) {
                    handle_cancel($context, params).await;
                }
            }
            $(
                <$sync_notif>::METHOD => {
                    if let Some(params) =
                        or_report::<<$sync_notif as LspNotification>::Params>($notification)
                    {
                        let snapshot = $context.snapshot();
                        // Catch panics locally so a handler cannot kill the main loop.
                        let _ = crate::util::catch_unwind($sync_handler(snapshot, params)).await;
                    }
                }
            )*
            $(
                <$async_notif>::METHOD => {
                    if let Some(params) =
                        or_report::<<$async_notif as LspNotification>::Params>($notification)
                    {
                        let snapshot = $context.snapshot();
                        let _ = crate::util::catch_unwind($async_handler(snapshot, params)).await;
                    }
                }
            )*
            method => {
                warn!("Unhandled notification method: {}", method);
            }
        }
    };
}

pub async fn on_notification_handler(
    notification: Notification,
    server_context: &mut ServerContext,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    if notification.method == DidChangeConfiguration::METHOD {
        if let Ok(params) =
            notification.extract::<DidChangeConfigurationParams>(DidChangeConfiguration::METHOD)
        {
            log::debug!(
                "configuration changed: {}",
                serde_json::to_string(&params).unwrap_or_else(|_| "<unserializable>".to_string())
            );
            // **Read here, apply in the queue.** The read is a server→client round trip and the queue worker must never
            // await the client — it holds the apply lock a request's barrier waits on, so the two would wait for each
            // other. The **application** is an update event, which is what gives a following request the barrier and an
            // in-flight one `ContentModified` (`docs/worklog.md` P1-2).
            let snapshot = server_context.snapshot();
            if let Some(config) = configuration::the_clients_configuration(&snapshot).await {
                snapshot.enqueue(UpdateEvent::Configuration(config)).await;
            }
        }
        return Ok(());
    }

    dispatch_notification!(notification, server_context, {
        sync: {}
        async: {
            DidChangeTextDocument => on_did_change_text_document,
            DidOpenTextDocument => on_did_open_text_document,
            DidSaveTextDocument => on_did_save_text_document,
            DidCloseTextDocument => on_did_close_document,
            DidChangeWatchedFiles => on_did_change_watched_files,
            SetTrace => on_set_trace,
        }
    });

    Ok(())
}

async fn handle_cancel(server_context: &mut ServerContext, params: CancelParams) {
    let req_id = match params.id {
        NumberOrString::Number(i) => i.into(),
        NumberOrString::String(s) => s.into(),
    };

    server_context.cancel(req_id).await;
}
