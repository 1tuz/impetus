//! ACP backend profile types.
//!
//! Manual executable profile: user указывает путь к agent CLI.
//! Credential strategy определяет владение секретом.

use agent_client_protocol::AcpAgentConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    /// Explicitly allow-listed, non-secret environment for the agent process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Agent-advertised authentication method selected explicitly by the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_method_id: Option<String>,
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

        if !matches!(self.credential_strategy, CredentialStrategy::AgentOwned) {
            return Err(ProfileError::InvalidProfile(
                "ACP authentication must be owned by the external agent",
            ));
        }

        if self.credential_ref.is_some() {
            return Err(ProfileError::InvalidProfile(
                "agent-owned login must not contain credential_ref",
            ));
        }

        if self.args.iter().any(|arg| arg.contains('\0')) {
            return Err(ProfileError::InvalidProfile(
                "agent arguments must not contain NUL bytes",
            ));
        }

        if self
            .auth_method_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty() || id.contains('\0'))
        {
            return Err(ProfileError::InvalidProfile(
                "auth_method_id must be a non-empty protocol identifier",
            ));
        }

        for (name, value) in &self.env {
            if !is_safe_env_name(name) || value.contains('\0') {
                return Err(ProfileError::InvalidProfile(
                    "agent environment contains an invalid field",
                ));
            }
            if is_secret_env_name(name) {
                return Err(ProfileError::InvalidProfile(
                    "ACP profile environment must not contain credentials",
                ));
            }
        }

        Ok(())
    }

    /// Convert the validated profile into the official SDK launch config.
    pub fn to_agent_config(&self) -> Result<AcpAgentConfig, ProfileError> {
        self.validate()?;
        Ok(AcpAgentConfig::new(&self.command)
            .args(self.args.clone())
            .envs(self.env.clone()))
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
            env: BTreeMap::new(),
            auth_method_id: None,
            credential_strategy: CredentialStrategy::AgentOwned,
            credential_ref: None,
        }
    }
}

fn is_safe_env_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_secret_env_name(name: &str) -> bool {
    const SECRET_MARKERS: &[&str] = &[
        "API_KEY",
        "CREDENTIAL",
        "PASSWORD",
        "PASSPHRASE",
        "PRIVATE_KEY",
        "SECRET",
        "TOKEN",
    ];
    SECRET_MARKERS.iter().any(|marker| name.contains(marker))
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

    #[test]
    fn profile_args_and_non_secret_env_reach_sdk_config() {
        let mut profile =
            AcpProfile::manual_executable("test", "Test", PathBuf::from("/usr/bin/test"));
        profile.args = vec!["acp".into(), "--stdio".into()];
        profile.env.insert("RUST_LOG".into(), "info".into());

        let config = profile.to_agent_config().expect("valid ACP config");

        assert_eq!(config.arguments(), &["acp", "--stdio"]);
        assert_eq!(config.environment().get("RUST_LOG"), Some(&"info".into()));
    }

    #[test]
    fn profile_rejects_secret_bearing_env_names() {
        let mut profile =
            AcpProfile::manual_executable("test", "Test", PathBuf::from("/usr/bin/test"));
        profile
            .env
            .insert("PROVIDER_API_TOKEN".into(), "opaque-value".into());

        assert!(profile.validate().is_err());
    }

    #[test]
    fn acp_profile_rejects_direct_provider_credential_strategy() {
        let mut profile =
            AcpProfile::manual_executable("test", "Test", PathBuf::from("/usr/bin/test"));
        profile.credential_strategy = CredentialStrategy::BrowserOAuth {
            provider_id: "provider".into(),
        };

        assert!(profile.validate().is_err());
    }

    #[test]
    fn profile_preserves_explicit_agent_owned_auth_method() {
        let json = r#"{
            "id": "test",
            "display_name": "Test",
            "command": "/usr/bin/test",
            "auth_method_id": "browser-login",
            "credential_strategy": {"kind": "agent_owned"}
        }"#;

        let profile: AcpProfile = serde_json::from_str(json).expect("valid ACP profile");

        assert_eq!(profile.auth_method_id.as_deref(), Some("browser-login"));
    }
}
