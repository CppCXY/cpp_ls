use super::*;

#[test]
fn cancellation_removes_an_initialization_queued_request() {
    let (_sender, receiver) = oneshot::channel();
    let mut processor = ServerMessageProcessor::new(receiver);
    processor
        .pending_messages
        .push(Message::Request(lsp_server::Request {
            id: 17.into(),
            method: "textDocument/hover".to_string(),
            params: serde_json::Value::Null,
        }));
    let cancel = Notification {
        method: "$/cancelRequest".to_string(),
        params: serde_json::json!({ "id": 17 }),
    };

    let response = processor
        .cancel_pending_request(&cancel)
        .expect("pending request");
    assert_eq!(response.id, 17.into());
    assert_eq!(
        response.response_result.unwrap_err().code,
        ErrorCode::RequestCanceled as i32
    );
    assert!(processor.pending_messages.is_empty());
}
