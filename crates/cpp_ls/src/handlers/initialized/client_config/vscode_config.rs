//! # VS Code: our section, plus the editor's own `files.exclude`
//!
//! VS Code answers `workspace/configuration` for any section, which makes two things readable that a generic
//! client does not offer:
//!
//! ```text
//! cppls     this server's own settings                              (see `default_config`)
//! files     the editor's `files.exclude` map — glob → true/false     the user's own idea of what is generated
//! ```
//!
//! `files.exclude` is read because it is where a user has *already* written down which trees are not source: a
//! build directory, a vendored copy, a `node_modules`-shaped dependency. Asking for it again under our own name
//! would be asking the user to say the same thing twice.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use crate::context::ServerContextSnapshot;
use crate::util::time_cancel_token;

use super::ClientConfig;
use super::default_config::get_client_config_default;

#[derive(Debug, Deserialize)]
struct VscodeFilesConfig {
    /// `files.exclude`: a map of glob → whether the editor hides it. A `false` entry means "do not hide", which
    /// for us is "do not exclude" — the map is a toggle, not a list.
    exclude: Option<HashMap<String, bool>>,
}

pub async fn get_client_config_vscode(
    context: &ServerContextSnapshot,
    config: &mut ClientConfig,
) -> Option<()> {
    get_client_config_default(context, config, None).await;

    let params = lsp_types::ConfigurationParams {
        items: vec![lsp_types::ConfigurationItem {
            scope_uri: None,
            section: Some("files".to_string()),
        }],
    };
    let cancel_token = time_cancel_token(Duration::from_secs(5));
    let files_configs = context
        .client()
        .get_configuration::<VscodeFilesConfig>(params, cancel_token)
        .await?;

    for files_config in files_configs {
        let Some(exclude) = files_config.exclude else {
            continue;
        };

        for (pattern, hidden) in exclude {
            // A `false` entry is the editor being told *not* to hide something. Adding it to an exclusion list
            // would ignore exactly the file the user asked to see.
            if hidden && !config.exclude.contains(&pattern) {
                config.exclude.push(pattern);
            }
        }
    }

    Some(())
}
