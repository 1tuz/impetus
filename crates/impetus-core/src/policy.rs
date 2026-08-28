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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    ReadFile,
    WriteFile,
    SpawnProcess,
    NetworkConnect,
    SshConnect,
    SftpTransfer,
    TmuxAttach,
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
}

impl SandboxScope {
    pub fn local_workspace(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            allow_network: false,
            allowed_hosts: vec![],
        }
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
        }
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
}
