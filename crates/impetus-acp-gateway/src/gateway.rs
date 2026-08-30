//! ACP Gateway — управляет дочерним agent CLI process.
//!
//! Один process на profile; stdout зарезервирован под ACP JSON-RPC,
//! stderr для logs. Session mapping и permission forwarding.

use crate::mock::{JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use crate::profile::AcpProfile;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
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
#[derive(Debug)]
pub struct AcpGateway {
    profile: AcpProfile,
    status: AgentStatus,
    #[allow(dead_code)]
    child: Option<Child>,
    request_id: AtomicU64,
    #[allow(dead_code)]
    notification_rx: Option<mpsc::UnboundedReceiver<JsonRpcNotification>>,
    pending_requests: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
}

impl AcpGateway {
    pub fn new(profile: AcpProfile) -> Result<Self, GatewayError> {
        profile.validate().map_err(GatewayError::InvalidProfile)?;
        Ok(Self {
            profile,
            status: AgentStatus::NotStarted,
            child: None,
            request_id: AtomicU64::new(1),
            notification_rx: None,
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn status(&self) -> AgentStatus {
        self.status
    }

    pub fn profile(&self) -> &AcpProfile {
        &self.profile
    }

    /// Запускает agent process и начинает читать stdout notifications.
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

        let mut child = cmd.spawn().map_err(|e| {
            error!("Failed to spawn agent process: {}", e);
            GatewayError::SpawnFailed(e.to_string())
        })?;

        // Spawn stdout reader task для responses и notifications
        let stdout = child.stdout.take().ok_or(GatewayError::NotStarted)?;
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();
        let pending_requests = Arc::clone(&self.pending_requests);

        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if let Ok(msg) = serde_json::from_str::<JsonRpcMessage>(&line) {
                    match msg {
                        JsonRpcMessage::Notification(notif) => {
                            if notif_tx.send(notif).is_err() {
                                break;
                            }
                        }
                        JsonRpcMessage::Response(resp) => {
                            if let Some(id) = resp.id.as_u64() {
                                let mut pending = pending_requests.lock().await;
                                if let Some(tx) = pending.remove(&id) {
                                    let _ = tx.send(resp);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        });

        self.notification_rx = Some(notif_rx);
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

    /// Отправляет JSON-RPC request и ждёт response.
    pub async fn send_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<JsonRpcResponse, GatewayError> {
        let child = self.child.as_mut().ok_or(GatewayError::NotStarted)?;
        let stdin = child.stdin.as_mut().ok_or(GatewayError::NotStarted)?;

        let id = self.request_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(id),
            method: method.into(),
            params,
        };

        // Регистрируем pending request
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending_requests.lock().await;
            pending.insert(id, tx);
        }

        let json = serde_json::to_string(&JsonRpcMessage::Request(request))
            .map_err(|e| GatewayError::JsonParse(e.to_string()))?;

        stdin.write_all(json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        // Ждём response через channel
        let timeout = tokio::time::Duration::from_secs(10);
        let response = tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| GatewayError::Timeout)?
            .map_err(|_| GatewayError::NoResponse)?;

        Ok(response)
    }

    /// Получает следующую notification от agent (non-blocking).
    pub async fn recv_notification(&mut self) -> Option<JsonRpcNotification> {
        self.notification_rx.as_mut()?.recv().await
    }

    /// Инициализирует ACP agent.
    pub async fn initialize(&mut self) -> Result<serde_json::Value, GatewayError> {
        let params = serde_json::json!({
            "clientInfo": {
                "name": "impetus",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        let response = self.send_request("initialize", Some(params)).await?;

        if let Some(error) = response.error {
            return Err(GatewayError::AgentError(error.message));
        }

        self.status = AgentStatus::Connected;
        response.result.ok_or(GatewayError::NoResponse)
    }

    /// Отвечает на auth/requestCredential notification от agent.
    /// Agent-owned login: harness forwards prompt, user provides answer.
    pub async fn respond_credential(
        &mut self,
        request_id: &str,
        credential: Option<String>,
    ) -> Result<(), GatewayError> {
        let child = self.child.as_mut().ok_or(GatewayError::NotStarted)?;
        let stdin = child.stdin.as_mut().ok_or(GatewayError::NotStarted)?;

        let (result, error) = if let Some(cred) = credential {
            (Some(serde_json::json!({"credential": cred})), None)
        } else {
            (
                None,
                Some(crate::mock::JsonRpcError {
                    code: -32000,
                    message: "User cancelled credential input".into(),
                    data: None,
                }),
            )
        };

        let response = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(request_id),
            result,
            error,
        };

        let json = serde_json::to_string(&JsonRpcMessage::Response(response))
            .map_err(|e| GatewayError::JsonParse(e.to_string()))?;

        stdin.write_all(json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        Ok(())
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

    #[error("Agent error: {0}")]
    AgentError(String),

    #[error("No response from agent")]
    NoResponse,

    #[error("Request timeout")]
    Timeout,

    #[error("JSON parse error: {0}")]
    JsonParse(String),

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
