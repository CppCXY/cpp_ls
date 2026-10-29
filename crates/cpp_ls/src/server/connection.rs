use lsp_server::{Connection, Message, Response};
use std::error::Error;
use std::sync::Arc;
use tokio::sync::mpsc;

/// **How long after the shutdown answer the `exit` notification is waited for.** Long enough for a client that sends
/// it immediately, short enough that a client that never sends one does not leave the process waiting.
const EXIT_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Async wrapper for LSP Connection with tokio support
pub struct AsyncConnection {
    pub(crate) connection: Arc<Connection>,
    receiver: mpsc::UnboundedReceiver<Message>,
    _receiver_task: std::thread::JoinHandle<()>,
}

impl AsyncConnection {
    /// Create async version from sync Connection
    pub fn from_sync(connection: Connection) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let connection = Arc::new(connection);

        // **A plain thread, not `spawn_blocking`.** This loop blocks on stdin, and a blocking *task* is a task the
        // runtime waits for when it is dropped: the process answered `shutdown`, printed its goodbye, and then stayed
        // alive until the client closed the pipe — measured, and the whole of one report. A thread that is simply
        // detached ends with the process, which is the answer `shutdown` asks for.
        let connection_clone = connection.clone();
        let receiver_task = std::thread::Builder::new()
            .name("cpp_ls-reader".to_string())
            .spawn(move || {
                for msg in &connection_clone.receiver {
                    if tx.send(msg).is_err() {
                        break; // Receiver closed
                    }
                }
            })
            .expect("the reader thread starts");

        Self {
            connection,
            receiver: rx,
            _receiver_task: receiver_task,
        }
    }

    /// Receive message asynchronously
    pub async fn recv(&mut self) -> Option<Message> {
        self.receiver.recv().await
    }

    /// Send message to client
    pub fn send(&self, msg: Message) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.connection
            .sender
            .send(msg)
            .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)
    }

    /// Handle shutdown request
    ///
    /// **`shutdown` is answered, and the process ends.** The protocol asks for an `exit` notification afterwards, and
    /// a client that sends it gets to see the answer first — but a client that only sends `shutdown` is a client that
    /// expects the process to end, and waiting for a notification that may never come is a server an editor has to
    /// kill. Measured: the wait used to be thirty seconds, and a client that stops after `shutdown` left the process
    /// alive for all of it.
    ///
    /// The wait is bounded rather than skipped because the answer has to reach the client: `exit` normally arrives
    /// within milliseconds of the response, and a longer wait only delays the ending.
    pub async fn handle_shutdown(
        &mut self,
        req: &lsp_server::Request,
    ) -> Result<bool, Box<dyn Error + Send + Sync>> {
        if req.method != "shutdown" {
            return Ok(false);
        }
        let resp = Response::new_ok(req.id.clone(), ());
        let _ = self.connection.sender.send(resp.into());

        match tokio::time::timeout(EXIT_WAIT, self.receiver.recv()).await {
            Ok(Some(Message::Notification(n))) if n.method == "exit" => {}
            // Anything else after `shutdown` is a client that does not follow the protocol; ending is still better
            // than waiting for a message that will not come.
            Ok(Some(msg)) => {
                log::debug!("after shutdown, before exit: {msg:?}");
            }
            Ok(None) => {
                log::debug!("the client closed the connection after shutdown");
            }
            Err(_) => {
                log::debug!("no exit notification within {EXIT_WAIT:?}; ending anyway");
            }
        }
        Ok(true)
    }
}
