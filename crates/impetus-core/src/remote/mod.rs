//! Remote connection capabilities: SSH profiles, host-key verification, tmux sessions, SFTP, and durable approvals.
//!
//! This module implements v0.6:
//! - Task 1: SSH profiles with host-key verification and Keychain integration
//! - Task 2: Controlled process/PTY execution (in execution module)
//! - Task 3: tmux integration for persistent remote sessions
//! - Task 4: SFTP for remote file access
//! - Durable approval saves host-key acceptance and survives restart

mod profile;
mod sftp;
mod storage;
mod tmux;
mod tmux_storage;

pub use profile::{HostKeyFingerprint, HostKeyVerificationError, SSHKeyReference, SSHProfile};
pub use sftp::{
    SftpError, SftpFileInfo, SftpOperation, SftpOperationRequest, SftpResult, SftpSession,
    SftpSessionManager,
};
pub use storage::{SSHApproval, SSHApprovalStore, SSHApprovalStoreError, SqliteSSHApprovalStore};
pub use tmux::{
    TmuxError, TmuxSession, TmuxSessionId, TmuxSessionManager, TmuxSessionRequest, TmuxSessionState,
};
pub use tmux_storage::{
    SqliteTmuxSessionStore, TmuxSessionRecord, TmuxSessionStore, TmuxSessionStoreError,
};

use crate::{Action, ActionKind, ActionOrigin, EffectAdmission, EffectSeam, NormalizedEffect};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SSHConnectionError {
    #[error("host key verification failed: {0}")]
    HostKeyVerificationFailed(#[from] HostKeyVerificationError),
    #[error("keychain unavailable or SSH key not found: {0}")]
    KeychainError(String),
    #[error("policy denied SSH connection: {0}")]
    PolicyDenied(String),
    #[error("approval required but not granted")]
    ApprovalRequired,
    #[error("SSH connection failed: {0}")]
    ConnectionFailed(String),
    #[error("approval store error: {0}")]
    ApprovalStoreError(#[from] SSHApprovalStoreError),
}

/// SSH connection request with policy check and host-key verification.
/// Before any network connection:
/// 1. Verify host key matches profile (if known)
/// 2. Check policy decision (needs approval for agent-initiated connections)
/// 3. If approved, save host-key acceptance durably
pub struct SSHConnectionRequest {
    pub profile: SSHProfile,
    pub origin: ActionOrigin,
    pub intent_revision: u64,
}

impl SSHConnectionRequest {
    pub fn new(profile: SSHProfile, origin: ActionOrigin, intent_revision: u64) -> Self {
        Self {
            profile,
            origin,
            intent_revision,
        }
    }

    /// Prepare SSH connection through policy and effect seam.
    /// Returns either immediate Allow, NeedsApproval with deferred effect, or Deny.
    pub fn request(&self, seam: &EffectSeam) -> Result<EffectAdmission, SSHConnectionError> {
        let action = Action {
            origin: self.origin,
            kind: ActionKind::SshConnect,
            summary: format!(
                "SSH connect to {}@{}:{}",
                self.profile.user, self.profile.host, self.profile.port
            ),
            target: Some(format!(
                "{}@{}:{}",
                self.profile.user, self.profile.host, self.profile.port
            )),
        };

        let effect = NormalizedEffect::ssh_connect(
            self.origin,
            action.summary.clone(),
            action.target.clone().unwrap(),
        );

        Ok(seam.request(effect, self.intent_revision))
    }

    /// Verify host key matches the expected fingerprint in the profile.
    /// Returns Ok if matches or no fingerprint is set (first connection).
    /// Returns Err if mismatch detected.
    pub fn verify_host_key(
        &self,
        presented_key: &[u8],
    ) -> Result<HostKeyFingerprint, SSHConnectionError> {
        let presented_fingerprint = HostKeyFingerprint::from_public_key(presented_key);

        if let Some(expected) = &self.profile.host_key_fingerprint
            && expected != &presented_fingerprint
        {
            return Err(SSHConnectionError::HostKeyVerificationFailed(
                HostKeyVerificationError::Mismatch {
                    expected: expected.clone(),
                    presented: presented_fingerprint,
                },
            ));
        }

        Ok(presented_fingerprint)
    }

    /// Save host-key acceptance durably after user approval.
    /// This makes the approval survive process restart.
    pub async fn save_host_key_approval(
        &self,
        fingerprint: &HostKeyFingerprint,
        store: &dyn SSHApprovalStore,
    ) -> Result<(), SSHConnectionError> {
        let approval = SSHApproval {
            host: self.profile.host.clone(),
            port: self.profile.port,
            user: self.profile.user.clone(),
            host_key_fingerprint: fingerprint.clone(),
            approved_at_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time after epoch")
                .as_millis() as u64,
        };

        store.save_approval(&approval).await?;
        Ok(())
    }

    /// Check if host-key approval exists in durable storage.
    /// Returns Some(fingerprint) if approved, None otherwise.
    pub async fn check_existing_approval(
        &self,
        store: &dyn SSHApprovalStore,
    ) -> Result<Option<HostKeyFingerprint>, SSHConnectionError> {
        Ok(store
            .get_approval(&self.profile.host, self.profile.port, &self.profile.user)
            .await?
            .map(|approval| approval.host_key_fingerprint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyEngine, Sandbox, SandboxScope};

    fn test_profile() -> SSHProfile {
        SSHProfile {
            host: "example.com".into(),
            user: "testuser".into(),
            port: 22,
            host_key_fingerprint: None,
            key_reference: SSHKeyReference::KeychainItem {
                service: "ssh".into(),
                account: "testuser".into(),
            },
        }
    }

    #[test]
    fn ssh_request_creates_correct_action() {
        let workspace = std::env::temp_dir();
        let mut scope = SandboxScope::local_workspace(workspace);
        scope.allow_network = true;

        let policy = PolicyEngine::new(scope.clone());
        let seam = EffectSeam::with_sandbox(policy, Sandbox::Provisioned { scope });

        let profile = test_profile();
        let request = SSHConnectionRequest::new(profile, ActionOrigin::Agent, 1);

        let result = request.request(&seam);
        assert!(result.is_ok());

        match result.unwrap() {
            EffectAdmission::NeedsApproval(deferred) => {
                let action = &deferred.approval().action;
                assert_eq!(action.kind, ActionKind::SshConnect);
                assert_eq!(action.origin, ActionOrigin::Agent);
                assert!(action.summary.contains("testuser@example.com:22"));
            }
            _ => panic!("expected needs approval for agent SSH connection"),
        }
    }

    #[test]
    fn host_key_verification_passes_for_matching_fingerprint() {
        let key_bytes = b"ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ";
        let fingerprint = HostKeyFingerprint::from_public_key(key_bytes);

        let profile = SSHProfile {
            host_key_fingerprint: Some(fingerprint.clone()),
            ..test_profile()
        };

        let request = SSHConnectionRequest::new(profile, ActionOrigin::User, 1);
        let result = request.verify_host_key(key_bytes);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), fingerprint);
    }

    #[test]
    fn host_key_verification_fails_on_mismatch() {
        let original_key = b"ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ";
        let different_key = b"ssh-rsa AAAAB3NzaC1yc2EXXXXXXXXXXXXXXXXX";

        let original_fingerprint = HostKeyFingerprint::from_public_key(original_key);

        let profile = SSHProfile {
            host_key_fingerprint: Some(original_fingerprint.clone()),
            ..test_profile()
        };

        let request = SSHConnectionRequest::new(profile, ActionOrigin::User, 1);
        let result = request.verify_host_key(different_key);

        assert!(result.is_err());
        match result.unwrap_err() {
            SSHConnectionError::HostKeyVerificationFailed(HostKeyVerificationError::Mismatch {
                expected,
                presented,
            }) => {
                assert_eq!(expected, original_fingerprint);
                assert_ne!(presented, original_fingerprint);
            }
            _ => panic!("expected host key mismatch error"),
        }
    }

    #[test]
    fn host_key_verification_passes_on_first_connection() {
        let key_bytes = b"ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ";

        let profile = SSHProfile {
            host_key_fingerprint: None, // First connection
            ..test_profile()
        };

        let request = SSHConnectionRequest::new(profile, ActionOrigin::User, 1);
        let result = request.verify_host_key(key_bytes);

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn save_host_key_approval_stores_fingerprint() {
        use crate::remote::SqliteSSHApprovalStore;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("ssh_approvals.db");
        let store = SqliteSSHApprovalStore::open(&db_path).unwrap();

        let profile = test_profile();
        let request = SSHConnectionRequest::new(profile.clone(), ActionOrigin::Agent, 1);
        let key_bytes = b"test_public_key";
        let fingerprint = HostKeyFingerprint::from_public_key(key_bytes);

        request
            .save_host_key_approval(&fingerprint, store.as_ref())
            .await
            .unwrap();

        let retrieved = store
            .get_approval(&profile.host, profile.port, &profile.user)
            .await
            .unwrap();

        assert!(retrieved.is_some());
        let approval = retrieved.unwrap();
        assert_eq!(approval.host, profile.host);
        assert_eq!(approval.port, profile.port);
        assert_eq!(approval.user, profile.user);
        assert_eq!(approval.host_key_fingerprint, fingerprint);
    }

    #[tokio::test]
    async fn check_existing_approval_returns_none_when_not_found() {
        use crate::remote::SqliteSSHApprovalStore;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("ssh_approvals.db");
        let store = SqliteSSHApprovalStore::open(&db_path).unwrap();

        let profile = test_profile();
        let request = SSHConnectionRequest::new(profile, ActionOrigin::User, 1);

        let result = request
            .check_existing_approval(store.as_ref())
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn check_existing_approval_returns_fingerprint_when_found() {
        use crate::remote::SqliteSSHApprovalStore;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("ssh_approvals.db");
        let store = SqliteSSHApprovalStore::open(&db_path).unwrap();

        let profile = test_profile();
        let request = SSHConnectionRequest::new(profile.clone(), ActionOrigin::Agent, 1);
        let key_bytes = b"stored_key";
        let fingerprint = HostKeyFingerprint::from_public_key(key_bytes);

        request
            .save_host_key_approval(&fingerprint, store.as_ref())
            .await
            .unwrap();

        let result = request
            .check_existing_approval(store.as_ref())
            .await
            .unwrap();
        assert_eq!(result, Some(fingerprint));
    }

    #[test]
    fn ssh_request_with_user_origin() {
        let workspace = std::env::temp_dir();
        let mut scope = SandboxScope::local_workspace(workspace);
        scope.allow_network = true;

        let policy = PolicyEngine::new(scope.clone());
        let seam = EffectSeam::with_sandbox(policy, Sandbox::Provisioned { scope });

        let profile = test_profile();
        let request = SSHConnectionRequest::new(profile, ActionOrigin::User, 1);

        let result = request.request(&seam);
        assert!(result.is_ok());
    }

    #[test]
    fn ssh_request_includes_target_info() {
        let workspace = std::env::temp_dir();
        let mut scope = SandboxScope::local_workspace(workspace);
        scope.allow_network = true;

        let policy = PolicyEngine::new(scope.clone());
        let seam = EffectSeam::with_sandbox(policy, Sandbox::Provisioned { scope });

        let profile = SSHProfile {
            host: "remote.server.com".into(),
            user: "admin".into(),
            port: 2222,
            host_key_fingerprint: None,
            key_reference: SSHKeyReference::KeychainItem {
                service: "ssh".into(),
                account: "admin".into(),
            },
        };

        let request = SSHConnectionRequest::new(profile, ActionOrigin::Agent, 5);
        let result = request.request(&seam).unwrap();

        match result {
            EffectAdmission::NeedsApproval(deferred) => {
                let action = &deferred.approval().action;
                assert!(action.summary.contains("admin@remote.server.com:2222"));
                assert_eq!(action.target, Some("admin@remote.server.com:2222".into()));
            }
            _ => panic!("expected needs approval"),
        }
    }

    #[test]
    fn host_key_verification_different_keys_produce_different_fingerprints() {
        let key1 = b"ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ";
        let key2 = b"ssh-rsa BBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

        let fp1 = HostKeyFingerprint::from_public_key(key1);
        let fp2 = HostKeyFingerprint::from_public_key(key2);

        assert_ne!(fp1, fp2);
    }

    #[test]
    fn host_key_verification_same_key_produces_same_fingerprint() {
        let key = b"ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ";

        let fp1 = HostKeyFingerprint::from_public_key(key);
        let fp2 = HostKeyFingerprint::from_public_key(key);

        assert_eq!(fp1, fp2);
    }

    #[tokio::test]
    async fn save_host_key_approval_updates_timestamp() {
        use crate::remote::SqliteSSHApprovalStore;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("ssh_approvals.db");
        let store = SqliteSSHApprovalStore::open(&db_path).unwrap();

        let profile = test_profile();
        let request = SSHConnectionRequest::new(profile.clone(), ActionOrigin::Agent, 1);
        let key_bytes = b"test_key";
        let fingerprint = HostKeyFingerprint::from_public_key(key_bytes);

        request
            .save_host_key_approval(&fingerprint, store.as_ref())
            .await
            .unwrap();

        let approval = store
            .get_approval(&profile.host, profile.port, &profile.user)
            .await
            .unwrap()
            .unwrap();

        assert!(approval.approved_at_unix_ms > 0);
    }
}
