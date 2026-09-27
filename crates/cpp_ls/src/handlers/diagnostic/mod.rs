//! # Diagnostics — what the analysis reported about one file
//!
//! The C++ analysis reports rather than fails: a file being typed at parses into a tree with errors attached, and
//! those errors **are** the diagnostics. There is no separate "check" pass, because the parse is the check this layer
//! has today — semantic diagnostics need the index to be complete, which is the same boundary [`Session::pending`]
//! marks, and carries that work.
//!
//! ```text
//! path ──> session.diagnostics(path) ──> FileDiagnostic { start, end, message }
//!                                             │
//!                the file's own text, or the cooked reading when the index holds one
//!                                             ▼
//!                 VfsFile's line index ──> Diagnostic { range, severity, message }
//! ```
//!
//! **Which reading answers is not this layer's decision** — it is [`Session::diagnostics`]'s, and the reason it has
//! one is the whole point of the cooked reading: the file's own text contains macros the parser can only guess at and
//! branches the preprocessor removes, so errors read there include ones a compiler never sees. When the index holds a
//! cooked reading, that is the text the compiler parses, and its errors are placed back in the file by the same map
//! every declaration's range goes through.
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
use crate::util::{path_to_uri, position_in_file};
use log::debug;

/// The diagnostics for one file, and the URI to publish them under.
///
/// `None` when the file cannot be read or its path is not a URI this server can spell: there is nothing to diagnose
/// and nothing to publish under, which is not the same as a file with no diagnostics (`Some(vec![])` — the answer
/// that *clears* a client's list, and the reason a close must come through here too).
///
/// # Which reading answers is the analysis's decision
///
/// [`Session::diagnostics`] picks it: the file's **cooked** reading when the index holds one — the text a compiler
/// parses, where a declaration written by a macro is not a guess and a branch nobody takes contributes nothing —
/// and the file's own text otherwise. This layer's job is the wire format: offsets become positions through the
/// VFS's line index (not through a parse of the file, which the layered answer no longer needs) and the message
/// becomes a [`Diagnostic`].
pub fn diagnose_file(session: &Session<DiskFiles>, path: &Path) -> Option<(Uri, Vec<Diagnostic>)> {
    let uri = path_to_uri(path)?;
    // Held by the VFS: this is the text — the buffer when the file is open, the disk otherwise — the offsets below
    // are offsets into, and the index that turns them into a line and a column.
    let held = session.files().held(path)?;

    let found = session.diagnostics(path)?;
    if found.unplaced > 0 {
        // **Said out loud rather than dropped.** A reading whose errors land in another file's macro bodies cannot
        // show them here, and a client that shows an empty list must not be read as "this file is clean" when the
        // parser had something to say and the answer is "not here".
        debug!(
            "{}: {} error(s) of the cooked reading are not written in this file and are not published",
            path.display(),
            found.unplaced
        );
    }

    let diagnostics = found
        .errors
        .iter()
        .map(|error| {
            // The parser's range, both ends placed by the same index — so a file with a hundred errors is a hundred
            // binary searches rather than a hundred scans of the file.
            let range = match (
                position_in_file(held, error.start),
                position_in_file(held, error.end),
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
                // **`true`, and it became true when the cooked reading did.** The answer for an open file is now
                // read off a rendering, and a rendering is made of the macros its translation unit's headers define:
                // editing one of them changes what this file compiles to, so a client that keeps a file's
                // diagnostics must re-ask when another file changes. Saying `false` would be claiming a locality the
                // answer no longer has — and the day it was `false` the claim was true, because the parse of one file
                // really was the whole answer.
                inter_file_dependencies: true,
                // Still not offered: workspace diagnostics would be this same per-file answer over every indexed
                // file, and the push path already runs that pass once the index settles (`DiagnosticService`).
                workspace_diagnostics: false,
                ..Default::default()
            }))
    }
}


