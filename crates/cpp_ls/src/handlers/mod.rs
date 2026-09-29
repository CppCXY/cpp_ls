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
use crate::context::ServerContextSnapshot;
use lsp_types::{ClientCapabilities, ServerCapabilities};
pub use notification_handler::on_notification_handler;
pub use request_handler::on_request_handler;
pub use response_handler::on_response_handler;
pub use text_document::register_files_watch;
pub use text_document::{
    process_did_change_text_document, process_did_change_watched_files, process_did_close_document,
    process_did_open_text_document, process_did_save_text_document,
};

/// **The module interface units this file imports, read in** — the half of "read the file in first" that a project
/// scan cannot do.
///
/// `import mathlib;` reaches a file because the project indexed it. `import std;` (C++23) does not: MSVC ships the
/// source of `std` as `<VC>/Tools/MSVC/<version>/modules/std.ixx`, **outside the project**, so until something reads
/// that file there is no summary of it anywhere and every `std::` name in a file that imports it answers nothing —
/// silently, which is the failure mode this server keeps having to remove. [`Analysis::prepare`] handles the other
/// half (the file the request is about); this is the half that lives outside the workspace.
///
/// # When it is called, and why the order matters
///
/// **After** the file's summary is current — `prepare` is not enough, because an edit drops a file's summary and the
/// pump rebuilds it later, and a file with no summary has no imports to read. A handler that has an edit waiting calls
/// [`Session::catch_up`] first for exactly that reason, and passing `caught_up` says so rather than leaving a reader
/// to work out whether this call belongs before or after it.
///
/// # What it costs
///
/// Measured on the module fixture: **6 151 ms the first time on a machine** (401 files — the whole standard library,
/// which is what `std.ixx` includes) and **547.7 ms** once the summaries are on disk. Asking a second time reads
/// nothing in and takes **0.004 ms**, so the cost is per session-machine pair rather than per keystroke. A handler
/// that answers a name query is the right place to pay it and the pump is not: nobody should pay for the standard
/// library who is not asking a question about it.
///
/// [`Analysis::prepare`]: crate::context::AnalysisState::prepare
/// [`Session::catch_up`]: cpp_code_analysis::Session::catch_up
pub async fn read_the_modules(context: &ServerContextSnapshot, path: &std::path::Path, caught_up: bool) {
    let read = path.to_path_buf();

    context
        .analysis()
        .update_session(move |session| {
            if !caught_up {
                // The caller's summary may still be an edit behind — `catch_up` is what makes the *reading* current,
                // and an import that is not in the reading cannot be read in. One parse of one file, and nothing at
                // all when there is no edit waiting.
                session.catch_up(&read);
            }

            session.read_the_modules_a_file_imports(&read)
        })
        .await;
}

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
