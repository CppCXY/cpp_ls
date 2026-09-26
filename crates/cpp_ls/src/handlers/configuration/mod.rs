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
//!                                                    └─ different? ──────▶ reload the workspace
//! ```

use lsp_types::{ClientCapabilities, DidChangeConfigurationParams, ServerCapabilities};

use crate::context::ServerContextSnapshot;
use crate::handlers::initialized::get_client_config;

use super::RegisterCapabilities;

pub async fn on_did_change_configuration(
    context: ServerContextSnapshot,
    params: DidChangeConfigurationParams,
) -> Option<()> {
    log::debug!(
        "configuration changed: {}",
        serde_json::to_string(&params).unwrap_or_else(|_| "<unserializable>".to_string())
    );

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
        return Some(());
    }

    let new_config = get_client_config(&context, client_id, supports_config_request).await;
    if new_config == current {
        log::debug!("the client's configuration is unchanged; not reloading");
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

    /// A configuration change that says the same thing as before must not schedule a reload — and the comparison
    /// that decides it is the whole configuration, so a new field is included the day it is added.
    #[test]
    fn two_configurations_with_the_same_settings_are_equal() {
        let one = crate::handlers::ClientConfig {
            client_id: ClientId::Other,
            exclude: vec!["**/build/**".to_string()],
        };
        let other = one.clone();
        assert_eq!(one, other);

        let different = crate::handlers::ClientConfig {
            exclude: vec!["**/out/**".to_string()],
            ..one.clone()
        };
        assert_ne!(one, different);
    }
}
