//! `textDocument/diagnostic` — the **pull** half of diagnostics.
//!
//! A client that supports pull diagnostics asks about a file after an edit; the push half
//! ([`crate::context::DiagnosticService`]) publishes without being asked, for clients that do not. Both call
//! [`super::diagnose_file`], so the two can differ in *when* they answer and not in *what*.
//!
//! # Two defects this handler had, and why they are one question
//!
//! **It returned `null`.** A `RequestOutcome::Missing` becomes a JSON `null` response, and a client that asked for a
//! `DocumentDiagnosticReport` drops it — so the diagnostics it already had stayed on screen, which is exactly
//! "输入代码之后诊断不改变". The answer to "I could not read this file" is an **empty full report**, which is a
//! valid `DocumentDiagnosticReport` and the only thing a client can act on.
//!
//! **It asked before the file was ready.** [`AnalysisState::prepare`] *queues* a file for cooking and returns; the
//! query then ran with no reading and produced the `Missing` above. Pull diagnostics are answered on demand, so the
//! work has to happen **here**, before the query — that is [`AnalysisState::prepare_for_a_diagnostic`].
//!
//! Both are the same mistake from two sides: treating "not ready" as "nothing to say". A client pulls precisely
//! because it wants an answer *now*, and one that has to be asked again is one that will not be.
//!
//! The work still goes through [`analysis_query`], which is what that mechanism is for: a client re-requests the
//! same file as the user keeps typing, and an answer already being computed for the same key is **replaced** rather
//! than duplicated (the older task is cancelled and the newer one's answer is the only one sent).

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

    // **Read and cook it first**, under the write lock, so the query below has a reading to answer from. Without
    // this the query answers `Missing`, and `Missing` reaches the client as `null`.
    let Some(path) = uri_to_file_path(&uri) else {
        return RequestOutcome::Ready(an_empty_report());
    };
    context
        .analysis()
        .prepare_for_a_diagnostic(&path)
        .await;

    let for_the_query = path.clone();
    let outcome = analysis_query(
        context.analysis(),
        context.request_manager(),
        &cache_key,
        Some(cancel_token),
        move |session| Some(super::diagnose_file(session, &for_the_query)),
    )
    .await;

    outcome.map(|found| {
        // `None` from `diagnose_file` is "this file cannot be read", and it is answered with an empty report rather
        // than with the `Missing` that would become a JSON `null` — see this module's note.
        let items = found.map(|(_, diagnostics)| diagnostics).unwrap_or_default();
        DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: None,
                items,
            },
        })
        .into()
    })
}

/// A `DocumentDiagnosticReport` saying nothing is wrong, which is a valid answer to a pull request.
///
/// A **full** report rather than `Unchanged`: `result_id` is always `None` here, because this server does not keep a
/// per-document result to diff against — and claiming "unchanged" without one is a claim it cannot support.
fn an_empty_report() -> DocumentDiagnosticReportResult {
    DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
        related_documents: None,
        full_document_diagnostic_report: FullDocumentDiagnosticReport {
            result_id: None,
            items: Vec::new(),
        },
    })
    .into()
}
