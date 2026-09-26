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
//! implementation), so **the two lists are read together** — see `docs/ls-architecture.md`.
//!
//! The table below is deliberately short: it is the set of capabilities this server answers *today*. Adding
//! one is three edits — the module, the dispatch row, the capability row — and the modules that were removed
//! from the Lua skeleton (`completion`, `semantic_token`, `rename`, …) come back the same way.

mod configuration;
mod definition;
mod diagnostic;
mod hover;
mod initialized;
mod notification_handler;
mod request_handler;
mod response_handler;
mod text_document;

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
/// what it can do, and the client's own capabilities are half of that decision — `inlayHint.dynamicRegistration`
/// decides whether a hint provider is registered statically or by a later request.
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
    diagnostic => DiagnosticCapabilities,
    configuration => ConfigurationCapabilities,
});
