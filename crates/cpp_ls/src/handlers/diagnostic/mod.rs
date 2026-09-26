//! # Diagnostics — what the parser reported about one file
//!
//! The C++ analysis reports rather than fails: a file being typed at parses into a tree with errors attached
//! (`FileView::errors`), and those errors **are** the diagnostics. There is no separate "check" pass, because the
//! parse is the check this layer has today — semantic diagnostics need the index to be complete, which is the same
//! boundary [`Session::pending`] marks, and `docs/roadmap.md` carries that work.
//!
//! ```text
//! path ──> session.view(path) ──> view.errors() ──> Diagnostic { range, severity, message }
//! ```
//!
//! Two clients, one conversion: the push path ([`crate::context::DiagnosticService`], which publishes after an
//! edit settles) and the pull path (`textDocument/diagnostic`) both call [`diagnose_file`]. That is deliberate —
//! a server whose two diagnostic paths disagree is worse than one with only one of them.
mod document_diagnostic;

use std::path::Path;

use cpp_code_analysis::{DiskFiles, Session};
use lsp_types::{
    ClientCapabilities, Diagnostic, DiagnosticOptions, DiagnosticServerCapabilities,
    DiagnosticSeverity, ServerCapabilities, Uri,
};

use super::RegisterCapabilities;
pub use document_diagnostic::on_pull_document_diagnostic;
use crate::util::{path_to_uri, position_at_offset};

/// The diagnostics for one file, and the URI to publish them under.
///
/// `None` when the file cannot be read or its path is not a URI this server can spell: there is nothing to diagnose
/// and nothing to publish under, which is not the same as a file with no diagnostics (`Some(vec![])` — the answer
/// that *clears* a client's list, and the reason a close must come through here too).
pub fn diagnose_file(session: &Session<DiskFiles>, path: &Path) -> Option<(Uri, Vec<Diagnostic>)> {
    let view = session.view(path)?;
    let uri = path_to_uri(path)?;

    let diagnostics = view
        .errors()
        .iter()
        .map(|error| {
            let (start, end) = error.offsets();
            // The view's own line index — the one the VFS built for this text — so a file with a hundred errors is
            // a hundred binary searches rather than a hundred scans of the file.
            let range = match (
                position_at_offset(&view, start),
                position_at_offset(&view, end),
            ) {
                (Some(start), Some(end)) => lsp_types::Range::new(start, end),
                // An offset the text does not contain: point at the top of the file rather than dropping the
                // error. The message is the part a user needs, and losing it would hide a real parse failure
                // because of a range this layer could not map.
                _ => lsp_types::Range::default(),
            };

            Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("cpp_ls".to_string()),
                message: error.message.clone(),
                ..Diagnostic::default()
            }
        })
        .collect();

    Some((uri, diagnostics))
}

pub struct DiagnosticCapabilities;

impl RegisterCapabilities for DiagnosticCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.diagnostic_provider =
            Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
                identifier: Some("cpp_ls".to_string()),
                // The parse of one file is the whole answer, so nothing here depends on another file being read —
                // and claiming otherwise would invite a client to re-request diagnostics on every edit anywhere.
                inter_file_dependencies: false,
                // Which is also why workspace diagnostics are not offered yet: they would be this same parse over
                // every indexed file, with no cross-file finding to add. See `docs/roadmap.md`.
                workspace_diagnostics: false,
                ..Default::default()
            }))
    }
}


