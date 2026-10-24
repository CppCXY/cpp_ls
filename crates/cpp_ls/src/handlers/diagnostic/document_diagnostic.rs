//! `textDocument/diagnostic` — **the diagnostics channel**, because there is no other one.
//!
//! A client asks about a file, and [`super::diagnose_file`] answers from the index as it stands at that moment. There
//! was a push half beside this — a service that published `textDocument/publishDiagnostics` after an edit settled, for
//! clients that do not pull — and it is deleted. One answer with two delivery schedules is how a screen ends up
//! holding a diagnostic about text that is gone; a pulled answer is computed when it is asked for, so it cannot be
//! overtaken by the edit that follows it.
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
    context.analysis().prepare_for_a_diagnostic(&path).await;

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
        // **What this pull can answer, and what it says when it cannot.**
        //
        // `diagnose_file` answers `None` for a file the analysis has no cooked reading for — which is the state right
        // after every edit, until the pump cooks the file. Turning that into an **empty report** tells the client
        // "this file is clean", it clears the squiggles the user is looking at, and the refresh that follows the cook
        // makes them come back: that is the flash, and it is a claim the server cannot support.
        //
        // So a pull that cannot answer repeats the last report it gave for that file (`AnalysisState::last_reports`)
        // and only a *real* answer replaces it. The visible effect is the one a user expects: the squiggles stay
        // until the analysis has something better to say, and then they change once.
        let found = found.map(|(_, items)| items);
        let (items, to_remember) = the_report(found, context.analysis().last_report(&path));
        if let Some(items) = to_remember {
            context
                .analysis()
                .remember_report(path.clone(), std::sync::Arc::new(items));
        }
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

/// **The one rule this handler applies**: an answer from the analysis wins and is remembered; no answer repeats the
/// last one; a file never answered is empty.
///
/// Split out to be testable as a rule rather than as a protocol exchange — the three cases the test below covers are
/// the whole content of "diagnostics do not flash", and the middle one is the case that made them flash. The second
/// return value is what the caller should remember: `None` when the answer came from the memory, because repeating
/// it must not refresh it.
fn the_report(
    found: Option<Vec<lsp_types::Diagnostic>>,
    last: Option<std::sync::Arc<Vec<lsp_types::Diagnostic>>>,
) -> (Vec<lsp_types::Diagnostic>, Option<Vec<lsp_types::Diagnostic>>) {
    match found {
        Some(items) => (items.clone(), Some(items)),
        None => (
            last.map(|held| (*held).clone()).unwrap_or_default(),
            None,
        ),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **The middle case is the one that made diagnostics flash**: no answer from the analysis, and a report the
    /// client is already showing. Answering `[]` there claims the file became clean, and the refresh that follows
    /// the cook takes that claim back a moment later.
    #[test]
    fn no_answer_repeats_the_last_report_instead_of_claiming_the_file_is_clean() {
        let one = lsp_types::Diagnostic::default();
        let last = std::sync::Arc::new(vec![one]);
        let found_nothing: Option<Vec<lsp_types::Diagnostic>> = None;

        // 1. Nothing to say, but something was said before: repeat it, and do not overwrite it.
        let (sent, remember) = the_report(None, Some(std::sync::Arc::clone(&last)));
        assert_eq!(sent.len(), 1, "a pull with no answer must not say the file is clean");
        assert!(remember.is_none(), "repeating a report must not refresh it");

        // 2. A real answer — including a clean one — wins and becomes what is repeated afterwards.
        let (sent, remember) = the_report(Some(Vec::new()), Some(last));
        assert!(sent.is_empty(), "an answer from the analysis is the answer");
        assert!(remember.is_some(), "and it is what the next silent pull repeats");

        // 3. A file nothing was ever said about is empty: all a first pull can be.
        let (sent, remember) = the_report(None, None);
        assert!(sent.is_empty());
        assert!(remember.is_none());

        // 4. The `found` case carries its own items through unchanged.
        let (sent, remember) = the_report(Some(vec![lsp_types::Diagnostic::default()]), None);
        assert_eq!(sent.len(), 1);
        assert_eq!(remember.map(|items| items.len()), Some(1));
    }
}