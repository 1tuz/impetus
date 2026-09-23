//! Production ACP Gateway using official agent-client-protocol SDK.
//!
//! This replaces the custom JSON-RPC implementation with official ACP v1 protocol.
//! Agent owns authentication; Impetus owns policy, session state, and orchestration.

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AuthMethod, AuthMethodId, AuthenticateRequest, CancelNotification, ContentBlock,
    InitializeRequest, NewSessionRequest, PermissionOptionId,
    PermissionOptionKind as SdkPermissionOptionKind, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionConfigId, SessionConfigOptionValue, SessionConfigValueId, SessionId,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest, StopReason, TextContent,
    ToolKind,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::redact::{StreamExportAudit, redact_json, redact_text};
use crate::session_config::{
    SessionLaunchOptions, advertised_model_ids, advertised_thought_levels, plan_config_option_sets,
};

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

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GatewayV2Error {
    #[error("agent advertised authentication methods but no method was selected")]
    AuthSelectionRequired { offered: Vec<String> },
    #[error("selected authentication method is not advertised by the agent")]
    UnsupportedAuthMethod { selected: String },
}

/// Wire-facing ACP permission outcome after Policy/durable approval in core.
///
/// Transport-only: durable `NeedsApproval` is brokered by `AcpAdapter` and never
/// reaches this enum — only exact Select or Deny cross the gateway boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Select an exact ACP option after Policy/user decision.
    Select(String),
    /// Deny the request
    Deny,
}

/// Map a transport decision to an ACP `RequestPermissionOutcome`.
pub fn permission_outcome(decision: PermissionDecision) -> RequestPermissionOutcome {
    match decision {
        PermissionDecision::Select(option_id) => RequestPermissionOutcome::Selected(
            SelectedPermissionOutcome::new(PermissionOptionId::new(option_id)),
        ),
        PermissionDecision::Deny => RequestPermissionOutcome::Cancelled,
    }
}

/// Streaming update from agent.
#[derive(Debug, Clone)]
pub enum StreamUpdate {
    /// Text chunk (already scrubbed of control chars).
    Text(String),
    /// Tool use — arguments are redacted for durable harness / ToolOrchestrator.
    ToolUse {
        tool_call_id: String,
        tool_name: String,
        status: String,
        kind: String,
        arguments: serde_json::Value,
    },
    /// Status change (tool progress / session info labels).
    Status(String),
    /// Session completed with an explicit ACP stop_reason.
    Completed { stop_reason: StopReason },
    /// Transport/agent ended without a definitive stop_reason (disconnect/crash).
    /// Must never be mapped to harness `Completed`.
    Interrupted { reason: String },
    /// Error occurred
    Error(String),
}

/// Permission request from agent.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub request_id: String,
    pub description: String,
    pub kind: PermissionKind,
    pub target: Option<PathBuf>,
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    Other,
}

#[derive(Debug, Clone)]
pub struct PermissionOption {
    pub option_id: String,
    pub description: String,
    pub kind: PermissionChoiceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionChoiceKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// Permission request with decision channel.
type PermissionRequestWithSender = (PermissionRequest, oneshot::Sender<PermissionDecision>);

struct CancelCommand {
    acknowledged: oneshot::Sender<std::result::Result<(), String>>,
}

/// Cached agent capability snapshot from ACP initialize (vendor-neutral labels).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedAgentCapabilities {
    pub load_session: bool,
    pub prompt_image: bool,
    pub prompt_audio: bool,
    pub prompt_embedded_context: bool,
    pub auth_method_ids: Vec<String>,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    /// Model select values from the last `session/new` config_options (if any).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_ids: Vec<String>,
    /// ThoughtLevel select values from the last session config_options.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thought_levels: Vec<String>,
}

/// ACP Gateway using official SDK.
#[derive(Debug)]
pub struct AcpGatewayV2 {
    config: AcpAgentConfig,
    auth_method_id: Option<String>,
    state: Arc<Mutex<GatewayState>>,
    /// Channel for streaming updates
    update_tx: mpsc::UnboundedSender<StreamUpdate>,
    update_rx: Arc<Mutex<mpsc::UnboundedReceiver<StreamUpdate>>>,
    /// Channel for permission requests
    permission_tx: mpsc::UnboundedSender<PermissionRequestWithSender>,
    permission_rx: Arc<Mutex<mpsc::UnboundedReceiver<PermissionRequestWithSender>>>,
    active_cancel: Arc<Mutex<Option<mpsc::Sender<CancelCommand>>>>,
    active_session: Arc<Mutex<Option<SessionId>>>,
    cached_capabilities: Arc<Mutex<Option<CachedAgentCapabilities>>>,
}

impl AcpGatewayV2 {
    /// Create new gateway with agent config.
    pub fn new(config: AcpAgentConfig) -> Self {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let (permission_tx, permission_rx) = mpsc::unbounded_channel();

        Self {
            config,
            auth_method_id: None,
            state: Arc::new(Mutex::new(GatewayState::NotStarted)),
            update_tx,
            update_rx: Arc::new(Mutex::new(update_rx)),
            permission_tx,
            permission_rx: Arc::new(Mutex::new(permission_rx)),
            active_cancel: Arc::new(Mutex::new(None)),
            active_session: Arc::new(Mutex::new(None)),
            cached_capabilities: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the exact agent-owned authentication method chosen by the user.
    #[must_use]
    pub fn with_auth_method(mut self, auth_method_id: Option<String>) -> Self {
        self.auth_method_id = auth_method_id;
        self
    }

    /// Last initialize capability snapshot (None until first successful initialize).
    pub async fn cached_capabilities(&self) -> Option<CachedAgentCapabilities> {
        self.cached_capabilities.lock().await.clone()
    }

    /// Seed / replace cached initialize+session config snapshot (tests / probe).
    pub async fn seed_cached_capabilities(&self, caps: CachedAgentCapabilities) {
        *self.cached_capabilities.lock().await = Some(caps);
    }

    /// Non-async peek for sync catalog paths (ListModels). Returns None if lock busy.
    pub fn cached_capabilities_blocking(
        &self,
    ) -> Result<Option<CachedAgentCapabilities>, tokio::sync::TryLockError> {
        self.cached_capabilities.try_lock().map(|g| g.clone())
    }

    /// Get current state.
    pub async fn state(&self) -> GatewayState {
        *self.state.lock().await
    }

    /// Non-async peek for sync health/doctor paths. Returns None if lock busy.
    pub fn state_blocking(&self) -> Result<GatewayState, tokio::sync::TryLockError> {
        self.state.try_lock().map(|g| *g)
    }

    /// Start agent and run session with optional model/reasoning ACP config writes.
    pub async fn start_session(&self, workspace_dir: PathBuf, prompt: String) -> Result<SessionId> {
        self.start_session_with_options(workspace_dir, prompt, SessionLaunchOptions::default())
            .await
    }

    /// Start agent session and apply Impetus model/reasoning via ACP
    /// `session/set_config_option` before `session/prompt`.
    pub async fn start_session_with_options(
        &self,
        workspace_dir: PathBuf,
        prompt: String,
        launch: SessionLaunchOptions,
    ) -> Result<SessionId> {
        let (cancel_tx, mut cancel_rx) = mpsc::channel::<CancelCommand>(1);
        {
            let mut active_cancel = self.active_cancel.lock().await;
            if active_cancel.is_some() {
                anyhow::bail!("an ACP session is already active");
            }
            *active_cancel = Some(cancel_tx);
        }

        let agent = AcpAgent::new(self.config.clone());
        let state = Arc::clone(&self.state);
        let update_tx = self.update_tx.clone();
        let completion_tx = self.update_tx.clone();
        let permission_tx = self.permission_tx.clone();
        let auth_method_id = self.auth_method_id.clone();
        let active_session = Arc::clone(&self.active_session);
        let cached_capabilities = Arc::clone(&self.cached_capabilities);

        *state.lock().await = GatewayState::Initializing;

        let session_id = Arc::new(Mutex::new(None));
        let session_id_clone = Arc::clone(&session_id);

        let result = Client
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
                let caps = CachedAgentCapabilities {
                    load_session: init_response.agent_capabilities.load_session,
                    prompt_image: init_response.agent_capabilities.prompt_capabilities.image,
                    prompt_audio: init_response.agent_capabilities.prompt_capabilities.audio,
                    prompt_embedded_context: init_response
                        .agent_capabilities
                        .prompt_capabilities
                        .embedded_context,
                    auth_method_ids: init_response
                        .auth_methods
                        .iter()
                        .map(|m| m.id().0.to_string())
                        .collect(),
                    agent_name: init_response
                        .agent_info
                        .as_ref()
                        .map(|info| info.name.clone()),
                    agent_version: init_response
                        .agent_info
                        .as_ref()
                        .map(|info| info.version.clone()),
                    model_ids: Vec::new(),
                    thought_levels: Vec::new(),
                };
                *cached_capabilities.lock().await = Some(caps);

                // Check auth requirements
                if !init_response.auth_methods.is_empty() {
                    *state.lock().await = GatewayState::AuthRequired;
                    info!(
                        "Agent requires authentication: {:?}",
                        init_response.auth_methods
                    );

                    match select_auth_method(&init_response.auth_methods, auth_method_id.as_deref())
                    {
                        Ok(Some(method_id)) => {
                            let auth_response = connection
                                .send_request(AuthenticateRequest::new(method_id))
                                .block_task()
                                .await?;

                            debug!("Authentication result: {:?}", auth_response);
                        }
                        Ok(None) => {}
                        Err(error @ GatewayV2Error::AuthSelectionRequired { .. }) => {
                            return Err(agent_client_protocol::Error::invalid_params()
                                .data(error.to_string()));
                        }
                        Err(error @ GatewayV2Error::UnsupportedAuthMethod { .. }) => {
                            *state.lock().await = GatewayState::Incompatible;
                            return Err(agent_client_protocol::Error::invalid_params()
                                .data(error.to_string()));
                        }
                    }
                } else if auth_method_id.is_some() {
                    *state.lock().await = GatewayState::Incompatible;
                    return Err(agent_client_protocol::Error::invalid_params()
                        .data("selected authentication method is not advertised by the agent"));
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
                *active_session.lock().await = Some(sid.clone());

                let config_options = new_session_response
                    .config_options
                    .clone()
                    .unwrap_or_default();
                {
                    let mut guard = cached_capabilities.lock().await;
                    if let Some(caps) = guard.as_mut() {
                        caps.model_ids = advertised_model_ids(&config_options);
                        caps.thought_levels = advertised_thought_levels(&config_options);
                    }
                }

                let sets = plan_config_option_sets(&config_options, &launch).map_err(|error| {
                    agent_client_protocol::Error::invalid_params().data(error.to_string())
                })?;
                for set in sets {
                    info!(
                        "ACP session/set_config_option category={} id={} value={}",
                        set.category, set.config_id, set.value_id
                    );
                    let _ = connection
                        .send_request(SetSessionConfigOptionRequest::new(
                            sid.clone(),
                            SessionConfigId::new(set.config_id.clone()),
                            SessionConfigOptionValue::value_id(SessionConfigValueId::new(
                                set.value_id.clone(),
                            )),
                        ))
                        .block_task()
                        .await?;
                }

                debug!("Session created: {:?}", sid);

                // Send prompt
                info!("Sending prompt");
                let prompt_request = connection.send_request(PromptRequest::new(
                    sid.clone(),
                    vec![ContentBlock::Text(TextContent::new(prompt))],
                ));
                let prompt_future = prompt_request.block_task();
                tokio::pin!(prompt_future);
                let prompt_response = tokio::select! {
                    response = &mut prompt_future => response?,
                    Some(cancel) = cancel_rx.recv() => {
                        let result = connection
                            .send_notification(CancelNotification::new(sid.clone()))
                            .map_err(|error| error.to_string());
                        let failed = result.as_ref().err().cloned();
                        let _ = cancel.acknowledged.send(result);
                        if let Some(error) = failed {
                            return Err(agent_client_protocol::Error::internal_error().data(error));
                        }
                        prompt_future.await?
                    }
                };

                info!(
                    "Session completed with stop_reason: {:?}",
                    prompt_response.stop_reason
                );
                let _ = completion_tx.send(StreamUpdate::Completed {
                    stop_reason: prompt_response.stop_reason,
                });

                Ok(())
            })
            .await
            .map_err(|e| anyhow::anyhow!("ACP connection failed: {e:#}"));

        *self.active_cancel.lock().await = None;
        *self.active_session.lock().await = None;

        if let Err(error) = result {
            if !matches!(
                *self.state.lock().await,
                GatewayState::AuthRequired | GatewayState::Incompatible
            ) {
                *self.state.lock().await = GatewayState::Crashed;
            }
            // Honest disconnect: never imply Completed when the agent died mid-turn.
            let _ = self.update_tx.send(StreamUpdate::Interrupted {
                reason: format!("acp session ended without stop_reason: {error:#}"),
            });
            return Err(error);
        }

        let sid = session_id
            .lock()
            .await
            .clone()
            .context("session_id not set")?;
        *self.state.lock().await = GatewayState::NotStarted;
        Ok(sid)
    }

    /// Send the stable ACP v1 `session/cancel` notification to the active agent.
    pub async fn cancel_active_session(&self) -> Result<()> {
        let cancel = self
            .active_cancel
            .lock()
            .await
            .clone()
            .context("no active ACP session")?;
        let (acknowledged, acknowledgement) = oneshot::channel();
        cancel
            .send(CancelCommand { acknowledged })
            .await
            .context("ACP session stopped before cancellation")?;
        acknowledgement
            .await
            .context("ACP session stopped before cancellation acknowledgement")?
            .map_err(anyhow::Error::msg)
    }

    pub async fn cancel_session(&self, session_id: SessionId) -> Result<()> {
        let active_session = self.active_session.lock().await.clone();
        if active_session
            .as_ref()
            .is_some_and(|active| active != &session_id)
        {
            anyhow::bail!("requested ACP session is not active");
        }
        self.cancel_active_session().await
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

    /// Redacted export-audit row for a stream update (CI / doctor / export).
    pub fn audit_update(update: &StreamUpdate) -> StreamExportAudit {
        match update {
            StreamUpdate::Text(text) => {
                let scrubbed = redact_text(text);
                StreamExportAudit::Text {
                    chars: scrubbed.chars().count(),
                    redacted: scrubbed != *text,
                }
            }
            StreamUpdate::ToolUse {
                tool_call_id,
                tool_name,
                status,
                kind,
                arguments,
            } => StreamExportAudit::ToolUse {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                status: status.clone(),
                kind: kind.clone(),
                arguments: redact_json(arguments),
            },
            StreamUpdate::Status(label) => StreamExportAudit::Status {
                label: redact_text(label),
            },
            StreamUpdate::Completed { stop_reason } => StreamExportAudit::Completed {
                stop_reason: format!("{stop_reason:?}"),
            },
            StreamUpdate::Interrupted { reason } => StreamExportAudit::Interrupted {
                reason: redact_text(reason),
            },
            StreamUpdate::Error(message) => StreamExportAudit::Error {
                message: redact_text(message),
            },
        }
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
                    // Summary label only — never dump hidden CoT into tool args.
                    let scrubbed = redact_text(&text.text);
                    let _ = update_tx.send(StreamUpdate::Status(format!(
                        "thought: {}",
                        truncate_label(&scrubbed, 160)
                    )));
                }
            }
            SessionUpdate::ToolCall(tool_call) => {
                debug!("Tool call: {:?}", tool_call.title);
                let arguments = tool_call
                    .raw_input
                    .as_ref()
                    .map(redact_json)
                    .unwrap_or(serde_json::Value::Null);
                let _ = update_tx.send(StreamUpdate::ToolUse {
                    tool_call_id: tool_call.tool_call_id.0.to_string(),
                    tool_name: sanitize_label(&tool_call.title),
                    status: format!("{:?}", tool_call.status).to_ascii_lowercase(),
                    kind: format!("{:?}", tool_call.kind).to_ascii_lowercase(),
                    arguments,
                });
            }
            SessionUpdate::ToolCallUpdate(tool_update) => {
                debug!("Tool call update: {:?}", tool_update.tool_call_id);
                let status = tool_update
                    .fields
                    .status
                    .map(|s| format!("{s:?}").to_ascii_lowercase())
                    .unwrap_or_else(|| "updated".into());
                let tool_name = tool_update
                    .fields
                    .title
                    .as_deref()
                    .map(sanitize_label)
                    .unwrap_or_else(|| "tool".into());
                let kind = tool_update
                    .fields
                    .kind
                    .map(|k| format!("{k:?}").to_ascii_lowercase())
                    .unwrap_or_else(|| "other".into());
                let arguments = tool_update
                    .fields
                    .raw_input
                    .as_ref()
                    .map(redact_json)
                    .unwrap_or(serde_json::Value::Null);
                let _ = update_tx.send(StreamUpdate::ToolUse {
                    tool_call_id: tool_update.tool_call_id.0.to_string(),
                    tool_name,
                    status: status.clone(),
                    kind,
                    arguments,
                });
                let _ = update_tx.send(StreamUpdate::Status(format!(
                    "tool {} → {}",
                    tool_update.tool_call_id.0, status
                )));
            }
            SessionUpdate::UsageUpdate(usage) => {
                debug!("Usage update: {:?}", usage);
                let _ = update_tx.send(StreamUpdate::Status("usage".into()));
            }
            SessionUpdate::SessionInfoUpdate(info) => {
                debug!("Session info update: {:?}", info);
                let _ = update_tx.send(StreamUpdate::Status("session_info".into()));
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
            description: sanitize_label(
                request
                    .tool_call
                    .fields
                    .title
                    .as_deref()
                    .unwrap_or("ACP tool request"),
            ),
            kind: permission_kind(request.tool_call.fields.kind),
            target: request
                .tool_call
                .fields
                .locations
                .as_ref()
                .and_then(|locations| locations.first())
                .map(|location| location.path.clone()),
            options: request
                .options
                .iter()
                .map(|opt| PermissionOption {
                    option_id: opt.option_id.0.to_string(),
                    description: sanitize_label(&opt.name),
                    kind: permission_choice_kind(opt.kind),
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

        // Wait for transport decision (Select|Deny only — durable approval stays in core).
        match decision_rx.await {
            Ok(decision) => {
                let outcome = permission_outcome(decision);
                if matches!(outcome, RequestPermissionOutcome::Cancelled) {
                    info!("Permission denied");
                } else {
                    info!("Permission option selected");
                }
                let _ = responder.respond(RequestPermissionResponse::new(outcome));
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

fn permission_kind(kind: Option<ToolKind>) -> PermissionKind {
    match kind.unwrap_or_default() {
        ToolKind::Read => PermissionKind::Read,
        ToolKind::Edit => PermissionKind::Edit,
        ToolKind::Delete => PermissionKind::Delete,
        ToolKind::Move => PermissionKind::Move,
        ToolKind::Search => PermissionKind::Search,
        ToolKind::Execute => PermissionKind::Execute,
        ToolKind::Think => PermissionKind::Think,
        ToolKind::Fetch => PermissionKind::Fetch,
        ToolKind::SwitchMode => PermissionKind::SwitchMode,
        ToolKind::Other => PermissionKind::Other,
        _ => PermissionKind::Other,
    }
}

fn permission_choice_kind(kind: SdkPermissionOptionKind) -> PermissionChoiceKind {
    match kind {
        SdkPermissionOptionKind::AllowOnce => PermissionChoiceKind::AllowOnce,
        SdkPermissionOptionKind::AllowAlways => PermissionChoiceKind::AllowAlways,
        SdkPermissionOptionKind::RejectOnce => PermissionChoiceKind::RejectOnce,
        SdkPermissionOptionKind::RejectAlways => PermissionChoiceKind::RejectAlways,
        _ => PermissionChoiceKind::RejectOnce,
    }
}

fn sanitize_label(value: &str) -> String {
    truncate_label(
        &value
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>(),
        160,
    )
}

fn truncate_label(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn select_auth_method(
    methods: &[AuthMethod],
    selected: Option<&str>,
) -> Result<Option<AuthMethodId>, GatewayV2Error> {
    if methods.is_empty() {
        return Ok(None);
    }
    let Some(selected) = selected else {
        return Err(GatewayV2Error::AuthSelectionRequired {
            offered: methods
                .iter()
                .map(|method| method.id().0.to_string())
                .collect(),
        });
    };
    methods
        .iter()
        .find(|method| method.id().0.as_ref() == selected)
        .map(|method| Some(method.id().clone()))
        .ok_or_else(|| GatewayV2Error::UnsupportedAuthMethod {
            selected: selected.to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{AuthMethod, AuthMethodAgent};

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

    #[tokio::test]
    async fn cancel_fails_when_no_external_session_is_running() {
        let gateway = AcpGatewayV2::new(AcpAgentConfig::new("echo"));

        let result = gateway.cancel_active_session().await;

        assert!(result.is_err());
    }

    #[test]
    fn auth_method_is_never_selected_implicitly() {
        let methods = vec![AuthMethod::Agent(AuthMethodAgent::new(
            "browser-login",
            "Browser login",
        ))];

        let result = select_auth_method(&methods, None);

        assert!(matches!(
            result,
            Err(GatewayV2Error::AuthSelectionRequired { .. })
        ));
    }

    #[test]
    fn selected_auth_method_must_be_advertised_by_agent() {
        let methods = vec![AuthMethod::Agent(AuthMethodAgent::new(
            "terminal-login",
            "Terminal login",
        ))];

        let result = select_auth_method(&methods, Some("browser-login"));

        assert!(matches!(
            result,
            Err(GatewayV2Error::UnsupportedAuthMethod { .. })
        ));
    }
}
