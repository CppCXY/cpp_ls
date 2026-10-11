//! **Which rules are on** — the config a check run is filtered through.
//!
//! The shape follows `emmylua_code_analysis`'s `check/config.rs`: a set of enabled codes, two lists that build it
//! (`enable`, `disable`), and **one place** that answers "is this code on" — [`CheckConfig::is_enabled`], asked by
//! [`CheckContext::add`](super::CheckContext::add) rather than by each check.
//!
//! # Why the filter is in the context and not in the checks
//!
//! A check that asked `is_enabled` first would be a check with a filtering rule inside it, and there are eleven of
//! them; the twelfth would forget. Worse, a disabled rule would still cost whatever it costs to *look* — the walk
//! over the tree is the expensive part, not the message — so the skip that actually saves anything is
//! [`run_check`](super::run_check)'s, which skips a whole check when every code it reports is off. This layer only
//! decides what a check that **did** run is allowed to say.
//!
//! # Disabled and enabled at once
//!
//! `disable` wins over `enable`, which is what makes the pair useful: a project can enable everything and turn off
//! one rule, and a project can start from nothing and turn on three. The lists are names, not codes —
//! [`CheckConfig::with_disabled`] resolves each through [`DiagnosticCode::from_name`] and keeps the misses in
//! [`CheckConfig::unknown`], so a configuration with a typo can tell the user rather than quietly analysing nothing.
//!
//! That last part is the whole reason this is a type rather than a `Vec<String>`: silently ignoring an unknown name
//! is the failure mode this analysis is written against — a rule the user believes is on, that never runs.

use super::DiagnosticCode;

/// Which codes a check run may report, and what it was told that this build does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckConfig {
    /// The codes that may be reported. Everything in [`DiagnosticCode::ALL`] unless a list said otherwise.
    enabled: Vec<DiagnosticCode>,
    /// **Names that are not codes in this build**, kept so that a caller can say so.
    ///
    /// Not an error type: a config file from a newer version of the analysis, or from a plugin that no longer
    /// exists, is a thing a user should be *told* about and not a reason to fail.
    unknown: Vec<String>,
}

impl Default for CheckConfig {
    /// **Everything on**, which is what a session with no configuration gets.
    ///
    /// The default is "on" rather than "off" because a user who installed the server asked for its opinion; the
    /// opposite default would make an unconfigured project look analysed and be silent.
    fn default() -> Self {
        CheckConfig {
            enabled: DiagnosticCode::ALL.to_vec(),
            unknown: Vec::new(),
        }
    }
}

impl CheckConfig {
    /// **The default, with `disabled` turned off** — the ordinary configuration, and the one a settings file
    /// builds.
    pub fn with_disabled<I, S>(disabled: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut config = CheckConfig::default();
        for name in disabled {
            let name = name.as_ref();
            match DiagnosticCode::from_name(name) {
                Some(code) => config.enabled.retain(|enabled| *enabled != code),
                None => config.unknown.push(name.to_string()),
            }
        }
        config
    }

    /// **Only these codes**, whatever the rest of the defaults are — what a test turns on to ask about one rule.
    pub fn with_only<I, S>(only: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut config = CheckConfig {
            enabled: Vec::new(),
            unknown: Vec::new(),
        };
        for name in only {
            let name = name.as_ref();
            match DiagnosticCode::from_name(name) {
                Some(code) => config.enabled.push(code),
                None => config.unknown.push(name.to_string()),
            }
        }
        config
    }

    /// May this code be reported?
    ///
    /// Linear over a list of at most eleven, which is faster than a hash of the same size and has no allocation —
    /// this is asked once per finding, and findings are the one thing here that can be numerous.
    pub fn is_enabled(&self, code: DiagnosticCode) -> bool {
        self.enabled.contains(&code)
    }

    /// The names this build does not know, in the order they were given. Empty is the ordinary answer.
    pub fn unknown(&self) -> &[String] {
        &self.unknown
    }

    /// The codes that are on — for a caller that shows a settings screen, or a test that asserts the default.
    pub fn enabled(&self) -> &[DiagnosticCode] {
        &self.enabled
    }
}
