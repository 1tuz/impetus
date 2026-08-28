//! ACP Gateway — управляет дочерним agent CLI process.
//!
//! Один process на profile; stdout зарезервирован под ACP JSON-RPC,
//! stderr для logs. Session mapping и permission forwarding.

use crate::profile::AcpProfile;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, error, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    NotStarted,
    Initializing,
    Connected,
    Unavailable,
    Incompatible,
    Crashed,
}

/// ACP Gateway для external coding-agent.
pub struct AcpGateway {
    profile: AcpProfile,
    status: AgentStatus,
    child: Option<Child>,
}

impl AcpGateway {
    pub fn new(profile: AcpProfile) -> Result<Self, GatewayError> {
        profile.validate().map_err(GatewayError::InvalidProfile)?;
        Ok(Self {
            profile,
            status: AgentStatus::NotStarted,
            child: None,
        })
    }

    pub fn status(&self) -> AgentStatus {
        self.status
    }

    pub fn profile(&self) -> &AcpProfile {
        &self.profile
    }

    /// Запускает agent process (без инициализации ACP session).
    pub async fn start(&mut self) -> Result<(), GatewayError> {
        if self.child.is_some() {
            return Err(GatewayError::AlreadyStarted);
        }

        debug!(
            "Starting ACP agent: {} ({})",
            self.profile.display_name,
            self.profile.command.display()
        );

        let mut cmd = Command::new(&self.profile.command);
        cmd.args(&self.profile.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = cmd.spawn().map_err(|e| {
            error!("Failed to spawn agent process: {}", e);
            GatewayError::SpawnFailed(e.to_string())
        })?;

        self.child = Some(child);
        self.status = AgentStatus::Initializing;

        Ok(())
    }

    /// Останавливает agent process (graceful или kill).
    pub async fn stop(&mut self) -> Result<(), GatewayError> {
        if let Some(mut child) = self.child.take() {
            debug!("Stopping ACP agent: {}", self.profile.display_name);

            // Попытка graceful shutdown (можно расширить с JSON-RPC exit)
            match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
                Ok(Ok(status)) => {
                    debug!("Agent exited with status: {}", status);
                }
                Ok(Err(e)) => {
                    warn!("Error waiting for agent: {}", e);
                }
                Err(_) => {
                    warn!("Agent did not exit gracefully, killing");
                    child.kill().await.ok();
                }
            }
        }

        self.status = AgentStatus::NotStarted;
        Ok(())
    }

    /// Читает stderr для логов agent (не блокирует).
    pub async fn read_agent_logs(&mut self) -> Result<Vec<String>, GatewayError> {
        let child = self.child.as_mut().ok_or(GatewayError::NotStarted)?;
        let stderr = child.stderr.take().ok_or(GatewayError::NotStarted)?;

        let mut lines = Vec::new();
        let mut reader = BufReader::new(stderr).lines();

        // Читаем доступные строки (non-blocking)
        while let Ok(Some(line)) = reader.next_line().await {
            lines.push(line);
        }

        Ok(lines)
    }
}

impl Drop for AcpGateway {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            tokio::spawn(async move {
                child.kill().await.ok();
            });
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("Invalid profile: {0}")]
    InvalidProfile(#[from] crate::profile::ProfileError),

    #[error("Agent already started")]
    AlreadyStarted,

    #[error("Agent not started")]
    NotStarted,

    #[error("Failed to spawn agent: {0}")]
    SpawnFailed(String),

    #[error("Agent crashed")]
    AgentCrashed,

    #[error("Incompatible agent version")]
    IncompatibleVersion,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn gateway_starts_and_stops() {
        let profile =
            AcpProfile::manual_executable("test", "Test Agent", PathBuf::from("/bin/echo"));

        let mut gateway = AcpGateway::new(profile).expect("create gateway");
        assert_eq!(gateway.status(), AgentStatus::NotStarted);

        gateway.start().await.expect("start");
        assert_eq!(gateway.status(), AgentStatus::Initializing);

        gateway.stop().await.expect("stop");
        assert_eq!(gateway.status(), AgentStatus::NotStarted);
    }

    #[tokio::test]
    async fn gateway_rejects_double_start() {
        let profile = AcpProfile::manual_executable("test", "Test", PathBuf::from("/bin/cat"));

        let mut gateway = AcpGateway::new(profile).unwrap();
        gateway.start().await.unwrap();

        assert!(matches!(
            gateway.start().await,
            Err(GatewayError::AlreadyStarted)
        ));

        gateway.stop().await.ok();
    }
}
