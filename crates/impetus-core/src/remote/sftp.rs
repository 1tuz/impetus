//! SFTP integration for remote file access.
//!
//! Part of v0.6: SFTP для remote file access через SSH profiles.

use crate::{
    Action, ActionKind, ActionOrigin, AdmittedOperation, EffectAdmission, EffectFenceLedger,
    EffectSeam, NormalizedEffect, SSHProfile,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SftpError {
    #[error("policy denied SFTP operation: {0}")]
    PolicyDenied(String),
    #[error("approval required but not granted")]
    ApprovalRequired,
    #[error("SFTP operation failed: {0}")]
    OperationFailed(String),
    #[error("SSH connection failed: {0}")]
    ConnectionFailed(String),
    #[error("path outside allowed scope: {0}")]
    PathNotAllowed(String),
    #[error("effect fence denied SFTP operation: {0}")]
    EffectFence(String),
}

/// SFTP operation types for policy checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SftpOperation {
    Read,
    Write,
    Delete,
    List,
}

impl SftpOperation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Delete => "delete",
            Self::List => "list",
        }
    }

    /// Write/Delete mutate remote state; Read/List are observe-only for fence policy.
    pub fn is_mutating(self) -> bool {
        matches!(self, Self::Write | Self::Delete)
    }
}

/// SFTP file metadata returned by list operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SftpFileInfo {
    pub path: PathBuf,
    pub size: u64,
    pub is_dir: bool,
    pub modified: Option<u64>, // Unix timestamp
}

/// SFTP session for remote file operations.
/// Currently a stub; real SSH/SFTP implementation is future work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SftpSession {
    pub profile: SSHProfile,
    pub origin: ActionOrigin,
    pub connected: bool,
}

impl SftpSession {
    pub fn new(profile: SSHProfile, origin: ActionOrigin) -> Self {
        Self {
            profile,
            origin,
            connected: false,
        }
    }

    /// Simulate connection. Real implementation would establish SSH + SFTP channel.
    pub fn connect(&mut self) -> Result<(), SftpError> {
        // TODO: Real SSH/SFTP connection via openssh-sftp-client or ssh2
        self.connected = true;
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn disconnect(&mut self) {
        self.connected = false;
    }
}

/// SFTP operation request with policy check and SSH profile.
pub struct SftpOperationRequest {
    pub profile: SSHProfile,
    pub operation: SftpOperation,
    pub path: PathBuf,
    pub origin: ActionOrigin,
    pub intent_revision: u64,
}

impl SftpOperationRequest {
    pub fn new(
        profile: SSHProfile,
        operation: SftpOperation,
        path: impl Into<PathBuf>,
        origin: ActionOrigin,
        intent_revision: u64,
    ) -> Self {
        Self {
            profile,
            operation,
            path: path.into(),
            origin,
            intent_revision,
        }
    }

    /// Normalized effect identity for admission and Effect Fence digests.
    pub fn fence_effect(&self) -> NormalizedEffect {
        let target = format!(
            "{}@{}:{}",
            self.profile.user,
            self.profile.host,
            self.path.display()
        );

        let summary = format!(
            "SFTP {} on {}@{}:{}",
            self.operation.as_str(),
            self.profile.user,
            self.profile.host,
            self.path.display()
        );

        NormalizedEffect {
            origin: self.origin,
            capability: crate::EffectCapability::NetworkConnect,
            version: crate::CapabilityVersion::V1,
            action: Action {
                origin: self.origin,
                kind: ActionKind::SftpTransfer,
                summary,
                target: Some(target),
            },
        }
    }

    /// Prepare SFTP operation through policy and effect seam.
    /// Returns either immediate Allow with token, NeedsApproval, or Deny.
    pub fn request(&self, seam: &EffectSeam) -> Result<EffectAdmission, SftpError> {
        Ok(seam.request(self.fence_effect(), self.intent_revision))
    }

    /// Execute SFTP operation after admission.
    /// Currently a stub; real implementation would use ssh2 or openssh-sftp-client.
    pub fn execute(&self, _admission: &AdmittedOperation) -> Result<SftpResult, SftpError> {
        if !self.connected_check() {
            return Err(SftpError::ConnectionFailed(
                "SFTP session not connected".into(),
            ));
        }

        // TODO: Real SFTP operation via ssh2::sftp or openssh-sftp-client
        match self.operation {
            SftpOperation::Read => Ok(SftpResult::Read {
                path: self.path.clone(),
                content: Vec::new(),
            }),
            SftpOperation::Write => Ok(SftpResult::Write {
                path: self.path.clone(),
                bytes_written: 0,
            }),
            SftpOperation::Delete => Ok(SftpResult::Delete {
                path: self.path.clone(),
            }),
            SftpOperation::List => Ok(SftpResult::List {
                path: self.path.clone(),
                entries: Vec::new(),
            }),
        }
    }

    fn connected_check(&self) -> bool {
        // TODO: Check actual SFTP session state
        true
    }
}

/// SFTP operation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SftpResult {
    Read {
        path: PathBuf,
        content: Vec<u8>,
    },
    Write {
        path: PathBuf,
        bytes_written: u64,
    },
    Delete {
        path: PathBuf,
    },
    List {
        path: PathBuf,
        entries: Vec<SftpFileInfo>,
    },
}

/// SFTP session manager coordinates SSH, policy, and operation execution.
///
/// Agent-origin mutating Allow (`Write`/`Delete`) with a ledger runs under
/// write-ahead Effect Fence. User-origin and Read/List stay unfenced.
pub struct SftpSessionManager {
    seam: EffectSeam,
}

impl SftpSessionManager {
    pub fn new(seam: EffectSeam) -> Self {
        Self { seam }
    }

    /// Create and connect SFTP session.
    pub fn create_session(
        &self,
        profile: SSHProfile,
        origin: ActionOrigin,
    ) -> Result<SftpSession, SftpError> {
        let mut session = SftpSession::new(profile, origin);
        session.connect()?;
        Ok(session)
    }

    /// Request SFTP operation admission.
    pub fn request(&self, req: &SftpOperationRequest) -> Result<EffectAdmission, SftpError> {
        req.request(&self.seam)
    }

    /// Execute SFTP operation with admission token.
    pub fn execute(
        &self,
        req: &SftpOperationRequest,
        admission: &AdmittedOperation,
    ) -> Result<SftpResult, SftpError> {
        req.execute(admission)
    }

    /// Request and execute in one call (convenience wrapper, unfenced).
    pub fn execute_with_admission(
        &self,
        req: &SftpOperationRequest,
    ) -> Result<SftpResult, SftpError> {
        self.execute_with_admission_and_optional_fence(req, None)
    }

    /// Agent-origin mutating Allow → write-ahead fence. User / Read / List skip fence.
    pub fn execute_with_admission_and_fence(
        &self,
        req: &SftpOperationRequest,
        ledger: &EffectFenceLedger,
    ) -> Result<SftpResult, SftpError> {
        self.execute_with_admission_and_optional_fence(req, Some(ledger))
    }

    fn execute_with_admission_and_optional_fence(
        &self,
        req: &SftpOperationRequest,
        ledger: Option<&EffectFenceLedger>,
    ) -> Result<SftpResult, SftpError> {
        let admission = self.request(req)?;
        match admission {
            EffectAdmission::Allow(token) => {
                let needs_fence = req.origin == ActionOrigin::Agent
                    && req.operation.is_mutating()
                    && ledger.is_some();
                if needs_fence {
                    let ledger = ledger.expect("checked");
                    let effect = req.fence_effect();
                    let identity = self
                        .seam
                        .prepare_fence_for_allowed(&effect, ledger)
                        .map_err(SftpError::EffectFence)?;
                    match self.execute(req, &token) {
                        Ok(result) => {
                            if let Err(err) = ledger.observe(&identity, "sftp executed") {
                                let _ = ledger.mark_unknown(
                                    &identity,
                                    format!("observe persist failed after sftp: {err}"),
                                );
                                return Err(SftpError::EffectFence(format!(
                                    "effect fence observe failed after sftp: {err}"
                                )));
                            }
                            Ok(result)
                        }
                        Err(error) => {
                            let _ = ledger.mark_failed(&identity, error.to_string());
                            Err(error)
                        }
                    }
                } else {
                    self.execute(req, &token)
                }
            }
            EffectAdmission::NeedsApproval(_) => Err(SftpError::ApprovalRequired),
            EffectAdmission::Deny { reason } => Err(SftpError::PolicyDenied(reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyEngine, SSHKeyReference, Sandbox, SandboxScope};

    fn test_profile() -> SSHProfile {
        SSHProfile {
            host: "example.com".into(),
            port: 22,
            user: "testuser".into(),
            host_key_fingerprint: None,
            key_reference: SSHKeyReference::KeychainItem {
                service: "ssh".into(),
                account: "testuser@example.com".into(),
            },
        }
    }

    fn test_seam() -> EffectSeam {
        let workspace = std::env::temp_dir();
        let mut scope = SandboxScope::local_workspace(workspace.clone());
        scope.allow_network = true;
        let policy = PolicyEngine::new(scope.clone());
        let sandbox = Sandbox::Provisioned { scope };
        EffectSeam::with_sandbox(policy, sandbox)
    }

    #[test]
    fn sftp_session_lifecycle() {
        let profile = test_profile();
        let mut session = SftpSession::new(profile.clone(), ActionOrigin::User);

        assert!(!session.is_connected());
        session.connect().unwrap();
        assert!(session.is_connected());
        session.disconnect();
        assert!(!session.is_connected());
    }

    #[test]
    fn sftp_read_request_creates_correct_action() {
        let seam = test_seam();
        let profile = test_profile();
        let request = SftpOperationRequest::new(
            profile,
            SftpOperation::Read,
            "/remote/file.txt",
            ActionOrigin::Agent,
            1,
        );

        let result = request.request(&seam);
        assert!(result.is_ok());

        match result.unwrap() {
            EffectAdmission::NeedsApproval(deferred) => {
                let action = &deferred.approval().action;
                assert_eq!(action.kind, ActionKind::SftpTransfer);
                assert_eq!(action.origin, ActionOrigin::Agent);
                assert!(action.summary.contains("SFTP read"));
            }
            EffectAdmission::Allow(_) => {
                panic!("expected needs approval for agent SFTP read, got Allow")
            }
            EffectAdmission::Deny { reason } => {
                panic!(
                    "expected needs approval for agent SFTP read, got Deny: {}",
                    reason
                )
            }
        }
    }

    #[test]
    fn sftp_write_requires_approval_for_agent() {
        let seam = test_seam();
        let profile = test_profile();
        let request = SftpOperationRequest::new(
            profile,
            SftpOperation::Write,
            "/remote/file.txt",
            ActionOrigin::Agent,
            1,
        );

        let admission = request.request(&seam).unwrap();
        match admission {
            EffectAdmission::Allow(_) => {
                panic!("agent origin SFTP write should require approval")
            }
            EffectAdmission::NeedsApproval(deferred) => {
                assert_eq!(deferred.effect().origin, ActionOrigin::Agent);
            }
            EffectAdmission::Deny { .. } => {
                // Also acceptable
            }
        }
    }

    #[test]
    fn sftp_manager_creates_session() {
        let seam = test_seam();
        let manager = SftpSessionManager::new(seam);
        let profile = test_profile();

        let session = manager.create_session(profile, ActionOrigin::User).unwrap();
        assert!(session.is_connected());
    }

    fn network_allow_seam() -> EffectSeam {
        use crate::{PolicyConfig, PolicyEngine, Sandbox, SandboxScope};
        let workspace = std::env::temp_dir();
        let mut scope = SandboxScope::local_workspace(workspace);
        scope.allow_network = true;
        let config = PolicyConfig::parse(
            r#"{"version":1,"overrides":{"network_connect":"allow","ssh_connect":"allow","sftp_transfer":"allow"}}"#,
        )
        .unwrap();
        let policy = PolicyEngine::with_config(scope.clone(), config);
        EffectSeam::with_sandbox(policy, Sandbox::Provisioned { scope })
    }

    #[test]
    fn agent_sftp_write_refuses_when_prior_unknown() {
        use crate::{
            EffectFenceLedger, EffectInvocationIdentity, EventStore, FenceState, MemoryEventStore,
            records_from_events,
        };
        use std::sync::Arc;
        use uuid::Uuid;

        let manager = SftpSessionManager::new(network_allow_seam());
        let events: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let session = Uuid::new_v4();
        let ledger = EffectFenceLedger::new(Arc::clone(&events), session);

        let request = SftpOperationRequest::new(
            test_profile(),
            SftpOperation::Write,
            "/remote/file.txt",
            ActionOrigin::Agent,
            1,
        );
        let effect = request.fence_effect();
        let identity = EffectInvocationIdentity::from_effect(&effect);
        ledger.prepare_and_start(&identity).expect("prepare");
        ledger
            .mark_unknown(&identity, "crash mid-flight")
            .expect("unknown");

        let err = manager
            .execute_with_admission_and_fence(&request, &ledger)
            .expect_err("Unknown fence must refuse");
        assert!(
            matches!(err, SftpError::EffectFence(ref reason) if reason.contains("Unknown") || reason.contains("refuse")),
            "expected fence refuse, got {err:?}"
        );

        let records = records_from_events(&events.list(session).expect("list"));
        assert!(
            records.values().any(|r| r.state == FenceState::Unknown
                && r.identity.args_digest == identity.args_digest),
            "durable Unknown fence must remain"
        );
    }

    #[test]
    fn agent_sftp_write_emits_fence_observed() {
        use crate::{
            EffectFenceLedger, EventStore, FenceState, MemoryEventStore, args_digest_for_effect,
            records_from_events,
        };
        use std::sync::Arc;
        use uuid::Uuid;

        let manager = SftpSessionManager::new(network_allow_seam());
        let events: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let session = Uuid::new_v4();
        let ledger = EffectFenceLedger::new(Arc::clone(&events), session);

        let request = SftpOperationRequest::new(
            test_profile(),
            SftpOperation::Write,
            "/remote/file.txt",
            ActionOrigin::Agent,
            1,
        );
        manager
            .execute_with_admission_and_fence(&request, &ledger)
            .expect("agent write + fence");

        let effect = request.fence_effect();
        let records = records_from_events(&events.list(session).expect("list"));
        assert!(
            records.values().any(|r| r.state == FenceState::Observed
                && r.identity.args_digest == args_digest_for_effect(&effect)),
            "expected Observed fence; records={records:?}"
        );
    }
}
