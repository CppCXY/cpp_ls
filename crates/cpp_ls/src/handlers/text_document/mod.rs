mod register_file_watch;
mod set_trace;
mod text_document_handler;
mod watched_file_handler;

use lsp_types::{
    ClientCapabilities, SaveOptions, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncSaveOptions,
};
pub use register_file_watch::register_files_watch;
pub use set_trace::on_set_trace;
pub use text_document_handler::{
    on_did_change_text_document, on_did_close_document, on_did_open_text_document,
    on_did_save_text_document, process_did_change_text_document, process_did_close_document,
    process_did_open_text_document, process_did_save_text_document,
};
pub use watched_file_handler::{
    on_did_change_watched_files, process_did_change_watched_files, process_reconcile,
};

use super::RegisterCapabilities;

pub struct TextDocumentCapabilities;

impl RegisterCapabilities for TextDocumentCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.text_document_sync = Some(TextDocumentSyncCapability::Options(
            lsp_types::TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                will_save: None,
                // **Advertised because it is implemented.** 	extDocument/willSaveWaitUntil is dispatched to the
                // formatter (ormatting::on_will_save_wait_until), and with None here no compliant client ever sent
                // it: a save-time format-on-save setting could not work, and the comment in the formatter module claimed
                // the opposite of the wire (docs/worklog.md P2-1).
                will_save_wait_until: Some(true),
                save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                    include_text: Some(false),
                })),
            },
        ));
    }
}
