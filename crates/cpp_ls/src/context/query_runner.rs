//! # Unified analysis query runner
//!
//! Every request goes through here:
//! - completed normally → `Ready`
//! - genuinely no data → `Missing`
//! - `$/cancelRequest` → `Cancelled(Client)`

use super::AnalysisState;
use cpp_code_analysis::{DiskFiles, Session};
use tokio_util::sync::CancellationToken;

/// Cancellation source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelSource {
    /// `$/cancelRequest`.
    Client,
    /// A later document update began before this request completed.
    ContentModified,
}

/// Unified request result.
#[derive(Debug)]
pub enum RequestOutcome<T> {
    Ready(T),
    Cancelled(CancelSource),
    Missing,
    Failed {
        code: lsp_server::ErrorCode,
        message: String,
    },
}

impl<T> RequestOutcome<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> RequestOutcome<U> {
        match self {
            RequestOutcome::Ready(value) => RequestOutcome::Ready(f(value)),
            RequestOutcome::Cancelled(source) => RequestOutcome::Cancelled(source),
            RequestOutcome::Missing => RequestOutcome::Missing,
            RequestOutcome::Failed { code, message } => RequestOutcome::Failed { code, message },
        }
    }
}

/// **The one executor every request uses**: a read-only query against the analysis, answerable only once there is a
/// reading.
pub async fn snapshot_query<T, F>(
    analysis: &AnalysisState,
    cancel_token: CancellationToken,
    compute: F,
) -> RequestOutcome<T>
where
    T: Send + Sync + 'static,
    F: FnOnce(&Session<DiskFiles>) -> Option<T> + Send + 'static,
{
    if cancel_token.is_cancelled() {
        return RequestOutcome::Cancelled(CancelSource::Client);
    }

    let started = std::time::Instant::now();
    let worker_token = cancel_token.clone();
    let outcome = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => RequestOutcome::Cancelled(CancelSource::Client),
        outcome = analysis.query_blocking(move |session| {
            if worker_token.is_cancelled() {
                None
            } else {
                compute(session)
            }
        }) => {
            if cancel_token.is_cancelled() {
                RequestOutcome::Cancelled(CancelSource::Client)
            } else {
                outcome
            }
        }
    };
    let took = started.elapsed().as_millis();
    if took > 100 {
        log::debug!("a query waited {took} ms for the analysis");
    }
    match outcome {
        RequestOutcome::Ready(value) => RequestOutcome::Ready(value),
        RequestOutcome::Missing => {
            if cancel_token.is_cancelled() {
                RequestOutcome::Cancelled(CancelSource::Client)
            } else {
                RequestOutcome::Missing
            }
        }
        RequestOutcome::Cancelled(source) => RequestOutcome::Cancelled(source),
        RequestOutcome::Failed { code, message } => RequestOutcome::Failed { code, message },
    }
}
