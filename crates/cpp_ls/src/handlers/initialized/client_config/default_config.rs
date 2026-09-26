//! # Asking a client for this server's own settings
//!
//! The generic path: `workspace/configuration` with a section name, one request per scope, the first scope that
//! answers wins. A client that does not implement the request answers nothing, and everything keeps its default —
//! which is not an error and is logged as what it is.

use std::time::Duration;

use log::{info, warn};
use serde::Deserialize;
use serde_json::Value;

use crate::context::ServerContextSnapshot;
use crate::util::{path_to_uri, time_cancel_token};

use super::{ClientConfig, skip_nulls};

/// The section this server's own settings live under, in a client that has no schema for us.
const CONFIG_SECTION: &str = "cppls";

/// Our settings, as a client writes them down.
///
/// Every field is optional because a settings file is a *partial* description: `null` (or an absent key) means the
/// user said nothing about it, and a default here would be a value they did not choose.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CppLsSettings {
    /// Globs to leave alone — a build tree, a vendored copy, generated sources.
    exclude: Vec<String>,
}

pub async fn get_client_config_default(
    context: &ServerContextSnapshot,
    config: &mut ClientConfig,
    scopes: Option<&[&str]>,
) -> Option<()> {
    let workspace_root = {
        let workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.root().map(|root| root.to_path_buf())
    };
    let scope_uri = workspace_root.as_deref().and_then(path_to_uri);
    let client = context.client();

    for scope in scopes.unwrap_or(&[CONFIG_SECTION]) {
        let params = lsp_types::ConfigurationParams {
            items: vec![lsp_types::ConfigurationItem {
                scope_uri: scope_uri.clone(),
                section: Some((*scope).to_string()),
            }],
        };

        let cancel_token = time_cancel_token(Duration::from_secs(5));
        let Some(fetched) = client.get_configuration::<Value>(params, cancel_token).await else {
            warn!("the client did not answer for the {scope:?} section");
            continue;
        };

        let mut settings = fetched
            .into_iter()
            .filter(|value| !value.is_null())
            .collect::<Vec<Value>>();
        for value in &mut settings {
            skip_nulls(value);
        }

        if settings.is_empty() {
            continue;
        }

        let Some(settings) = settings
            .into_iter()
            .find_map(|value| serde_json::from_value::<CppLsSettings>(value).ok())
        else {
            warn!("the {scope:?} section did not look like this server's settings");
            continue;
        };

        info!(
            "client settings from {scope:?}: {} exclude patterns",
            settings.exclude.len()
        );
        config.exclude.extend(settings.exclude);
        return Some(());
    }

    None
}
