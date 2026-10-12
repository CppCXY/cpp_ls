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

/// **What the user has said about configuring their project's build.**
///
/// The one setting this server reads that is about *consent* rather than about what to analyse, and it exists
/// because configuring a build runs the project's own code: a `CMakeLists.txt` may invoke anything, and a server
/// that configured because somebody opened a file would be running a build script without being asked. The three
/// answers are the three a person can mean:
///
/// ```text
///         Ask    the default: offer once per workspace, and honour the answer
///         Auto   they have said yes for this project and do not want to be asked
///         Never  they configure their own builds, and this server should not offer
/// ```
///
/// Spelled `ask`/`auto`/`never` in a settings file. An unrecognised value reads as `Ask` rather than as an error:
/// a settings file is a partial description, and a typo should not turn into a project analysed with no database
/// and no explanation of why.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConfigureCmake {
    /// Offer once, and only where discovery found something worth doing — the default.
    #[default]
    Ask,
    /// Configure without asking. The user has said so for this project.
    Auto,
    /// Never offer. The user configures their own builds.
    Never,
}

impl ConfigureCmake {
    /// Read the setting as a person would write it.
    pub fn parse(written: Option<&str>) -> Self {
        match written.map(str::trim) {
            Some(value) if value.eq_ignore_ascii_case("auto") => ConfigureCmake::Auto,
            Some(value) if value.eq_ignore_ascii_case("never") => ConfigureCmake::Never,
            // `ask`, an empty string, an unknown word and "nothing was said" all mean the same: offer it and let
            // the user decide. See the note above for why an unknown word is not an error.
            _ => ConfigureCmake::Ask,
        }
    }

    /// Would this server configure the project without asking?
    pub fn configures_without_asking(self) -> bool {
        matches!(self, ConfigureCmake::Auto)
    }

    /// May this server offer at all?
    pub fn may_offer(self) -> bool {
        !matches!(self, ConfigureCmake::Never)
    }
}

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
    /// What the user has said about configuring the project's build — see [`ConfigureCmake`].
    pub configure_cmake: ConfigureCmake,
}

impl ClientConfig {
    /// The configuration for a client that answers no configuration request.
    pub fn for_client(client_id: ClientId) -> Self {
        ClientConfig {
            client_id,
            exclude: Vec::new(),
            // **`Ask` rather than `Never`**, which is the interesting default: a client that cannot be asked for
            // settings can still be *shown* a question, so a user on such a client is asked the one time it
            // matters instead of being denied the offer because their editor cannot hold a setting.
            configure_cmake: ConfigureCmake::Ask,
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
        // **And it is asked rather than refused.** A client that cannot be asked for settings can still be shown a
        // question, so a user on such a client gets the offer the one time it matters instead of being denied it
        // because their editor cannot hold a setting.
        assert_eq!(config.configure_cmake, ConfigureCmake::Ask);
    }
    #[test]
    fn the_setting_reads_as_a_person_would_write_it() {
        assert_eq!(ConfigureCmake::parse(Some("auto")), ConfigureCmake::Auto);
        assert_eq!(ConfigureCmake::parse(Some("never")), ConfigureCmake::Never);
        assert_eq!(ConfigureCmake::parse(Some("ask")), ConfigureCmake::Ask);
        // A settings file is written by hand: case and surrounding space are what a person types.
        assert_eq!(ConfigureCmake::parse(Some("  Auto ")), ConfigureCmake::Auto);
    }

    #[test]
    fn a_typo_is_the_default_rather_than_an_error() {
        // **Not an error, and the reason matters.** A word this server does not know means the user said something
        // it did not understand — and the two ways to get that wrong are to configure a project they did not ask
        // about, or to refuse to offer with no explanation. Neither is this: it offers, and the user decides.
        assert_eq!(ConfigureCmake::parse(Some("yes please")), ConfigureCmake::Ask);
        assert_eq!(ConfigureCmake::parse(Some("")), ConfigureCmake::Ask);
        assert_eq!(ConfigureCmake::parse(None), ConfigureCmake::Ask);
    }

    #[test]
    fn only_auto_runs_without_asking_and_only_never_refuses_to_offer() {
        assert!(ConfigureCmake::Auto.configures_without_asking());
        assert!(!ConfigureCmake::Ask.configures_without_asking());
        assert!(!ConfigureCmake::Never.configures_without_asking());

        assert!(ConfigureCmake::Ask.may_offer());
        assert!(ConfigureCmake::Auto.may_offer());
        assert!(!ConfigureCmake::Never.may_offer());
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
