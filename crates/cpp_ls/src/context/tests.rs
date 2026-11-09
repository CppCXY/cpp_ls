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
    );
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    context
        .task(1.into(), 0, move |_| async move {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_is_not_answered_from_a_later_document_version() {
    let (server, client) = Connection::memory();
    let context = ServerContext::new(
        Arc::new(server),
        ClientCapabilities::default(),
        ClientId::Other,
    );
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    context
        .task(2.into(), 0, move |_| async move {
            entered_tx.send(()).unwrap();
            std::future::pending::<RequestOutcome<u32>>().await
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
    let response = response(&client).await;
    assert_eq!(
        response.response_result.unwrap_err().code,
        lsp_server::ErrorCode::ContentModified as i32
    );
}
