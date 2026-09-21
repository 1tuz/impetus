//! User-supplied policy overrides (JSON). Fail-closed defaults stay when absent.

use crate::policy::{ActionKind, PolicyDecision};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use thiserror::Error;

/// Supported policy config schema version.
pub const POLICY_CONFIG_VERSION: u32 = 1;

/// Decision override written in a user policy config file.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyConfigDecision {
    Allow,
    Deny,
    NeedsApproval,
}

impl PolicyConfigDecision {
    pub fn to_decision(self) -> PolicyDecision {
        match self {
            Self::Allow => PolicyDecision::Allow,
            Self::Deny => PolicyDecision::Deny {
                reason: "denied by user policy config".into(),
            },
            Self::NeedsApproval => PolicyDecision::NeedsApproval {
                reason: "requires approval per user policy config".into(),
            },
        }
    }
}

/// User policy config: optional per-kind overrides on top of fail-closed defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PolicyConfig {
    pub version: u32,
    #[serde(default)]
    pub overrides: BTreeMap<ActionKind, PolicyConfigDecision>,
}

#[derive(Debug, Error)]
pub enum PolicyConfigError {
    #[error("failed to read policy config: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid policy config JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported policy config version: {0} (expected {POLICY_CONFIG_VERSION})")]
    UnsupportedVersion(u32),
}

impl PolicyConfig {
    /// Parse and validate a JSON policy config document.
    pub fn parse(json: &str) -> Result<Self, PolicyConfigError> {
        let config: Self = serde_json::from_str(json)?;
        config.validate()?;
        Ok(config)
    }

    /// Load policy config from a UTF-8 JSON file.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, PolicyConfigError> {
        let raw = fs::read_to_string(path)?;
        Self::parse(&raw)
    }

    fn validate(&self) -> Result<(), PolicyConfigError> {
        if self.version != POLICY_CONFIG_VERSION {
            return Err(PolicyConfigError::UnsupportedVersion(self.version));
        }
        Ok(())
    }

    pub fn override_for(&self, kind: ActionKind) -> Option<PolicyConfigDecision> {
        self.overrides.get(&kind).copied()
    }

    /// Load startup policy: missing file → empty overrides; present but invalid → error.
    ///
    /// Used by `impetusd` / CLI so PolicyConfig applies at process start, not only
    /// as a library API. Callers pass an explicit path or a conventional default.
    pub fn load_optional(path: impl AsRef<Path>) -> Result<Self, PolicyConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self {
                version: POLICY_CONFIG_VERSION,
                overrides: BTreeMap::new(),
            });
        }
        Self::load_from_path(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_empty_overrides() {
        let config = PolicyConfig::parse(r#"{"version":1}"#).expect("parse");
        assert_eq!(config.version, POLICY_CONFIG_VERSION);
        assert!(config.overrides.is_empty());
    }

    #[test]
    fn parse_few_action_overrides() {
        let config = PolicyConfig::parse(
            r#"{
              "version": 1,
              "overrides": {
                "write_file": "allow",
                "spawn_process": "deny",
                "network_connect": "needs_approval"
              }
            }"#,
        )
        .expect("parse");
        assert_eq!(
            config.override_for(ActionKind::WriteFile),
            Some(PolicyConfigDecision::Allow)
        );
        assert_eq!(
            config.override_for(ActionKind::SpawnProcess),
            Some(PolicyConfigDecision::Deny)
        );
        assert_eq!(
            config.override_for(ActionKind::NetworkConnect),
            Some(PolicyConfigDecision::NeedsApproval)
        );
        assert_eq!(config.override_for(ActionKind::ReadFile), None);
    }

    #[test]
    fn reject_unsupported_version() {
        let err = PolicyConfig::parse(r#"{"version":99}"#).expect_err("version");
        assert!(matches!(err, PolicyConfigError::UnsupportedVersion(99)));
    }

    #[test]
    fn reject_invalid_decision() {
        let err = PolicyConfig::parse(r#"{"version":1,"overrides":{"write_file":"maybe"}}"#)
            .expect_err("decision");
        assert!(matches!(err, PolicyConfigError::Json(_)));
    }

    #[test]
    fn load_from_path_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("policy.json");
        {
            let mut file = std::fs::File::create(&path).expect("create");
            write!(
                file,
                r#"{{"version":1,"overrides":{{"tmux_attach":"deny"}}}}"#
            )
            .expect("write");
        }
        let config = PolicyConfig::load_from_path(&path).expect("load");
        assert_eq!(
            config.override_for(ActionKind::TmuxAttach),
            Some(PolicyConfigDecision::Deny)
        );
    }

    #[test]
    fn decision_reasons_contain_no_secrets() {
        for decision in [
            PolicyConfigDecision::Deny,
            PolicyConfigDecision::NeedsApproval,
        ] {
            let text = match decision.to_decision() {
                PolicyDecision::Deny { reason } | PolicyDecision::NeedsApproval { reason } => {
                    reason
                }
                PolicyDecision::Allow => continue,
            };
            assert!(!text.contains("token"));
            assert!(!text.contains("sk-"));
            assert!(!text.contains("password"));
        }
    }

    #[test]
    fn load_optional_missing_file_is_empty_overrides() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing-policy.json");
        let config = PolicyConfig::load_optional(&path).expect("optional");
        assert_eq!(config.version, POLICY_CONFIG_VERSION);
        assert!(config.overrides.is_empty());
    }

    #[test]
    fn load_optional_rejects_invalid_present_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.json");
        std::fs::write(&path, r#"{"version":99}"#).expect("write");
        let err = PolicyConfig::load_optional(&path).expect_err("bad version");
        assert!(matches!(err, PolicyConfigError::UnsupportedVersion(99)));
    }
}
