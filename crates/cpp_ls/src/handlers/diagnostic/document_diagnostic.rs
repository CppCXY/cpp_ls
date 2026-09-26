//! `textDocument/diagnostic` — the **pull** half of diagnostics.
//!
//! A client that supports pull diagnostics asks about a file after an edit; the push half
//! ([`crate::context::DiagnosticService`]) publishes without being asked, for clients that do not. Both call
//! [`super::diagnose_file`], so the two can differ in *when* they answer and not in *what*.
//!
//! The work goes through [`analysis_query`], which is what that mechanism is for: a client re-requests the same
//! file as the user keeps typing, and an answer that is already being computed for the same key is **replaced**
//! rather than duplicated (the older task is cancelled and the newer one's answer is the only one sent). The key is
//! the file, and the cancellation is the client's own `$/cancelRequest` — the two reasons the pull cache exists.

use lsp_types::{
    DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    FullDocumentDiagnosticReport, RelatedFullDocumentDiagnosticReport,
};
use tokio_util::sync::CancellationToken;

use crate::context::{RequestOutcome, ServerContextSnapshot, analysis_query};
use crate::util::uri_to_file_path;

pub async fn on_pull_document_diagnostic(
    context: ServerContextSnapshot,
    params: DocumentDiagnosticParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<DocumentDiagnosticReportResult> {
    let uri = params.text_document.uri;
    let cache_key = format!("diagnostic:{}", uri.as_str());

    analysis_query(
        context.analysis(),
        context.request_manager(),
        &cache_key,
        Some(cancel_token),
        move |session| {
            let path = uri_to_file_path(&uri)?;
            let (_, diagnostics) = super::diagnose_file(session, &path)?;
            Some(diagnostics)
        },
    )
    .await
    .map(|diagnostics| {
        DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: None,
                items: diagnostics,
            },
        })
        .into()
    })
}
