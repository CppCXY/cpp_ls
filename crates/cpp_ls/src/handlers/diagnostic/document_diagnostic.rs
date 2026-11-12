//! Pull diagnostics for clients that request `textDocument/diagnostic`.

use lsp_types::{
    Diagnostic, DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    FullDocumentDiagnosticReport, RelatedFullDocumentDiagnosticReport,
};
use tokio_util::sync::CancellationToken;

use crate::context::{CancelSource, RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::uri_to_file_path;

pub async fn on_pull_document_diagnostic(
    context: ServerContextSnapshot,
    params: DocumentDiagnosticParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<DocumentDiagnosticReportResult> {
    let uri = params.text_document.uri;
    let Some(path) = uri_to_file_path(&uri) else {
        return RequestOutcome::Failed {
            code: lsp_server::ErrorCode::InvalidParams,
            message: format!("diagnostics require a file URI: {}", uri.as_str()),
        };
    };
    let prepared = tokio::select! {
        _ = cancel_token.cancelled() => return RequestOutcome::Cancelled(CancelSource::Client),
        prepared = context.analysis().prepare_for_a_diagnostic(&path) => prepared,
    };
    if !prepared {
        return RequestOutcome::Failed {
            code: lsp_server::ErrorCode::RequestFailed,
            message: format!("diagnostics are not ready for {}", path.display()),
        };
    }

    let for_the_query = path.clone();
    let outcome = snapshot_query(context.analysis(), cancel_token, move |session| {
        let answer = super::diagnose_file(session, &for_the_query).map(|(_, items)| items);
        // **A pull is where a session's tables are biggest**, so it is where the breakdown is worth printing: the
        // report is the session's own accounting, not the resident set, and the two differ by the allocator's slack —
        // which is the number a "memory keeps growing" report is about. `CPPLS_TRACE_MEMORY=1` in the server's
        // environment.
        if std::env::var_os("CPPLS_TRACE_MEMORY").is_some() {
            eprintln!("[memory] pull: {}", session.memory_report());
        }
        answer
    })
    .await;

    match outcome {
        RequestOutcome::Ready(items) => {
            log::debug!("pull: {} -> {} item(s)", path.display(), items.len());
            RequestOutcome::Ready(full_report(items))
        }
        RequestOutcome::Missing => RequestOutcome::Failed {
            code: lsp_server::ErrorCode::RequestFailed,
            message: format!("diagnostics could not be read for {}", path.display()),
        },
        RequestOutcome::Cancelled(source) => RequestOutcome::Cancelled(source),
        RequestOutcome::Failed { code, message } => RequestOutcome::Failed { code, message },
    }
}

fn full_report(items: Vec<Diagnostic>) -> DocumentDiagnosticReportResult {
    DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
        related_documents: None,
        full_document_diagnostic_report: FullDocumentDiagnosticReport {
            result_id: None,
            items,
        },
    })
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_report_contains_exactly_the_current_findings() {
        let current = vec![Diagnostic {
            message: "a real finding".to_string(),
            ..Diagnostic::default()
        }];
        let report = full_report(current.clone());
        let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(report)) = report
        else {
            panic!("expected a full report");
        };
        assert_eq!(report.full_document_diagnostic_report.items, current);
    }
}
