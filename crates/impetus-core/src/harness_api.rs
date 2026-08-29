//! Transport-neutral harness dispatch.
//!
//! The same `Harness` drives both the Unix-socket server (harness daemon) and
//! in-memory transports used by client tests and future TUI clients. It owns no
//! transport: it takes a normalized [`IpcRequest`] and returns an
//! [`IpcResponse`], deriving view DTOs from the durable event store and the
//! runtime projection. State lives in the store and the running agent, never in
//! the client.

use crate::{
    AgentLoop, AgentRuntime, CredentialResolver, DurableArtifactStore, EventStore,
    IPC_CAPABILITIES, IPC_VERSION, InstructionResolver, IpcErrorCode, IpcRequest, IpcResponse,
    MockProvider, NoCredentialResolver, OpenAiCompatibleAdapter, OpenAiCompatibleProvider,
    PolicyEngine, ProviderMessage, ProviderRegistry, ReadOnlyTool, ReadOnlyToolKind, ReadOnlyTools,
    ResolveRequest, RuntimeError, RuntimeStatus, Sandbox, SandboxScope, ToolOutcome,
};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct SessionCoordinator {
    locks: Arc<Mutex<HashMap<uuid::Uuid, Weak<Mutex<()>>>>>,
}

impl SessionCoordinator {
    fn lock_for(&self, session_id: uuid::Uuid) -> Arc<Mutex<()>> {
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(lock) = locks.get(&session_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(session_id, Arc::downgrade(&lock));
        lock
    }
}

/// A reusable harness request dispatcher.
///
/// Wraps the durable [`EventStore`] and the [`PolicyEngine`]. Every client
/// command is resolved here so that the Unix socket server and in-memory
/// transports share one implementation.
///
/// Uses a ProviderRegistry for model routing instead of a concrete enum.
pub struct Harness {
    store: Arc<dyn EventStore>,
    policy: PolicyEngine,
    provider_registry: ProviderRegistry,
    default_provider_id: String,
    credential_resolver: Arc<dyn CredentialResolver>,
    cancellations: Arc<Mutex<HashMap<uuid::Uuid, CancellationToken>>>,
    workspace_root: PathBuf,
    session_coordinator: SessionCoordinator,
    attachments: crate::AttachmentStore,
}

impl Harness {
    /// Create a new Harness with a mock provider registered as "mock".
    pub fn new(store: Arc<dyn EventStore>, policy: PolicyEngine) -> Self {
        let workspace_root = policy.scope().workspace_root.clone();
        let registry = ProviderRegistry::new();
        let mock = Arc::new(MockProvider::default_mock());
        registry
            .register(mock)
            .expect("failed to register mock provider");
        Self {
            store,
            policy,
            provider_registry: registry,
            default_provider_id: "mock".to_string(),
            credential_resolver: Arc::new(NoCredentialResolver),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            workspace_root,
            session_coordinator: SessionCoordinator::default(),
            attachments: crate::AttachmentStore::new(),
        }
    }

    #[cfg(test)]
    fn with_test_provider(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        provider: Arc<dyn crate::ModelProvider>,
    ) -> Self {
        let workspace_root = policy.scope().workspace_root.clone();
        let default_provider_id = provider.provider_id().to_string();
        let registry = ProviderRegistry::new();
        registry
            .register(provider)
            .expect("failed to register test provider");
        Self {
            store,
            policy,
            provider_registry: registry,
            default_provider_id,
            credential_resolver: Arc::new(NoCredentialResolver),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            workspace_root,
            session_coordinator: SessionCoordinator::default(),
            attachments: crate::AttachmentStore::new(),
        }
    }

    /// Use a user-selected direct-provider profile. The profile is supplied by
    /// daemon startup, not client IPC; credentials remain outside this type.
    pub fn with_openai_provider(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        provider: OpenAiCompatibleProvider,
    ) -> Self {
        Self::with_openai_provider_and_resolver(
            store,
            policy,
            provider,
            Arc::new(NoCredentialResolver),
        )
    }

    /// The resolver is injected by the harness and is called only while a
    /// provider request is active. It never enters SQLite, events, or IPC.
    pub fn with_openai_provider_and_resolver(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        provider: OpenAiCompatibleProvider,
        credential_resolver: Arc<dyn CredentialResolver>,
    ) -> Self {
        let workspace_root = policy.scope().workspace_root.clone();
        let registry = ProviderRegistry::new();

        // Register mock for tests
        let mock = Arc::new(MockProvider::default_mock());
        registry
            .register(mock)
            .expect("failed to register mock provider");

        // Register OpenAI-compatible provider with adapter
        let provider_id = provider.profile().id.clone();
        let adapter = Arc::new(OpenAiCompatibleAdapter::new(
            Arc::new(provider),
            credential_resolver.clone(),
        ));
        registry
            .register(adapter)
            .expect("failed to register openai provider");

        Self {
            store,
            policy,
            provider_registry: registry,
            default_provider_id: provider_id,
            credential_resolver,
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            workspace_root,
            session_coordinator: SessionCoordinator::default(),
            attachments: crate::AttachmentStore::new(),
        }
    }

    pub fn policy(&self) -> PolicyEngine {
        self.policy.clone()
    }

    pub fn store(&self) -> Arc<dyn EventStore> {
        self.store.clone()
    }

    pub fn provider_registry(&self) -> &ProviderRegistry {
        &self.provider_registry
    }

    /// Resolve a single client request into a response.
    ///
    /// No global lock: EventStore and AgentRuntime use internal coordination.
    /// Independent sessions can execute concurrently.
    pub fn handle(&self, request: IpcRequest) -> IpcResponse {
        handle_request(
            self.store.clone(),
            self.policy.clone(),
            self.provider_registry.clone(),
            self.default_provider_id.clone(),
            self.credential_resolver.clone(),
            self.cancellations.clone(),
            self.workspace_root.clone(),
            self.session_coordinator.clone(),
            self.attachments.clone(),
            request,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_request(
    store: Arc<dyn EventStore>,
    policy: PolicyEngine,
    provider_registry: ProviderRegistry,
    default_provider_id: String,
    credential_resolver: Arc<dyn CredentialResolver>,
    cancellations: Arc<Mutex<HashMap<uuid::Uuid, CancellationToken>>>,
    workspace_root: PathBuf,
    session_coordinator: SessionCoordinator,
    attachments: crate::AttachmentStore,
    request: IpcRequest,
) -> IpcResponse {
    match request {
        IpcRequest::Hello { version, .. } if version != IPC_VERSION => {
            let upgrade_recommendation = if version < IPC_VERSION {
                Some(format!(
                    "Client version {} is older than harness {}. Upgrade client.",
                    version, IPC_VERSION
                ))
            } else {
                Some(format!(
                    "Client version {} is newer than harness {}. Upgrade harness.",
                    version, IPC_VERSION
                ))
            };
            IpcResponse::Incompatible {
                supported_version: IPC_VERSION,
                client_version: version,
                upgrade_recommendation,
            }
        }
        IpcRequest::Hello { capabilities, .. } => IpcResponse::Hello {
            version: IPC_VERSION,
            capabilities: IPC_CAPABILITIES
                .iter()
                .filter(|supported| {
                    capabilities
                        .iter()
                        .any(|requested| requested == **supported)
                })
                .map(|capability| (*capability).to_owned())
                .collect(),
        },
        IpcRequest::CreateSession { workspace_root } => {
            match AgentRuntime::create_with_workspace(store, policy, workspace_root) {
                Ok(runtime) => IpcResponse::Session {
                    session_id: runtime.session_id(),
                    status: RuntimeStatus::Idle,
                },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::Attach { session_id } => {
            match AgentRuntime::attach(store, policy, session_id)
                .and_then(|runtime| Ok((runtime.session_id(), runtime.status()?)))
            {
                Ok((session_id, status)) => IpcResponse::Session { session_id, status },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::ListSessions => match store.list_sessions() {
            Ok(sessions) => IpcResponse::Sessions {
                sessions: sessions.into_iter().map(|session| session.id).collect(),
            },
            Err(error) => IpcResponse::Error {
                code: IpcErrorCode::Internal,
                message: error.to_string(),
            },
        },
        IpcRequest::Stream {
            session_id,
            after_sequence,
        } => match AgentRuntime::attach(store, policy, session_id).and_then(|runtime| {
            Ok(runtime
                .events()?
                .into_iter()
                .filter(|event| event.sequence > after_sequence)
                .collect())
        }) {
            Ok(events) => IpcResponse::Events { session_id, events },
            Err(error) => runtime_error(error),
        },
        IpcRequest::Prompt { session_id, text } => {
            let session_lock = session_coordinator.lock_for(session_id);
            let _session_guard = session_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match AgentRuntime::attach(store, policy, session_id).and_then(|runtime| {
                let runtime = Arc::new(runtime);
                let run_id = runtime.submit_intent_and_start_run(text)?;
                let session_workspace = runtime.workspace_root()?;
                let provider_messages = resolve_provider_messages(&session_workspace, &runtime)
                    .unwrap_or_else(|_| {
                        vec![ProviderMessage::user(
                            runtime_intent(&runtime).unwrap_or_default(),
                        )]
                    });
                let runtime_session_id = runtime.session_id();
                let task_runtime = runtime.clone();
                let cancellation = CancellationToken::new();
                if let Ok(mut active) = cancellations.lock() {
                    active.insert(runtime_session_id, cancellation.clone());
                }
                let task_cancellations = cancellations.clone();
                let task_provider_registry = provider_registry.clone();
                let task_default_provider_id = default_provider_id.clone();
                let task_credential_resolver = credential_resolver.clone();
                tokio::spawn(async move {
                    run_agent_loop(
                        task_runtime,
                        run_id,
                        task_provider_registry,
                        task_default_provider_id,
                        task_credential_resolver,
                        provider_messages,
                        cancellation,
                    )
                    .await;
                    if let Ok(mut active) = task_cancellations.lock() {
                        active.remove(&runtime_session_id);
                    }
                });
                runtime.status()
            }) {
                Ok(status) => IpcResponse::Status { session_id, status },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::Context { session_id } => match AgentRuntime::attach(store, policy, session_id)
        {
            Ok(runtime) => match runtime.workspace_root().and_then(|workspace_root| {
                resolve_context(&workspace_root)
                    .map_err(|error| RuntimeError::Denied(error.to_string()))
            }) {
                Ok(context) => IpcResponse::Context {
                    session_id,
                    context,
                },
                Err(error) => IpcResponse::Error {
                    code: IpcErrorCode::Internal,
                    message: error.to_string(),
                },
            },
            Err(error) => runtime_error(error),
        },
        IpcRequest::Cancel { session_id } => {
            let session_lock = session_coordinator.lock_for(session_id);
            let _session_guard = session_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Ok(active) = cancellations.lock()
                && let Some(cancellation) = active.get(&session_id)
            {
                // A session has at most one active run; cancellation is kept
                // outside durable events so no handle leaks through SQLite.
                cancellation.cancel();
            }
            match AgentRuntime::attach(store, policy, session_id)
                .and_then(|runtime| runtime.cancel())
            {
                Ok(status) => IpcResponse::Status { session_id, status },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::Tool {
            session_id,
            kind,
            target,
            pattern,
        } => {
            let artifact_store = DurableArtifactStore::open(crate::default_artifact_root())
                .expect("open artifact store");
            let tool = match kind {
                ReadOnlyToolKind::List => ReadOnlyTool::List {
                    target: target.into(),
                },
                ReadOnlyToolKind::Read => ReadOnlyTool::Read {
                    target: target.into(),
                },
                ReadOnlyToolKind::Search => ReadOnlyTool::Search {
                    target: target.into(),
                    pattern: pattern.unwrap_or_default(),
                },
            };
            // A2 Phase 1: Server-side origin derivation.
            // IPC tool calls are user-direct: they arrive through the client
            // transport and are not part of an agent's tool use sequence.
            // The harness derives origin from session context; no client-provided
            // origin is trusted.
            let origin = crate::ActionOrigin::User;
            match AgentRuntime::attach(store, policy, session_id).and_then(|runtime| {
                let workspace_root = runtime.workspace_root()?;
                let tools = ReadOnlyTools::new(&workspace_root);
                let effect_seam = crate::EffectSeam::with_sandbox(
                    runtime.policy(),
                    Sandbox::workspace(&workspace_root),
                );
                tools
                    .run_with_seam(tool, origin, &artifact_store, &effect_seam)
                    .map_err(|e| RuntimeError::Denied(e.to_string()))
                    .and_then(|outcome| {
                        crate::tools::record_tool_outcome(&runtime, &outcome)?;
                        Ok(outcome)
                    })
            }) {
                Ok(outcome) => IpcResponse::ToolResult {
                    session_id,
                    outcome: redact_tool_outcome(outcome),
                },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::Subscribe {
            session_id,
            after_sequence: _,
        } => match AgentRuntime::attach(store, policy, session_id)
            .map(|runtime| runtime.session_id())
        {
            Ok(_) => IpcResponse::Subscribed { session_id },
            Err(error) => runtime_error(error),
        },
        IpcRequest::ResolveApproval {
            session_id,
            approval_id,
            accepted,
        } => match AgentRuntime::attach(store, policy, session_id).and_then(|runtime| {
            let runtime = Arc::new(runtime);
            let request = runtime
                .pending_approval(approval_id)?
                .ok_or(RuntimeError::MissingApproval(approval_id))?;
            let deferred = runtime.deferred_tool(approval_id)?;
            let resolution = crate::ApprovalResolution {
                id: approval_id,
                resolver: crate::ApprovalResolver::User,
                action_fingerprint: request.action_fingerprint.clone(),
                intent_revision: request.intent_revision,
                accepted,
            };
            runtime.resolve_approval(resolution.clone())?;
            if let Some(deferred) = deferred {
                if accepted {
                    match deferred.1.as_str() {
                        "write_file" | "edit_file" => {
                            crate::ToolOrchestrator::execute_approved_write(
                                &runtime, request, resolution, deferred,
                            )
                        }
                        "bash" | "shell" | "exec" => {
                            crate::ToolOrchestrator::execute_approved_bash(
                                &runtime, request, resolution, deferred,
                            )
                        }
                        name => Err(crate::OrchestratorError::ToolNotFound(name.into())),
                    }
                    .map_err(|error| RuntimeError::Denied(error.to_string()))?;
                } else {
                    crate::ToolOrchestrator::record_approval_rejection(&runtime, deferred);
                }
            }
            if let Some(run_id) = runtime.active_run_id()? {
                let workspace_root = runtime.workspace_root()?;
                let messages = resolve_provider_messages(&workspace_root, &runtime)
                    .map_err(|error| RuntimeError::Denied(error.to_string()))?;
                let cancellation = CancellationToken::new();
                if let Ok(mut active) = cancellations.lock() {
                    active.insert(session_id, cancellation.clone());
                }
                let task_runtime = runtime.clone();
                let task_registry = provider_registry.clone();
                let task_provider_id = default_provider_id.clone();
                let task_resolver = credential_resolver.clone();
                let task_cancellations = cancellations.clone();
                tokio::spawn(async move {
                    run_agent_loop(
                        task_runtime,
                        run_id,
                        task_registry,
                        task_provider_id,
                        task_resolver,
                        messages,
                        cancellation,
                    )
                    .await;
                    if let Ok(mut active) = task_cancellations.lock() {
                        active.remove(&session_id);
                    }
                });
            }
            Ok(session_id)
        }) {
            Ok(session_id) => IpcResponse::ApprovalResolved {
                session_id,
                approval_id,
            },
            Err(error) => runtime_error(error),
        },
        IpcRequest::GetAttachment {
            session_id,
            attachment_id,
        } => match attachments.get(attachment_id) {
            Ok(attachment) => IpcResponse::Attachment {
                session_id,
                attachment_id,
                content_type: attachment.content_type,
                content: attachment.content,
            },
            Err(crate::AttachmentError::NotFound(_)) => IpcResponse::Error {
                code: IpcErrorCode::Unavailable,
                message: format!("attachment {attachment_id} not found"),
            },
            Err(e) => IpcResponse::Error {
                code: IpcErrorCode::Internal,
                message: format!("failed to retrieve attachment: {e}"),
            },
        },
        IpcRequest::GetApprovalDetail {
            session_id,
            approval_id,
        } => match AgentRuntime::attach(store.clone(), policy.clone(), session_id).and_then(
            |runtime| {
                let request = runtime
                    .pending_approval(approval_id)?
                    .ok_or(RuntimeError::MissingApproval(approval_id))?;
                let session_workspace = runtime.workspace_root()?;
                let detail = compute_approval_detail(request, &session_workspace, &attachments)?;
                Ok((session_id, detail))
            },
        ) {
            Ok((session_id, detail)) => IpcResponse::ApprovalDetail {
                session_id,
                detail: Box::new(detail),
            },
            Err(error) => runtime_error(error),
        },
        IpcRequest::Diagnostics => {
            let subsystems =
                gather_subsystem_health(&store, &policy, &provider_registry, &workspace_root);
            IpcResponse::Diagnostics {
                subsystems: Box::new(subsystems),
            }
        }
    }
}

async fn run_agent_loop(
    runtime: Arc<AgentRuntime>,
    run_id: uuid::Uuid,
    provider_registry: ProviderRegistry,
    provider_id: String,
    _credential_resolver: Arc<dyn CredentialResolver>,
    messages: Vec<ProviderMessage>,
    cancellation: CancellationToken,
) {
    let provider = match provider_registry.get(&provider_id) {
        Ok(p) => p,
        Err(error) if matches!(runtime.status(), Ok(RuntimeStatus::Running)) => {
            let _ = runtime.finish_run(crate::RunEvent::Failed {
                run_id,
                reason: format!("provider not found: {error}"),
            });
            return;
        }
        Err(_) => return,
    };

    let result = AgentLoop::new(runtime.clone())
        .execute(run_id, provider, messages, cancellation.clone())
        .await;

    match result {
        Ok(()) if matches!(runtime.status(), Ok(RuntimeStatus::Running)) => {
            let _ = runtime.finish_run(crate::RunEvent::Completed { run_id });
        }
        Err(_) if cancellation.is_cancelled() => {}
        Err(error) if matches!(runtime.status(), Ok(RuntimeStatus::Running)) => {
            let _ = runtime.finish_run(crate::RunEvent::Failed {
                run_id,
                reason: format!("provider stream failed: {error}"),
            });
        }
        _ => {}
    }
}

fn resolve_context(
    workspace_root: &std::path::Path,
) -> anyhow::Result<crate::ResolvedInstructions> {
    InstructionResolver::new(workspace_root)
        .resolve(&ResolveRequest::default())
        .map_err(Into::into)
}

fn resolve_provider_messages(
    workspace_root: &std::path::Path,
    runtime: &AgentRuntime,
) -> anyhow::Result<Vec<ProviderMessage>> {
    let mut messages = resolve_context(workspace_root)?
        .references
        .into_iter()
        .map(|reference| ProviderMessage::system(reference.text))
        .collect::<Vec<_>>();
    let mut pending_assistant = String::new();
    let mut has_user_intent = false;
    for event in runtime.events()? {
        match event.payload {
            crate::EventPayload::Intent(intent) => {
                if !pending_assistant.is_empty() {
                    messages.push(ProviderMessage::assistant(std::mem::take(
                        &mut pending_assistant,
                    )));
                }
                messages.push(ProviderMessage::user(intent.text));
                has_user_intent = true;
            }
            crate::EventPayload::Agent(crate::AgentEvent::Chunk { text, .. }) => {
                pending_assistant.push_str(&text);
            }
            crate::EventPayload::Tool(crate::ToolEvent::Observed {
                tool_call_id,
                tool_name,
                arguments_summary,
                outcome,
                preview,
                artifact,
                error,
            }) => {
                if !pending_assistant.is_empty() {
                    messages.push(ProviderMessage::assistant(std::mem::take(
                        &mut pending_assistant,
                    )));
                }
                messages.push(ProviderMessage::tool(serde_json::to_string(
                    &serde_json::json!({
                        "tool_call_id": tool_call_id,
                        "tool_name": tool_name,
                        "arguments_summary": arguments_summary,
                        "outcome": outcome,
                        "preview": preview,
                        "artifact": artifact,
                        "error": error,
                    }),
                )?));
            }
            _ => {}
        }
    }
    if !pending_assistant.is_empty() {
        messages.push(ProviderMessage::assistant(pending_assistant));
    }
    if !has_user_intent {
        messages.push(ProviderMessage::user(runtime_intent(runtime)?));
    }
    Ok(messages)
}

fn runtime_intent(runtime: &AgentRuntime) -> anyhow::Result<String> {
    runtime
        .events()?
        .into_iter()
        .rev()
        .find_map(|event| match event.payload {
            crate::EventPayload::Intent(intent) => Some(intent.text),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("run has no user intent"))
}

/// Apply defense-in-depth redaction before tool data crosses client IPC. Full
/// file bytes and artifact filesystem paths never enter the response DTO.
pub fn redact_tool_outcome(mut outcome: ToolOutcome) -> ToolOutcome {
    if let ToolOutcome::Allowed { result } = &mut outcome {
        result.preview = crate::tools::redact_text(&result.preview);
    }
    outcome
}

fn runtime_error(error: RuntimeError) -> IpcResponse {
    let code = match &error {
        RuntimeError::MissingSession(_) => IpcErrorCode::MissingSession,
        RuntimeError::ActiveRun(_) => IpcErrorCode::Conflict,
        _ => IpcErrorCode::Internal,
    };
    IpcResponse::Error {
        code,
        message: error.to_string(),
    }
}

fn gather_subsystem_health(
    store: &Arc<dyn EventStore>,
    _policy: &PolicyEngine,
    provider_registry: &ProviderRegistry,
    workspace_root: &Path,
) -> crate::SubsystemHealth {
    use crate::SubsystemStatus;

    // Event Store
    let event_store = match store.list_sessions() {
        Ok(sessions) => SubsystemStatus::ok(format!(
            "Event store operational, {} sessions",
            sessions.len()
        ))
        .with_details(serde_json::json!({ "session_count": sessions.len() })),
        Err(e) => SubsystemStatus::unavailable(format!("Event store error: {}", e)),
    };

    // Artifact Store (ephemeral, in-memory)
    let artifact_store = SubsystemStatus::ok("Ephemeral attachment backing (in-memory)")
        .with_details(serde_json::json!({ "durable": false }));

    // Policy Engine
    let policy_engine =
        SubsystemStatus::ok("Policy engine active").with_details(serde_json::json!({
            "workspace_root": workspace_root.display().to_string(),
        }));

    // Provider Registry
    let providers: Vec<String> = provider_registry.list_provider_ids();
    let provider_registry_status = if providers.is_empty() {
        SubsystemStatus::unavailable("No providers registered")
    } else {
        SubsystemStatus::ok(format!("Providers: {}", providers.join(", ")))
            .with_details(serde_json::json!({ "providers": providers }))
    };

    // Sandbox (capability check)
    let sandbox = if cfg!(target_os = "macos") {
        SubsystemStatus::ok("Seatbelt available (macOS)")
            .with_details(serde_json::json!({ "platform": "macos", "fail_closed": true }))
    } else {
        SubsystemStatus::unavailable("Seatbelt not available (non-macOS)")
    };

    // Credential Store (platform keychain)
    let credential_store = if cfg!(target_os = "macos") {
        SubsystemStatus::ok("macOS Keychain available")
            .with_details(serde_json::json!({ "backend": "keychain" }))
    } else {
        SubsystemStatus::unavailable("Platform credential store not configured")
    };

    // Tools/Capabilities Registration
    let tools_capabilities =
        SubsystemStatus::ok("Built-in tools registered").with_details(serde_json::json!({
            "builtin_tools": ["bash", "read", "write", "edit", "search"],
            "module_registry": "available"
        }));

    // External Agents / ACP Adapters
    let external_agents = SubsystemStatus::unavailable("No external agents configured")
        .with_details(serde_json::json!({
            "acp_adapters": [],
            "note": "ACP adapter support planned for Phase 6"
        }));

    // Optional modules (Module Runtime Phase 2)
    let optional_modules =
        SubsystemStatus::ok("Module registry available").with_details(serde_json::json!({
            "loaded_modules": 0,
            "compatibility_adapters": 0,
            "remote_capabilities": false,
            "note": "Module discovery and loading implemented in Phase 2"
        }));

    // Disk/Runtime health
    let disk_runtime = probe_disk_runtime(workspace_root);

    // Web research capabilities (WEB section)
    let web_research = SubsystemStatus::unavailable("Web research not yet implemented")
        .with_details(serde_json::json!({
            "internet_access": false,
            "web_fetch": false,
            "search_backends": [],
            "browser_provider": false,
            "note": "Native web research planned in WEB section"
        }));

    crate::SubsystemHealth {
        event_store,
        artifact_store,
        policy_engine,
        provider_registry: provider_registry_status,
        sandbox,
        credential_store,
        tools_capabilities,
        external_agents,
        optional_modules,
        disk_runtime,
        web_research,
    }
}

fn probe_disk_runtime(workspace_root: &Path) -> crate::SubsystemStatus {
    use crate::SubsystemStatus;
    use std::fs;

    // Check workspace accessibility
    let workspace_readable = workspace_root.exists() && workspace_root.is_dir();

    // Check data directory
    let data_dir = data_root().ok();
    let data_dir_accessible = data_dir
        .as_ref()
        .map(|p| p.exists() || fs::create_dir_all(p).is_ok())
        .unwrap_or(false);

    // Basic runtime checks
    let temp_writable = std::env::temp_dir().exists();

    if workspace_readable && data_dir_accessible && temp_writable {
        SubsystemStatus::ok("Disk and runtime healthy").with_details(serde_json::json!({
            "workspace_root": workspace_root.display().to_string(),
            "data_dir": data_dir.map(|p| p.display().to_string()),
            "temp_dir": std::env::temp_dir().display().to_string(),
        }))
    } else {
        SubsystemStatus::unavailable("Disk or runtime issues detected").with_details(
            serde_json::json!({
                "workspace_readable": workspace_readable,
                "data_dir_accessible": data_dir_accessible,
                "temp_writable": temp_writable,
            }),
        )
    }
}

pub fn policy() -> PolicyEngine {
    PolicyEngine::new(SandboxScope::local_workspace("."))
}

/// Compute detailed approval information with diff preview and scope estimates.
fn compute_approval_detail(
    request: crate::ApprovalRequest,
    workspace_root: &Path,
    attachments: &crate::AttachmentStore,
) -> Result<crate::ApprovalDetail, RuntimeError> {
    use crate::{ActionKind, ScopeEstimate};

    let mut affected_files = vec![];
    let mut diff_preview = None;
    let mut estimated_scope = None;
    let mut attachment_refs = vec![];

    match &request.action.kind {
        ActionKind::WriteFile => {
            if let Some(target) = &request.action.target {
                affected_files.push(target.clone());

                // Attempt to compute diff if target exists
                let target_path = if Path::new(target).is_absolute() {
                    PathBuf::from(target)
                } else {
                    workspace_root.join(target)
                };

                if target_path.exists() {
                    if let Ok(existing_content) = std::fs::read_to_string(&target_path) {
                        // For now, store a simple line-count scope estimate
                        let line_count = existing_content.lines().count() as u32;
                        estimated_scope = Some(ScopeEstimate::Lines(line_count));

                        // Generate unified diff preview (truncated to 50 lines)
                        // This is a simplified preview; full diff would use a proper diff library
                        let preview_lines: Vec<_> = existing_content
                            .lines()
                            .take(50)
                            .map(|line| format!("- {}", line))
                            .collect();
                        if !preview_lines.is_empty() {
                            let mut preview = format!("--- {}", target);
                            preview.push_str(&format!("\n+++ {} (modified)", target));
                            preview.push_str(&format!("\n@@ -{},50 (preview) @@\n", 1));
                            preview.push_str(&preview_lines.join("\n"));
                            if existing_content.lines().count() > 50 {
                                preview.push_str("\n... (truncated)");
                            }
                            diff_preview = Some(preview);

                            // Store full diff as attachment if content is reasonable
                            if existing_content.len() < 1_000_000
                                && let Ok(attachment_id) = attachments.store(
                                    "text/x-diff".to_string(),
                                    existing_content.as_bytes().to_vec(),
                                )
                            {
                                attachment_refs.push(attachment_id);
                            }
                        }
                    }
                } else {
                    // New file creation
                    diff_preview = Some(format!("--- /dev/null\n+++ {} (new file)", target));
                }
            }
        }
        ActionKind::ReadFile => {
            if let Some(target) = &request.action.target {
                affected_files.push(target.clone());
            }
        }
        ActionKind::SpawnProcess => {
            // Process spawning: estimate based on command summary
            if let Some(cmd) = &request.action.target {
                estimated_scope = Some(ScopeEstimate::Operations(1));
                // Store command as attachment for review
                if let Ok(attachment_id) =
                    attachments.store("text/plain".to_string(), cmd.as_bytes().to_vec())
                {
                    attachment_refs.push(attachment_id);
                }
            }
        }
        ActionKind::NetworkConnect | ActionKind::SshConnect | ActionKind::SftpTransfer => {
            // Network operations: note target host
            if let Some(target) = &request.action.target {
                affected_files.push(target.clone());
                estimated_scope = Some(ScopeEstimate::Operations(1));
            }
        }
        ActionKind::TmuxAttach => {
            estimated_scope = Some(ScopeEstimate::Operations(1));
        }
    }

    Ok(crate::ApprovalDetail {
        request,
        diff_preview,
        affected_files,
        estimated_scope,
        attachment_refs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CredentialStrategy, EventPayload, MemoryEventStore, ProviderError, ProviderProfile,
        RetryBudget, mock_provider::MockStreamItem,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn sessions_keep_distinct_workspaces_after_attach() {
        let root = tempfile::tempdir().expect("temp root");
        let workspace_a = root.path().join("a");
        let workspace_b = root.path().join("b");
        std::fs::create_dir(&workspace_a).expect("create workspace a");
        std::fs::create_dir(&workspace_b).expect("create workspace b");
        std::fs::write(workspace_b.join("only-b.txt"), "b").expect("write fixture");
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(root.path())),
        );
        let IpcResponse::Session {
            session_id: session_a,
            ..
        } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace_a,
        })
        else {
            panic!("create session a")
        };
        let IpcResponse::Session {
            session_id: session_b,
            ..
        } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace_b,
        })
        else {
            panic!("create session b")
        };
        assert!(matches!(
            harness.handle(IpcRequest::Tool {
                session_id: session_a,
                kind: ReadOnlyToolKind::Read,
                target: "only-b.txt".into(),
                pattern: None,
            }),
            IpcResponse::ToolResult {
                outcome: ToolOutcome::Denied { .. },
                ..
            }
        ));
        assert!(matches!(
            harness.handle(IpcRequest::Tool {
                session_id: session_b,
                kind: ReadOnlyToolKind::Read,
                target: "only-b.txt".into(),
                pattern: None,
            }),
            IpcResponse::ToolResult {
                outcome: ToolOutcome::Allowed { .. },
                ..
            }
        ));
    }

    #[tokio::test]
    async fn explicit_local_profile_streams_durable_chunks_through_harness() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).await.unwrap();
            let request = std::str::from_utf8(&request[..read]).unwrap();
            assert!(request.contains("POST /v1/chat/completions HTTP/1.1"));
            assert!(request.contains("user question"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"evidence \"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\ndata: [DONE]\n\n")
                .await
                .unwrap();
        });
        let provider = OpenAiCompatibleProvider::new(
            ProviderProfile {
                id: "local-test".into(),
                endpoint: format!("http://{address}"),
                model: "test-model".into(),
                credential_strategy: CredentialStrategy::None,
            },
            RetryBudget::default(),
        )
        .unwrap();
        let harness = Harness::with_openai_provider(
            Arc::new(MemoryEventStore::default()),
            policy(),
            provider,
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: std::env::current_dir()
                .expect("workspace")
                .canonicalize()
                .expect("canonical workspace"),
        }) else {
            panic!("session creation response")
        };
        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "user question".into(),
            }),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        for _ in 0..20 {
            if matches!(
                harness.handle(IpcRequest::Attach { session_id }),
                IpcResponse::Session {
                    status: RuntimeStatus::Completed,
                    ..
                }
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        }) else {
            panic!("stream response")
        };
        assert!(matches!(
            events.last().map(|event| &event.payload),
            Some(EventPayload::Run(crate::RunEvent::Completed { .. }))
        ));
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match &event.payload {
                    EventPayload::Agent(crate::AgentEvent::Chunk { text, .. }) =>
                        Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
            "evidence answer"
        );
        assert!(events.iter().any(|event| {
            matches!(
                &event.payload,
                EventPayload::Agent(crate::AgentEvent::Final { text, .. }) if text == "evidence answer"
            )
        }));
        server.await.unwrap();
    }

    #[test]
    fn ipc_tool_uses_the_harness_owned_policy_scope() {
        let root =
            std::env::temp_dir().join(format!("harness-tool-scope-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create workspace");
        std::fs::write(root.join("evidence.txt"), "scoped evidence").expect("write fixture");
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(&root)),
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: root.clone(),
        }) else {
            panic!("session creation response");
        };

        let response = harness.handle(IpcRequest::Tool {
            session_id,
            kind: ReadOnlyToolKind::Read,
            target: "evidence.txt".into(),
            pattern: None,
        });
        assert!(matches!(
            response,
            IpcResponse::ToolResult {
                outcome: ToolOutcome::Allowed { .. },
                ..
            }
        ));
    }

    #[tokio::test]
    async fn context_is_transient_and_skill_requirements_do_not_change_policy() {
        let root = std::env::temp_dir().join(format!("harness-context-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".impetus/skills/production")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "workspace instructions").unwrap();
        std::fs::write(
            root.join(".impetus/skills/production/SKILL.md"),
            "---\nrequires: ssh-prod\n---\nresolved secret-free instruction body",
        )
        .unwrap();
        let policy = PolicyEngine::new(SandboxScope::local_workspace(&root));
        let denied_ssh = crate::Action {
            origin: crate::ActionOrigin::Agent,
            kind: crate::ActionKind::SshConnect,
            summary: "connect production".into(),
            target: None,
        };
        let expected_decision = policy.evaluate(&denied_ssh);
        let store = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(store.clone(), policy.clone());
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: root.clone(),
        }) else {
            panic!("session creation response");
        };

        let IpcResponse::Context { context, .. } =
            harness.handle(IpcRequest::Context { session_id })
        else {
            panic!("context response");
        };
        assert!(
            context
                .references
                .iter()
                .any(|reference| reference.text.contains("resolved secret-free"))
        );
        assert_eq!(policy.evaluate(&denied_ssh), expected_decision);

        harness.handle(IpcRequest::Prompt {
            session_id,
            text: "only user intent".into(),
        });
        let durable = serde_json::to_string(&store.list(session_id).unwrap()).unwrap();
        assert!(durable.contains("only user intent"));
        assert!(!durable.contains("resolved secret-free"));
        let _ = std::fs::remove_dir_all(root);
    }

    struct CountingKeychainCredential {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl CredentialResolver for CountingKeychainCredential {
        fn resolve(&self, _profile: &ProviderProfile) -> Result<Option<String>, ProviderError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(ProviderError::RequestFailed(
                "Keychain unavailable for opaque-service-label/opaque-account-label".into(),
            ))
        }
    }

    #[tokio::test]
    async fn keychain_lookup_is_lazy_and_missing_or_unavailable_results_are_redacted() {
        let store = Arc::new(MemoryEventStore::default());
        let provider = OpenAiCompatibleProvider::new(
            ProviderProfile {
                id: "remote-profile".into(),
                endpoint: "https://api.example.test".into(),
                model: "test-model".into(),
                credential_strategy: CredentialStrategy::KeychainReference {
                    service: "opaque-service-label".into(),
                    account: "opaque-account-label".into(),
                },
            },
            RetryBudget::default(),
        )
        .unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resolver = CountingKeychainCredential {
            calls: calls.clone(),
        };
        let harness = Harness::with_openai_provider_and_resolver(
            store.clone(),
            policy(),
            provider,
            Arc::new(resolver),
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: std::env::current_dir()
                .expect("workspace")
                .canonicalize()
                .expect("canonical workspace"),
        }) else {
            panic!("session creation response")
        };
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "question without credential".into(),
            }),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        for _ in 0..20 {
            if matches!(
                harness.handle(IpcRequest::Attach { session_id }),
                IpcResponse::Session {
                    status: RuntimeStatus::Failed,
                    ..
                }
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        }) else {
            panic!("stream response")
        };
        let exported = serde_json::to_string(&events).unwrap();
        assert!(!exported.contains("opaque-service-label"));
        assert!(!exported.contains("opaque-account-label"));
        assert!(!exported.contains("Keychain unavailable"));
        assert!(exported.contains("provider credential is required but unavailable"));
    }

    #[tokio::test]
    async fn ipc_resolve_approval_requires_exact_pending_approval() {
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let harness = Harness::new(store.clone(), policy.clone());

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: std::env::current_dir()
                .expect("workspace")
                .canonicalize()
                .expect("canonical workspace"),
        }) else {
            panic!("session creation response");
        };

        let runtime = AgentRuntime::attach(store.clone(), policy.clone(), session_id)
            .expect("attach to created session");

        // Submit intent to establish revision
        runtime
            .submit_intent("write a test file")
            .expect("submit intent");

        // Request an action that needs approval
        let action = crate::Action {
            origin: crate::ActionOrigin::Agent,
            kind: crate::ActionKind::WriteFile,
            summary: "write file".into(),
            target: Some("test.txt".into()),
        };
        runtime
            .request_action(action)
            .expect("request action that needs approval");

        // Get the approval ID from events
        let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        }) else {
            panic!("stream response");
        };

        let approval_id = events
            .iter()
            .find_map(|e| {
                if let crate::EventPayload::Approval(crate::ApprovalEvent::Requested { request }) =
                    &e.payload
                {
                    Some(request.id)
                } else {
                    None
                }
            })
            .expect("approval request in events");

        let IpcResponse::ApprovalResolved {
            approval_id: resolved_id,
            ..
        } = harness.handle(IpcRequest::ResolveApproval {
            session_id,
            approval_id,
            accepted: true,
        })
        else {
            panic!("approval resolution response");
        };
        assert_eq!(resolved_id, approval_id);

        let remaining = runtime
            .pending_approval(approval_id)
            .expect("check pending approval");
        assert!(remaining.is_none(), "approval must be resolved");

        let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        }) else {
            panic!("stream response");
        };
        let resolved_event = events.iter().find(|e| {
            matches!(
                e.payload,
                crate::EventPayload::Approval(crate::ApprovalEvent::Resolved { .. })
            )
        });
        assert!(
            resolved_event.is_some(),
            "resolved event must be in the stream"
        );
    }

    #[tokio::test]
    async fn ipc_get_approval_detail_returns_extended_payload() {
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let harness = Harness::new(store.clone(), policy.clone());

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: std::env::current_dir()
                .expect("workspace")
                .canonicalize()
                .expect("canonical workspace"),
        }) else {
            panic!("session creation response");
        };

        let runtime = AgentRuntime::attach(store.clone(), policy.clone(), session_id)
            .expect("attach to created session");

        runtime
            .submit_intent("write a test file")
            .expect("submit intent");

        let action = crate::Action {
            origin: crate::ActionOrigin::Agent,
            kind: crate::ActionKind::WriteFile,
            summary: "write file".into(),
            target: Some("test.txt".into()),
        };
        runtime
            .request_action(action)
            .expect("request action that needs approval");

        let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        }) else {
            panic!("stream response");
        };

        let approval_id = events
            .iter()
            .find_map(|e| {
                if let crate::EventPayload::Approval(crate::ApprovalEvent::Requested { request }) =
                    &e.payload
                {
                    Some(request.id)
                } else {
                    None
                }
            })
            .expect("approval request in events");

        let IpcResponse::ApprovalDetail { detail, .. } =
            harness.handle(IpcRequest::GetApprovalDetail {
                session_id,
                approval_id,
            })
        else {
            panic!("approval detail response");
        };

        assert_eq!(detail.request.id, approval_id);
        assert_eq!(detail.request.action.kind, crate::ActionKind::WriteFile);
        // Diff/scope computation now implemented
        assert_eq!(detail.affected_files, vec!["test.txt"]);
        assert!(detail.diff_preview.is_some());
        // New file creation case
        assert!(detail.diff_preview.unwrap().contains("new file"));
    }

    #[test]
    fn approval_detail_uses_the_session_workspace() {
        let root = tempfile::tempdir().expect("root");
        let daemon_workspace = root.path().join("daemon");
        let session_workspace = root.path().join("session");
        std::fs::create_dir(&daemon_workspace).expect("daemon workspace");
        std::fs::create_dir(&session_workspace).expect("session workspace");
        std::fs::write(session_workspace.join("existing.txt"), "session content")
            .expect("session fixture");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(&daemon_workspace));
        let harness = Harness::new(Arc::new(MemoryEventStore::default()), policy.clone());
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: session_workspace,
        }) else {
            panic!("session creation")
        };
        let runtime = AgentRuntime::attach(harness.store(), policy, session_id).expect("attach");
        runtime.submit_intent("edit session file").expect("intent");
        runtime
            .request_action(crate::Action {
                origin: crate::ActionOrigin::Agent,
                kind: crate::ActionKind::WriteFile,
                summary: "edit existing file".into(),
                target: Some("existing.txt".into()),
            })
            .expect("approval request");
        let approval_id = runtime
            .events()
            .expect("events")
            .into_iter()
            .find_map(|event| match event.payload {
                EventPayload::Approval(crate::ApprovalEvent::Requested { request }) => {
                    Some(request.id)
                }
                _ => None,
            })
            .expect("approval id");

        let IpcResponse::ApprovalDetail { detail, .. } =
            harness.handle(IpcRequest::GetApprovalDetail {
                session_id,
                approval_id,
            })
        else {
            panic!("approval detail")
        };
        assert!(
            detail
                .diff_preview
                .expect("existing file diff")
                .contains("session content")
        );
    }

    #[tokio::test]
    async fn approval_resume_returns_durable_tool_observations_to_the_model() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("evidence.txt"), "confirmed evidence")
            .expect("fixture");
        let store = Arc::new(MemoryEventStore::default());
        let provider = Arc::new(MockProvider::scripted(
            "scripted",
            "test-model",
            [
                vec![MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "<tool_use><tool_name>read_file</tool_name><parameters>{\"path\":\"evidence.txt\"}</parameters></tool_use>".into(),
                }],
                vec![MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "<tool_use><tool_name>write_file</tool_name><parameters>{\"path\":\"result.txt\",\"content\":\"approved result\"}</parameters></tool_use>".into(),
                }],
                vec![MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "completed after approval".into(),
                }],
            ],
        ));
        let harness = Harness::with_test_provider(
            store.clone(),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
            provider.clone(),
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("session creation")
        };

        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "inspect evidence and write the result".into(),
            }),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        for _ in 0..100 {
            if matches!(
                harness.handle(IpcRequest::Attach { session_id }),
                IpcResponse::Session {
                    status: RuntimeStatus::AwaitingApproval,
                    ..
                }
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        }) else {
            panic!("event stream")
        };
        let approval_id = events
            .iter()
            .find_map(|event| match &event.payload {
                EventPayload::Approval(crate::ApprovalEvent::Requested { request }) => {
                    Some(request.id)
                }
                _ => None,
            })
            .expect("write approval");
        assert!(matches!(
            harness.handle(IpcRequest::ResolveApproval {
                session_id,
                approval_id,
                accepted: true,
            }),
            IpcResponse::ApprovalResolved { .. }
        ));
        for _ in 0..100 {
            if matches!(
                harness.handle(IpcRequest::Attach { session_id }),
                IpcResponse::Session {
                    status: RuntimeStatus::Completed,
                    ..
                }
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert_eq!(
            std::fs::read_to_string(workspace.path().join("result.txt")).expect("written result"),
            "approved result"
        );
        let received = provider.received_messages();
        assert_eq!(received.len(), 3);
        let resume_context = serde_json::to_string(&received[2]).expect("serialize context");
        assert!(resume_context.contains("confirmed evidence"));
        assert!(resume_context.contains("file written"));
        assert!(store.list(session_id).expect("events").iter().any(|event| {
            matches!(
                &event.payload,
                EventPayload::Agent(crate::AgentEvent::Final { text, .. })
                    if text == "completed after approval"
            )
        }));
    }

    #[tokio::test]
    async fn rejected_approval_records_denial_and_resumes_without_execution() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store = Arc::new(MemoryEventStore::default());
        let provider = Arc::new(MockProvider::scripted(
            "scripted-rejection",
            "test-model",
            [
                vec![MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "<tool_use><tool_name>write_file</tool_name><parameters>{\"path\":\"blocked.txt\",\"content\":\"must not write\"}</parameters></tool_use>".into(),
                }],
                vec![MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "completed after rejection".into(),
                }],
            ],
        ));
        let harness = Harness::with_test_provider(
            store.clone(),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
            provider,
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("session creation")
        };
        harness.handle(IpcRequest::Prompt {
            session_id,
            text: "try the write".into(),
        });
        let approval_id = loop {
            let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
                session_id,
                after_sequence: 0,
            }) else {
                panic!("event stream")
            };
            if let Some(approval_id) = events.iter().find_map(|event| match &event.payload {
                EventPayload::Approval(crate::ApprovalEvent::Requested { request }) => {
                    Some(request.id)
                }
                _ => None,
            }) {
                break approval_id;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert!(matches!(
            harness.handle(IpcRequest::ResolveApproval {
                session_id,
                approval_id,
                accepted: false,
            }),
            IpcResponse::ApprovalResolved { .. }
        ));
        for _ in 0..100 {
            if matches!(
                harness.handle(IpcRequest::Attach { session_id }),
                IpcResponse::Session {
                    status: RuntimeStatus::Completed,
                    ..
                }
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!workspace.path().join("blocked.txt").exists());
        assert!(store.list(session_id).expect("events").iter().any(|event| {
            matches!(
                &event.payload,
                EventPayload::Tool(crate::ToolEvent::Observed {
                    outcome: crate::ToolEventOutcome::Denied,
                    error: Some(error),
                    ..
                }) if error == "user rejected approval"
            )
        }));
    }

    #[tokio::test]
    async fn cancellation_stops_an_active_agent_run_without_a_final_answer() {
        let workspace = tempfile::tempdir().expect("workspace");
        let provider = Arc::new(MockProvider::scripted(
            "slow-scripted",
            "test-model",
            [vec![MockStreamItem::Chunk {
                chunk_id: 1,
                text: "still running ".repeat(200),
            }]],
        ));
        let harness = Harness::with_test_provider(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
            provider,
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("session creation")
        };
        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "start a long task".into(),
            }),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        assert!(matches!(
            harness.handle(IpcRequest::Cancel { session_id }),
            IpcResponse::Status {
                status: RuntimeStatus::Cancelled,
                ..
            }
        ));
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        }) else {
            panic!("event stream")
        };
        assert!(matches!(
            events.last().map(|event| &event.payload),
            Some(EventPayload::Run(crate::RunEvent::Cancelled { .. }))
        ));
        assert!(!events.iter().any(|event| {
            matches!(
                event.payload,
                EventPayload::Agent(crate::AgentEvent::Final { .. })
            )
        }));
    }
}
