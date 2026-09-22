use crate::policy_config::{PolicyConfig, PolicyConfigDecision, PolicyConfigError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Policy rule version for audit and replay.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyVersion(pub u32);

impl PolicyVersion {
    pub const V1: Self = Self(1);
}

/// Snapshot of policy state for audit and compliance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicySnapshot {
    pub version: PolicyVersion,
    pub scope: SandboxScope,
    pub timestamp: u64,
}

impl PolicySnapshot {
    pub fn capture(engine: &PolicyEngine) -> Self {
        Self {
            version: PolicyVersion::V1,
            scope: engine.scope().clone(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time after epoch")
                .as_secs(),
        }
    }
}

pub use impetus_protocol::{
    Action, ActionFingerprint, ActionKind, ActionOrigin, ExecutionSemantics, PolicyDecision,
};

/// Web capability mapping kept in core (depends on web_research types).
pub trait ActionKindExt {
    fn web_capability(self) -> Option<crate::web_research::WebCapability>;
}

impl ActionKindExt for ActionKind {
    fn web_capability(self) -> Option<crate::web_research::WebCapability> {
        use crate::web_research::WebCapability;
        match self {
            ActionKind::WebSearch => Some(WebCapability::Search),
            ActionKind::WebFetch => Some(WebCapability::Read),
            ActionKind::WebDownload => Some(WebCapability::Download),
            ActionKind::WebBrowser => Some(WebCapability::Browser),
            ActionKind::WebSubmit => Some(WebCapability::Submit),
            ActionKind::WebUpload => Some(WebCapability::Upload),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxScope {
    pub workspace_root: PathBuf,
    pub allow_network: bool,
    pub allowed_hosts: Vec<String>,
    /// Session grant for mutating outbound web (submit/upload/browser/download).
    /// Default false: read-only web may Allow when `allow_network`; outbound NeedsApproval.
    #[serde(default)]
    pub allow_web_outbound: bool,
    /// Session grant for LAN/private/link-local/metadata web targets (`WebCapability::PrivateRead`).
    /// Default false: private targets stay Deny even when `allow_network` is true.
    #[serde(default)]
    pub allow_private_network: bool,
}

impl SandboxScope {
    pub fn local_workspace(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            allow_network: false,
            allowed_hosts: vec![],
            allow_web_outbound: false,
            allow_private_network: false,
        }
    }

    /// Enable or disable coarse network (read-only web follows this flag).
    pub fn with_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        self
    }

    /// Grant session-level outbound web (POST/upload/auth-class actions).
    pub fn with_web_outbound(mut self, allow: bool) -> Self {
        self.allow_web_outbound = allow;
        self
    }

    /// Grant session-level private/LAN web reads (`PrivateRead` / egress private network).
    pub fn with_private_network(mut self, allow: bool) -> Self {
        self.allow_private_network = allow;
        self
    }

    pub fn contains(&self, candidate: &Path) -> bool {
        self.contains_target(candidate, false)
    }

    pub fn contains_write_target(&self, candidate: &Path) -> bool {
        self.contains_target(candidate, true)
    }

    fn contains_target(&self, candidate: &Path, allow_missing_leaf: bool) -> bool {
        let Ok(root) = self.workspace_root.canonicalize() else {
            return false;
        };
        let target = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            root.join(candidate)
        };

        if let Ok(target) = target.canonicalize() {
            return target.starts_with(&root);
        }
        if !allow_missing_leaf {
            return false;
        }

        let mut ancestor = target.as_path();
        while !ancestor.exists() {
            let Some(parent) = ancestor.parent() else {
                return false;
            };
            ancestor = parent;
        }
        ancestor
            .canonicalize()
            .is_ok_and(|ancestor| ancestor.starts_with(root))
    }
}

#[derive(Debug, Clone)]
pub struct PolicyEngine {
    scope: SandboxScope,
    /// User overrides applied after fail-closed safety Denies.
    overrides: BTreeMap<ActionKind, PolicyConfigDecision>,
}

impl PolicyEngine {
    pub fn new(scope: SandboxScope) -> Self {
        Self {
            scope,
            overrides: BTreeMap::new(),
        }
    }

    /// Build an engine with validated user policy overrides.
    pub fn with_config(scope: SandboxScope, config: PolicyConfig) -> Self {
        Self {
            scope,
            overrides: config.overrides,
        }
    }

    /// Replace user overrides in place. Scope and fail-closed Denies stay unchanged.
    pub fn reload_config(&mut self, config: PolicyConfig) {
        self.overrides = config.overrides;
    }

    /// Load JSON policy config from path and apply it. On error, prior overrides stay.
    pub fn reload_config_from_path(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<(), PolicyConfigError> {
        let config = PolicyConfig::load_from_path(path)?;
        self.reload_config(config);
        Ok(())
    }

    /// Snapshot current overrides as a PolicyConfig document.
    pub fn config(&self) -> PolicyConfig {
        PolicyConfig {
            version: crate::policy_config::POLICY_CONFIG_VERSION,
            overrides: self.overrides.clone(),
        }
    }

    /// Evaluate an extension manifest permission against this engine's sandbox scope.
    ///
    /// Used at package activate (`permission_eval`). Does not grant `origin=user`
    /// and does not skip NeedsApproval for mutating ActionKinds at action time.
    pub fn evaluate_extension_permission(
        &self,
        permission: impetus_extension_sdk::ExtensionPermission,
    ) -> PolicyDecision {
        crate::extension_policy::evaluate_permission_against_scope(permission, &self.scope)
    }

    pub fn evaluate(&self, action: &Action) -> PolicyDecision {
        match action.kind {
            ActionKind::ReadFile if !self.target_is_in_scope(action, false) => {
                return PolicyDecision::Deny {
                    reason: "read target is missing or outside the workspace scope".into(),
                };
            }
            ActionKind::WriteFile if !self.target_is_in_scope(action, true) => {
                return PolicyDecision::Deny {
                    reason: "write target is outside the workspace scope".into(),
                };
            }
            _ => {}
        }

        // Fail-closed network / private web Denies stay ahead of user overrides.
        match action.kind {
            ActionKind::NetworkConnect | ActionKind::SshConnect | ActionKind::SftpTransfer
                if !self.scope.allow_network =>
            {
                return PolicyDecision::Deny {
                    reason: "network is disabled in this workspace scope".into(),
                };
            }
            ActionKind::WebSearch
            | ActionKind::WebFetch
            | ActionKind::WebDownload
            | ActionKind::WebBrowser
            | ActionKind::WebSubmit
            | ActionKind::WebUpload => {
                if let Some(deny) = self.evaluate_web_hard_deny(action) {
                    return deny;
                }
            }
            _ => {}
        }

        if let Some(overridden) = self.overrides.get(&action.kind) {
            return overridden.to_decision();
        }

        match action.kind {
            ActionKind::ReadFile => PolicyDecision::Allow,
            ActionKind::SpawnProcess if action.origin == ActionOrigin::User => {
                PolicyDecision::Allow
            }
            ActionKind::WriteFile | ActionKind::SpawnProcess | ActionKind::TmuxAttach => {
                PolicyDecision::NeedsApproval {
                    reason: "changes local machine state".into(),
                }
            }
            ActionKind::NetworkConnect | ActionKind::SshConnect | ActionKind::SftpTransfer => {
                PolicyDecision::NeedsApproval {
                    reason: "opens a network connection".into(),
                }
            }
            ActionKind::WebSearch
            | ActionKind::WebFetch
            | ActionKind::WebDownload
            | ActionKind::WebBrowser
            | ActionKind::WebSubmit
            | ActionKind::WebUpload => self.evaluate_web_action(action),
        }
    }

    /// Hard Deny checks for web that must not be softened by user overrides.
    fn evaluate_web_hard_deny(&self, action: &Action) -> Option<PolicyDecision> {
        use crate::web_research::target_requires_private_read;

        if action.kind.web_capability().is_none() {
            return Some(PolicyDecision::Deny {
                reason: "unknown web action".into(),
            });
        }
        if !self.scope.allow_network {
            return Some(PolicyDecision::Deny {
                reason: "network is disabled in this workspace scope".into(),
            });
        }
        let private_target = action
            .target
            .as_deref()
            .is_some_and(target_requires_private_read);
        if private_target && !self.scope.allow_private_network {
            return Some(PolicyDecision::Deny {
                reason: "private/LAN web targets require session private-network allowance".into(),
            });
        }
        None
    }

    fn evaluate_web_action(&self, action: &Action) -> PolicyDecision {
        use crate::web_research::WebCapability;

        if let Some(deny) = self.evaluate_web_hard_deny(action) {
            return deny;
        }

        let capability = action
            .kind
            .web_capability()
            .expect("web hard-deny already checked capability");
        let private_target = action
            .target
            .as_deref()
            .is_some_and(crate::web_research::target_requires_private_read);
        let effective = if private_target && matches!(capability, WebCapability::Read) {
            WebCapability::PrivateRead
        } else {
            capability
        };

        if effective.is_read_only() {
            return PolicyDecision::Allow;
        }
        if self.scope.allow_web_outbound {
            return PolicyDecision::Allow;
        }
        PolicyDecision::NeedsApproval {
            reason: "outbound web requires session allowance or user approval".into(),
        }
    }

    /// Egress policy aligned with this session's private-network grant.
    pub fn egress_policy(&self) -> crate::web_research::EgressPolicy {
        crate::web_research::EgressPolicy::with_private_network(self.scope.allow_private_network)
    }

    pub fn scope(&self) -> &SandboxScope {
        &self.scope
    }

    /// Replay a historical policy decision using a snapshot.
    /// Returns the same decision that would have been made at snapshot time.
    pub fn replay(&self, snapshot: &PolicySnapshot, action: &Action) -> PolicyDecision {
        if snapshot.version != PolicyVersion::V1 {
            return PolicyDecision::Deny {
                reason: format!("unsupported policy version: {:?}", snapshot.version),
            };
        }

        // Reconstruct historical engine state
        let historical = PolicyEngine::new(snapshot.scope.clone());
        historical.evaluate(action)
    }

    fn target_is_in_scope(&self, action: &Action, allow_missing_leaf: bool) -> bool {
        action.target.as_deref().is_some_and(|target| {
            if allow_missing_leaf {
                self.scope.contains_write_target(Path::new(target))
            } else {
                self.scope.contains(Path::new(target))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy_config::{POLICY_CONFIG_VERSION, PolicyConfig};

    #[test]
    fn network_is_denied_when_scope_is_local_only() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::SshConnect,
            summary: "connect".into(),
            target: None,
        });
        assert!(matches!(decision, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn user_started_local_process_does_not_need_a_second_approval() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::User,
            kind: ActionKind::SpawnProcess,
            summary: "open local terminal".into(),
            target: Some("zsh".into()),
        });
        assert_eq!(decision, PolicyDecision::Allow);
    }

    #[test]
    fn agent_started_process_needs_approval() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::SpawnProcess,
            summary: "run formatter".into(),
            target: Some("cargo fmt".into()),
        });
        assert!(matches!(decision, PolicyDecision::NeedsApproval { .. }));
    }

    #[test]
    fn file_target_outside_workspace_is_denied() {
        let workspace = std::env::current_dir().expect("current directory");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace));
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::ReadFile,
            summary: "read outside workspace".into(),
            target: Some("/etc/hosts".into()),
        });
        assert!(matches!(decision, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn new_file_inside_workspace_can_reach_approval() {
        let workspace = std::env::current_dir().expect("current directory");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace));
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WriteFile,
            summary: "create file".into(),
            target: Some("new-file-that-does-not-exist.txt".into()),
        });
        assert!(matches!(decision, PolicyDecision::NeedsApproval { .. }));
    }

    #[test]
    fn fingerprint_changes_when_the_reviewed_action_changes() {
        let action = Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WriteFile,
            summary: "update config".into(),
            target: Some("config.toml".into()),
        };
        let changed_target = Action {
            target: Some("Cargo.toml".into()),
            ..action.clone()
        };

        assert_ne!(action.fingerprint(), changed_target.fingerprint());
        assert_eq!(action.fingerprint(), action.fingerprint());
    }

    #[test]
    fn policy_snapshot_captures_current_state() {
        let workspace = std::env::current_dir().expect("current directory");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.clone()));
        let snapshot = PolicySnapshot::capture(&policy);

        assert_eq!(snapshot.version, PolicyVersion::V1);
        assert_eq!(snapshot.scope.workspace_root, workspace);
        assert!(snapshot.timestamp > 0);
    }

    #[test]
    fn policy_replay_gives_identical_decision() {
        let workspace = std::env::current_dir().expect("current directory");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace));
        let snapshot = PolicySnapshot::capture(&policy);

        let action = Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WriteFile,
            summary: "write file".into(),
            target: Some("test.txt".into()),
        };

        let current_decision = policy.evaluate(&action);
        let replayed_decision = policy.replay(&snapshot, &action);

        assert_eq!(current_decision, replayed_decision);
    }

    #[test]
    fn policy_replay_preserves_historical_scope() {
        let workspace = std::env::current_dir().expect("current directory");
        let old_policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.clone()));
        let snapshot = PolicySnapshot::capture(&old_policy);

        // Simulate policy change: different workspace
        let new_workspace = workspace.join("subdir");
        let new_policy = PolicyEngine::new(SandboxScope::local_workspace(new_workspace));

        let action = Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::ReadFile,
            summary: "read file".into(),
            target: Some("test.txt".into()),
        };

        // Replay uses historical scope, not current
        let replayed = new_policy.replay(&snapshot, &action);
        let historical = old_policy.evaluate(&action);

        assert_eq!(replayed, historical);
    }

    #[test]
    fn policy_replay_rejects_unsupported_version() {
        let workspace = std::env::current_dir().expect("current directory");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.clone()));
        let mut snapshot = PolicySnapshot::capture(&policy);
        snapshot.version = PolicyVersion(999);

        let action = Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::ReadFile,
            summary: "read".into(),
            target: Some("test.txt".into()),
        };

        let decision = policy.replay(&snapshot, &action);
        assert!(
            matches!(decision, PolicyDecision::Deny { reason } if reason.contains("unsupported"))
        );
    }

    #[test]
    fn web_search_allows_when_network_enabled() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace(".").with_network(true));
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WebSearch,
            summary: "search".into(),
            target: Some("web-search:auto".into()),
        });
        assert_eq!(decision, PolicyDecision::Allow);
        assert_eq!(
            ActionKind::WebSearch.web_capability(),
            Some(crate::web_research::WebCapability::Search)
        );
    }

    #[test]
    fn web_fetch_allows_when_network_enabled() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace(".").with_network(true));
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WebFetch,
            summary: "fetch".into(),
            target: Some("example.com".into()),
        });
        assert_eq!(decision, PolicyDecision::Allow);
    }

    #[test]
    fn web_read_denied_when_network_disabled() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        for kind in [ActionKind::WebSearch, ActionKind::WebFetch] {
            let decision = policy.evaluate(&Action {
                origin: ActionOrigin::Agent,
                kind,
                summary: "web".into(),
                target: None,
            });
            assert!(
                matches!(decision, PolicyDecision::Deny { .. }),
                "{kind:?} should deny without network"
            );
        }
    }

    #[test]
    fn web_outbound_needs_approval_by_default() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace(".").with_network(true));
        for kind in [
            ActionKind::WebSubmit,
            ActionKind::WebUpload,
            ActionKind::WebDownload,
            ActionKind::WebBrowser,
        ] {
            let decision = policy.evaluate(&Action {
                origin: ActionOrigin::Agent,
                kind,
                summary: "outbound".into(),
                target: Some("https://example.com/form".into()),
            });
            assert!(
                matches!(
                    decision,
                    PolicyDecision::NeedsApproval { ref reason }
                    if reason.contains("outbound web")
                ),
                "{kind:?} => {decision:?}"
            );
            assert!(
                !kind.web_capability().expect("web cap").is_read_only(),
                "{kind:?} must not be read-only"
            );
        }
    }

    #[test]
    fn web_outbound_allows_when_session_grants() {
        let policy = PolicyEngine::new(
            SandboxScope::local_workspace(".")
                .with_network(true)
                .with_web_outbound(true),
        );
        for kind in [
            ActionKind::WebSubmit,
            ActionKind::WebUpload,
            ActionKind::WebDownload,
            ActionKind::WebBrowser,
        ] {
            let decision = policy.evaluate(&Action {
                origin: ActionOrigin::Agent,
                kind,
                summary: "outbound granted".into(),
                target: Some("https://example.com/upload".into()),
            });
            assert_eq!(decision, PolicyDecision::Allow);
        }
    }

    #[test]
    fn web_outbound_denied_when_network_disabled_even_if_granted() {
        let policy = PolicyEngine::new(
            SandboxScope::local_workspace(".")
                .with_network(false)
                .with_web_outbound(true),
        );
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WebSubmit,
            summary: "submit".into(),
            target: None,
        });
        assert!(matches!(decision, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn policy_snapshot_preserves_web_outbound_flag() {
        let scope = SandboxScope::local_workspace(".")
            .with_network(true)
            .with_web_outbound(true);
        let policy = PolicyEngine::new(scope.clone());
        let snapshot = PolicySnapshot::capture(&policy);
        assert!(snapshot.scope.allow_web_outbound);
        assert_eq!(snapshot.scope, scope);
    }

    #[test]
    fn private_lan_fetch_denied_by_default_even_with_network() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace(".").with_network(true));
        for target in [
            "10.0.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "localhost",
            "router.local",
            "169.254.169.254",
        ] {
            let decision = policy.evaluate(&Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WebFetch,
                summary: "lan fetch".into(),
                target: Some(target.into()),
            });
            assert!(
                matches!(
                    decision,
                    PolicyDecision::Deny { ref reason }
                    if reason.contains("private/LAN")
                ),
                "{target} => {decision:?}"
            );
            assert_eq!(
                Action {
                    origin: ActionOrigin::Agent,
                    kind: ActionKind::WebFetch,
                    summary: "lan".into(),
                    target: Some(target.into()),
                }
                .kind
                .web_capability(),
                Some(crate::web_research::WebCapability::Read)
            );
        }
    }

    #[test]
    fn private_lan_fetch_allows_when_session_grants_private_network() {
        let policy = PolicyEngine::new(
            SandboxScope::local_workspace(".")
                .with_network(true)
                .with_private_network(true),
        );
        for target in ["10.0.0.1", "localhost", "service.internal"] {
            let decision = policy.evaluate(&Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WebFetch,
                summary: "lan fetch granted".into(),
                target: Some(target.into()),
            });
            assert_eq!(decision, PolicyDecision::Allow, "{target}");
        }
        assert!(policy.egress_policy().allow_private_network);
    }

    #[test]
    fn public_fetch_still_allows_without_private_network_grant() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace(".").with_network(true));
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WebFetch,
            summary: "public".into(),
            target: Some("example.com".into()),
        });
        assert_eq!(decision, PolicyDecision::Allow);
        assert!(!policy.egress_policy().allow_private_network);
    }

    #[test]
    fn private_network_denied_when_network_disabled_even_if_granted() {
        let policy = PolicyEngine::new(
            SandboxScope::local_workspace(".")
                .with_network(false)
                .with_private_network(true),
        );
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WebFetch,
            summary: "lan".into(),
            target: Some("10.0.0.1".into()),
        });
        assert!(matches!(decision, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn policy_snapshot_preserves_private_network_flag() {
        let scope = SandboxScope::local_workspace(".")
            .with_network(true)
            .with_private_network(true);
        let policy = PolicyEngine::new(scope.clone());
        let snapshot = PolicySnapshot::capture(&policy);
        assert!(snapshot.scope.allow_private_network);
        assert_eq!(snapshot.scope, scope);
    }

    #[test]
    fn web_policy_reasons_contain_no_secrets() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace(".").with_network(true));
        let secret = "sk-live-super-secret-token";
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WebUpload,
            summary: format!("upload with {secret}"),
            target: Some(format!("https://example.com/?token={secret}")),
        });
        match decision {
            PolicyDecision::NeedsApproval { reason } => {
                assert!(!reason.contains(secret));
                assert!(!reason.contains("sk-live"));
            }
            other => panic!("expected NeedsApproval, got {other:?}"),
        }

        let private_decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WebFetch,
            summary: format!("fetch {secret}"),
            target: Some(format!("http://10.0.0.1/?token={secret}")),
        });
        match private_decision {
            PolicyDecision::Deny { reason } => {
                assert!(!reason.contains(secret));
                assert!(!reason.contains("sk-live"));
                assert!(reason.contains("private/LAN"));
            }
            other => panic!("expected Deny for private target, got {other:?}"),
        }
    }

    #[test]
    fn config_override_allows_write_when_in_scope() {
        let workspace = std::env::current_dir().expect("current directory");
        let config = PolicyConfig::parse(
            r#"{"version":1,"overrides":{"write_file":"allow","spawn_process":"deny"}}"#,
        )
        .expect("config");
        let policy = PolicyEngine::with_config(SandboxScope::local_workspace(workspace), config);

        let write = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WriteFile,
            summary: "write".into(),
            target: Some("new-from-config.txt".into()),
        });
        assert_eq!(write, PolicyDecision::Allow);

        let spawn = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::SpawnProcess,
            summary: "spawn".into(),
            target: Some("echo".into()),
        });
        assert!(
            matches!(
                spawn,
                PolicyDecision::Deny { ref reason } if reason.contains("user policy config")
            ),
            "{spawn:?}"
        );
    }

    #[test]
    fn config_cannot_allow_network_when_scope_disables_it() {
        let config = PolicyConfig::parse(
            r#"{"version":1,"overrides":{"network_connect":"allow","ssh_connect":"allow"}}"#,
        )
        .expect("config");
        let policy = PolicyEngine::with_config(SandboxScope::local_workspace("."), config);
        for kind in [ActionKind::NetworkConnect, ActionKind::SshConnect] {
            let decision = policy.evaluate(&Action {
                origin: ActionOrigin::Agent,
                kind,
                summary: "net".into(),
                target: None,
            });
            assert!(
                matches!(
                    decision,
                    PolicyDecision::Deny { ref reason }
                    if reason.contains("network is disabled")
                ),
                "{kind:?} => {decision:?}"
            );
        }
    }

    #[test]
    fn config_cannot_allow_write_outside_workspace() {
        let workspace = std::env::current_dir().expect("current directory");
        let config = PolicyConfig::parse(r#"{"version":1,"overrides":{"write_file":"allow"}}"#)
            .expect("config");
        let policy = PolicyEngine::with_config(SandboxScope::local_workspace(workspace), config);
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WriteFile,
            summary: "write outside".into(),
            target: Some("/etc/hosts".into()),
        });
        assert!(
            matches!(
                decision,
                PolicyDecision::Deny { ref reason } if reason.contains("outside the workspace")
            ),
            "{decision:?}"
        );
    }

    #[test]
    fn config_override_network_when_scope_allows() {
        let config =
            PolicyConfig::parse(r#"{"version":1,"overrides":{"network_connect":"allow"}}"#)
                .expect("config");
        let policy = PolicyEngine::with_config(
            SandboxScope::local_workspace(".").with_network(true),
            config,
        );
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::NetworkConnect,
            summary: "net".into(),
            target: Some("example.com:443".into()),
        });
        assert_eq!(decision, PolicyDecision::Allow);
    }

    #[test]
    fn absent_config_keeps_fail_closed_defaults() {
        let with_config = PolicyEngine::with_config(
            SandboxScope::local_workspace("."),
            PolicyConfig {
                version: POLICY_CONFIG_VERSION,
                overrides: Default::default(),
            },
        );
        let without = PolicyEngine::new(SandboxScope::local_workspace("."));
        let action = Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::SpawnProcess,
            summary: "run".into(),
            target: Some("ls".into()),
        };
        assert_eq!(with_config.evaluate(&action), without.evaluate(&action));
    }

    #[test]
    fn reload_config_applies_new_overrides() {
        let workspace = std::env::current_dir().expect("current directory");
        let mut policy = PolicyEngine::new(SandboxScope::local_workspace(workspace));
        let write = Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WriteFile,
            summary: "write".into(),
            target: Some("reload-target.txt".into()),
        };
        assert!(matches!(
            policy.evaluate(&write),
            PolicyDecision::NeedsApproval { .. }
        ));

        let config = PolicyConfig::parse(
            r#"{"version":1,"overrides":{"write_file":"allow","spawn_process":"deny"}}"#,
        )
        .expect("config");
        policy.reload_config(config);

        assert_eq!(policy.evaluate(&write), PolicyDecision::Allow);
        let spawn = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::SpawnProcess,
            summary: "spawn".into(),
            target: Some("echo".into()),
        });
        assert!(
            matches!(
                spawn,
                PolicyDecision::Deny { ref reason } if reason.contains("user policy config")
            ),
            "{spawn:?}"
        );
    }

    #[test]
    fn reload_config_from_path_applies_file() {
        let workspace = std::env::current_dir().expect("current directory");
        let mut policy = PolicyEngine::new(SandboxScope::local_workspace(workspace));
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("policy.json");
        std::fs::write(&path, r#"{"version":1,"overrides":{"write_file":"allow"}}"#)
            .expect("write");

        policy
            .reload_config_from_path(&path)
            .expect("reload from path");

        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WriteFile,
            summary: "write".into(),
            target: Some("from-path.txt".into()),
        });
        assert_eq!(decision, PolicyDecision::Allow);
    }

    #[test]
    fn reload_from_path_failure_keeps_prior_overrides() {
        let workspace = std::env::current_dir().expect("current directory");
        let mut policy = PolicyEngine::with_config(
            SandboxScope::local_workspace(workspace),
            PolicyConfig::parse(r#"{"version":1,"overrides":{"write_file":"allow"}}"#)
                .expect("config"),
        );
        let err = policy
            .reload_config_from_path("/nonexistent/policy-config-reload.json")
            .expect_err("missing path");
        assert!(matches!(err, PolicyConfigError::Io(_)));

        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WriteFile,
            summary: "write".into(),
            target: Some("still-allowed.txt".into()),
        });
        assert_eq!(decision, PolicyDecision::Allow);
    }

    #[test]
    fn reload_cannot_soften_fail_closed_network_deny() {
        let mut policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        policy.reload_config(
            PolicyConfig::parse(
                r#"{"version":1,"overrides":{"network_connect":"allow","ssh_connect":"allow"}}"#,
            )
            .expect("config"),
        );
        for kind in [ActionKind::NetworkConnect, ActionKind::SshConnect] {
            let decision = policy.evaluate(&Action {
                origin: ActionOrigin::Agent,
                kind,
                summary: "net".into(),
                target: None,
            });
            assert!(
                matches!(
                    decision,
                    PolicyDecision::Deny { ref reason }
                    if reason.contains("network is disabled")
                ),
                "{kind:?} => {decision:?}"
            );
        }
    }

    #[test]
    fn reload_cannot_soften_private_web_deny() {
        let mut policy = PolicyEngine::new(SandboxScope::local_workspace(".").with_network(true));
        policy.reload_config(
            PolicyConfig::parse(r#"{"version":1,"overrides":{"web_fetch":"allow"}}"#)
                .expect("config"),
        );
        let decision = policy.evaluate(&Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WebFetch,
            summary: "lan".into(),
            target: Some("10.0.0.1".into()),
        });
        assert!(
            matches!(
                decision,
                PolicyDecision::Deny { ref reason } if reason.contains("private/LAN")
            ),
            "{decision:?}"
        );
    }
}
