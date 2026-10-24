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
        move |session| Some(super::diagnose_file_with_state(session, &for_the_query)),
    )
    .await;

    outcome.map(|found| {
        // **What this pull can answer, and what it says when it cannot.**
        //
        // Two states produce a report a client must not act on. `diagnose_file` answers `None` when the analysis has
        // no cooked reading for the file — the state right after every edit, until the pump cooks it — and it
        // answers a list *without the check layer* while the index still has work (`checks_may_be_trusted`). Either
        // one, sent as it stands, tells the client "this file is clean", it clears the squiggles the user is looking
        // at, and the refresh that follows the cook makes them come back: that is the flash, and both are claims the
        // server cannot support.
        //
        // So a pull with no answer repeats the last report the file was given, and a pull whose checks were
        // **withheld** unions what it has with that report — a parse error that just appeared is shown at once, and
        // the findings that are merely unconfirmed stay on screen until the analysis settles and the real answer
        // replaces them. `AnalysisState::last_reports` holds that report; the rule is `the_report` below.
        let (found, settled) = match found {
            Some((_, items, settled)) => (Some(items), settled),
            None => (None, false),
        };
        let was = found.as_ref().map(|items| items.len());
        let (items, to_remember) = the_report(found, context.analysis().last_report(&path), settled);
        log::debug!(
            "pull: {} -> {} item(s) (analysis said {:?}, settled {settled}, repeated {})",
            path.display(),
            items.len(),
            was,
            to_remember.is_none()
        );
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
///
/// **Three states, and the second is the one that made diagnostics flash.** A `settled` answer is the analysis's
/// final word and replaces what the client holds. An unsettled one is a report whose *check layer was withheld*
/// because the index still has work: it is merged with the last report rather than replacing it, so a parse error
/// that just appeared is shown at once while the findings that are merely unconfirmed stay on screen. No answer at
/// all repeats the last report.
fn the_report(
    found: Option<Vec<lsp_types::Diagnostic>>,
    last: Option<std::sync::Arc<Vec<lsp_types::Diagnostic>>>,
    settled: bool,
) -> (Vec<lsp_types::Diagnostic>, Option<Vec<lsp_types::Diagnostic>>) {
    let held = last.map(|held| (*held).clone());
    match found {
        Some(items) if settled => (items.clone(), Some(items)),
        Some(items) => {
            let mut merged = items;
            if let Some(held) = held {
                for diagnostic in held {
                    if !merged.iter().any(|sent| the_same_diagnostic(sent, &diagnostic)) {
                        merged.push(diagnostic);
                    }
                }
            }
            (merged, None)
        }
        None => (held.unwrap_or_default(), None),
    }
}

/// The identity a client uses to draw one diagnostic: the range, the rule, and the message.
///
/// Two reports that differ only in item order are the same picture, so the merge above has to recognise an item it
/// already has — and `lsp_types::Diagnostic` does not implement `PartialEq`.
fn the_same_diagnostic(one: &lsp_types::Diagnostic, other: &lsp_types::Diagnostic) -> bool {
    one.range == other.range && one.code == other.code && one.message == other.message
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

    /// **The middle state is the one that made diagnostics flash**: a report computed while the check layer was
    /// withheld. Replacing the client's report with it removes the findings that were only *unconfirmed*, and the
    /// refresh after the cook puts them back — a diagnostic that blinks at every keystroke.
    #[test]
    fn an_unsettled_report_keeps_what_the_client_already_holds() {
        let held = std::sync::Arc::new(vec![a_diagnostic("from the last report")]);

        // A settled answer is the final word, even when it is empty.
        let (sent, remember) = the_report(Some(Vec::new()), Some(std::sync::Arc::clone(&held)), true);
        assert!(sent.is_empty());
        assert!(remember.is_some(), "the settled answer is what gets repeated");

        // An unsettled one is merged: the parse error it has, plus the finding it could not confirm.
        let (sent, remember) = the_report(
            Some(vec![a_diagnostic("a parse error")]),
            Some(std::sync::Arc::clone(&held)),
            false,
        );
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert!(remember.is_none(), "an unsettled report is not remembered");

        // No answer at all repeats the last report.
        let (sent, remember) = the_report(None, Some(held), false);
        assert_eq!(sent.len(), 1);
        assert!(remember.is_none());

        // Nothing was ever said: empty, and nothing to remember.
        let (sent, remember) = the_report(None, None, false);
        assert!(sent.is_empty());
        assert!(remember.is_none());
    }
}