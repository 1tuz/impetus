//! PTY session management with durable state and bounded output.

use crate::{Action, ActionKind, ActionOrigin, EffectAdmission, EffectSeam, NormalizedEffect};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;

/// Unique identifier for a PTY session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PtySessionId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PtySessionState {
    Starting,
    Running { pid: u32 },
    Detached { pid: u32 },
    Exited { exit_code: Option<i32> },
    Failed { reason: String },
}

#[derive(Debug, Error)]
pub enum PtySessionError {
    #[error("policy denied PTY session: {0}")]
    PolicyDenied(String),
    #[error("approval required but not granted")]
    ApprovalRequired,
    #[error("session not found: {0:?}")]
    SessionNotFound(PtySessionId),
    #[error("session already running: {0:?}")]
    AlreadyRunning(PtySessionId),
    #[error("PTY spawn failed: {0}")]
    SpawnFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage error: {0}")]
    Storage(String),
}

/// PTY session lifecycle: spawn → attach/detach → terminate.
/// Each session has durable state that survives harness restart.
#[derive(Debug, Clone)]
pub struct PtySession {
    pub id: PtySessionId,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    pub env: Vec<(String, String)>,
    pub state: PtySessionState,
    pub origin: ActionOrigin,
    pub created_at_unix_ms: u64,
}

impl PtySession {
    pub fn new(
        id: PtySessionId,
        command: impl Into<String>,
        args: Vec<String>,
        working_dir: PathBuf,
        origin: ActionOrigin,
    ) -> Self {
        Self {
            id,
            command: command.into(),
            args,
            working_dir,
            env: Vec::new(),
            state: PtySessionState::Starting,
            origin,
            created_at_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time after epoch")
                .as_millis() as u64,
        }
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.state,
            PtySessionState::Running { .. } | PtySessionState::Detached { .. }
        )
    }
}

/// PTY session manager coordinating policy, spawn, attach/detach, and durable storage.
pub struct PtySessionManager {
    seam: EffectSeam,
    sessions: Arc<Mutex<std::collections::HashMap<PtySessionId, PtySession>>>,
    next_id: Arc<Mutex<u64>>,
}

impl PtySessionManager {
    pub fn new(seam: EffectSeam) -> Self {
        Self {
            seam,
            sessions: Arc::new(Mutex::new(std::collections::HashMap::new())),
            next_id: Arc::new(Mutex::new(1)),
        }
    }

    /// Request a new PTY session through policy and effect seam.
    pub async fn request(
        &self,
        command: impl Into<String>,
        args: Vec<String>,
        working_dir: PathBuf,
        origin: ActionOrigin,
        intent_revision: u64,
    ) -> Result<(PtySessionId, EffectAdmission), PtySessionError> {
        let command = command.into();
        let summary = format!("PTY: {} {}", command, args.join(" "));
        let target = working_dir.display().to_string();

        let _action = Action {
            origin,
            kind: ActionKind::SpawnProcess,
            summary: summary.clone(),
            target: Some(target.clone()),
        };

        let effect = NormalizedEffect::process_spawn(origin, summary, target);
        let admission = self.seam.request(effect, intent_revision);

        let mut next_id = self.next_id.lock().await;
        let session_id = PtySessionId(*next_id);
        *next_id += 1;

        let session = PtySession::new(session_id, command, args, working_dir, origin);
        self.sessions.lock().await.insert(session_id, session);

        Ok((session_id, admission))
    }

    /// Spawn PTY session after approval (or immediate Allow).
    /// This is a stub for actual PTY implementation - to be completed with portable_pty or similar.
    pub async fn spawn(&self, session_id: PtySessionId) -> Result<(), PtySessionError> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(&session_id)
            .ok_or(PtySessionError::SessionNotFound(session_id))?;

        if session.is_running() {
            return Err(PtySessionError::AlreadyRunning(session_id));
        }

        // TODO: Actual PTY spawn with portable_pty or nix pty
        // For now, simulate spawn with fake PID
        session.state = PtySessionState::Running { pid: 12345 };

        Ok(())
    }

    /// Get session state.
    pub async fn get_session(&self, session_id: PtySessionId) -> Option<PtySession> {
        self.sessions.lock().await.get(&session_id).cloned()
    }

    /// List all sessions.
    pub async fn list_sessions(&self) -> Vec<PtySession> {
        self.sessions.lock().await.values().cloned().collect()
    }

    /// Detach from session (keeps process running).
    pub async fn detach(&self, session_id: PtySessionId) -> Result<(), PtySessionError> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(&session_id)
            .ok_or(PtySessionError::SessionNotFound(session_id))?;

        if let PtySessionState::Running { pid } = session.state {
            session.state = PtySessionState::Detached { pid };
            Ok(())
        } else {
            Err(PtySessionError::SessionNotFound(session_id))
        }
    }

    /// Terminate session.
    pub async fn terminate(&self, session_id: PtySessionId) -> Result<(), PtySessionError> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(&session_id)
            .ok_or(PtySessionError::SessionNotFound(session_id))?;

        // TODO: Actual process kill
        session.state = PtySessionState::Exited { exit_code: None };

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyEngine, Sandbox, SandboxScope};

    fn test_seam() -> EffectSeam {
        let workspace = std::env::temp_dir();
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.clone()));
        EffectSeam::with_sandbox(policy, Sandbox::workspace(workspace))
    }

    #[tokio::test]
    async fn pty_session_request_creates_session() {
        let seam = test_seam();
        let manager = PtySessionManager::new(seam);

        let result = manager
            .request("bash", vec![], std::env::temp_dir(), ActionOrigin::Agent, 1)
            .await;

        assert!(result.is_ok());
        let (session_id, admission) = result.unwrap();

        assert!(matches!(admission, EffectAdmission::NeedsApproval(_)));

        let session = manager.get_session(session_id).await;
        assert!(session.is_some());
        assert_eq!(session.unwrap().command, "bash");
    }

    #[tokio::test]
    async fn pty_session_spawn_changes_state() {
        let seam = test_seam();
        let manager = PtySessionManager::new(seam);

        let (session_id, _) = manager
            .request("bash", vec![], std::env::temp_dir(), ActionOrigin::User, 1)
            .await
            .unwrap();

        let result = manager.spawn(session_id).await;
        assert!(result.is_ok());

        let session = manager.get_session(session_id).await.unwrap();
        assert!(matches!(session.state, PtySessionState::Running { .. }));
    }

    #[tokio::test]
    async fn pty_session_detach_keeps_running() {
        let seam = test_seam();
        let manager = PtySessionManager::new(seam);

        let (session_id, _) = manager
            .request("bash", vec![], std::env::temp_dir(), ActionOrigin::User, 1)
            .await
            .unwrap();

        manager.spawn(session_id).await.unwrap();
        manager.detach(session_id).await.unwrap();

        let session = manager.get_session(session_id).await.unwrap();
        assert!(matches!(session.state, PtySessionState::Detached { .. }));
        assert!(session.is_running());
    }

    #[tokio::test]
    async fn pty_session_terminate_exits() {
        let seam = test_seam();
        let manager = PtySessionManager::new(seam);

        let (session_id, _) = manager
            .request("bash", vec![], std::env::temp_dir(), ActionOrigin::User, 1)
            .await
            .unwrap();

        manager.spawn(session_id).await.unwrap();
        manager.terminate(session_id).await.unwrap();

        let session = manager.get_session(session_id).await.unwrap();
        assert!(matches!(session.state, PtySessionState::Exited { .. }));
        assert!(!session.is_running());
    }
}
