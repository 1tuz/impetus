//! tmux integration for persistent remote sessions over SSH.
//!
//! This module implements v0.6 task 3: tmux integration.
//! - TmuxSession lifecycle: create, attach, detach, list, kill
//! - Remote command execution через SSH + tmux
//! - Durable session state survives harness restart
//! - Policy check for tmux session creation

use crate::{
    ActionKind, ActionOrigin, EffectAdmission, EffectFenceLedger, EffectSeam, NormalizedEffect,
    SSHConnectionError, SSHProfile,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TmuxSessionId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TmuxSessionState {
    Creating,
    Active { windows: usize },
    Detached { windows: usize },
    Dead,
}

#[derive(Debug, Error)]
pub enum TmuxError {
    #[error("policy denied tmux session: {0}")]
    PolicyDenied(String),
    #[error("approval required but not granted")]
    ApprovalRequired,
    #[error("SSH connection error: {0}")]
    SshError(#[from] SSHConnectionError),
    #[error("tmux session not found: {0}")]
    SessionNotFound(String),
    #[error("tmux command failed: {0}")]
    CommandFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("effect fence denied tmux session: {0}")]
    EffectFence(String),
}

/// tmux session on a remote host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmuxSession {
    pub id: TmuxSessionId,
    pub name: String,
    pub ssh_profile: SSHProfile,
    pub working_dir: Option<PathBuf>,
    pub initial_command: Option<String>,
    pub state: TmuxSessionState,
    pub origin: ActionOrigin,
    pub created_at_unix_ms: u64,
}

impl TmuxSession {
    pub fn new(
        id: TmuxSessionId,
        name: impl Into<String>,
        ssh_profile: SSHProfile,
        origin: ActionOrigin,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            ssh_profile,
            working_dir: None,
            initial_command: None,
            state: TmuxSessionState::Creating,
            origin,
            created_at_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time after epoch")
                .as_millis() as u64,
        }
    }

    pub fn with_working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }

    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.initial_command = Some(command.into());
        self
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            TmuxSessionState::Active { .. } | TmuxSessionState::Detached { .. }
        )
    }
}

/// Request to create a new tmux session on a remote host.
pub struct TmuxSessionRequest {
    pub session: TmuxSession,
    pub intent_revision: u64,
}

impl TmuxSessionRequest {
    pub fn new(session: TmuxSession, intent_revision: u64) -> Self {
        Self {
            session,
            intent_revision,
        }
    }

    /// Normalized effect for create admission (ssh_connect capability).
    pub fn fence_effect(&self) -> NormalizedEffect {
        self.fence_effect_for_op("create")
    }

    /// Distinct digests per mutating op so create→attach→kill are not replays.
    pub fn fence_effect_for_op(&self, op: &str) -> NormalizedEffect {
        let summary = format!(
            "tmux {} session '{}' on {}@{}:{}",
            op,
            self.session.name,
            self.session.ssh_profile.user,
            self.session.ssh_profile.host,
            self.session.ssh_profile.port
        );

        let target = format!(
            "{}|{}|{}@{}:{}",
            op,
            self.session.name,
            self.session.ssh_profile.user,
            self.session.ssh_profile.host,
            self.session.ssh_profile.port
        );

        match op {
            "attach" => NormalizedEffect {
                origin: self.session.origin,
                capability: crate::EffectCapability::NetworkConnect,
                version: crate::CapabilityVersion::V1,
                action: crate::Action {
                    origin: self.session.origin,
                    kind: ActionKind::TmuxAttach,
                    summary,
                    target: Some(target),
                },
            },
            _ => NormalizedEffect::ssh_connect(self.session.origin, summary, target),
        }
    }

    /// Prepare tmux session creation through policy and effect seam.
    pub fn request(&self, seam: &EffectSeam) -> Result<EffectAdmission, TmuxError> {
        Ok(seam.request(self.fence_effect(), self.intent_revision))
    }
}

/// tmux session manager coordinating SSH, policy, and durable storage.
///
/// Agent-origin create/attach/kill with a ledger runs under write-ahead Effect
/// Fence. User-origin stays unfenced.
pub struct TmuxSessionManager {
    seam: EffectSeam,
    sessions:
        std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<TmuxSessionId, TmuxSession>>>,
    next_id: std::sync::Arc<tokio::sync::Mutex<u64>>,
}

impl TmuxSessionManager {
    pub fn new(seam: EffectSeam) -> Self {
        Self {
            seam,
            sessions: std::sync::Arc::new(
                tokio::sync::Mutex::new(std::collections::HashMap::new()),
            ),
            next_id: std::sync::Arc::new(tokio::sync::Mutex::new(1)),
        }
    }

    /// Request a new tmux session through policy.
    pub async fn request(
        &self,
        name: impl Into<String>,
        ssh_profile: SSHProfile,
        origin: ActionOrigin,
        intent_revision: u64,
    ) -> Result<(TmuxSessionId, EffectAdmission), TmuxError> {
        let mut next_id = self.next_id.lock().await;
        let session_id = TmuxSessionId(*next_id);
        *next_id += 1;

        let session = TmuxSession::new(session_id, name, ssh_profile, origin);
        let request = TmuxSessionRequest::new(session.clone(), intent_revision);
        let admission = request.request(&self.seam)?;

        self.sessions.lock().await.insert(session_id, session);

        Ok((session_id, admission))
    }

    /// Create tmux session after policy approval (unfenced).
    /// This is a stub - actual SSH + tmux execution to be implemented.
    pub async fn create(&self, session_id: TmuxSessionId) -> Result<(), TmuxError> {
        self.create_with_optional_fence(session_id, None).await
    }

    /// Agent-origin create under write-ahead Effect Fence. User-origin ignores ledger.
    pub async fn create_with_fence(
        &self,
        session_id: TmuxSessionId,
        ledger: &EffectFenceLedger,
    ) -> Result<(), TmuxError> {
        self.create_with_optional_fence(session_id, Some(ledger))
            .await
    }

    async fn create_with_optional_fence(
        &self,
        session_id: TmuxSessionId,
        ledger: Option<&EffectFenceLedger>,
    ) -> Result<(), TmuxError> {
        let (origin, effect) = {
            let sessions = self.sessions.lock().await;
            let session = sessions
                .get(&session_id)
                .ok_or_else(|| TmuxError::SessionNotFound(format!("{:?}", session_id)))?;
            let request = TmuxSessionRequest::new(session.clone(), 1);
            (session.origin, request.fence_effect_for_op("create"))
        };

        let run = || async {
            let mut sessions = self.sessions.lock().await;
            let session = sessions
                .get_mut(&session_id)
                .ok_or_else(|| TmuxError::SessionNotFound(format!("{:?}", session_id)))?;
            // TODO: Actual SSH connection + tmux new-session
            session.state = TmuxSessionState::Active { windows: 1 };
            Ok::<(), TmuxError>(())
        };

        match (origin, ledger) {
            (ActionOrigin::Agent, Some(ledger)) => {
                let identity = self
                    .seam
                    .prepare_fence_for_allowed(&effect, ledger)
                    .map_err(TmuxError::EffectFence)?;
                match run().await {
                    Ok(()) => {
                        if let Err(err) = ledger.observe(&identity, "tmux created") {
                            let _ = ledger.mark_unknown(
                                &identity,
                                format!("observe persist failed after tmux create: {err}"),
                            );
                            return Err(TmuxError::EffectFence(format!(
                                "effect fence observe failed after tmux create: {err}"
                            )));
                        }
                        Ok(())
                    }
                    Err(error) => {
                        let _ = ledger.mark_failed(&identity, error.to_string());
                        Err(error)
                    }
                }
            }
            _ => run().await,
        }
    }

    /// Get session state.
    pub async fn get_session(&self, session_id: TmuxSessionId) -> Option<TmuxSession> {
        self.sessions.lock().await.get(&session_id).cloned()
    }

    /// List all sessions.
    pub async fn list_sessions(&self) -> Vec<TmuxSession> {
        self.sessions.lock().await.values().cloned().collect()
    }

    /// Attach to existing tmux session (resume), unfenced.
    pub async fn attach(&self, session_id: TmuxSessionId) -> Result<(), TmuxError> {
        self.attach_with_optional_fence(session_id, None).await
    }

    /// Agent-origin attach under write-ahead Effect Fence.
    pub async fn attach_with_fence(
        &self,
        session_id: TmuxSessionId,
        ledger: &EffectFenceLedger,
    ) -> Result<(), TmuxError> {
        self.attach_with_optional_fence(session_id, Some(ledger))
            .await
    }

    async fn attach_with_optional_fence(
        &self,
        session_id: TmuxSessionId,
        ledger: Option<&EffectFenceLedger>,
    ) -> Result<(), TmuxError> {
        let (origin, effect) = {
            let sessions = self.sessions.lock().await;
            let session = sessions
                .get(&session_id)
                .ok_or_else(|| TmuxError::SessionNotFound(format!("{:?}", session_id)))?;
            let request = TmuxSessionRequest::new(session.clone(), 1);
            (session.origin, request.fence_effect_for_op("attach"))
        };

        let run = || async {
            let mut sessions = self.sessions.lock().await;
            let session = sessions
                .get_mut(&session_id)
                .ok_or_else(|| TmuxError::SessionNotFound(format!("{:?}", session_id)))?;

            if let TmuxSessionState::Detached { windows } = session.state {
                session.state = TmuxSessionState::Active { windows };
                Ok(())
            } else {
                Err(TmuxError::SessionNotFound(format!("{:?}", session_id)))
            }
        };

        match (origin, ledger) {
            (ActionOrigin::Agent, Some(ledger)) => {
                let identity = self
                    .seam
                    .prepare_fence_for_allowed(&effect, ledger)
                    .map_err(TmuxError::EffectFence)?;
                match run().await {
                    Ok(()) => {
                        if let Err(err) = ledger.observe(&identity, "tmux attached") {
                            let _ = ledger.mark_unknown(
                                &identity,
                                format!("observe persist failed after tmux attach: {err}"),
                            );
                            return Err(TmuxError::EffectFence(format!(
                                "effect fence observe failed after tmux attach: {err}"
                            )));
                        }
                        Ok(())
                    }
                    Err(error) => {
                        let _ = ledger.mark_failed(&identity, error.to_string());
                        Err(error)
                    }
                }
            }
            _ => run().await,
        }
    }

    /// Detach from tmux session (keeps running remotely).
    pub async fn detach(&self, session_id: TmuxSessionId) -> Result<(), TmuxError> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(&session_id)
            .ok_or_else(|| TmuxError::SessionNotFound(format!("{:?}", session_id)))?;

        if let TmuxSessionState::Active { windows } = session.state {
            session.state = TmuxSessionState::Detached { windows };
            Ok(())
        } else {
            Err(TmuxError::SessionNotFound(format!("{:?}", session_id)))
        }
    }

    /// Kill tmux session (unfenced).
    pub async fn kill(&self, session_id: TmuxSessionId) -> Result<(), TmuxError> {
        self.kill_with_optional_fence(session_id, None).await
    }

    /// Agent-origin kill under write-ahead Effect Fence.
    pub async fn kill_with_fence(
        &self,
        session_id: TmuxSessionId,
        ledger: &EffectFenceLedger,
    ) -> Result<(), TmuxError> {
        self.kill_with_optional_fence(session_id, Some(ledger))
            .await
    }

    async fn kill_with_optional_fence(
        &self,
        session_id: TmuxSessionId,
        ledger: Option<&EffectFenceLedger>,
    ) -> Result<(), TmuxError> {
        let (origin, effect) = {
            let sessions = self.sessions.lock().await;
            let session = sessions
                .get(&session_id)
                .ok_or_else(|| TmuxError::SessionNotFound(format!("{:?}", session_id)))?;
            let request = TmuxSessionRequest::new(session.clone(), 1);
            (session.origin, request.fence_effect_for_op("kill"))
        };

        let run = || async {
            let mut sessions = self.sessions.lock().await;
            let session = sessions
                .get_mut(&session_id)
                .ok_or_else(|| TmuxError::SessionNotFound(format!("{:?}", session_id)))?;
            // TODO: Actual tmux kill-session command over SSH
            session.state = TmuxSessionState::Dead;
            Ok::<(), TmuxError>(())
        };

        match (origin, ledger) {
            (ActionOrigin::Agent, Some(ledger)) => {
                let identity = self
                    .seam
                    .prepare_fence_for_allowed(&effect, ledger)
                    .map_err(TmuxError::EffectFence)?;
                match run().await {
                    Ok(()) => {
                        if let Err(err) = ledger.observe(&identity, "tmux killed") {
                            let _ = ledger.mark_unknown(
                                &identity,
                                format!("observe persist failed after tmux kill: {err}"),
                            );
                            return Err(TmuxError::EffectFence(format!(
                                "effect fence observe failed after tmux kill: {err}"
                            )));
                        }
                        Ok(())
                    }
                    Err(error) => {
                        let _ = ledger.mark_failed(&identity, error.to_string());
                        Err(error)
                    }
                }
            }
            _ => run().await,
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
            user: "testuser".into(),
            port: 22,
            host_key_fingerprint: None,
            key_reference: SSHKeyReference::KeychainItem {
                service: "ssh".into(),
                account: "testuser".into(),
            },
        }
    }

    fn test_seam() -> EffectSeam {
        let workspace = std::env::temp_dir();
        let mut scope = SandboxScope::local_workspace(workspace);
        scope.allow_network = true;

        let policy = PolicyEngine::new(scope.clone());
        EffectSeam::with_sandbox(policy, Sandbox::Provisioned { scope })
    }

    #[tokio::test]
    async fn tmux_session_request_creates_session() {
        let seam = test_seam();
        let manager = TmuxSessionManager::new(seam);

        let result = manager
            .request("dev-session", test_profile(), ActionOrigin::Agent, 1)
            .await;

        assert!(result.is_ok());
        let (session_id, admission) = result.unwrap();

        assert!(matches!(admission, EffectAdmission::NeedsApproval(_)));

        let session = manager.get_session(session_id).await;
        assert!(session.is_some());
        assert_eq!(session.unwrap().name, "dev-session");
    }

    #[tokio::test]
    async fn tmux_session_create_changes_state() {
        let seam = test_seam();
        let manager = TmuxSessionManager::new(seam);

        let (session_id, _) = manager
            .request("test", test_profile(), ActionOrigin::User, 1)
            .await
            .unwrap();

        let result = manager.create(session_id).await;
        assert!(result.is_ok());

        let session = manager.get_session(session_id).await.unwrap();
        assert!(matches!(session.state, TmuxSessionState::Active { .. }));
        assert!(session.is_active());
    }

    #[tokio::test]
    async fn tmux_session_detach_keeps_running() {
        let seam = test_seam();
        let manager = TmuxSessionManager::new(seam);

        let (session_id, _) = manager
            .request("test", test_profile(), ActionOrigin::User, 1)
            .await
            .unwrap();

        manager.create(session_id).await.unwrap();
        manager.detach(session_id).await.unwrap();

        let session = manager.get_session(session_id).await.unwrap();
        assert!(matches!(session.state, TmuxSessionState::Detached { .. }));
        assert!(session.is_active());
    }

    #[tokio::test]
    async fn tmux_session_attach_resumes() {
        let seam = test_seam();
        let manager = TmuxSessionManager::new(seam);

        let (session_id, _) = manager
            .request("test", test_profile(), ActionOrigin::User, 1)
            .await
            .unwrap();

        manager.create(session_id).await.unwrap();
        manager.detach(session_id).await.unwrap();
        manager.attach(session_id).await.unwrap();

        let session = manager.get_session(session_id).await.unwrap();
        assert!(matches!(session.state, TmuxSessionState::Active { .. }));
    }

    #[tokio::test]
    async fn tmux_session_kill_terminates() {
        let seam = test_seam();
        let manager = TmuxSessionManager::new(seam);

        let (session_id, _) = manager
            .request("test", test_profile(), ActionOrigin::User, 1)
            .await
            .unwrap();

        manager.create(session_id).await.unwrap();
        manager.kill(session_id).await.unwrap();

        let session = manager.get_session(session_id).await.unwrap();
        assert!(matches!(session.state, TmuxSessionState::Dead));
        assert!(!session.is_active());
    }

    fn network_allow_seam() -> EffectSeam {
        use crate::{PolicyConfig, PolicyEngine, Sandbox, SandboxScope};
        let workspace = std::env::temp_dir();
        let mut scope = SandboxScope::local_workspace(workspace);
        scope.allow_network = true;
        let config = PolicyConfig::parse(
            r#"{"version":1,"overrides":{"network_connect":"allow","ssh_connect":"allow"}}"#,
        )
        .unwrap();
        let policy = PolicyEngine::with_config(scope.clone(), config);
        EffectSeam::with_sandbox(policy, Sandbox::Provisioned { scope })
    }

    #[tokio::test]
    async fn agent_tmux_create_refuses_when_prior_unknown() {
        use crate::{
            EffectFenceLedger, EffectInvocationIdentity, EventStore, FenceState, MemoryEventStore,
            records_from_events,
        };
        use std::sync::Arc;
        use uuid::Uuid;

        let manager = TmuxSessionManager::new(network_allow_seam());
        let events: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let owner = Uuid::new_v4();
        let ledger = EffectFenceLedger::new(Arc::clone(&events), owner);

        let (session_id, _) = manager
            .request("fence-tmux", test_profile(), ActionOrigin::Agent, 1)
            .await
            .unwrap();
        let session = manager.get_session(session_id).await.unwrap();
        let request = TmuxSessionRequest::new(session, 1);
        let effect = request.fence_effect_for_op("create");
        let identity = EffectInvocationIdentity::from_effect(&effect);
        ledger.prepare_and_start(&identity).expect("prepare");
        ledger
            .mark_unknown(&identity, "crash mid-flight")
            .expect("unknown");

        let err = manager
            .create_with_fence(session_id, &ledger)
            .await
            .expect_err("Unknown fence must refuse");
        assert!(
            matches!(err, TmuxError::EffectFence(ref reason) if reason.contains("Unknown") || reason.contains("refuse")),
            "expected fence refuse, got {err:?}"
        );

        let records = records_from_events(&events.list(owner).expect("list"));
        assert!(
            records.values().any(|r| r.state == FenceState::Unknown
                && r.identity.args_digest == identity.args_digest),
            "durable Unknown fence must remain"
        );
    }

    #[tokio::test]
    async fn agent_tmux_create_emits_fence_observed() {
        use crate::{
            EffectFenceLedger, EventStore, FenceState, MemoryEventStore, args_digest_for_effect,
            records_from_events,
        };
        use std::sync::Arc;
        use uuid::Uuid;

        let manager = TmuxSessionManager::new(network_allow_seam());
        let events: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let owner = Uuid::new_v4();
        let ledger = EffectFenceLedger::new(Arc::clone(&events), owner);

        let (session_id, _) = manager
            .request("fence-obs", test_profile(), ActionOrigin::Agent, 1)
            .await
            .unwrap();
        manager
            .create_with_fence(session_id, &ledger)
            .await
            .expect("agent create + fence");

        let session = manager.get_session(session_id).await.unwrap();
        let request = TmuxSessionRequest::new(session, 1);
        let effect = request.fence_effect_for_op("create");
        let records = records_from_events(&events.list(owner).expect("list"));
        assert!(
            records.values().any(|r| r.state == FenceState::Observed
                && r.identity.args_digest == args_digest_for_effect(&effect)),
            "expected Observed fence; records={records:?}"
        );
    }
}
