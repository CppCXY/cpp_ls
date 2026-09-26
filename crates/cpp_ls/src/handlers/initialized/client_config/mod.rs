//! # The client's own configuration
//!
//! Two questions are asked of the client, and both have the same shape: *which files are yours* and *which files
//! are not*. The first is what the client's `exclude` list answers, and it is the only setting this server reads
//! today — a C++ project's build tree is a fact about the project, and the client is where a user has already
//! written it down.
//!
//! ```text
//! VS Code      workspace/configuration  section "cppls"     our own settings
//!              workspace/configuration  section "files"     the editor's exclude list, as a map of glob → bool
//! anything     workspace/configuration  section "cppls"     our own settings (see `default_config`)
//! ```
//!
//! A client that does not answer `workspace/configuration` (the capability is optional) simply has no
//! configuration: every field keeps its default, and the workspace opens with the engine's own rules — `.git` and
//! the cache directory, and nothing else. That is the honest fallback rather than an invented exclusion list.

mod default_config;
mod vscode_config;

use serde_json::Value;
use vscode_config::get_client_config_vscode;

use crate::context::{ClientId, ServerContextSnapshot};

use default_config::get_client_config_default;

/// What the client told this server about how to analyse the workspace.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClientConfig {
    pub client_id: ClientId,
    /// Glob patterns for files the analysis should leave alone — a build tree, a vendored copy, generated sources.
    ///
    /// The same list the editor uses to hide files, which is why it is read from the client rather than invented
    /// here: a project that keeps its sources in `build/` is unusual, not wrong, and a server that guessed would
    /// silently analyse nothing.
    pub exclude: Vec<String>,
}

impl ClientConfig {
    /// The configuration for a client that answers no configuration request.
    pub fn for_client(client_id: ClientId) -> Self {
        ClientConfig {
            client_id,
            exclude: Vec::new(),
        }
    }
}

/// Ask the client for its configuration, as far as its capabilities allow.
pub async fn get_client_config(
    context: &ServerContextSnapshot,
    client_id: ClientId,
    supports_config_request: bool,
) -> ClientConfig {
    let mut config = ClientConfig::for_client(client_id);

    match client_id {
        ClientId::VSCode => {
            get_client_config_vscode(context, &mut config).await;
        }
        _ if supports_config_request => {
            get_client_config_default(context, &mut config, Some(&["cppls"])).await;
        }
        _ => {}
    }

    config
}

/// Remove the `null`s a client sends for settings the user never touched.
///
/// VS Code answers `workspace/configuration` with every option in the section, `null` for the ones that are not
/// set. A settings parser that took those literally would read "null excludes" as "exclude nothing" and be right
/// by accident; one that read a typed field would fail. Dropping them says what is true: the user said nothing
/// about this option.
pub(crate) fn skip_nulls(value: &mut Value) {
    if let Value::Object(object) = value {
        object.retain(|_, value| !value.is_null());
        for (_, value) in object {
            skip_nulls(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_that_answers_nothing_gets_no_exclusions() {
        // The fallback, stated as a test because it is a decision: this server does not invent a build directory.
        let config = ClientConfig::for_client(ClientId::Other);

        assert!(config.exclude.is_empty());
        assert_eq!(config, ClientConfig::default());
    }

    #[test]
    fn nulls_are_dropped_and_what_was_said_is_kept() {
        let mut value = serde_json::json!({
            "exclude": ["**/build/**"],
            "encoding": null,
            "nested": { "value": null, "other": true },
        });

        skip_nulls(&mut value);

        assert_eq!(
            value,
            serde_json::json!({ "exclude": ["**/build/**"], "nested": { "other": true } })
        );
    }
}
