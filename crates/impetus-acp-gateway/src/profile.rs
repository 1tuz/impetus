//! ACP backend profile types.
//!
//! Manual executable profile: user указывает путь к agent CLI.
//! Credential strategy определяет владение секретом.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Стратегия авторизации для ACP backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum CredentialStrategy {
    /// Agent владеет своим login; harness только forwards prompts.
    AgentOwned,
    /// Opaque macOS Keychain reference (для direct provider, не ACP).
    KeychainReference { service: String, account: String },
    /// System-browser OAuth (для direct provider, не ACP).
    BrowserOAuth { provider_id: String },
    /// Локальный endpoint без секрета.
    None,
}

/// ACP backend profile для external coding-agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcpProfile {
    pub id: String,
    pub display_name: String,
    /// Путь к executable agent CLI.
    pub command: PathBuf,
    /// Аргументы для запуска (без session-specific параметров).
    #[serde(default)]
    pub args: Vec<String>,
    pub credential_strategy: CredentialStrategy,
    /// Opaque reference для credential (не сам секрет).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
}

impl AcpProfile {
    pub fn validate(&self) -> Result<(), ProfileError> {
        if self.id.trim().is_empty() || self.display_name.trim().is_empty() {
            return Err(ProfileError::InvalidProfile(
                "id and display_name are required",
            ));
        }

        if !self.command.is_absolute() {
            return Err(ProfileError::InvalidProfile(
                "command must be an absolute path",
            ));
        }

        // Agent-owned login не должен содержать raw credential в profile
        if matches!(self.credential_strategy, CredentialStrategy::AgentOwned)
            && self.credential_ref.is_some()
        {
            return Err(ProfileError::InvalidProfile(
                "agent-owned login must not contain credential_ref",
            ));
        }

        Ok(())
    }

    /// Manual executable profile для тестирования.
    pub fn manual_executable(
        id: impl Into<String>,
        display_name: impl Into<String>,
        command: PathBuf,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            command,
            args: Vec::new(),
            credential_strategy: CredentialStrategy::AgentOwned,
            credential_ref: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("Invalid profile: {0}")]
    InvalidProfile(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_executable_profile_validates() {
        let profile = AcpProfile::manual_executable(
            "test-agent",
            "Test Agent",
            PathBuf::from("/usr/local/bin/test-agent"),
        );
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn agent_owned_rejects_credential_ref() {
        let mut profile =
            AcpProfile::manual_executable("test", "Test", PathBuf::from("/usr/bin/test"));
        profile.credential_ref = Some("should-not-be-here".into());
        assert!(profile.validate().is_err());
    }

    #[test]
    fn profile_requires_absolute_path() {
        let profile = AcpProfile::manual_executable("test", "Test", PathBuf::from("relative/path"));
        assert!(profile.validate().is_err());
    }

    #[test]
    fn profile_denies_raw_credential_in_json() {
        let json = r#"{
            "id": "test",
            "display_name": "Test",
            "command": "/usr/bin/test",
            "credential_strategy": {"kind": "agent_owned"},
            "api_key": "raw-secret"
        }"#;
        assert!(serde_json::from_str::<AcpProfile>(json).is_err());
    }
}
