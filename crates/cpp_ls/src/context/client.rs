use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicI32},
};

use lsp_server::{Connection, Message, Notification, RequestId, Response};
use lsp_types::{
    ApplyWorkspaceEditParams, ApplyWorkspaceEditResponse, ConfigurationParams, MessageActionItem,
    RegistrationParams, ShowMessageParams, ShowMessageRequestParams, UnregistrationParams,
};
use serde::de::DeserializeOwned;
use tokio::{
    select,
    sync::{Mutex, oneshot},
};
use tokio_util::sync::CancellationToken;

pub struct ClientProxy {
    conn: Arc<Connection>,
    id_counter: AtomicI32,
    response_manager: Arc<Mutex<HashMap<RequestId, oneshot::Sender<Response>>>>,
}

#[allow(unused)]
impl ClientProxy {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self {
            conn,
            id_counter: AtomicI32::new(0),
            response_manager: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Unified outgoing message entry point.
    pub fn send_message(&self, message: Message) {
        let _ = self.conn.sender.send(message);
    }

    /// Send a Response to the client.
    pub fn send_response(&self, response: Response) {
        self.send_message(Message::Response(response));
    }

    pub fn send_notification(&self, method: &str, params: impl serde::Serialize) {
        self.send_message(Message::Notification(Notification {
            method: method.to_string(),
            params: serde_json::to_value(params).unwrap(),
        }));
    }

    pub async fn send_request(
        &self,
        id: RequestId,
        method: &str,
        params: impl serde::Serialize,
        cancel_token: CancellationToken,
    ) -> Option<Response> {
        let (sender, receiver) = oneshot::channel();
        self.response_manager
            .lock()
            .await
            .insert(id.clone(), sender);
        self.send_message(Message::Request(lsp_server::Request {
            id: id.clone(),
            method: method.to_string(),
            params: serde_json::to_value(params).unwrap(),
        }));
        let response = select! {
            response = receiver => response.ok(),
            _ = cancel_token.cancelled() => None,
        };
        self.response_manager.lock().await.remove(&id);
        response
    }

    fn send_request_no_wait(&self, id: RequestId, method: &str, params: impl serde::Serialize) {
        self.send_message(Message::Request(lsp_server::Request {
            id,
            method: method.to_string(),
            params: serde_json::to_value(params).unwrap(),
        }));
    }

    pub async fn on_response(&self, response: Response) -> Option<()> {
        let sender = self.response_manager.lock().await.remove(&response.id)?;
        let _ = sender.send(response);
        Some(())
    }

    pub fn next_id(&self) -> RequestId {
        let id = self
            .id_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        id.into()
    }

    pub async fn get_configuration<C>(
        &self,
        params: ConfigurationParams,
        cancel_token: CancellationToken,
    ) -> Option<Vec<C>>
    where
        C: DeserializeOwned,
    {
        let request_id = self.next_id();
        let response = self
            .send_request(request_id, "workspace/configuration", params, cancel_token)
            .await?;
        match response.response_result {
            Ok(r) => serde_json::from_value(r).ok(),
            Err(e) => {
                log::error!("get_configuration error: {:?}", e);
                None
            }
        }
    }

    pub fn dynamic_register_capability(&self, registration_param: RegistrationParams) {
        let request_id = self.next_id();
        self.send_request_no_wait(request_id, "client/registerCapability", registration_param);
    }

    pub fn dynamic_unregister_capability(&self, registration_param: UnregistrationParams) {
        let request_id = self.next_id();
        self.send_request_no_wait(
            request_id,
            "client/unregisterCapability",
            registration_param,
        );
    }

    pub fn show_message(&self, message: ShowMessageParams) {
        self.send_notification("window/showMessage", message);
    }

    pub async fn show_message_request(
        &self,
        params: ShowMessageRequestParams,
        cancel_token: CancellationToken,
    ) -> Option<MessageActionItem> {
        let request_id = self.next_id();
        let response = self
            .send_request(
                request_id,
                "window/showMessageRequest",
                params,
                cancel_token,
            )
            .await?;
        match response.response_result {
            Ok(r) => serde_json::from_value(r).ok(),
            Err(e) => {
                log::error!("show_message_request error: {:?}", e);
                None
            }
        }
    }

    /// **`textDocument/publishDiagnostics` is deliberately not here.**
    ///
    /// It was, and it is gone with the push path: diagnostics are **pulled** (`textDocument/diagnostic`), so the
    /// server never sends them unasked. A client that asks is answered from the index as it stands, which is what
    /// removes the whole class of defect the push path kept having — an answer about text the user has since
    /// replaced, arriving after the edit that replaced it.
    ///
    /// The one notification that remains is [`ClientProxy::refresh_workspace_diagnostics`], which is not an answer
    /// but a request for one.
    pub async fn apply_edit(
        &self,
        params: ApplyWorkspaceEditParams,
        cancel_token: CancellationToken,
    ) -> Option<ApplyWorkspaceEditResponse> {
        let request_id = self.next_id();
        let response = self
            .send_request(request_id, "workspace/applyEdit", params, cancel_token)
            .await?;
        match response.response_result {
            Ok(r) => serde_json::from_value(r).ok(),
            Err(e) => {
                log::error!("apply_edit error: {:?}", e);
                None
            }
        }
    }

    pub fn send_request_no_response(&self, method: &str, params: impl serde::Serialize) {
        let request_id = self.next_id();
        self.send_request_no_wait(request_id, method, params);
    }

    pub fn refresh_workspace_diagnostics(&self) {
        self.send_request_no_response("workspace/diagnostic/refresh", ());
    }

    /// Ask the client to ask again for the inlay hints it is showing.
    ///
    /// **The answer changes without the document changing**, which is the one case a client cannot notice by itself.
    /// A hint's parameter names come from the declaration the callee resolves to, and a callee in a file the index
    /// has not read yet resolves to nothing — so a request that arrives during indexing is answered with **fewer**
    /// hints, and the client has no reason to ask a second time: its document did not change, and inlay hints carry
    /// no "incomplete" flag the way completion does.
    ///
    /// Measured on a real report, this is what "the hints never come back after I type" was: an edit re-queues the
    /// file, the hint request the editor sends immediately afterwards lands inside that window, and a person typing
    /// is inside it at every keystroke. The window is ~48 ms and it is not the cost that matters — it is that
    /// **nothing ever told the client to look again**.
    pub fn refresh_inlay_hints(&self) {
        self.send_request_no_response("workspace/inlayHint/refresh", ());
    }

    /// Ask the client to ask again for the **semantic tokens** it is showing.
    ///
    /// The same reason as [`ClientProxy::refresh_inlay_hints`], for the answer a reader looks at constantly: a name
    /// is coloured from its declaration, a declaration in a file the index has not read yet gives no colour, and a
    /// client that asked once has no reason to ask again — its document did not change. Measured from an editor:
    /// `std::endl` was drawn as a variable at startup and as a function after any edit, because the first request
    /// was answered while the standard library's headers were still being read and nothing ever said "look again".
    pub fn refresh_semantic_tokens(&self) {
        self.send_request_no_response("workspace/semanticTokens/refresh", ());
    }
}
