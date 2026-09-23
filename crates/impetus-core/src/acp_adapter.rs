//! ACP adapter for ModelProvider trait.
//!
//! Links an external coding-agent via AcpGatewayV2 to the harness ModelProvider.
//! Agent owns authentication; Impetus owns policy, session state, and orchestration.

use crate::{
    Action, ActionKind, ActionOrigin, AgentRuntime, ApprovalResolution, FinishReason,
    ModelProvider, PolicyDecision, PolicyEngine, ProviderError, ProviderHealth, ProviderMessage,
    StreamEvent, StreamOptions,
};
use agent_client_protocol::AcpAgentConfig;
use async_trait::async_trait;
use impetus_acp_gateway::{
    AcpBackendStatus, AcpGatewayV2, AcpHealthKind, GatewayState, PermissionDecision,
    PermissionKind, PermissionRequest, SessionLaunchOptions, StreamUpdate,
};
use impetus_protocol::AgentCapabilitySnapshot;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Adapter for ACP gateway V2 as ModelProvider.
#[derive(Debug)]
pub struct AcpAdapter {
    gateway: Arc<AcpGatewayV2>,
    policy: Arc<PolicyEngine>,
    provider_id: String,
    model_id: String,
    workspace_dir: PathBuf,
}

impl AcpAdapter {
    pub fn new(
        config: AcpAgentConfig,
        auth_method_id: Option<String>,
        provider_id: String,
        model_id: String,
        workspace_dir: PathBuf,
        policy: Arc<PolicyEngine>,
    ) -> Self {
        let gateway = AcpGatewayV2::new(config).with_auth_method(auth_method_id);

        Self {
            gateway: Arc::new(gateway),
            policy,
            provider_id,
            model_id,
            workspace_dir,
        }
    }

    /// Test/probe: seed ACP cached config_options without a live agent session.
    pub async fn seed_cached_capabilities_for_test(
        &self,
        caps: impetus_acp_gateway::CachedAgentCapabilities,
    ) {
        self.gateway.seed_cached_capabilities(caps).await;
    }

    /// Doctor / status surface: gateway state + cached agent labels.
    pub fn backend_status(&self) -> AcpBackendStatus {
        let state = self
            .gateway
            .state_blocking()
            .unwrap_or(GatewayState::NotStarted);
        let caps = self.gateway.cached_capabilities_blocking().ok().flatten();
        AcpBackendStatus::from_state(state, caps.as_ref())
    }
}

fn map_stop_reason(stop: agent_client_protocol::schema::v1::StopReason) -> FinishReason {
    use agent_client_protocol::schema::v1::StopReason;
    match stop {
        StopReason::EndTurn => FinishReason::Stop,
        StopReason::MaxTokens => FinishReason::Length,
        StopReason::MaxTurnRequests => FinishReason::Other,
        StopReason::Refusal => FinishReason::ContentFilter,
        StopReason::Cancelled => FinishReason::Other,
        _ => FinishReason::Other,
    }
}

fn tool_use_to_stream_event(
    tool_call_id: String,
    tool_name: String,
    status: &str,
    arguments: serde_json::Value,
) -> Option<StreamEvent> {
    // Emit ToolCall when the agent starts / has input — ToolOrchestrator consumes these.
    let status = status.to_ascii_lowercase();
    if status.contains("failed") {
        return None;
    }
    if status.contains("completed") || status.contains("update") {
        // Progress-only updates stay as Status via caller; skip duplicate ToolCall.
        if arguments.is_null() {
            return None;
        }
    }
    Some(StreamEvent::ToolCall {
        id: tool_call_id,
        name: tool_name,
        arguments,
    })
}

fn action_for_permission(request: &PermissionRequest, workspace: &Path) -> Option<Action> {
    let kind = match request.kind {
        PermissionKind::Read | PermissionKind::Search => ActionKind::ReadFile,
        PermissionKind::Edit | PermissionKind::Delete | PermissionKind::Move => {
            ActionKind::WriteFile
        }
        PermissionKind::Execute => ActionKind::SpawnProcess,
        PermissionKind::Fetch => ActionKind::NetworkConnect,
        PermissionKind::Think | PermissionKind::SwitchMode | PermissionKind::Other => return None,
    };
    let target = match kind {
        ActionKind::ReadFile | ActionKind::WriteFile => request
            .target
            .as_ref()
            .map(|target| target.to_string_lossy().into_owned()),
        ActionKind::SpawnProcess | ActionKind::NetworkConnect => {
            Some(workspace.to_string_lossy().into_owned())
        }
        _ => None,
    };
    Some(Action {
        origin: ActionOrigin::Agent,
        kind,
        summary: request.description.clone(),
        target,
    })
}

fn select_allow_or_deny(request: &PermissionRequest) -> PermissionDecision {
    let allow_option = request.options.iter().find(|opt| {
        matches!(
            opt.kind,
            impetus_acp_gateway::PermissionChoiceKind::AllowOnce
                | impetus_acp_gateway::PermissionChoiceKind::AllowAlways
        )
    });
    if let Some(opt) = allow_option {
        PermissionDecision::Select(opt.option_id.clone())
    } else {
        PermissionDecision::Deny
    }
}

/// Map Policy → ACP PermissionDecision. `NeedsApproval` creates a durable
/// ApprovalRequest and waits for user `ResolveApproval` (never self-approves).
async fn decide_permission(
    policy: &PolicyEngine,
    request: &PermissionRequest,
    workspace: &Path,
    runtime: Option<&AgentRuntime>,
    cancel: &CancellationToken,
) -> PermissionDecision {
    let Some(action) = action_for_permission(request, workspace) else {
        // Think / SwitchMode / Other: fail-closed (no silent Allow).
        debug!(
            "Permission kind {:?} has no Policy Action mapping — Deny",
            request.kind
        );
        return PermissionDecision::Deny;
    };

    match policy.evaluate(&action) {
        PolicyDecision::Allow => {
            let decision = select_allow_or_deny(request);
            if matches!(decision, PermissionDecision::Select(_)) {
                info!("Policy allowed: selecting allow option");
            } else {
                warn!("Policy allowed but no allow option available");
            }
            decision
        }
        PolicyDecision::Deny { reason } => {
            info!("Policy denied: {}", reason);
            PermissionDecision::Deny
        }
        PolicyDecision::NeedsApproval { reason } => {
            info!("Policy requires approval: {}", reason);
            broker_needs_approval(runtime, action, request, cancel).await
        }
    }
}

async fn broker_needs_approval(
    runtime: Option<&AgentRuntime>,
    action: Action,
    request: &PermissionRequest,
    cancel: &CancellationToken,
) -> PermissionDecision {
    let Some(runtime) = runtime else {
        warn!("NeedsApproval without durable runtime; denying (no silent Allow)");
        return PermissionDecision::Deny;
    };

    match runtime.request_action(action.clone()) {
        Ok(crate::RuntimeStatus::AwaitingApproval) => {}
        Ok(status) => {
            warn!(
                "NeedsApproval expected AwaitingApproval, got {:?}; denying",
                status
            );
            return PermissionDecision::Deny;
        }
        Err(error) => {
            warn!("Failed to create durable approval: {}; denying", error);
            return PermissionDecision::Deny;
        }
    }

    let Some(pending) = (match runtime.latest_pending_approval_for(&action) {
        Ok(pending) => pending,
        Err(error) => {
            warn!("Failed to load pending approval: {}; denying", error);
            return PermissionDecision::Deny;
        }
    }) else {
        warn!("Durable approval missing after request_action; denying");
        return PermissionDecision::Deny;
    };

    info!("ACP permission awaiting durable approval {}", pending.id);

    match runtime
        .wait_approval_resolution(pending.id, cancel.clone())
        .await
    {
        Ok(true) => {
            info!("User approved ACP permission {}", pending.id);
            select_allow_or_deny(request)
        }
        Ok(false) => {
            info!("User denied ACP permission {}", pending.id);
            PermissionDecision::Deny
        }
        Err(error) => {
            warn!(
                "ACP approval wait ended without accept ({}): denying",
                error
            );
            // Cancel/timeout honesty: clear durable pending so status is not
            // stuck AwaitingApproval after the ACP oneshot already Denies.
            if let Ok(Some(still_pending)) = runtime.pending_approval(pending.id) {
                let _ = runtime.resolve_approval(ApprovalResolution::user(&still_pending, false));
            }
            PermissionDecision::Deny
        }
    }
}

#[async_trait]
impl ModelProvider for AcpAdapter {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn health(&self) -> ProviderHealth {
        match self.backend_status().health {
            AcpHealthKind::Healthy => ProviderHealth::Healthy,
            AcpHealthKind::Unknown => ProviderHealth::Unknown,
            AcpHealthKind::Unavailable => ProviderHealth::Unavailable {
                last_error_redacted: self
                    .backend_status()
                    .detail_redacted
                    .unwrap_or_else(|| "acp backend unavailable".into()),
            },
        }
    }

    fn agent_capabilities(&self) -> Option<AgentCapabilitySnapshot> {
        // Sync snapshot: try_lock so ListModels never blocks the IPC thread.
        let Ok(guard) = self.gateway.cached_capabilities_blocking() else {
            return None;
        };
        guard.map(|caps| AgentCapabilitySnapshot {
            load_session: caps.load_session,
            prompt_image: caps.prompt_image,
            prompt_audio: caps.prompt_audio,
            prompt_embedded_context: caps.prompt_embedded_context,
            auth_method_ids: caps.auth_method_ids,
            agent_name: caps.agent_name,
            agent_version: caps.agent_version,
            model_ids: caps.model_ids,
            thought_levels: caps.thought_levels,
        })
    }

    async fn discover_models(&self) -> crate::ModelCatalogResult {
        use crate::{ModelCatalogEntry, ModelCatalogResult};
        use impetus_protocol::ModelCapabilityFlags;

        let caps = self.gateway.cached_capabilities().await;
        let Some(caps) = caps else {
            // Honest: no session yet → profile model id only, empty efforts.
            return ModelCatalogResult::StaticFallback {
                models: vec![ModelCatalogEntry::id_only(self.model_id.clone())],
                reason_redacted: "ACP config_options not probed until first session".into(),
            };
        };

        let efforts = caps.thought_levels.clone();
        // Honest: never invent tools=true; only signal reasoning when agent
        // advertised non-empty thought_levels via initialize/config_options.
        let mut capabilities = ModelCapabilityFlags::default();
        if !efforts.is_empty() {
            capabilities.reasoning = true;
        }

        let model_ids = if caps.model_ids.is_empty() {
            vec![self.model_id.clone()]
        } else {
            caps.model_ids.clone()
        };

        let models = model_ids
            .into_iter()
            .map(|model_id| ModelCatalogEntry {
                model_id,
                model_display_name: None,
                reasoning_efforts: efforts.clone(),
                // Honest: do not invent a default effort when agent did not say.
                default_reasoning_effort: None,
                capabilities: capabilities.clone(),
                provider_options: serde_json::json!({ "source": "acp_config_options" }),
            })
            .collect();

        ModelCatalogResult::Discovered { models }
    }

    async fn stream_messages(
        &self,
        messages: &[ProviderMessage],
        _credential: Option<&str>,
        runtime: Option<Arc<crate::AgentRuntime>>,
        cancel: CancellationToken,
        options: StreamOptions,
        mut on_event: Box<dyn FnMut(StreamEvent) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        let effective_model = options
            .model_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .unwrap_or(self.model_id.as_str())
            .to_owned();
        let model_overridden = options
            .model_id
            .as_deref()
            .is_some_and(|id| !id.is_empty() && id != self.model_id);
        let reasoning_set = options
            .reasoning_effort
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
        let launch = SessionLaunchOptions {
            model_id: Some(effective_model),
            reasoning_effort: options.reasoning_effort.clone(),
            // Fail closed when the client explicitly changed model/reasoning.
            strict: model_overridden || reasoning_set,
        };

        // Check state
        let state = self.gateway.state().await;
        if state == GatewayState::Crashed || state == GatewayState::Incompatible {
            return Err(ProviderError::RequestFailed(format!(
                "agent in bad state: {:?}",
                state
            )));
        }

        // Build prompt from messages; model/reasoning applied via ACP
        // session/set_config_option before session/prompt (not log-only).
        let prompt = messages
            .iter()
            .map(|msg| format!("{}: {}", msg.role(), msg.content()))
            .collect::<Vec<_>>()
            .join("\n\n");

        info!("Starting ACP session with prompt length: {}", prompt.len());

        let gateway = Arc::clone(&self.gateway);
        let workspace = self.workspace_dir.clone();

        let mut session_handle = tokio::spawn(async move {
            gateway
                .start_session_with_options(workspace, prompt, launch)
                .await
        });

        // Honest completion: Ok(()) only after an explicit ACP stop_reason.
        let mut saw_completed = false;
        let mut session_task_done = false;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    warn!("ACP session cancelled");
                    if let Err(error) = self.gateway.cancel_active_session().await {
                        debug!("ACP cancellation notification was not acknowledged: {}", error);
                    }
                    session_handle.abort();
                    let _ = session_handle.await;
                    return Err(ProviderError::Cancelled);
                }

                update = self.gateway.recv_update(), if !session_task_done || !saw_completed => {
                    match update {
                        Some(StreamUpdate::Text(text)) => {
                            debug!("Received text chunk: {} chars", text.len());
                            on_event(StreamEvent::TextDelta { delta: text }).map_err(|e| {
                                error!("Event callback failed: {:?}", e);
                                e
                            })?;
                        }
                        Some(StreamUpdate::ToolUse {
                            tool_call_id,
                            tool_name,
                            status,
                            kind: _,
                            arguments,
                        }) => {
                            debug!("Tool use: {} ({}) - {}", tool_name, tool_call_id, status);
                            if let Some(event) = tool_use_to_stream_event(
                                tool_call_id,
                                tool_name,
                                &status,
                                arguments,
                            ) {
                                on_event(event)?;
                            } else {
                                on_event(StreamEvent::Reasoning {
                                    content: format!("tool status: {status}"),
                                })?;
                            }
                        }
                        Some(StreamUpdate::Status(status)) => {
                            debug!("Status update: {}", status);
                            on_event(StreamEvent::Reasoning {
                                content: status,
                            })?;
                        }
                        Some(StreamUpdate::Completed { stop_reason }) => {
                            info!("Session completed: {:?}", stop_reason);
                            saw_completed = true;
                            if matches!(
                                stop_reason,
                                agent_client_protocol::schema::v1::StopReason::Cancelled
                            ) {
                                return Err(ProviderError::Cancelled);
                            }
                            on_event(StreamEvent::Finish {
                                reason: map_stop_reason(stop_reason),
                            })?;
                            if session_task_done {
                                break;
                            }
                        }
                        Some(StreamUpdate::Interrupted { reason }) => {
                            error!("ACP interrupted without stop_reason: {}", reason);
                            return Err(ProviderError::InterruptedUnknown(reason));
                        }
                        Some(StreamUpdate::Error(err)) => {
                            error!("Agent error: {}", err);
                            return Err(ProviderError::RequestFailed(err));
                        }
                        None => {
                            if saw_completed {
                                break;
                            }
                            return Err(ProviderError::InterruptedUnknown(
                                "acp update channel closed without stop_reason".into(),
                            ));
                        }
                    }
                }

                perm_req = self.gateway.recv_permission_request(), if !session_task_done => {
                    match perm_req {
                        Some((request, response_tx)) => {
                            warn!(
                                "Permission request received: {} - {}",
                                request.request_id, request.description
                            );

                            let decision = decide_permission(
                                self.policy.as_ref(),
                                &request,
                                &self.workspace_dir,
                                runtime.as_deref(),
                                &cancel,
                            )
                            .await;

                            if response_tx.send(decision).is_err() {
                                error!("Failed to send permission decision");
                            }
                        }
                        None => {
                            // Permission channel closed; keep draining updates.
                        }
                    }
                }

                session_result = &mut session_handle, if !session_task_done => {
                    session_task_done = true;
                    match session_result {
                        Ok(Ok(session_id)) => {
                            info!("Session task finished: {:?}", session_id);
                            if saw_completed {
                                break;
                            }
                            // Drain: Completed may still be in the update channel.
                        }
                        Ok(Err(e)) => {
                            error!("Session failed: {}", e);
                            if saw_completed {
                                break;
                            }
                            return Err(ProviderError::InterruptedUnknown(format!(
                                "acp session error without stop_reason: {e}"
                            )));
                        }
                        Err(e) => {
                            error!("Session task panicked: {}", e);
                            return Err(ProviderError::InterruptedUnknown(format!(
                                "acp session task panic: {e}"
                            )));
                        }
                    }
                }
            }
        }

        if saw_completed {
            Ok(())
        } else {
            Err(ProviderError::InterruptedUnknown(
                "acp stream ended without stop_reason".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApprovalResolution, EventPayload, EventStore, MemoryEventStore, SandboxScope};
    use impetus_acp_gateway::{PermissionChoiceKind, PermissionKind, PermissionOption};

    fn edit_permission(workspace: &Path) -> PermissionRequest {
        let target = workspace.join("file.txt");
        PermissionRequest {
            request_id: "permission-1".into(),
            description: "Edit file".into(),
            kind: PermissionKind::Edit,
            target: Some(target),
            options: vec![PermissionOption {
                option_id: "allow-once".into(),
                description: "Allow once".into(),
                kind: PermissionChoiceKind::AllowOnce,
            }],
        }
    }

    #[test]
    fn edit_permission_is_normalized_as_agent_write_action() {
        let workspace = tempfile::tempdir().expect("workspace");
        let request = edit_permission(workspace.path());
        let action = action_for_permission(&request, workspace.path()).expect("known action");

        assert_eq!(action.origin, crate::ActionOrigin::Agent);
        assert_eq!(action.kind, crate::ActionKind::WriteFile);
        assert_eq!(
            action.target.as_deref(),
            request.target.as_ref().and_then(|p| p.to_str())
        );
    }

    #[test]
    fn agent_capability_snapshot_has_no_vendor_prefix_fields() {
        let snap = AgentCapabilitySnapshot {
            load_session: true,
            prompt_image: false,
            prompt_audio: false,
            prompt_embedded_context: true,
            auth_method_ids: vec!["env".into()],
            agent_name: Some("mock-agent".into()),
            agent_version: Some("0.1.0".into()),
            model_ids: vec!["gpt-test".into()],
            thought_levels: vec!["low".into(), "xhigh".into()],
        };
        let encoded = serde_json::to_string(&snap).expect("encode");
        assert!(!encoded.contains("codex_"));
        assert!(encoded.contains("load_session"));
        assert!(encoded.contains("auth_method_ids"));
        assert!(encoded.contains("thought_levels"));
        assert!(encoded.contains("model_ids"));
    }

    #[tokio::test]
    async fn discover_models_uses_cached_config_options_as_sot() {
        use crate::{ModelCatalogResult, ModelProvider};
        use agent_client_protocol::AcpAgentConfig;
        use impetus_acp_gateway::CachedAgentCapabilities;

        let workspace = tempfile::tempdir().expect("workspace");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let adapter = AcpAdapter::new(
            AcpAgentConfig::new("echo"),
            None,
            "acp".into(),
            "profile-default".into(),
            workspace.path().to_path_buf(),
            Arc::new(policy),
        );
        adapter
            .seed_cached_capabilities_for_test(CachedAgentCapabilities {
                model_ids: vec!["codex-a".into(), "codex-b".into()],
                thought_levels: vec!["minimal".into(), "high".into(), "xhigh".into()],
                ..Default::default()
            })
            .await;

        let catalog = adapter.discover_models().await;
        let ModelCatalogResult::Discovered { models } = catalog else {
            panic!("expected Discovered, got {catalog:?}");
        };
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].model_id, "codex-a");
        assert_eq!(
            models[0].reasoning_efforts,
            vec!["minimal", "high", "xhigh"]
        );
        assert!(models[0].default_reasoning_effort.is_none());
        assert!(models[0].capabilities.reasoning);
        assert!(
            !models[0].capabilities.tools,
            "discover_models must not invent tools=true"
        );
        let snap = adapter.agent_capabilities().expect("caps");
        assert_eq!(snap.model_ids, vec!["codex-a", "codex-b"]);
        assert_eq!(snap.thought_levels, vec!["minimal", "high", "xhigh"]);
    }

    #[tokio::test]
    async fn needs_approval_without_runtime_denies() {
        let workspace = tempfile::tempdir().expect("workspace");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let request = edit_permission(workspace.path());
        let cancel = CancellationToken::new();

        let decision = decide_permission(&policy, &request, workspace.path(), None, &cancel).await;
        assert!(matches!(decision, PermissionDecision::Deny));
    }

    #[tokio::test]
    async fn needs_approval_creates_pending_and_allow_selects_option() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = AgentRuntime::create_with_workspace(
            store.clone(),
            policy.clone(),
            workspace.path().to_path_buf(),
        )
        .expect("runtime");
        runtime
            .submit_intent("edit via acp")
            .expect("intent revision");

        let request = edit_permission(workspace.path());
        let cancel = CancellationToken::new();
        let session_id = runtime.session_id();
        let action = action_for_permission(&request, workspace.path()).expect("action");

        let waiter = tokio::spawn({
            let policy = policy.clone();
            let workspace = workspace.path().to_path_buf();
            let request = request.clone();
            let cancel = cancel.clone();
            async move {
                decide_permission(&policy, &request, &workspace, Some(&runtime), &cancel).await
            }
        });

        // Wait until durable pending approval appears.
        let approval_id = {
            let mut id = None;
            for _ in 0..100 {
                let attach = AgentRuntime::attach(store.clone(), policy.clone(), session_id)
                    .expect("attach");
                if let Some(pending) = attach
                    .latest_pending_approval_for(&action)
                    .expect("pending lookup")
                {
                    id = Some(pending.id);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            id.expect("pending approval created")
        };

        let resolver = AgentRuntime::attach(store.clone(), policy.clone(), session_id)
            .expect("resolver attach");
        let pending = resolver
            .pending_approval(approval_id)
            .expect("load")
            .expect("still pending");
        resolver
            .resolve_approval(ApprovalResolution::user(&pending, true))
            .expect("user allow");

        let decision = waiter.await.expect("join");
        assert!(matches!(
            decision,
            PermissionDecision::Select(ref id) if id == "allow-once"
        ));

        let events = store.list(session_id).expect("events");
        assert!(events.iter().any(|event| {
            matches!(
                &event.payload,
                EventPayload::Approval(crate::ApprovalEvent::Requested { request })
                    if request.id == approval_id
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                &event.payload,
                EventPayload::Approval(crate::ApprovalEvent::Resolved { request })
                    if request.id == approval_id
                        && matches!(request.state, crate::ApprovalState::Approved)
            )
        }));
    }

    #[tokio::test]
    async fn needs_approval_deny_resolution_maps_to_permission_deny() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = AgentRuntime::create_with_workspace(
            store.clone(),
            policy.clone(),
            workspace.path().to_path_buf(),
        )
        .expect("runtime");
        runtime.submit_intent("edit via acp").expect("intent");

        let request = edit_permission(workspace.path());
        let cancel = CancellationToken::new();
        let session_id = runtime.session_id();
        let action = action_for_permission(&request, workspace.path()).expect("action");

        let waiter = tokio::spawn({
            let policy = policy.clone();
            let workspace = workspace.path().to_path_buf();
            let request = request.clone();
            let cancel = cancel.clone();
            async move {
                decide_permission(&policy, &request, &workspace, Some(&runtime), &cancel).await
            }
        });

        let approval_id = {
            let mut id = None;
            for _ in 0..100 {
                let attach = AgentRuntime::attach(store.clone(), policy.clone(), session_id)
                    .expect("attach");
                if let Some(pending) = attach
                    .latest_pending_approval_for(&action)
                    .expect("pending lookup")
                {
                    id = Some(pending.id);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            id.expect("pending approval created")
        };

        let resolver = AgentRuntime::attach(store.clone(), policy.clone(), session_id)
            .expect("resolver attach");
        let pending = resolver
            .pending_approval(approval_id)
            .expect("load")
            .expect("still pending");
        resolver
            .resolve_approval(ApprovalResolution::user(&pending, false))
            .expect("user deny");

        let decision = waiter.await.expect("join");
        assert!(matches!(decision, PermissionDecision::Deny));
    }

    #[tokio::test]
    async fn needs_approval_cancel_while_waiting_denies() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = AgentRuntime::create_with_workspace(
            store.clone(),
            policy.clone(),
            workspace.path().to_path_buf(),
        )
        .expect("runtime");
        runtime.submit_intent("edit via acp").expect("intent");

        let request = edit_permission(workspace.path());
        let cancel = CancellationToken::new();
        let session_id = runtime.session_id();
        let action = action_for_permission(&request, workspace.path()).expect("action");

        let waiter = tokio::spawn({
            let policy = policy.clone();
            let workspace = workspace.path().to_path_buf();
            let request = request.clone();
            let cancel = cancel.clone();
            async move {
                decide_permission(&policy, &request, &workspace, Some(&runtime), &cancel).await
            }
        });

        for _ in 0..100 {
            let attach =
                AgentRuntime::attach(store.clone(), policy.clone(), session_id).expect("attach");
            if attach
                .latest_pending_approval_for(&action)
                .expect("pending")
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        cancel.cancel();
        let decision = waiter.await.expect("join");
        assert!(matches!(decision, PermissionDecision::Deny));

        let attach =
            AgentRuntime::attach(store.clone(), policy.clone(), session_id).expect("attach");
        assert!(
            attach
                .latest_pending_approval_for(&action)
                .expect("lookup")
                .is_none(),
            "cancel must clear durable pending approval"
        );
    }

    #[test]
    fn tool_use_maps_to_stream_event_for_orchestrator() {
        let event = tool_use_to_stream_event(
            "tc-9".into(),
            "read_file".into(),
            "inprogress",
            serde_json::json!({"path": "README.md"}),
        )
        .expect("tool call");
        match event {
            StreamEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, "tc-9");
                assert_eq!(name, "read_file");
                assert_eq!(arguments["path"], "README.md");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn health_unknown_before_session() {
        use agent_client_protocol::AcpAgentConfig;
        let workspace = tempfile::tempdir().expect("workspace");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let adapter = AcpAdapter::new(
            AcpAgentConfig::new("echo"),
            None,
            "acp".into(),
            "model".into(),
            workspace.path().to_path_buf(),
            Arc::new(policy),
        );
        assert_eq!(adapter.health(), ProviderHealth::Unknown);
        assert_eq!(
            adapter.backend_status().gateway_state,
            GatewayState::NotStarted
        );
    }

    #[test]
    fn stop_reason_end_turn_maps_to_finish_stop() {
        assert_eq!(
            map_stop_reason(agent_client_protocol::schema::v1::StopReason::EndTurn),
            FinishReason::Stop
        );
    }

    #[test]
    fn interrupted_unknown_is_distinct_from_request_failed() {
        let err = ProviderError::InterruptedUnknown("disconnect".into());
        assert!(matches!(err, ProviderError::InterruptedUnknown(_)));
        assert!(!err.is_transient());
    }
}
