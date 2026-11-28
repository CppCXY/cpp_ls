//! Pull diagnostics for clients that request `textDocument/diagnostic`.

use lsp_types::{
    Diagnostic, DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    FullDocumentDiagnosticReport, RelatedFullDocumentDiagnosticReport,
};
use tokio_util::sync::CancellationToken;

use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
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
    let for_the_query = path.clone();
    let retry_token = cancel_token.clone();
    // **The reading first, before the question.** `prepare_for_a_diagnostic` is free when the file is read, cooked
    // and has a view — three lookups under a `try_read` — and it is what makes the answer the *compiler's* reading
    // rather than the raw parse. It used to be called only when the index had no summary at all, and that missed the
    // window a pull actually arrives in: `didOpen` reads the file and the pump cooks it a moment later, so a pull
    // between the two found a summary, answered from the raw text, and reported `expected ';' after expression` for
    // a file that parses. Measured: `handshake::diagnostics::the_reported_file_is_answered` failed one full run in
    // three on exactly that diagnostic, and passed when it ran alone.
    let _ = context.analysis().prepare_for_a_diagnostic(&path).await;
    let mut outcome = snapshot_query(context.analysis(), cancel_token, move |session| {
        super::diagnose_file(session, &for_the_query).map(|(_, items)| items)
    })
    .await;
    if matches!(outcome, RequestOutcome::Missing) {
        if !context.analysis().prepare_for_a_diagnostic(&path).await {
            return RequestOutcome::Failed {
                code: lsp_server::ErrorCode::RequestFailed,
                message: format!("diagnostics could not be read for {}", path.display()),
            };
        }
        let retry_path = path.clone();
        outcome = snapshot_query(context.analysis(), retry_token, move |session| {
            super::diagnose_file(session, &retry_path).map(|(_, items)| items)
        })
        .await;
    }

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
