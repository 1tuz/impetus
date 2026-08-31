//! Production ACP Gateway using official agent-client-protocol SDK.
//!
//! This replaces the custom JSON-RPC implementation with official ACP v1 protocol.
//! Agent owns authentication; Impetus owns policy, session state, and orchestration.

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AuthenticateRequest, ContentBlock, InitializeRequest, NewSessionRequest, PermissionOptionId,
    PromptRequest, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SessionNotification, SessionUpdate, StopReason,
    TextContent,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// ACP Gateway state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayState {
    NotStarted,
    Initializing,
    AuthRequired,
    Ready,
    Incompatible,
    Crashed,
}

/// Permission decision from Impetus Policy.
#[derive(Debug, Clone)]
pub enum PermissionDecision {
    /// Allow with specific option ID
    Allow(String),
    /// Deny the request
    Deny,
    /// Need user approval (blocked)
    NeedsApproval,
}

/// Streaming update from agent.
#[derive(Debug, Clone)]
pub enum StreamUpdate {
    /// Text chunk
    Text(String),
    /// Tool use update
    ToolUse { tool_name: String, status: String },
    /// Status change
    Status(String),
    /// Session completed
    Completed { stop_reason: StopReason },
    /// Error occurred
    Error(String),
}

/// Permission request from agent.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub request_id: String,
    pub description: String,
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone)]
pub struct PermissionOption {
    pub option_id: String,
    pub description: String,
}

/// Permission request with decision channel.
type PermissionRequestWithSender = (PermissionRequest, oneshot::Sender<PermissionDecision>);

/// ACP Gateway using official SDK.
#[derive(Debug)]
pub struct AcpGatewayV2 {
    config: AcpAgentConfig,
    state: Arc<Mutex<GatewayState>>,
    /// Channel for streaming updates
    update_tx: mpsc::UnboundedSender<StreamUpdate>,
    update_rx: Arc<Mutex<mpsc::UnboundedReceiver<StreamUpdate>>>,
    /// Channel for permission requests
    permission_tx: mpsc::UnboundedSender<PermissionRequestWithSender>,
    permission_rx: Arc<Mutex<mpsc::UnboundedReceiver<PermissionRequestWithSender>>>,
}

impl AcpGatewayV2 {
    /// Create new gateway with agent config.
    pub fn new(config: AcpAgentConfig) -> Self {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let (permission_tx, permission_rx) = mpsc::unbounded_channel();

        Self {
            config,
            state: Arc::new(Mutex::new(GatewayState::NotStarted)),
            update_tx,
            update_rx: Arc::new(Mutex::new(update_rx)),
            permission_tx,
            permission_rx: Arc::new(Mutex::new(permission_rx)),
        }
    }

    /// Get current state.
    pub async fn state(&self) -> GatewayState {
        *self.state.lock().await
    }

    /// Start agent and run session.
    pub async fn start_session(&self, workspace_dir: PathBuf, prompt: String) -> Result<SessionId> {
        let agent = AcpAgent::new(self.config.clone());
        let state = Arc::clone(&self.state);
        let update_tx = self.update_tx.clone();
        let permission_tx = self.permission_tx.clone();

        *state.lock().await = GatewayState::Initializing;

        let session_id = Arc::new(Mutex::new(None));
        let session_id_clone = Arc::clone(&session_id);

        Client
            .builder()
            .on_receive_notification(
                move |notification: SessionNotification, _cx| {
                    let update_tx = update_tx.clone();
                    async move {
                        Self::handle_session_notification(notification, update_tx).await;
                        Ok(())
                    }
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                move |request: RequestPermissionRequest, responder, _connection| {
                    let permission_tx = permission_tx.clone();
                    async move {
                        Self::handle_permission_request(request, responder, permission_tx).await
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
                // Initialize
                info!("Initializing ACP agent");
                let init_response = connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;

                debug!("Agent initialized: {:?}", init_response.agent_info);

                // Check auth requirements
                if !init_response.auth_methods.is_empty() {
                    *state.lock().await = GatewayState::AuthRequired;
                    info!(
                        "Agent requires authentication: {:?}",
                        init_response.auth_methods
                    );

                    // Authenticate (agent-owned login) - use first auth method's id
                    let first_method_id =
                        init_response.auth_methods.first().map(|m| m.id().clone());
                    if let Some(method_id) = first_method_id {
                        let auth_response = connection
                            .send_request(AuthenticateRequest::new(method_id))
                            .block_task()
                            .await?;

                        debug!("Authentication result: {:?}", auth_response);
                    }
                }

                *state.lock().await = GatewayState::Ready;

                // Create session
                info!("Creating new session in {:?}", workspace_dir);
                let new_session_response = connection
                    .send_request(NewSessionRequest::new(workspace_dir))
                    .block_task()
                    .await?;

                let sid = new_session_response.session_id.clone();
                *session_id_clone.lock().await = Some(sid.clone());

                debug!("Session created: {:?}", sid);

                // Send prompt
                info!("Sending prompt");
                let prompt_response = connection
                    .send_request(PromptRequest::new(
                        sid.clone(),
                        vec![ContentBlock::Text(TextContent::new(prompt))],
                    ))
                    .block_task()
                    .await?;

                info!(
                    "Session completed with stop_reason: {:?}",
                    prompt_response.stop_reason
                );

                Ok(())
            })
            .await
            .map_err(|e| anyhow::anyhow!("ACP connection failed: {:?}", e))?;

        let sid = session_id
            .lock()
            .await
            .clone()
            .context("session_id not set")?;
        Ok(sid)
    }

    /// Cancel active session (simplified - connection close).
    /// In ACP v1, cancellation is typically done by closing the connection.
    pub async fn cancel_session(&self, _session_id: SessionId) -> Result<()> {
        info!("Cancelling session: {:?}", _session_id);
        warn!("Session cancellation not fully implemented - requires connection management");
        Ok(())
    }

    /// Receive next streaming update.
    pub async fn recv_update(&self) -> Option<StreamUpdate> {
        self.update_rx.lock().await.recv().await
    }

    /// Receive next permission request (with response channel).
    pub async fn recv_permission_request(
        &self,
    ) -> Option<(PermissionRequest, oneshot::Sender<PermissionDecision>)> {
        self.permission_rx.lock().await.recv().await
    }

    /// Respond to permission request.
    pub async fn respond_permission(
        &self,
        _request_id: String,
        _decision: PermissionDecision,
    ) -> Result<()> {
        // This would need to be paired with the oneshot sender from recv_permission_request
        // For now, this is a simplified interface
        warn!("respond_permission not fully implemented yet");
        Ok(())
    }

    async fn handle_session_notification(
        notification: SessionNotification,
        update_tx: mpsc::UnboundedSender<StreamUpdate>,
    ) {
        match notification.update {
            SessionUpdate::UserMessageChunk(chunk) => {
                debug!("User message chunk");
                if let ContentBlock::Text(text) = &chunk.content {
                    let _ = update_tx.send(StreamUpdate::Text(text.text.clone()));
                }
            }
            SessionUpdate::AgentMessageChunk(chunk) => {
                debug!("Agent message chunk");
                if let ContentBlock::Text(text) = &chunk.content {
                    let _ = update_tx.send(StreamUpdate::Text(text.text.clone()));
                }
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                debug!("Agent thought chunk");
                if let ContentBlock::Text(text) = &chunk.content {
                    let _ = update_tx.send(StreamUpdate::Text(format!("[thought] {}", text.text)));
                }
            }
            SessionUpdate::ToolCall(tool_call) => {
                debug!("Tool call: {:?}", tool_call.title);
                let _ = update_tx.send(StreamUpdate::ToolUse {
                    tool_name: tool_call.title.clone(),
                    status: "started".into(),
                });
            }
            SessionUpdate::ToolCallUpdate(tool_update) => {
                debug!("Tool call update: {:?}", tool_update.tool_call_id);
                if let Some(status) = &tool_update.fields.status {
                    let _ = update_tx.send(StreamUpdate::Status(format!("Tool: {:?}", status)));
                }
            }
            SessionUpdate::UsageUpdate(usage) => {
                debug!("Usage update: {:?}", usage);
            }
            SessionUpdate::SessionInfoUpdate(info) => {
                debug!("Session info update: {:?}", info);
            }
            _ => {
                debug!("Other session update: {:?}", notification.update);
            }
        }
    }

    async fn handle_permission_request(
        request: RequestPermissionRequest,
        responder: agent_client_protocol::Responder<RequestPermissionResponse>,
        permission_tx: mpsc::UnboundedSender<PermissionRequestWithSender>,
    ) -> Result<(), agent_client_protocol::Error> {
        info!(
            "Permission request for tool: {:?}",
            request
                .tool_call
                .fields
                .title
                .as_deref()
                .unwrap_or("<unknown>")
        );

        let (decision_tx, decision_rx) = oneshot::channel();

        let perm_req = PermissionRequest {
            request_id: Uuid::new_v4().to_string(),
            description: format!(
                "Tool: {} ({})",
                request
                    .tool_call
                    .fields
                    .title
                    .as_deref()
                    .unwrap_or("<unknown>"),
                request.tool_call.tool_call_id.0.as_ref()
            ),
            options: request
                .options
                .iter()
                .map(|opt| PermissionOption {
                    option_id: opt.option_id.0.to_string(),
                    description: opt.name.clone(),
                })
                .collect(),
        };

        // Send to policy layer
        if permission_tx.send((perm_req, decision_tx)).is_err() {
            error!("Permission channel closed");
            let _ = responder.respond(RequestPermissionResponse::new(
                RequestPermissionOutcome::Cancelled,
            ));
            return Ok(());
        }

        // Wait for decision
        match decision_rx.await {
            Ok(PermissionDecision::Allow(option_id)) => {
                info!("Permission allowed: {}", option_id);
                let _ = responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                        PermissionOptionId::new(option_id),
                    )),
                ));
            }
            Ok(PermissionDecision::Deny) => {
                info!("Permission denied");
                let _ = responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ));
            }
            Ok(PermissionDecision::NeedsApproval) => {
                warn!("Permission needs approval but no approval flow yet");
                let _ = responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ));
            }
            Err(_) => {
                error!("Permission decision channel closed");
                let _ = responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_state_transitions() {
        assert_ne!(GatewayState::NotStarted, GatewayState::Ready);
        assert_eq!(GatewayState::Ready, GatewayState::Ready);
    }

    #[tokio::test]
    async fn gateway_creates_with_not_started_state() {
        let config = AcpAgentConfig::new("echo");
        let gateway = AcpGatewayV2::new(config);
        assert_eq!(gateway.state().await, GatewayState::NotStarted);
    }
}
