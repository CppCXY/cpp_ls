//! # `workspace/didChangeConfiguration` — the client's settings changed
//!
//! A notification with no structure to it: the protocol says "something changed", and a client that wants to say
//! *what* says it with a `workspace/configuration` request. So this handler asks again, compares, and reloads only
//! when the answer is different — the same rule the Lua server this was ported from arrived at, and for the same
//! reason: VS Code sends this notification whenever *any* setting changes, including settings this server has
//! never heard of, and re-opening a project because somebody changed their font size is a visible waste.
//!
//! ```text
//! didChangeConfiguration ──▶ workspace/configuration ──▶ same as before? ──▶ nothing
//!                                                    └─ different? ──────▶ queued → applied → reload
//! ```
//!
//! The read is the handler's; the application is an [`UpdateEvent`](crate::context::UpdateEvent) so that it is ordered
//! with the document events — see [`the_clients_configuration`] for the deadlock that keeps the two apart.

use crate::handlers;
use lsp_types::{ClientCapabilities, ServerCapabilities};

use crate::context::ServerContextSnapshot;
use crate::handlers::initialized::get_client_config;

use super::RegisterCapabilities;

/// **Read the client's configuration after it said something changed** — the read, and not the application.
///
/// The two halves are deliberately separate, and the reason is a deadlock rather than tidiness: reading is a
/// **server→client round trip**, so it must not happen inside the update queue's worker. The worker holds the queue's
/// apply lock while it applies an event, and a request's barrier waits on that lock — a worker awaiting the client
/// would therefore wait for a client that is itself waiting for an answer queued behind the same lock
/// (`docs/worklog.md` P1-2). The notification handler reads (its own async context, no queue lock); the
/// queue applies the result, which is what gives the change the barrier and the `ContentModified` cancellation.
///
/// `None` when the client cannot be asked, or when what it says is what was already in force — the second is the rule
/// the handler has always had: VS Code sends this notification whenever *any* setting changes, including settings this
/// server has never heard of, and re-opening a project because somebody changed their font size is a visible waste.
pub async fn the_clients_configuration(
    context: &ServerContextSnapshot,
) -> Option<handlers::ClientConfig> {
    let (client_id, current) = {
        let workspace_manager = context.workspace_manager().lock().await;
        (
            workspace_manager.client_config.client_id,
            workspace_manager.client_config.clone(),
        )
    };

    let supports_config_request = context.lsp_features().supports_config_request();
    if !supports_config_request {
        log::info!("the client cannot be asked what changed; nothing to reload for");
        return None;
    }

    let new_config = get_client_config(context, client_id, supports_config_request).await;
    match new_config == current {
        true => {
            log::debug!("the client's configuration is unchanged; not reloading");
            None
        }
        false => Some(new_config),
    }
}

/// **Apply a configuration the client reported**, inside the update sequence.
///
/// Idempotent against the manager's current value, which is what makes it safe for the event to be applied by whichever
/// party gets there first (the worker, or a request that drains the queue ahead of it).
pub async fn apply_the_clients_configuration(
    context: ServerContextSnapshot,
    new_config: handlers::ClientConfig,
) -> Option<()> {
    let unchanged = {
        let workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.client_config == new_config
    };
    if unchanged {
        log::debug!(
            "the client's configuration was applied by somebody else; nothing to reload for"
        );
        return Some(());
    }

    log::info!("reloading the workspace: the client's configuration changed");
    let mut workspace_manager = context.workspace_manager().lock().await;
    workspace_manager.set_client_config(new_config);
    workspace_manager.add_reload_workspace_task(context.clone());

    Some(())
}

pub struct ConfigurationCapabilities;

impl RegisterCapabilities for ConfigurationCapabilities {
    fn register_capabilities(_: &mut ServerCapabilities, _: &ClientCapabilities) {}
}

#[cfg(test)]
mod tests {
    use crate::context::ClientId;
    use crate::handlers;

    /// A configuration change that says the same thing as before must not schedule a reload — and the comparison
    /// that decides it is the whole configuration, so a new field is included the day it is added.
    #[test]
    fn two_configurations_with_the_same_settings_are_equal() {
        // Every field is written out, which is the point of the test: a field added to the struct later has to be
        // written here too, and the comparison then covers it by construction.
        let one = handlers::ClientConfig {
            client_id: ClientId::Other,
            exclude: vec!["**/build/**".to_string()],
        };
        let other = one.clone();
        assert_eq!(one, other);

        let different = handlers::ClientConfig {
            exclude: vec!["**/out/**".to_string()],
            ..one.clone()
        };
        assert_ne!(one, different);
    }
}
