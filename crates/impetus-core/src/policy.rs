use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionOrigin {
    User,
    Agent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    ReadFile,
    WriteFile,
    SpawnProcess,
    NetworkConnect,
    SshConnect,
    SftpTransfer,
    TmuxAttach,
    WebSearch,
    WebFetch,
    WebDownload,
    WebBrowser,
    WebSubmit,
    WebUpload,
}

impl ActionKind {
    /// Classify action for parallel execution safety
    pub fn execution_semantics(&self) -> crate::module::ExecutionSemantics {
        use crate::module::ExecutionSemantics;
        match self {
            ActionKind::ReadFile => ExecutionSemantics::ReadOnly,
            ActionKind::WriteFile => ExecutionSemantics::Mutating,
            ActionKind::SpawnProcess => ExecutionSemantics::NonReplayable,
            ActionKind::NetworkConnect => ExecutionSemantics::Idempotent,
            ActionKind::SshConnect => ExecutionSemantics::NonReplayable,
            ActionKind::SftpTransfer => ExecutionSemantics::Mutating,
            ActionKind::TmuxAttach => ExecutionSemantics::NonReplayable,
            ActionKind::WebSearch => ExecutionSemantics::Idempotent,
            ActionKind::WebFetch => ExecutionSemantics::ReadOnly,
            ActionKind::WebDownload => ExecutionSemantics::Mutating,
            ActionKind::WebBrowser => ExecutionSemantics::NonReplayable,
            ActionKind::WebSubmit => ExecutionSemantics::Mutating,
            ActionKind::WebUpload => ExecutionSemantics::Mutating,
        }
    }

    /// Check if action can be executed in parallel with other actions
    pub fn can_parallelize(&self) -> bool {
        matches!(
            self.execution_semantics(),
            crate::module::ExecutionSemantics::ReadOnly
                | crate::module::ExecutionSemantics::Idempotent
        )
    }

    /// Map web action kinds onto the fine-grained [`WebCapability`] model.
    pub fn web_capability(&self) -> Option<crate::web_research::WebCapability> {
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
pub struct Action {
    pub origin: ActionOrigin,
    pub kind: ActionKind,
    pub summary: String,
    pub target: Option<String>,
}

/// A stable digest of the complete, normalized action that a person reviews.
/// It is persisted with an approval so a different action cannot reuse it.
/// Includes capability version to prevent reuse across incompatible changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct ActionFingerprint(String);

impl ActionFingerprint {
    pub fn for_action(action: &Action) -> Self {
        Self::for_action_with_version(action, None)
    }

    pub fn for_action_with_version(action: &Action, version: Option<u32>) -> Self {
        let mut payload = serde_json::to_vec(action).expect("action serialization is infallible");
        if let Some(v) = version {
            payload.extend_from_slice(b"\0version:");
            payload.extend_from_slice(v.to_string().as_bytes());
        }
        let digest = Sha256::digest([b"impetus-action-v1\0".as_slice(), &payload].concat());
        Self(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

impl Action {
    pub fn fingerprint(&self) -> ActionFingerprint {
        ActionFingerprint::for_action(self)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    NeedsApproval { reason: String },
    Deny { reason: String },
}

#[derive(Debug, Clone)]
pub struct PolicyEngine {
    scope: SandboxScope,
}

impl PolicyEngine {
    pub fn new(scope: SandboxScope) -> Self {
        Self { scope }
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
                if !self.scope.allow_network {
                    PolicyDecision::Deny {
                        reason: "network is disabled in this workspace scope".into(),
                    }
                } else {
                    PolicyDecision::NeedsApproval {
                        reason: "opens a network connection".into(),
                    }
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

    fn evaluate_web_action(&self, action: &Action) -> PolicyDecision {
        use crate::web_research::{WebCapability, target_requires_private_read};

        let Some(capability) = action.kind.web_capability() else {
            return PolicyDecision::Deny {
                reason: "unknown web action".into(),
            };
        };
        if !self.scope.allow_network {
            return PolicyDecision::Deny {
                reason: "network is disabled in this workspace scope".into(),
            };
        }

        let private_target = action
            .target
            .as_deref()
            .is_some_and(target_requires_private_read);
        if private_target && !self.scope.allow_private_network {
            return PolicyDecision::Deny {
                reason: "private/LAN web targets require session private-network allowance".into(),
            };
        }

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
}
