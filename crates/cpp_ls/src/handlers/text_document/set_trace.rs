use lsp_types::SetTraceParams;

use crate::context::ServerContextSnapshot;

/// **`$/setTrace` is accepted and not implemented**, and the log says so.
///
/// The value a client sets (`messages`/`verbose`) is the level it wants the server's **protocol log** at, which this
/// server does not send; its own log level comes from `--log-level`. Recording the request is the difference between
/// "unsupported" and "silently swallowed" (`docs/worklog.md` P2-6).
pub async fn on_set_trace(_: ServerContextSnapshot, params: SetTraceParams) -> Option<()> {
    log::debug!(
        "$/setTrace {:?} is not supported: the server's log level is its own (--log-level)",
        params.value
    );
    Some(())
}
