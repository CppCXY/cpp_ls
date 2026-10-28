//! `textDocument/diagnostic` — **the diagnostics channel**, because there is no other one.
//!
//! A client asks about a file, and [`super::diagnose_file`] answers from the index as it stands at that moment. There
//! was a push half beside this — a service that published `textDocument/publishDiagnostics` after an edit settled, for
//! clients that do not pull — and it is deleted. One answer with two delivery schedules is how a screen ends up
//! holding a diagnostic about text that is gone; a pulled answer is computed when it is asked for, so it cannot be
//! overtaken by the edit that follows it.
//!
//! # The rule, and the three defects it took to find it
//!
//! **A pull answers what the analysis says about the text that is there now, and nothing else.** Three defects were
//! three different ways of breaking that sentence, and each was reported from an editor:
//!
//! ```text
//! it returned `null`      a client drops a `null` and keeps what it had   → "输入代码之后诊断不改变"
//! it asked too early      no reading yet, so the answer was `null` again  → the same symptom
//! it remembered           the last report was repeated, and unioned with  → a corrected file kept the findings
//!                         the current one while the checks were withheld     from the moment in between
//! ```
//!
//! The third is the one that lasted, because it *looks* like a fix: repeating the last report stops the list
//! flashing while the index fills. What it also does is keep a finding about a `;` the reader deleted one edit ago on
//! screen for as long as the index is busy — which on a real project is until the whole project has been read, and a
//! reader has no way to tell that squiggle from a real one. **A flash is momentary and a stale finding is not**, so
//! the memory is gone: no `last_reports`, no union, and no `resultId` either (`result_id` is `None` in every report,
//! because this server has nothing to diff against and claiming "unchanged" is a claim it cannot support).
//!
//! The other two are still answered here — an unreadable file is an **empty full report**, which is a valid
//! `DocumentDiagnosticReport` and the only thing a client can act on — and the work is done **before** the query, so
//! that "not ready yet" cannot become "nothing to say": [`AnalysisState::prepare_for_a_diagnostic`] reads, indexes and
//! cooks the file under the write lock, which is what makes the answer about the text the client just sent.
//!
//! The work still goes through [`analysis_query`], which is what that mechanism is for: a client re-requests the
//! same file as the user keeps typing, and an answer already being computed for the same key is **replaced** rather
//! than duplicated (the older task is cancelled and the newer one's answer is the only one sent).

use lsp_types::{
    Diagnostic, DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
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
        let items = the_report(found.map(|(_, items)| items));
        log::debug!("pull: {} -> {} item(s)", path.display(), items.len());
        DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                // **Never a `resultId`**: this server keeps no per-document result to diff against, so every answer
                // is the whole list and a client has no reason to hold one of ours.
                result_id: None,
                items,
            },
        })
        .into()
    })
}

/// **The one rule this handler applies**: the report is the answer for the text that is there now.
///
/// An identity, and named so that the identity is the thing that can be read and tested. It used to do more — repeat
/// the last report when the analysis had no answer, and union it with the current one while the checks were withheld
/// — and that is deleted, with the measurement in the module documentation: a corrected file kept reporting the
/// findings from the moment in between, for as long as the index was busy.
///
/// The second argument of the old signature is gone with it, and so is the state behind it
/// (`AnalysisState::last_reports`): an answer that is *only* the analysis's cannot be stale, because there is nothing
/// to be stale with.
fn the_report(found: Option<Vec<Diagnostic>>) -> Vec<Diagnostic> {
    found.unwrap_or_default()
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

    fn a_diagnostic(message: &str) -> lsp_types::Diagnostic {
        lsp_types::Diagnostic {
            message: message.to_string(),
            ..lsp_types::Diagnostic::default()
        }
    }

    /// **A pull answers what the analysis says now.** Reported from an editor, with a screenshot: a file that had
    /// been typed into and *corrected* — completely correct at the time of the report — still showed the two findings
    /// from the moment in between, and editing again did not necessarily clear them.
    ///
    /// The cause was this handler's own rule. It **repeated** the last report it had sent when the analysis had no
    /// answer, and **unioned** the last report with the current one whenever the checks were withheld. Both were
    /// written to stop the list flashing while the index filled; both keep a finding about text that no longer exists
    /// on screen for as long as the state lasts, and on a real project that state lasts until the whole project is
    /// read. A reader cannot tell a stale squiggle from a real one, so every later finding becomes untrustworthy —
    /// which is worse than a flash.
    ///
    /// So the rule is an identity: the report is the current answer, empty or not. `None` — the analysis could not
    /// read the file at all — is an **empty** report, not an old one.
    #[test]
    fn a_report_is_the_answer_for_the_text_that_is_there_now() {
        // The analysis answered, and its answer is empty: the file is clean, and saying so is the point. There is
        // nothing left in the handler that could put an older finding back — that is the assertion.
        assert!(
            the_report(Some(Vec::new())).is_empty(),
            "an empty answer is the answer: the file is clean"
        );

        // No answer at all: empty, because an old report would describe a text nobody is looking at.
        assert!(
            the_report(None).is_empty(),
            "a pull that cannot answer says nothing, not the last thing it said"
        );

        // And what the analysis says goes out unchanged, in its own order.
        let current = vec![a_diagnostic("a real finding")];
        let sent = the_report(Some(current.clone()));
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].message, current[0].message);
    }
}
