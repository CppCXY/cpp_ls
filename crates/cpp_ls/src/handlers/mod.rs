//! # LSP handlers — one module per capability
//!
//! Every module here has the same two jobs:
//!
//! ```text
//! 1. a handler function with the signature the dispatch table calls:
//!      request:      async fn(ServerContextSnapshot, Params, CancellationToken) -> RequestOutcome<Result>
//!      notification: async fn(ServerContextSnapshot, Params) -> Option<()>
//! 2. a `RegisterCapabilities` implementation, which is what the server advertises in `initialize`
//! ```
//!
//! Both are wired in two places and nowhere else: [`request_handler`] / [`notification_handler`] for the
//! dispatch, and the `capabilities!` table at the bottom of this file for what the client is told. A module
//! that is in one and not the other is a handler the client will never call (or a capability with no
//! implementation), so **the two lists are read together**
//!
//! The table below is deliberately short: it is the set of capabilities this server answers *today*. Adding
//! one is three edits — the module, the dispatch row, the capability row — and the modules that were removed
//! from the Lua skeleton (`semantic_token`, `signature_help`, …) come back the same way: `completion`,
//! `document_symbol`, `folding_range`, `references`, `rename`, `selection_range`, `workspace_symbol` and
//! `inlay_hint` all came back that way.

mod completion;
mod configuration;
mod definition;
mod diagnostic;
mod document_symbol;
mod folding_range;
mod hover;
mod inlay_hint;
mod initialized;
mod notification_handler;
mod references;
mod rename;
mod selection_range;
mod signature_help;
mod semantic_token;
mod request_handler;
mod response_handler;
mod text_document;
mod workspace_symbol;

pub use diagnostic::diagnose_file;

pub use initialized::{ClientConfig, initialized_handler, start_analysis};
use lsp_types::{ClientCapabilities, ServerCapabilities};
pub use notification_handler::on_notification_handler;
pub use request_handler::on_request_handler;
pub use response_handler::on_response_handler;
pub use text_document::register_files_watch;
pub use text_document::{
    process_did_change_text_document, process_did_change_watched_files, process_did_close_document,
    process_did_open_text_document, process_did_save_text_document,
};

/// What a module has to answer to be advertised in `initialize`.
///
/// One trait rather than a match over method names, because the *decision* belongs to the module that knows
/// what it can do, and the client's own capabilities are half of that decision — a client that cannot show a
/// popup at all is one a hover provider does not have to be announced to, and the flag that says so is read by
/// the module that would have to honour it rather than by a table here.
pub trait RegisterCapabilities {
    fn register_capabilities(
        server_capabilities: &mut ServerCapabilities,
        client_capabilities: &ClientCapabilities,
    );
}

macro_rules! capabilities {
    // module name => capability type mapping
    (modules: {
        $($module:ident => $capability:ident),* $(,)?
    }) => {
        pub fn server_capabilities(client_capabilities: &ClientCapabilities) -> ServerCapabilities {
            let mut server_capabilities = ServerCapabilities::default();

            $(
                $module::$capability::register_capabilities(&mut server_capabilities, client_capabilities);
            )*

            server_capabilities
        }
    };
}

capabilities!(modules: {
    // The document lifecycle is not optional: without it no request ever has a file to answer about.
    text_document => TextDocumentCapabilities,
    // The capabilities the C++ analysis can already answer, or is being built to answer first.
    definition => DefinitionCapabilities,
    hover => HoverCapabilities,
    completion => CompletionCapabilities,
    document_symbol => DocumentSymbolCapabilities,
    folding_range => FoldingRangeCapabilities,
    inlay_hint => InlayHintCapabilities,
    references => ReferencesCapabilities,
    rename => RenameCapabilities,
    selection_range => SelectionRangeCapabilities,
signature_help => SignatureHelpCapabilities,
semantic_token => SemanticTokenCapabilities,
    workspace_symbol => WorkspaceSymbolCapabilities,
    diagnostic => DiagnosticCapabilities,
    configuration => ConfigurationCapabilities,
});
