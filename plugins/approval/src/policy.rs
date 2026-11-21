//! The decision itself, apart from the bindings, so it can be tested natively.

use serde::Deserialize;

/// How much the agent may do without asking, after Goose's permission modes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Every call runs.
    Auto,
    /// Every call waits for a person.
    Approve,
    /// Calls to read-only tools run; the rest wait for a person.
    #[default]
    Smart,
    /// No call runs: the agent only talks.
    Chat,
}

/// Plugin configuration, from the host.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    /// The mode for tools on neither list.
    pub mode: Mode,
    /// Tools that always run, whatever the mode.
    pub allow: Vec<String>,
    /// Tools that never run, whatever the mode or the allow list.
    pub deny: Vec<String>,
}

/// What the policy says about one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Let it run.
    Allow,
    /// Ask a person first, saying why.
    Ask(String),
    /// Refuse it, saying why.
    Deny(String),
}

impl Policy {
    /// The decision on a call to `tool`, which only looks if `read_only`.
    #[must_use]
    pub fn decide(&self, tool: &str, read_only: bool) -> Decision {
        let listed = |list: &[String]| list.iter().any(|name| name == tool);
        if listed(&self.deny) {
            return Decision::Deny(format!("`{tool}` is on the deny list"));
        }
        if listed(&self.allow) {
            return Decision::Allow;
        }
        match self.mode {
            Mode::Auto => Decision::Allow,
            Mode::Smart if read_only => Decision::Allow,
            Mode::Smart => Decision::Ask(format!("`{tool}` can change things")),
            Mode::Approve => Decision::Ask(format!("every call needs approval, `{tool}` too")),
            Mode::Chat => Decision::Deny("tools are switched off: answer in words".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(json: &str) -> Policy {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn smart_is_the_default_and_asks_only_about_changes() {
        let smart = policy("{}");
        assert_eq!(smart.decide("read", true), Decision::Allow);
        assert_eq!(
            smart.decide("bash", false),
            Decision::Ask("`bash` can change things".into())
        );
    }

    #[test]
    fn each_mode_decides_for_unlisted_tools() {
        assert_eq!(
            policy(r#"{"mode":"auto"}"#).decide("bash", false),
            Decision::Allow
        );
        assert!(matches!(
            policy(r#"{"mode":"approve"}"#).decide("read", true),
            Decision::Ask(_)
        ));
        assert!(matches!(
            policy(r#"{"mode":"chat"}"#).decide("read", true),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn the_deny_list_beats_the_allow_list_and_both_beat_the_mode() {
        let lists = policy(r#"{"mode":"chat","allow":["read","bash"],"deny":["bash"]}"#);
        assert_eq!(lists.decide("read", true), Decision::Allow);
        assert_eq!(
            lists.decide("bash", false),
            Decision::Deny("`bash` is on the deny list".into())
        );
    }

    #[test]
    fn a_misspelt_setting_is_refused() {
        assert!(serde_json::from_str::<Policy>(r#"{"mode":"yolo"}"#).is_err());
        assert!(serde_json::from_str::<Policy>(r#"{"denied":["bash"]}"#).is_err());
    }
}
