use lsp_server::{ErrorCode, Message, Notification, RequestId, Response};
use lsp_types::{CancelParams, NumberOrString};
use std::error::Error;
use tokio::sync::oneshot;

use crate::context;
use crate::handlers::{on_notification_handler, on_request_handler, on_response_handler};

use super::connection::AsyncConnection;

/// Server initialization and message processing state
pub(super) struct ServerMessageProcessor {
    pub(super) pending_messages: Vec<Message>,
    pub(super) init_rx: oneshot::Receiver<()>,
}

impl ServerMessageProcessor {
    pub(super) fn new(init_rx: oneshot::Receiver<()>) -> Self {
        Self {
            pending_messages: Vec::new(),
            init_rx,
        }
    }

    /// Check if message can be processed during initialization
    pub(super) fn can_process_during_init(&self, msg: &Message) -> bool {
        match msg {
            // Allow all responses (including configuration responses)
            Message::Response(_) => true,
            // Allow specific notifications
            Message::Notification(notify) => {
                matches!(notify.method.as_str(), "$/cancelRequest" | "initialized")
            }
            // Don't process other requests during initialization
            Message::Request(_) => false,
        }
    }

    /// Process message during normal operation (after initialization)
    pub(super) async fn process_message(
        &mut self,
        msg: Message,
        connection: &mut AsyncConnection,
        server_context: &mut context::ServerContext,
    ) -> Result<bool, Box<dyn Error + Sync + Send>> {
        // During normal operation, process all messages
        self.handle_message(msg, connection, server_context).await
    }

    pub(super) fn cancel_pending_request(
        &mut self,
        notification: &Notification,
    ) -> Option<Response> {
        if notification.method != "$/cancelRequest" {
            return None;
        }
        let params: CancelParams = match serde_json::from_value(notification.params.clone()) {
            Ok(params) => params,
            Err(error) => {
                log::warn!("invalid cancellation parameters during initialization: {error}");
                return None;
            }
        };
        let id: RequestId = match params.id {
            NumberOrString::Number(id) => id.into(),
            NumberOrString::String(id) => id.into(),
        };
        let at = self
            .pending_messages
            .iter()
            .position(|message| matches!(message, Message::Request(request) if request.id == id))?;
        self.pending_messages.remove(at);
        Some(Response::new_err(
            id,
            ErrorCode::RequestCanceled as i32,
            "cancel".to_string(),
        ))
    }

    /// Process all pending messages after initialization
    pub(super) async fn process_pending_messages(
        &mut self,
        connection: &mut AsyncConnection,
        server_context: &mut context::ServerContext,
    ) -> Result<bool, Box<dyn Error + Sync + Send>> {
        let messages = std::mem::take(&mut self.pending_messages);
        for msg in messages {
            if self.handle_message(msg, connection, server_context).await? {
                return Ok(true); // Shutdown requested
            }
        }
        Ok(false)
    }

    /// Handle individual message
    pub(super) async fn handle_message(
        &self,
        msg: Message,
        connection: &mut AsyncConnection,
        server_context: &mut context::ServerContext,
    ) -> Result<bool, Box<dyn Error + Sync + Send>> {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req).await? {
                    server_context.close().await;
                    return Ok(true); // Shutdown requested
                }
                on_request_handler(req, server_context).await?;
            }
            Message::Notification(notify) => {
                on_notification_handler(notify, server_context).await?;
            }
            Message::Response(response) => {
                on_response_handler(response, server_context).await?;
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
#[path = "message_processor_tests.rs"]
mod tests;
