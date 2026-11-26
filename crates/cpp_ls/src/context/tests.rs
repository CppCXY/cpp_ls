use std::{sync::Arc, time::Duration};

use lsp_server::{Connection, Message, Response};
use lsp_types::{
    ClientCapabilities, DidChangeTextDocumentParams, TextDocumentContentChangeEvent,
    VersionedTextDocumentIdentifier,
};

use super::{ClientId, RequestOutcome, ServerContext, UpdateEvent};

async fn response(connection: &Connection) -> Response {
    let receiver = connection.receiver.clone();
    let message =
        tokio::task::spawn_blocking(move || receiver.recv_timeout(Duration::from_secs(2)))
            .await
            .expect("the reader did not panic")
            .expect("a response arrived");
    match message {
        Message::Response(response) => response,
        other => panic!("expected a response, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_request_answers_without_waiting_for_its_work() {
    let (server, client) = Connection::memory();
    let context = ServerContext::new(
        Arc::new(server),
        ClientCapabilities::default(),
        ClientId::Other,
        None,
    );
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    context
        .task(1.into(), 0, false, None, move |_| async move {
            entered_tx.send(()).unwrap();
            std::future::pending::<RequestOutcome<u32>>().await
        })
        .await;
    entered_rx.await.unwrap();
    context.cancel(1.into()).await;
    let response = response(&client).await;
    assert_eq!(
        response.response_result.unwrap_err().code,
        lsp_server::ErrorCode::RequestCanceled as i32
    );
}

/// **A newer edit does not cancel a request that has already asked.** — the reason a completion is visible at all
/// while a reader types.
///
/// The previous version of this test asserted the opposite (`ContentModified`), and the rule it encoded cost the
/// feature: a client typing at 80 ms against a barrier that takes 300–500 ms had *every* request answered `-32801`,
/// so the completion popup never appeared (`docs/worklog.md`). The answer is still about text the client sent — the
/// barrier guarantees that — and a newer version of the same answer is the client's business, which is what
/// `$/cancelRequest` is for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_that_starts_does_not_cancel_a_request_that_has_asked() {
    let (server, client) = Connection::memory();
    let context = ServerContext::new(
        Arc::new(server),
        ClientCapabilities::default(),
        ClientId::Other,
        None,
    );
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    context
        .task(2.into(), 0, false, None, move |_| async move {
            entered_tx.send(()).unwrap();
            let _ = release_rx.await;
            RequestOutcome::Ready(7u32)
        })
        .await;
    entered_rx.await.unwrap();
    context
        .snapshot()
        .enqueue(UpdateEvent::Changed(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: "file:///a.cpp".parse().unwrap(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "int changed;".to_string(),
            }],
        }))
        .await;
    release_tx.send(()).unwrap();
    let response = response(&client).await;
    assert_eq!(
        response.response_result.expect("the request answers"),
        serde_json::json!(7)
    );
}
