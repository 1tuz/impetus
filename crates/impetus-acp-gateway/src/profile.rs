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
            if is_control_plane_env_name(name) {
                return Err(ProfileError::InvalidProfile(
                    "ACP profile environment must not forward Impetus control-plane variables",
                ));
            }
        }

        Ok(())
    }

    /// Convert the validated profile into the official SDK launch config.
    ///
    /// Official SDK spawn inherits the parent environment and only overlays
    /// `.envs(...)` (no `env_clear`). Overlay blanks forbidden control-plane /
    /// secret names and always sets [`ACP_CHILD_ENV`].
    pub fn to_agent_config(&self) -> Result<AcpAgentConfig, ProfileError> {
        self.validate()?;
        Ok(AcpAgentConfig::new(&self.command)
            .args(self.args.clone())
            .envs(agent_sdk_env_overlay(&self.env)))
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

/// Marker on ACP agent child processes; daemon control-plane peer filter
/// rejects peers whose exec-time environ has `IMPETUS_ACP_CHILD=1` unless they
/// also carry `IMPETUS_ACP_CHILD_CONTROL_OK=1` (operator escape hatch; not
/// grantable via ACP profile — treated as control-plane and blanked by overlay).
pub const ACP_CHILD_ENV: &str = "IMPETUS_ACP_CHILD";

/// Operator authorization for an ACP-marked process to open the control socket.
/// Rejected by [`AcpProfile::validate`] (control-plane) and blanked by SDK overlay.
pub const ACP_CHILD_CONTROL_OK_ENV: &str = "IMPETUS_ACP_CHILD_CONTROL_OK";

/// Impetus control-plane names that must never reach an ACP agent child.
///
/// SDK spawn cannot `env_clear`; these are blanked in the overlay. Known names
/// are always blanked even when unset in the parent, so a later parent export
/// cannot sneak through an empty overlay map.
const CONTROL_PLANE_ENV_ALWAYS_BLANK: &[&str] = &[
    "IMPETUS_SOCKET",
    "IMPETUS_DATA_DIR",
    "IMPETUS_POLICY_CONFIG",
    "IMPETUS_NONINTERACTIVE",
    "IMPETUS_CREDENTIAL_BACKEND",
    "IMPETUS_LSP_BINARY",
];

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

fn is_control_plane_env_name(name: &str) -> bool {
    if name == ACP_CHILD_ENV || name.starts_with("IMPETUS_ACP_MOCK") {
        return false;
    }
    name.starts_with("IMPETUS_")
}

fn is_forbidden_inherited_env(name: &str) -> bool {
    is_secret_env_name(name) || is_control_plane_env_name(name)
}

/// Env overlay for official SDK spawn: blank forbidden inherited names, apply
/// profile allow-list, set [`ACP_CHILD_ENV`]=`1`.
pub fn agent_sdk_env_overlay(profile_env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    build_sdk_env_overlay(std::env::vars(), profile_env)
}

/// Testable core for [`agent_sdk_env_overlay`].
pub fn build_sdk_env_overlay(
    parent: impl IntoIterator<Item = (String, String)>,
    profile_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, _) in parent {
        if is_forbidden_inherited_env(&name) {
            out.insert(name, String::new());
        }
    }
    for name in CONTROL_PLANE_ENV_ALWAYS_BLANK {
        out.insert((*name).to_string(), String::new());
    }
    for (name, value) in profile_env {
        out.insert(name.clone(), value.clone());
    }
    out.insert(ACP_CHILD_ENV.to_string(), "1".to_string());
    out
}

/// Full child env when the caller can `env_clear` (legacy gateway spawn).
///
/// Keeps non-forbidden parent vars + profile allow-list + [`ACP_CHILD_ENV`].
pub fn filtered_agent_process_env(
    profile_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    build_filtered_agent_process_env(std::env::vars(), profile_env)
}

/// Testable core for [`filtered_agent_process_env`].
pub fn build_filtered_agent_process_env(
    parent: impl IntoIterator<Item = (String, String)>,
    profile_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in parent {
        if is_forbidden_inherited_env(&name) {
            continue;
        }
        out.insert(name, value);
    }
    for (name, value) in profile_env {
        out.insert(name.clone(), value.clone());
    }
    out.insert(ACP_CHILD_ENV.to_string(), "1".to_string());
    out
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
        assert_eq!(
            config.environment().get(ACP_CHILD_ENV),
            Some(&"1".into()),
            "agent child must be marked for peer filtering"
        );
        assert_eq!(
            config.environment().get("IMPETUS_SOCKET"),
            Some(&"".into()),
            "SDK overlay must blank harness socket even when unset in parent"
        );
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
    fn profile_rejects_impetus_socket_in_forwarded_env() {
        let mut profile =
            AcpProfile::manual_executable("test", "Test", PathBuf::from("/usr/bin/test"));
        profile
            .env
            .insert("IMPETUS_SOCKET".into(), "/tmp/harness.sock".into());

        assert!(profile.validate().is_err());
        assert!(profile.to_agent_config().is_err());
    }

    #[test]
    fn profile_rejects_acp_child_control_ok_escape_hatch() {
        let mut profile =
            AcpProfile::manual_executable("test", "Test", PathBuf::from("/usr/bin/test"));
        profile
            .env
            .insert(ACP_CHILD_CONTROL_OK_ENV.into(), "1".into());

        assert!(profile.validate().is_err());
    }

    #[test]
    fn sdk_overlay_blanks_inherited_acp_child_control_ok() {
        let parent = [(ACP_CHILD_CONTROL_OK_ENV.into(), "1".into())];
        let overlay = build_sdk_env_overlay(parent, &BTreeMap::new());
        assert_eq!(
            overlay.get(ACP_CHILD_CONTROL_OK_ENV),
            Some(&"".into()),
            "control-ok must not survive into ACP child via inheritance"
        );
        assert_eq!(overlay.get(ACP_CHILD_ENV), Some(&"1".into()));
    }

    #[test]
    fn sdk_overlay_blanks_parent_control_plane_and_tokens() {
        let parent = [
            ("PATH".into(), "/usr/bin".into()),
            ("IMPETUS_SOCKET".into(), "/tmp/harness.sock".into()),
            ("IMPETUS_DATA_DIR".into(), "/tmp/impetus-data".into()),
            ("CI_JOB_TOKEN".into(), "leak-me".into()),
            ("OPENAI_API_KEY".into(), "sk-test".into()),
        ];
        let mut profile_env = BTreeMap::new();
        profile_env.insert("NO_COLOR".into(), "1".into());

        let overlay = build_sdk_env_overlay(parent, &profile_env);

        assert_eq!(overlay.get("NO_COLOR"), Some(&"1".into()));
        assert_eq!(overlay.get(ACP_CHILD_ENV), Some(&"1".into()));
        assert_eq!(overlay.get("IMPETUS_SOCKET"), Some(&"".into()));
        assert_eq!(overlay.get("IMPETUS_DATA_DIR"), Some(&"".into()));
        assert_eq!(overlay.get("CI_JOB_TOKEN"), Some(&"".into()));
        assert_eq!(overlay.get("OPENAI_API_KEY"), Some(&"".into()));
        assert!(
            !overlay.contains_key("PATH"),
            "SDK overlay only carries blanks + allow-list; PATH stays via inheritance"
        );
    }

    #[test]
    fn filtered_process_env_strips_control_plane_instead_of_inheriting() {
        let parent = [
            ("PATH".into(), "/usr/bin".into()),
            ("IMPETUS_SOCKET".into(), "/tmp/harness.sock".into()),
            ("GITHUB_TOKEN".into(), "ghs_leak".into()),
        ];
        let profile_env = BTreeMap::new();

        let env = build_filtered_agent_process_env(parent, &profile_env);

        assert_eq!(env.get("PATH"), Some(&"/usr/bin".into()));
        assert_eq!(env.get(ACP_CHILD_ENV), Some(&"1".into()));
        assert!(!env.contains_key("IMPETUS_SOCKET"));
        assert!(!env.contains_key("GITHUB_TOKEN"));
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
