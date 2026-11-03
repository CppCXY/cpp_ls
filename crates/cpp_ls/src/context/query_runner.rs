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
}

/// Unified request result.
#[derive(Debug)]
pub enum RequestOutcome<T> {
    Ready(T),
    Cancelled(CancelSource),
    Missing,
}

impl<T> RequestOutcome<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> RequestOutcome<U> {
        match self {
            RequestOutcome::Ready(value) => RequestOutcome::Ready(f(value)),
            RequestOutcome::Cancelled(source) => RequestOutcome::Cancelled(source),
            RequestOutcome::Missing => RequestOutcome::Missing,
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
    let outcome = analysis.query_blocking(compute).await;
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
        RequestOutcome::Cancelled(CancelSource::Client) => {
            RequestOutcome::Cancelled(CancelSource::Client)
        }
    }
}
