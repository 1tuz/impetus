//! Transport-neutral harness dispatch.
//!
//! The same `Harness` drives both the Unix-socket server (harness daemon) and
//! in-memory transports used by client tests and future TUI clients. It owns no
//! transport: it takes a normalized [`IpcRequest`] and returns an
//! [`IpcResponse`], deriving view DTOs from the durable event store and the
//! runtime projection. State lives in the store and the running agent, never in
//! the client.

use crate::{
    AgentLoop, AgentRuntime, ContextBuilder, CredentialResolver, DurableArtifactStore,
    EventPayload, EventStore, IPC_CAPABILITIES, IPC_VERSION, InstructionResolver, IpcErrorCode,
    IpcRequest, IpcResponse, MockProvider, NoCredentialResolver, NoticeEvent, OpenAiNativeAdapter,
    OpenAiProvider, PolicyConfig, PolicyEngine, Profile, ProviderMessage, ProviderRegistry,
    QueuedFollowUp, ReadOnlyTool, ReadOnlyToolKind, ReadOnlyTools, ResolveRequest, RuntimeError,
    RuntimeStatus, SandboxScope, SessionEvent, SteerActiveContext, SteerPendingQueue, SteerRewrite,
    TokenBudget, ToolOutcome, UserIntentRouter, UserIntentSubmission, UserPromptIntent,
    context_optimizer::{
        DEFAULT_CONTEXT_BUDGET_TOKENS, default_tool_stubs, system_messages_for_binding,
    },
    default_steer_rewrite,
    model_router::{ModelRouter, ModelRouterConfig},
    policy::ActionOrigin,
    provider_steer_rewrite, reduce,
    user_intent::UserIntentError,
};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use tokio_util::sync::CancellationToken;

/// Per-session cancel handle keyed by run so a drained follow-up cannot be
/// cleared by the previous loop's cleanup (cancel/replace race).
#[derive(Clone)]
struct ActiveCancellation {
    run_id: uuid::Uuid,
    token: CancellationToken,
}

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
    policy: Arc<Mutex<PolicyEngine>>,
    provider_registry: ProviderRegistry,
    default_provider_id: String,
    model_router: ModelRouter,
    credential_resolver: Arc<dyn CredentialResolver>,
    cancellations: Arc<Mutex<HashMap<uuid::Uuid, ActiveCancellation>>>,
    workspace_root: PathBuf,
    session_coordinator: SessionCoordinator,
    attachments: crate::AttachmentStore,
    uploads: crate::ArtifactUploadStore,
    /// In-memory Prompt/Steer/FollowUp router (synced from projection on submit).
    intent_router: Arc<Mutex<UserIntentRouter>>,
    coding_tools: Arc<dyn crate::CodingToolsService>,
    /// Steer prompt rewrite seam (default passthrough; live provider when wired).
    steer_rewrite: Arc<dyn SteerRewrite>,
    /// Rewritten steer fragments queued for the active agent loop.
    steer_pending: SteerPendingQueue,
    /// Optional explore-child infrastructure (gate + store + executor bridge).
    explore_spawn: Option<Arc<dyn crate::explore_child::ExploreSpawnBridge>>,
    /// Optional session MCP tool providers (lazy connect; not used by Explore).
    tool_providers: Option<Arc<tokio::sync::Mutex<crate::ToolProviderRuntime>>>,
    /// Daemon-owned hook prefilter catalog for process spawn paths.
    hook_prefilter: crate::HookPrefilter,
    /// Optional governed-instruction catalog (labels/refs only; not PolicyConfig).
    policy_store: Arc<Mutex<Option<Arc<crate::PolicyStore>>>>,
    /// Optional live WorkflowEngine runtime (schedule → child spawn).
    workflow_runtime: Option<Arc<crate::WorkflowRuntime>>,
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
        let router_config = ModelRouterConfig::default();
        let model_router = ModelRouter::new(router_config);
        Self {
            store,
            policy: Arc::new(Mutex::new(policy)),
            provider_registry: registry,
            default_provider_id: "mock".to_string(),
            model_router,
            credential_resolver: Arc::new(NoCredentialResolver),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            workspace_root,
            session_coordinator: SessionCoordinator::default(),
            attachments: crate::AttachmentStore::new(),
            uploads: crate::ArtifactUploadStore::new(crate::default_artifact_root()),
            intent_router: Arc::new(Mutex::new(UserIntentRouter::new())),
            coding_tools: Arc::new(crate::OptionalCodingToolsService::absent()),
            steer_rewrite: default_steer_rewrite(),
            steer_pending: SteerPendingQueue::new(),
            explore_spawn: None,
            tool_providers: None,
            hook_prefilter: crate::HookPrefilter::default(),
            policy_store: Arc::new(Mutex::new(None)),
            workflow_runtime: None,
        }
    }

    /// Override durable artifact root (tests / portable installs).
    pub fn with_artifact_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.uploads = crate::ArtifactUploadStore::new(root);
        self
    }

    /// Attach optional coding-tools provider (tests / future LSP bridge).
    pub fn with_coding_tools(mut self, service: Arc<dyn crate::CodingToolsService>) -> Self {
        self.coding_tools = service;
        self
    }

    /// Attach Steer prompt rewriter (tests / explicit override).
    pub fn with_steer_rewrite(mut self, rewriter: Arc<dyn SteerRewrite>) -> Self {
        self.steer_rewrite = rewriter;
        self
    }

    /// Wire provider-backed steer rewrite from the default registered provider.
    pub fn with_provider_steer_rewrite(mut self) -> Result<Self, crate::ProviderError> {
        let provider = self.provider_registry.get(&self.default_provider_id)?;
        self.steer_rewrite = provider_steer_rewrite(provider);
        Ok(self)
    }

    /// Attach Explore spawn bridge (gate + store + executor). Default is None.
    pub fn with_explore_spawn(
        mut self,
        bridge: Arc<dyn crate::explore_child::ExploreSpawnBridge>,
    ) -> Self {
        self.explore_spawn = Some(bridge);
        self
    }

    /// Attach session MCP tool provider runtime (lazy connect). Default is None.
    /// Explore children must not receive this — keep Explore allowlist MCP-free.
    pub fn with_tool_providers(
        mut self,
        runtime: Arc<tokio::sync::Mutex<crate::ToolProviderRuntime>>,
    ) -> Self {
        self.tool_providers = Some(runtime);
        self
    }

    /// Attach daemon-owned hook prefilter catalog for process spawn paths.
    pub fn with_hook_prefilter(mut self, prefilter: crate::HookPrefilter) -> Self {
        self.hook_prefilter = prefilter;
        self
    }

    /// Attach optional governed-instruction catalog.
    pub fn with_policy_store(self, store: Arc<crate::PolicyStore>) -> Self {
        *self
            .policy_store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(store);
        self
    }

    /// Attach live WorkflowEngine runtime.
    pub fn with_workflow_runtime(mut self, runtime: Arc<crate::WorkflowRuntime>) -> Self {
        self.workflow_runtime = Some(runtime);
        self
    }

    pub fn hook_prefilter(&self) -> &crate::HookPrefilter {
        &self.hook_prefilter
    }

    pub fn policy_store(&self) -> Option<Arc<crate::PolicyStore>> {
        self.policy_store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn workflow_runtime(&self) -> Option<Arc<crate::WorkflowRuntime>> {
        self.workflow_runtime.clone()
    }

    pub fn reload_policy_store(&self, store: crate::PolicyStore) {
        *self
            .policy_store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(store));
    }

    /// Spawn one Explore child via configured bridge.
    pub fn spawn_explore(
        &self,
        request: crate::ExploreChildRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::ExploreChildOutcome, crate::ExploreChildError> {
        self.explore_spawn
            .as_ref()
            .ok_or(crate::ExploreChildError::NotConfigured)?
            .spawn_explore(request, cancel)
    }

    /// Gate parent resume after Explore children and return joined summary labels.
    pub fn complete_explore_and_gate(
        &self,
        parent_session_id: &str,
        child_ids: &[&str],
    ) -> Result<String, crate::ExploreChildError> {
        let bridge = self
            .explore_spawn
            .as_ref()
            .ok_or(crate::ExploreChildError::NotConfigured)?;
        let results = crate::explore_child::resume_parent_after_explore(
            bridge.child_results(),
            parent_session_id,
            child_ids,
        )?;
        Ok(results
            .into_iter()
            .map(|r| r.summary_label)
            .collect::<Vec<_>>()
            .join("\n"))
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
        let router_config = ModelRouterConfig::default();
        let model_router = ModelRouter::new(router_config);
        Self {
            store,
            policy: Arc::new(Mutex::new(policy)),
            provider_registry: registry,
            default_provider_id,
            model_router,
            credential_resolver: Arc::new(NoCredentialResolver),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            workspace_root,
            session_coordinator: SessionCoordinator::default(),
            attachments: crate::AttachmentStore::new(),
            uploads: crate::ArtifactUploadStore::new(crate::default_artifact_root()),
            intent_router: Arc::new(Mutex::new(UserIntentRouter::new())),
            coding_tools: Arc::new(crate::OptionalCodingToolsService::absent()),
            steer_rewrite: default_steer_rewrite(),
            steer_pending: SteerPendingQueue::new(),
            explore_spawn: None,
            tool_providers: None,
            hook_prefilter: crate::HookPrefilter::default(),
            policy_store: Arc::new(Mutex::new(None)),
            workflow_runtime: None,
        }
    }

    /// Use a user-selected direct-provider profile. The profile is supplied by
    /// daemon startup, not client IPC; credentials remain outside this type.
    /// Registers native Chat Completions SSE provider (tool-call aware).
    pub fn with_openai_provider(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        provider: OpenAiProvider,
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
        provider: OpenAiProvider,
        credential_resolver: Arc<dyn CredentialResolver>,
    ) -> Self {
        let workspace_root = policy.scope().workspace_root.clone();
        let registry = ProviderRegistry::new();

        // Register mock for tests
        let mock = Arc::new(MockProvider::default_mock());
        registry
            .register(mock)
            .expect("failed to register mock provider");

        let provider_id = provider.profile().id.clone();
        let adapter = Arc::new(OpenAiNativeAdapter::new(
            Arc::new(provider),
            credential_resolver.clone(),
        ));
        registry
            .register(adapter.clone())
            .expect("failed to register openai provider");

        let router_config = ModelRouterConfig::default();
        let model_router = ModelRouter::new(router_config);
        let steer_rewrite = provider_steer_rewrite(adapter);

        Self {
            store,
            policy: Arc::new(Mutex::new(policy)),
            provider_registry: registry,
            default_provider_id: provider_id,
            model_router,
            credential_resolver,
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            workspace_root,
            session_coordinator: SessionCoordinator::default(),
            attachments: crate::AttachmentStore::new(),
            uploads: crate::ArtifactUploadStore::new(crate::default_artifact_root()),
            intent_router: Arc::new(Mutex::new(UserIntentRouter::new())),
            coding_tools: Arc::new(crate::OptionalCodingToolsService::absent()),
            steer_rewrite,
            steer_pending: SteerPendingQueue::new(),
            explore_spawn: None,
            tool_providers: None,
            hook_prefilter: crate::HookPrefilter::default(),
            policy_store: Arc::new(Mutex::new(None)),
            workflow_runtime: None,
        }
    }

    /// Use an external ACP-compatible coding agent.
    /// Agent owns authentication; Impetus owns policy, session state, and orchestration.
    pub fn with_acp_gateway(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        config: agent_client_protocol::AcpAgentConfig,
        auth_method_id: Option<String>,
        provider_id: String,
        model_id: String,
    ) -> Self {
        let workspace_root = policy.scope().workspace_root.clone();
        let registry = ProviderRegistry::new();

        // Register mock for tests
        let mock = Arc::new(MockProvider::default_mock());
        registry
            .register(mock)
            .expect("failed to register mock provider");

        // Register ACP adapter V2
        let adapter = Arc::new(crate::AcpAdapter::new(
            config,
            auth_method_id,
            provider_id.clone(),
            model_id,
            workspace_root.clone(),
            Arc::new(policy.clone()),
        ));
        registry
            .register(adapter.clone())
            .expect("failed to register acp gateway");

        let router_config = ModelRouterConfig::default();
        let model_router = ModelRouter::new(router_config);
        let steer_rewrite = provider_steer_rewrite(adapter);

        Self {
            store,
            policy: Arc::new(Mutex::new(policy)),
            provider_registry: registry,
            default_provider_id: provider_id,
            model_router,
            credential_resolver: Arc::new(NoCredentialResolver),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            workspace_root,
            session_coordinator: SessionCoordinator::default(),
            attachments: crate::AttachmentStore::new(),
            uploads: crate::ArtifactUploadStore::new(crate::default_artifact_root()),
            intent_router: Arc::new(Mutex::new(UserIntentRouter::new())),
            coding_tools: Arc::new(crate::OptionalCodingToolsService::absent()),
            steer_rewrite,
            steer_pending: SteerPendingQueue::new(),
            explore_spawn: None,
            tool_providers: None,
            hook_prefilter: crate::HookPrefilter::default(),
            policy_store: Arc::new(Mutex::new(None)),
            workflow_runtime: None,
        }
    }

    pub fn policy(&self) -> PolicyEngine {
        policy_snapshot(&self.policy)
    }

    pub fn store(&self) -> Arc<dyn EventStore> {
        self.store.clone()
    }

    pub fn provider_registry(&self) -> &ProviderRegistry {
        &self.provider_registry
    }

    pub fn default_provider_id(&self) -> &str {
        &self.default_provider_id
    }

    pub fn has_explore_spawn(&self) -> bool {
        self.explore_spawn.is_some()
    }

    pub fn has_tool_providers(&self) -> bool {
        self.tool_providers.is_some()
    }

    pub fn has_hook_prefilter(&self) -> bool {
        !self.hook_prefilter.rules().is_empty()
    }

    pub fn has_policy_store(&self) -> bool {
        self.policy_store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    pub fn has_workflow_runtime(&self) -> bool {
        self.workflow_runtime.is_some()
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
            self.model_router.clone(),
            self.credential_resolver.clone(),
            self.cancellations.clone(),
            self.workspace_root.clone(),
            self.session_coordinator.clone(),
            self.attachments.clone(),
            self.uploads.clone(),
            self.intent_router.clone(),
            self.coding_tools.clone(),
            self.steer_rewrite.clone(),
            self.steer_pending.clone(),
            self.tool_providers.clone(),
            self.hook_prefilter.clone(),
            self.policy_store.clone(),
            self.workflow_runtime.clone(),
            self.explore_spawn.clone(),
            request,
        )
    }
}

fn policy_snapshot(policy: &Arc<Mutex<PolicyEngine>>) -> PolicyEngine {
    policy
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn audit_policy_reload_notice(store: &Arc<dyn EventStore>, message: impl Into<String>) {
    let message = message.into();
    if let Ok(sessions) = store.list_sessions() {
        for session in sessions {
            let _ = store.append_next(
                session.id,
                EventPayload::Notice(NoticeEvent::Runtime {
                    message: message.clone(),
                }),
            );
        }
    }
}

fn resolve_reload_policy_config(
    path: Option<&Path>,
    config_json: Option<&str>,
) -> Result<PolicyConfig, String> {
    match (path, config_json) {
        (None, None) => Err("path or config_json is required".into()),
        (Some(_), Some(_)) => Err("specify path or config_json, not both".into()),
        (Some(path), None) => PolicyConfig::load_from_path(path).map_err(|error| error.to_string()),
        (None, Some(json)) => PolicyConfig::parse(json).map_err(|error| error.to_string()),
    }
}

fn resolve_reload_policy_store(
    path: Option<&Path>,
    store_json: Option<&str>,
) -> Result<crate::PolicyStore, String> {
    match (path, store_json) {
        (None, None) => Err("path or store_json is required".into()),
        (Some(_), Some(_)) => Err("specify path or store_json, not both".into()),
        (Some(path), None) => {
            crate::PolicyStore::load_from_path(path).map_err(|error| error.to_string())
        }
        (None, Some(json)) => crate::PolicyStore::parse(json).map_err(|error| error.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_request(
    store: Arc<dyn EventStore>,
    policy: Arc<Mutex<PolicyEngine>>,
    provider_registry: ProviderRegistry,
    default_provider_id: String,
    model_router: ModelRouter,
    credential_resolver: Arc<dyn CredentialResolver>,
    cancellations: Arc<Mutex<HashMap<uuid::Uuid, ActiveCancellation>>>,
    workspace_root: PathBuf,
    session_coordinator: SessionCoordinator,
    attachments: crate::AttachmentStore,
    uploads: crate::ArtifactUploadStore,
    intent_router: Arc<Mutex<UserIntentRouter>>,
    coding_tools: Arc<dyn crate::CodingToolsService>,
    steer_rewrite: Arc<dyn SteerRewrite>,
    steer_pending: SteerPendingQueue,
    tool_providers: Option<Arc<tokio::sync::Mutex<crate::ToolProviderRuntime>>>,
    hook_prefilter: crate::HookPrefilter,
    policy_store_slot: Arc<Mutex<Option<Arc<crate::PolicyStore>>>>,
    workflow_runtime: Option<Arc<crate::WorkflowRuntime>>,
    explore_spawn: Option<Arc<dyn crate::explore_child::ExploreSpawnBridge>>,
    request: IpcRequest,
) -> IpcResponse {
    let policy_store = policy_store_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
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
            match AgentRuntime::create_with_workspace(
                store,
                policy_snapshot(&policy),
                workspace_root,
            ) {
                Ok(runtime) => {
                    let session_id = runtime.session_id();
                    if let Ok(mut router) = intent_router.lock() {
                        router.open_session(session_id);
                    }
                    IpcResponse::Session {
                        session_id,
                        status: RuntimeStatus::Idle,
                    }
                }
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::Attach { session_id } => {
            match AgentRuntime::attach(store, policy_snapshot(&policy), session_id)
                .and_then(|runtime| Ok((runtime.session_id(), runtime.status()?)))
            {
                Ok((session_id, status)) => {
                    if let Ok(mut router) = intent_router.lock() {
                        router.open_session(session_id);
                    }
                    IpcResponse::Session { session_id, status }
                }
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::ListSessions => match store.list_sessions() {
            Ok(sessions) => IpcResponse::Sessions { sessions },
            Err(error) => IpcResponse::Error {
                code: IpcErrorCode::Internal,
                message: error.to_string(),
            },
        },
        IpcRequest::ForkSession {
            session_id,
            up_to_sequence,
        } => {
            match AgentRuntime::fork(store, policy_snapshot(&policy), session_id, up_to_sequence) {
                Ok(runtime) => IpcResponse::Session {
                    session_id: runtime.session_id(),
                    status: RuntimeStatus::Idle,
                },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::CreateCheckpoint {
            session_id,
            name,
            sequence,
        } => {
            let resolved_sequence = match sequence {
                Some(sequence) => Ok(sequence),
                None => store.list(session_id).and_then(|events| {
                    events
                        .last()
                        .map(|event| event.sequence)
                        .ok_or(crate::StoreError::MissingSession(session_id))
                }),
            };
            match resolved_sequence
                .and_then(|sequence| store.create_checkpoint(session_id, name, sequence))
            {
                Ok(checkpoint) => IpcResponse::Checkpoint { checkpoint },
                Err(error) => store_error(error),
            }
        }
        IpcRequest::ListCheckpoints { session_id } => match store.list_checkpoints(session_id) {
            Ok(checkpoints) => IpcResponse::Checkpoints { checkpoints },
            Err(error) => store_error(error),
        },
        IpcRequest::RestoreCheckpoint { checkpoint_id } => {
            match AgentRuntime::restore_checkpoint(store, policy_snapshot(&policy), checkpoint_id) {
                Ok(runtime) => IpcResponse::Session {
                    session_id: runtime.session_id(),
                    status: RuntimeStatus::Idle,
                },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::Stream {
            session_id,
            after_sequence,
        } => match AgentRuntime::attach(store, policy_snapshot(&policy), session_id).and_then(
            |runtime| {
                Ok(runtime
                    .events()?
                    .into_iter()
                    .filter(|event| event.sequence > after_sequence)
                    .collect())
            },
        ) {
            Ok(events) => IpcResponse::Events { session_id, events },
            Err(error) => runtime_error(error),
        },
        IpcRequest::Prompt {
            session_id,
            text,
            artifact,
            intent,
        } => {
            let session_lock = session_coordinator.lock_for(session_id);
            let _session_guard = session_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let artifact_root = uploads.artifact_root().to_path_buf();
            match AgentRuntime::attach(store.clone(), policy_snapshot(&policy), session_id)
                .and_then(|runtime| {
                    let runtime = Arc::new(runtime);

                    // Sync projection → router, then validate typed intent (no origin/policy bypass).
                    let accepted = {
                        let mut router = intent_router
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        router.open_session(session_id);
                        let active = runtime.active_run_id()?;
                        router
                            .set_active_run(session_id, active)
                            .map_err(user_intent_to_runtime)?;
                        router
                            .submit(UserIntentSubmission {
                                session_id,
                                intent,
                                text: text.clone(),
                                origin: ActionOrigin::User,
                            })
                            .map_err(user_intent_to_runtime)?
                    };

                    // Steer: rewrite hook (passthrough/mock; live LLM deferred), then
                    // durable Intent event. FollowUp: durable event only.
                    // Origin/Policy unchanged; router already required active run for Steer.
                    if accepted.intent == UserPromptIntent::Steer {
                        let active_run_id = accepted.active_run_id.ok_or_else(|| {
                            RuntimeError::Denied(format!(
                                "steer rejected: session {session_id} has no active run"
                            ))
                        })?;
                        let context = SteerActiveContext {
                            session_id,
                            active_run_id,
                            active_prompt: runtime_intent(&runtime).ok(),
                        };
                        let rewritten = steer_rewrite
                            .rewrite(&context, &accepted.text)
                            .map_err(|err| RuntimeError::Denied(err.to_string()))?;
                        steer_pending.push(session_id, rewritten.fragment);
                        runtime.submit_intent_with_artifact_and_kind(
                            accepted.text,
                            artifact,
                            UserPromptIntent::Steer,
                        )?;
                        return runtime.status();
                    }
                    if accepted.intent == UserPromptIntent::FollowUp {
                        runtime.submit_intent_with_artifact_and_kind(
                            accepted.text,
                            artifact,
                            UserPromptIntent::FollowUp,
                        )?;
                        return runtime.status();
                    }

                    let run_id =
                        runtime.submit_intent_and_start_run_with_artifact(text, artifact)?;
                    if let Ok(mut router) = intent_router.lock() {
                        let _ = router.set_active_run(session_id, Some(run_id));
                    }
                    let session_workspace = runtime.workspace_root()?;
                    let provider_messages = resolve_provider_messages(
                        &session_workspace,
                        &runtime,
                        Some(&artifact_root),
                        policy_store.as_deref(),
                    )
                    .unwrap_or_else(|_| {
                        vec![ProviderMessage::user(
                            runtime_intent(&runtime).unwrap_or_default(),
                        )]
                    });
                    launch_agent_run(
                        runtime.clone(),
                        run_id,
                        provider_messages,
                        &provider_registry,
                        &default_provider_id,
                        &model_router,
                        credential_resolver.clone(),
                        cancellations.clone(),
                        intent_router.clone(),
                        store.clone(),
                        policy_snapshot(&policy),
                        session_coordinator.clone(),
                        artifact_root,
                        tool_providers.clone(),
                        steer_pending.clone(),
                        hook_prefilter.clone(),
                        policy_store.clone(),
                    )?;
                    runtime.status()
                }) {
                Ok(status) => IpcResponse::Status { session_id, status },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::Context { session_id } => {
            match AgentRuntime::attach(store, policy_snapshot(&policy), session_id) {
                Ok(runtime) => match runtime.workspace_root().and_then(|workspace_root| {
                    resolve_context(&workspace_root, policy_store.as_deref())
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
            }
        }
        IpcRequest::Cancel { session_id } => {
            let session_lock = session_coordinator.lock_for(session_id);
            let _session_guard = session_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(rt) = &workflow_runtime {
                rt.cancel_session(session_id);
            }
            if let Ok(active) = cancellations.lock()
                && let Some(handle) = active.get(&session_id)
            {
                // A session has at most one active run; cancellation is kept
                // outside durable events so no handle leaks through SQLite.
                handle.token.cancel();
            }
            match AgentRuntime::attach(store.clone(), policy_snapshot(&policy), session_id)
                .and_then(|runtime| {
                    let finished_run_id = runtime.active_run_id()?;
                    let _ = runtime.cancel()?;
                    if let Some(run_id) = finished_run_id {
                        // Cancel owns drain; loop exit for same run is a no-op.
                        // Session lock already held — do not re-enter the mutex.
                        start_drained_follow_up_if_any(
                            store.clone(),
                            policy_snapshot(&policy),
                            provider_registry.clone(),
                            default_provider_id.clone(),
                            model_router.clone(),
                            credential_resolver.clone(),
                            cancellations.clone(),
                            intent_router.clone(),
                            session_coordinator.clone(),
                            uploads.artifact_root().to_path_buf(),
                            tool_providers.clone(),
                            steer_pending.clone(),
                            hook_prefilter.clone(),
                            policy_store.clone(),
                            session_id,
                            run_id,
                            false,
                        );
                    }
                    runtime.status()
                }) {
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
            let artifact_store =
                DurableArtifactStore::open(uploads.artifact_root()).expect("open artifact store");
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
            match AgentRuntime::attach(store, policy_snapshot(&policy), session_id).and_then(
                |runtime| {
                    let workspace_root = runtime.workspace_root()?;
                    let tools = ReadOnlyTools::new(&workspace_root);
                    let effect_seam = runtime.effect_seam()?;
                    tools
                        .run_with_seam(tool, origin, &artifact_store, &effect_seam)
                        .map_err(|e| RuntimeError::Denied(e.to_string()))
                        .and_then(|outcome| {
                            crate::tools::record_tool_outcome(&runtime, &outcome)?;
                            Ok(outcome)
                        })
                },
            ) {
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
        } => match AgentRuntime::attach(store, policy_snapshot(&policy), session_id)
            .map(|runtime| runtime.session_id())
        {
            Ok(_) => IpcResponse::Subscribed { session_id },
            Err(error) => runtime_error(error),
        },
        IpcRequest::ResolveApproval {
            session_id,
            approval_id,
            accepted,
        } => match AgentRuntime::attach(store.clone(), policy_snapshot(&policy), session_id)
            .and_then(|runtime| {
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
                                crate::ToolOrchestrator::execute_approved_bash_with_artifacts(
                                    &runtime,
                                    request,
                                    resolution,
                                    deferred,
                                    uploads.artifact_root(),
                                    &hook_prefilter,
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
                    let messages = resolve_provider_messages(
                        &workspace_root,
                        &runtime,
                        Some(uploads.artifact_root()),
                        policy_store.as_deref(),
                    )
                    .map_err(|error| RuntimeError::Denied(error.to_string()))?;
                    launch_agent_run(
                        runtime.clone(),
                        run_id,
                        messages,
                        &provider_registry,
                        &default_provider_id,
                        &model_router,
                        credential_resolver.clone(),
                        cancellations.clone(),
                        intent_router.clone(),
                        store.clone(),
                        policy_snapshot(&policy),
                        session_coordinator.clone(),
                        uploads.artifact_root().to_path_buf(),
                        tool_providers.clone(),
                        steer_pending.clone(),
                        hook_prefilter.clone(),
                        policy_store.clone(),
                    )?;
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
        } => match AgentRuntime::attach(store.clone(), policy_snapshot(&policy), session_id)
            .and_then(|runtime| {
                let request = runtime
                    .pending_approval(approval_id)?
                    .ok_or(RuntimeError::MissingApproval(approval_id))?;
                let session_workspace = runtime.workspace_root()?;
                let detail = compute_approval_detail(request, &session_workspace, &attachments)?;
                Ok((session_id, detail))
            }) {
            Ok((session_id, detail)) => IpcResponse::ApprovalDetail {
                session_id,
                detail: Box::new(detail),
            },
            Err(error) => runtime_error(error),
        },
        IpcRequest::BeginArtifactUpload {
            session_id,
            declared_bytes,
            content_type: _,
        } => {
            match AgentRuntime::attach(store, policy_snapshot(&policy), session_id).and_then(|_| {
                uploads
                    .begin(session_id, declared_bytes)
                    .map_err(|error| RuntimeError::Denied(crate::upload_error_message(&error)))
            }) {
                Ok(upload_id) => IpcResponse::ArtifactUploadBegun {
                    upload_id,
                    max_bytes: crate::MAX_ARTIFACT_UPLOAD_BYTES,
                    max_chunk_bytes: crate::MAX_ARTIFACT_UPLOAD_CHUNK_BYTES,
                },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::AppendArtifactChunk {
            upload_id,
            seq,
            data_b64,
        } => match uploads.append_b64(upload_id, seq, &data_b64) {
            Ok(bytes_received) => IpcResponse::ArtifactChunkAccepted {
                upload_id,
                bytes_received,
                next_seq: seq.saturating_add(1),
            },
            Err(error) => IpcResponse::Error {
                code: match error {
                    crate::ArtifactUploadError::NotFound(_) => IpcErrorCode::Unavailable,
                    crate::ArtifactUploadError::TooLarge
                    | crate::ArtifactUploadError::ChunkTooLarge
                    | crate::ArtifactUploadError::SequenceMismatch { .. }
                    | crate::ArtifactUploadError::InvalidBase64
                    | crate::ArtifactUploadError::DeclaredSizeExceeded { .. }
                    | crate::ArtifactUploadError::DeclaredSizeMismatch { .. }
                    | crate::ArtifactUploadError::TooManyUploads => IpcErrorCode::InvalidRequest,
                    crate::ArtifactUploadError::Poisoned | crate::ArtifactUploadError::Store => {
                        IpcErrorCode::Internal
                    }
                },
                message: crate::upload_error_message(&error),
            },
        },
        IpcRequest::FinishArtifactUpload { upload_id } => match uploads.finish(upload_id) {
            Ok(artifact) => IpcResponse::ArtifactStored { artifact },
            Err(error) => IpcResponse::Error {
                code: match error {
                    crate::ArtifactUploadError::NotFound(_) => IpcErrorCode::Unavailable,
                    crate::ArtifactUploadError::DeclaredSizeMismatch { .. }
                    | crate::ArtifactUploadError::TooLarge => IpcErrorCode::InvalidRequest,
                    _ => IpcErrorCode::Internal,
                },
                message: crate::upload_error_message(&error),
            },
        },
        IpcRequest::AbortArtifactUpload { upload_id } => match uploads.abort(upload_id) {
            Ok(()) => IpcResponse::ArtifactUploadAborted { upload_id },
            Err(error) => IpcResponse::Error {
                code: IpcErrorCode::Unavailable,
                message: crate::upload_error_message(&error),
            },
        },
        IpcRequest::Diagnostics => {
            let policy_engine = policy_snapshot(&policy);
            let subsystems = gather_subsystem_health(
                &store,
                &policy_engine,
                &provider_registry,
                &workspace_root,
            );
            IpcResponse::Diagnostics {
                subsystems: Box::new(subsystems),
            }
        }
        IpcRequest::GotoDefinition {
            path,
            line,
            character,
        } => {
            let query = crate::PositionQuery::new(path, line, character);
            let coding_tools = coding_tools.clone();
            match crate::block_on_coding_tools(async move { coding_tools.definition(&query).await })
            {
                Ok(locations) => IpcResponse::Definition { locations },
                Err(error) if error.is_unavailable() => IpcResponse::Error {
                    code: IpcErrorCode::Unavailable,
                    message: error.to_string(),
                },
                Err(error) => IpcResponse::Error {
                    code: IpcErrorCode::Internal,
                    message: error.to_string(),
                },
            }
        }
        IpcRequest::SetExecutionMode { session_id, mode } => {
            if let Some(required) = mode.required_ipc_capability()
                && !IPC_CAPABILITIES.contains(&required)
            {
                return IpcResponse::Error {
                    code: IpcErrorCode::Unavailable,
                    message: format!(
                        "execution mode {} requires harness capability `{required}`",
                        mode.label()
                    ),
                };
            }
            match AgentRuntime::attach(store.clone(), policy_snapshot(&policy), session_id) {
                Ok(runtime) => {
                    match runtime.record_event(EventPayload::Session(
                        SessionEvent::ExecutionModeChanged { mode },
                    )) {
                        Ok(()) => IpcResponse::ExecutionMode { session_id, mode },
                        Err(error) => runtime_error(error),
                    }
                }
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::GetExecutionMode { session_id } => {
            match AgentRuntime::attach(store.clone(), policy_snapshot(&policy), session_id) {
                Ok(runtime) => match runtime.events() {
                    Ok(events) => match reduce(&events) {
                        Ok(Some(projection)) => IpcResponse::ExecutionMode {
                            session_id,
                            mode: projection.execution_mode,
                        },
                        Ok(None) => IpcResponse::Error {
                            code: IpcErrorCode::MissingSession,
                            message: format!("session {session_id} has no events"),
                        },
                        Err(error) => IpcResponse::Error {
                            code: IpcErrorCode::Internal,
                            message: error.to_string(),
                        },
                    },
                    Err(error) => runtime_error(error),
                },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::ReloadPolicyConfig { path, config_json } => {
            match resolve_reload_policy_config(path.as_deref(), config_json.as_deref()) {
                Ok(config) => {
                    policy
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .reload_config(config);
                    let applied = policy
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .config();
                    IpcResponse::PolicyConfig { config: applied }
                }
                Err(message) => {
                    audit_policy_reload_notice(
                        &store,
                        format!("policy config reload rejected: {message}"),
                    );
                    IpcResponse::Error {
                        code: IpcErrorCode::InvalidRequest,
                        message,
                    }
                }
            }
        }
        IpcRequest::GetPolicyStore => {
            let store = policy_store
                .as_deref()
                .cloned()
                .unwrap_or(crate::PolicyStore {
                    version: crate::POLICY_STORE_VERSION,
                    instructions: Vec::new(),
                });
            IpcResponse::PolicyStore { store }
        }
        IpcRequest::ReloadPolicyStore { path, store_json } => {
            match resolve_reload_policy_store(path.as_deref(), store_json.as_deref()) {
                Ok(store) => {
                    *policy_store_slot
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                        Some(Arc::new(store.clone()));
                    IpcResponse::PolicyStore { store }
                }
                Err(message) => IpcResponse::Error {
                    code: IpcErrorCode::InvalidRequest,
                    message,
                },
            }
        }
        IpcRequest::ListChildRuns { session_id } => {
            let parent = session_id.to_string();
            let runs = if let Some(rt) = &workflow_runtime {
                rt.child_results().list_by_parent(&parent)
            } else if let Some(bridge) = &explore_spawn {
                bridge.child_results().list_by_parent(&parent)
            } else {
                Ok(Vec::new())
            };
            match runs {
                Ok(runs) => IpcResponse::ChildRuns { session_id, runs },
                Err(error) => IpcResponse::Error {
                    code: IpcErrorCode::Internal,
                    message: error.to_string(),
                },
            }
        }
        IpcRequest::GetChildRun { child_id } => {
            let loaded = if let Some(rt) = &workflow_runtime {
                rt.child_results().load_result(&child_id)
            } else if let Some(bridge) = &explore_spawn {
                bridge.child_results().load_result(&child_id)
            } else {
                Ok(None)
            };
            match loaded {
                Ok(Some(run)) => IpcResponse::ChildRun { run },
                Ok(None) => IpcResponse::Error {
                    code: IpcErrorCode::MissingSession,
                    message: format!("unknown child run: {child_id}"),
                },
                Err(error) => IpcResponse::Error {
                    code: IpcErrorCode::Internal,
                    message: error.to_string(),
                },
            }
        }
        IpcRequest::Hover {
            path,
            line,
            character,
        } => {
            let query = crate::PositionQuery::new(path, line, character);
            let coding_tools = coding_tools.clone();
            match crate::block_on_coding_tools(async move { coding_tools.hover(&query).await }) {
                Ok(info) => IpcResponse::Hover { info },
                Err(error) if error.is_unavailable() => IpcResponse::Error {
                    code: IpcErrorCode::Unavailable,
                    message: error.to_string(),
                },
                Err(error) => IpcResponse::Error {
                    code: IpcErrorCode::Internal,
                    message: error.to_string(),
                },
            }
        }
        IpcRequest::StartWorkflow { session_id, recipe } => {
            let Some(rt) = workflow_runtime.as_ref() else {
                return IpcResponse::Error {
                    code: IpcErrorCode::Unavailable,
                    message: "workflow runtime not configured".into(),
                };
            };
            let recipe = match recipe.as_str() {
                "bug" => crate::WorkflowEngine::bug_skeleton_recipe(),
                "feature" => crate::WorkflowEngine::feature_skeleton_recipe(),
                "refactor" => crate::WorkflowEngine::refactor_skeleton_recipe(),
                other => {
                    return IpcResponse::Error {
                        code: IpcErrorCode::InvalidRequest,
                        message: format!("unknown recipe: {other}"),
                    };
                }
            };
            match rt.start(session_id, recipe, workspace_root.clone()) {
                Ok(()) => IpcResponse::WorkflowStatus {
                    session_id,
                    status: format!(
                        "{:?}",
                        rt.status(session_id).unwrap_or(crate::WorkflowStatus::Idle)
                    ),
                    last_summary: None,
                },
                Err(error) => IpcResponse::Error {
                    code: IpcErrorCode::Internal,
                    message: error.to_string(),
                },
            }
        }
        IpcRequest::CancelWorkflow { session_id } => {
            if let Some(rt) = &workflow_runtime {
                rt.cancel_session(session_id);
            }
            IpcResponse::WorkflowStatus {
                session_id,
                status: "Cancelled".into(),
                last_summary: None,
            }
        }
        IpcRequest::AdvanceWorkflow { session_id } => {
            let Some(rt) = workflow_runtime.as_ref() else {
                return IpcResponse::Error {
                    code: IpcErrorCode::Unavailable,
                    message: "workflow runtime not configured".into(),
                };
            };
            match rt.run_next_ready_step(session_id) {
                Ok(summary) => IpcResponse::WorkflowStatus {
                    session_id,
                    status: format!(
                        "{:?}",
                        rt.status(session_id)
                            .unwrap_or(crate::WorkflowStatus::Running)
                    ),
                    last_summary: Some(summary),
                },
                Err(error) => IpcResponse::Error {
                    code: IpcErrorCode::Internal,
                    message: error.to_string(),
                },
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_agent_loop(
    runtime: Arc<AgentRuntime>,
    run_id: uuid::Uuid,
    provider_registry: ProviderRegistry,
    provider_id: String,
    _credential_resolver: Arc<dyn CredentialResolver>,
    messages: Vec<ProviderMessage>,
    cancellation: CancellationToken,
    tool_providers: Option<Arc<tokio::sync::Mutex<crate::ToolProviderRuntime>>>,
    steer_pending: SteerPendingQueue,
    hook_prefilter: crate::HookPrefilter,
) -> bool {
    let provider = match provider_registry.get(&provider_id) {
        Ok(p) => p,
        Err(error) if matches!(runtime.status(), Ok(RuntimeStatus::Running)) => {
            let _ = runtime.finish_run(crate::RunEvent::Failed {
                run_id,
                reason: format!("provider not found: {error}"),
            });
            return false;
        }
        Err(_) => return false,
    };

    let agent_loop = match build_agent_loop(runtime.clone(), tool_providers, hook_prefilter).await {
        Ok(agent) => agent,
        Err(error) if matches!(runtime.status(), Ok(RuntimeStatus::Running)) => {
            let _ = runtime.finish_run(crate::RunEvent::Failed {
                run_id,
                reason: format!("tool providers failed: {error}"),
            });
            return false;
        }
        Err(_) => return false,
    };

    let result = agent_loop
        .execute(
            run_id,
            provider,
            messages,
            cancellation.clone(),
            Some(&steer_pending),
        )
        .await;

    match result {
        Ok(()) if matches!(runtime.status(), Ok(RuntimeStatus::Running)) => {
            let _ = runtime.finish_run(crate::RunEvent::Completed { run_id });
            true
        }
        Err(_) if cancellation.is_cancelled() => {
            // Cancel IPC already finished the run and owns follow-up drain.
            false
        }
        Err(error) if matches!(runtime.status(), Ok(RuntimeStatus::Running)) => {
            let _ = runtime.finish_run(crate::RunEvent::Failed {
                run_id,
                reason: format!("provider stream failed: {error}"),
            });
            false
        }
        _ => false,
    }
}

/// Build AgentLoop with optional MCP injection from ToolProviderRuntime.
/// Explore path never passes tool_providers — stays MCP-free.
async fn build_agent_loop(
    runtime: Arc<AgentRuntime>,
    tool_providers: Option<Arc<tokio::sync::Mutex<crate::ToolProviderRuntime>>>,
    hook_prefilter: crate::HookPrefilter,
) -> anyhow::Result<AgentLoop> {
    let policy = runtime.policy();
    let workspace_root = runtime
        .workspace_root()
        .expect("runtime always has a workspace root");
    let mut web_research =
        crate::web_research::WebResearchEngine::production(policy.egress_policy());
    if let Ok(artifacts) = crate::DurableArtifactStore::open(crate::default_artifact_root()) {
        web_research = web_research.with_artifact_store(
            Arc::new(artifacts),
            crate::web_research::ArtifactPolicy::default(),
        );
    }
    let mut orchestrator = crate::ToolOrchestrator::new(policy, workspace_root)
        .with_web_research(Arc::new(web_research))
        .with_hook_prefilter(hook_prefilter);
    if let Some(providers) = tool_providers {
        let bridge = {
            let mut guard = providers.lock().await;
            guard.ensure_all_registered().await?;
            guard.bridge(None)
        };
        if let Some(bridge) = bridge {
            orchestrator = orchestrator.with_mcp_live(bridge);
        }
    }
    Ok(AgentLoop::with_tool_orchestrator(runtime, orchestrator))
}

/// Record model selection notice and spawn the agent loop; on Completed drain
/// the next FollowUp into a Prompt turn.
#[allow(clippy::too_many_arguments)]
fn launch_agent_run(
    runtime: Arc<AgentRuntime>,
    run_id: uuid::Uuid,
    provider_messages: Vec<ProviderMessage>,
    provider_registry: &ProviderRegistry,
    default_provider_id: &str,
    model_router: &ModelRouter,
    credential_resolver: Arc<dyn CredentialResolver>,
    cancellations: Arc<Mutex<HashMap<uuid::Uuid, ActiveCancellation>>>,
    intent_router: Arc<Mutex<UserIntentRouter>>,
    store: Arc<dyn EventStore>,
    policy: PolicyEngine,
    session_coordinator: SessionCoordinator,
    artifact_root: PathBuf,
    tool_providers: Option<Arc<tokio::sync::Mutex<crate::ToolProviderRuntime>>>,
    steer_pending: SteerPendingQueue,
    hook_prefilter: crate::HookPrefilter,
    policy_store: Option<Arc<crate::PolicyStore>>,
) -> Result<(), RuntimeError> {
    let runtime_session_id = runtime.session_id();
    let requirements = crate::model_router::CapabilityRequirements {
        tools: true,
        ..Default::default()
    };
    let budget = runtime.budget_config().unwrap_or_default();
    let selected = model_router.select_model(&requirements, &budget);
    let selected_provider_id = selected
        .as_ref()
        .map(|s| s.provider_id.clone())
        .unwrap_or_else(|| default_provider_id.to_owned());

    let selection_message = if let Some(ref selection) = selected {
        format!(
            "ModelRouter selected {}/{}: {}",
            selection.provider_id, selection.model_id, selection.reasoning
        )
    } else {
        format!("ModelRouter fallback to default provider: {selected_provider_id}")
    };
    let _ = runtime.append_event(crate::EventPayload::Notice(crate::NoticeEvent::Runtime {
        message: selection_message,
    }));

    let cancellation = CancellationToken::new();
    if let Ok(mut active) = cancellations.lock() {
        active.insert(
            runtime_session_id,
            ActiveCancellation {
                run_id,
                token: cancellation.clone(),
            },
        );
    }

    let task_runtime = runtime;
    let task_cancellations = cancellations.clone();
    let task_provider_registry = provider_registry.clone();
    let task_credential_resolver = credential_resolver;
    let task_intent_router = intent_router;
    let task_store = store;
    let task_policy = policy;
    let task_session_coordinator = session_coordinator;
    let task_default_provider_id = default_provider_id.to_owned();
    let task_model_router = model_router.clone();
    let task_artifact_root = artifact_root;
    let task_tool_providers = tool_providers;
    let task_steer_pending = steer_pending;
    let task_hook_prefilter = hook_prefilter.clone();
    let task_hook_prefilter_drain = hook_prefilter;
    let task_policy_store = policy_store;

    tokio::spawn(async move {
        let completed = run_agent_loop(
            task_runtime,
            run_id,
            task_provider_registry.clone(),
            selected_provider_id.clone(),
            task_credential_resolver.clone(),
            provider_messages,
            cancellation,
            task_tool_providers.clone(),
            task_steer_pending.clone(),
            task_hook_prefilter,
        )
        .await;
        if let Ok(mut active) = task_cancellations.lock()
            && active
                .get(&runtime_session_id)
                .is_some_and(|handle| handle.run_id == run_id)
        {
            active.remove(&runtime_session_id);
        }
        if completed {
            start_drained_follow_up_if_any(
                task_store,
                task_policy,
                task_provider_registry,
                task_default_provider_id,
                task_model_router,
                task_credential_resolver,
                task_cancellations,
                task_intent_router,
                task_session_coordinator,
                task_artifact_root,
                task_tool_providers,
                task_steer_pending,
                task_hook_prefilter_drain,
                task_policy_store,
                runtime_session_id,
                run_id,
                true,
            );
        }
    });
    Ok(())
}

/// Dequeue one FollowUp after Completed/Cancelled and start it as Prompt.
/// No-op when queue empty or another caller already drained this run.
#[allow(clippy::too_many_arguments)]
fn start_drained_follow_up_if_any(
    store: Arc<dyn EventStore>,
    policy: PolicyEngine,
    provider_registry: ProviderRegistry,
    default_provider_id: String,
    model_router: ModelRouter,
    credential_resolver: Arc<dyn CredentialResolver>,
    cancellations: Arc<Mutex<HashMap<uuid::Uuid, ActiveCancellation>>>,
    intent_router: Arc<Mutex<UserIntentRouter>>,
    session_coordinator: SessionCoordinator,
    artifact_root: PathBuf,
    tool_providers: Option<Arc<tokio::sync::Mutex<crate::ToolProviderRuntime>>>,
    steer_pending: SteerPendingQueue,
    hook_prefilter: crate::HookPrefilter,
    policy_store: Option<Arc<crate::PolicyStore>>,
    session_id: uuid::Uuid,
    finished_run_id: uuid::Uuid,
    acquire_session_lock: bool,
) {
    let Some(queued) = (|| -> Option<QueuedFollowUp> {
        let mut router = intent_router.lock().ok()?;
        router
            .take_follow_up_on_run_terminal(session_id, finished_run_id)
            .ok()
            .flatten()
    })() else {
        return;
    };

    // queued.origin preserved on QueuedFollowUp for Policy; turn starts as Prompt.
    let session_lock = if acquire_session_lock {
        Some(session_coordinator.lock_for(session_id))
    } else {
        None
    };
    let _session_guard = session_lock
        .as_ref()
        .map(|lock| lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));

    let Ok(runtime) = AgentRuntime::attach(store.clone(), policy.clone(), session_id) else {
        return;
    };
    let runtime = Arc::new(runtime);
    let Ok(run_id) = runtime.submit_intent_and_start_run(queued.text) else {
        return;
    };
    if let Ok(mut router) = intent_router.lock() {
        let _ = router.set_active_run(session_id, Some(run_id));
    }
    let Ok(session_workspace) = runtime.workspace_root() else {
        return;
    };
    let provider_messages = resolve_provider_messages(
        &session_workspace,
        &runtime,
        Some(&artifact_root),
        policy_store.as_deref(),
    )
    .unwrap_or_else(|_| {
        vec![ProviderMessage::user(
            runtime_intent(&runtime).unwrap_or_default(),
        )]
    });
    let _ = launch_agent_run(
        runtime,
        run_id,
        provider_messages,
        &provider_registry,
        &default_provider_id,
        &model_router,
        credential_resolver,
        cancellations,
        intent_router,
        store,
        policy,
        session_coordinator,
        artifact_root,
        tool_providers,
        steer_pending,
        hook_prefilter,
        policy_store,
    );
}

fn resolve_context(
    workspace_root: &std::path::Path,
    policy_store: Option<&crate::PolicyStore>,
) -> anyhow::Result<crate::ResolvedInstructions> {
    let resolved = InstructionResolver::new(workspace_root).resolve(&ResolveRequest::default())?;
    if let Some(store) = policy_store {
        let _governed = store.governed_ids_in(&resolved);
    }
    Ok(resolved)
}

fn resolve_provider_messages(
    workspace_root: &std::path::Path,
    runtime: &AgentRuntime,
    artifact_root: Option<&std::path::Path>,
    policy_store: Option<&crate::PolicyStore>,
) -> anyhow::Result<Vec<ProviderMessage>> {
    resolve_provider_messages_with_binding(
        workspace_root,
        runtime,
        &Profile::Standard.default_bindings().context,
        DEFAULT_CONTEXT_BUDGET_TOKENS,
        artifact_root,
        policy_store,
    )
}

fn resolve_provider_messages_with_binding(
    workspace_root: &std::path::Path,
    runtime: &AgentRuntime,
    context_binding: &crate::ServiceBinding,
    budget_tokens: usize,
    artifact_root: Option<&std::path::Path>,
    policy_store: Option<&crate::PolicyStore>,
) -> anyhow::Result<Vec<ProviderMessage>> {
    let instructions = resolve_context(workspace_root, policy_store)?;
    let tools = default_tool_stubs();
    let mut messages =
        system_messages_for_binding(context_binding, &instructions, &tools, budget_tokens);
    let root = artifact_root
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::default_artifact_root);
    let artifact_store = DurableArtifactStore::open(root).ok();
    let artifact_budget = TokenBudget {
        max_tokens: budget_tokens.clamp(64, 2_000),
    };
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
                let user_text = materialize_intent_text(
                    artifact_store.as_ref(),
                    &intent.text,
                    intent.artifact.as_ref(),
                    artifact_budget,
                );
                messages.push(ProviderMessage::user(user_text));
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
                let (preview, artifact_error) = materialize_tool_preview(
                    artifact_store.as_ref(),
                    &preview,
                    artifact.as_ref(),
                    artifact_budget,
                );
                let error = match (error, artifact_error) {
                    (Some(existing), Some(extra)) => Some(format!("{existing}; {extra}")),
                    (Some(existing), None) => Some(existing),
                    (None, Some(extra)) => Some(extra),
                    (None, None) => None,
                };
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

/// Expand an intent that carries an artifact ref into budgeted prompt text.
/// Compact placeholder text stays as a label; raw paste never enters the event.
fn materialize_intent_text(
    store: Option<&DurableArtifactStore>,
    label: &str,
    artifact: Option<&crate::DurableArtifactRef>,
    budget: TokenBudget,
) -> String {
    let Some(artifact) = artifact else {
        return label.to_string();
    };
    let Some(store) = store else {
        return if label.is_empty() {
            format!(
                "[pasted artifact {} unavailable: artifact store missing]",
                artifact.id
            )
        } else {
            format!("{label}\n\n[artifact body unavailable: artifact store missing]")
        };
    };
    match ContextBuilder::new(store, budget).materialize(artifact) {
        Ok(materialized) => {
            if label.is_empty() {
                materialized.content
            } else {
                format!("{label}\n\n{}", materialized.content)
            }
        }
        Err(error) => {
            if label.is_empty() {
                format!("[pasted artifact {} unavailable: {error}]", artifact.id)
            } else {
                format!("{label}\n\n[artifact body unavailable: {error}]")
            }
        }
    }
}

/// When a tool observation carries an artifact, replace the inline preview with
/// a budgeted materialization from chunked `read_range` (never full `read`).
fn materialize_tool_preview(
    store: Option<&DurableArtifactStore>,
    preview: &str,
    artifact: Option<&crate::DurableArtifactRef>,
    budget: TokenBudget,
) -> (String, Option<String>) {
    let Some(artifact) = artifact else {
        return (preview.to_string(), None);
    };
    let Some(store) = store else {
        return (
            preview.to_string(),
            Some("artifact store unavailable for context materialization".into()),
        );
    };
    match ContextBuilder::new(store, budget).materialize(artifact) {
        Ok(materialized) => (materialized.content, None),
        Err(error) => (preview.to_string(), Some(error.to_string())),
    }
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
        RuntimeError::Store(crate::StoreError::MissingSession(_))
        | RuntimeError::Store(crate::StoreError::MissingCheckpoint(_))
        | RuntimeError::Store(crate::StoreError::MissingSequence { .. }) => {
            IpcErrorCode::MissingSession
        }
        RuntimeError::Store(crate::StoreError::DuplicateCheckpointName { .. })
        | RuntimeError::ActiveRun(_) => IpcErrorCode::Conflict,
        RuntimeError::Store(crate::StoreError::EmptyCheckpointName) => IpcErrorCode::InvalidRequest,
        RuntimeError::Denied(message) if message.contains("steer rejected") => {
            IpcErrorCode::Conflict
        }
        RuntimeError::Denied(message) if message.contains("fanout rejected") => {
            IpcErrorCode::InvalidRequest
        }
        _ => IpcErrorCode::Internal,
    };
    IpcResponse::Error {
        code,
        message: error.to_string(),
    }
}

fn user_intent_to_runtime(error: UserIntentError) -> RuntimeError {
    match error {
        UserIntentError::UnknownSession(id) => RuntimeError::MissingSession(id),
        UserIntentError::NoActiveRun(id) => {
            RuntimeError::Denied(format!("steer rejected: session {id} has no active run"))
        }
        UserIntentError::EmptyFanout => {
            RuntimeError::Denied("fanout rejected: empty session id list".into())
        }
    }
}

fn store_error(error: crate::StoreError) -> IpcResponse {
    runtime_error(RuntimeError::Store(error))
}

fn gather_subsystem_health(
    store: &Arc<dyn EventStore>,
    policy: &PolicyEngine,
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

    let providers: Vec<String> = provider_registry.list_provider_ids();
    let capability_truth = crate::CapabilityTruthReport::gather(&providers);

    // Durable artifacts + separate ephemeral approval attachments (do not conflate).
    let artifact_store = SubsystemStatus::ok(
        "DurableArtifactStore active; AttachmentStore remains ephemeral for approval previews",
    )
    .with_details(serde_json::json!({
        "durable": true,
        "durable_artifact_store": true,
        "ephemeral_attachment_store": true,
        "capability": capability_truth.entry("durable_artifact_store"),
    }));

    // Policy Engine
    let policy_engine =
        SubsystemStatus::ok("Policy engine active").with_details(serde_json::json!({
            "workspace_root": workspace_root.display().to_string(),
        }));

    // Provider Registry
    let provider_registry_status = if providers.is_empty() {
        SubsystemStatus::unavailable("No providers registered")
    } else {
        SubsystemStatus::ok(format!("Providers: {}", providers.join(", "))).with_details(
            serde_json::json!({
                "providers": providers,
                "openai_native": capability_truth.entry("openai_native_chat_completions"),
                "openai_responses": capability_truth.entry("openai_responses_api"),
                "openai_compat": capability_truth.entry("openai_compat_text_adapter"),
            }),
        )
    };

    // Path-scope admission is production. Seatbelt process wrap is wired on macOS.
    let sandbox =
        SubsystemStatus::ok("Path-scope sandbox fail-closed; Seatbelt process wrap on macOS")
            .with_details(serde_json::json!({
                "platform": std::env::consts::OS,
                "fail_closed": true,
                "admission": "path_scope",
                "seatbelt_process_wrap": true,
                "capability": capability_truth.entry("seatbelt_process_wrap"),
            }));

    // Credential Store (platform keychain)
    let credential_store = if cfg!(target_os = "macos") {
        SubsystemStatus::ok("macOS Keychain available")
            .with_details(serde_json::json!({ "backend": "keychain" }))
    } else {
        SubsystemStatus::unavailable("Platform credential store not configured")
    };

    // Tools/Capabilities Registration + schema gate truth
    let tools_capabilities =
        SubsystemStatus::ok("Built-in tools registered; tool_schema validates args before policy")
            .with_details(serde_json::json!({
                "builtin_tools": ["bash", "read", "write", "edit", "search"],
                "module_registry": "available",
                "tool_schema_gate": true,
                "provider_http_tools": true,
                "capability": capability_truth.entry("tool_schema_validation"),
            }));

    // External Agents / ACP Adapters
    let external_agents = SubsystemStatus::unavailable("No external agents configured")
        .with_details(serde_json::json!({
            "acp_adapters": [],
            "note": "ACP adapter support planned; not a production Seatbelt claim"
        }));

    // Optional modules + extension import vs runtime honesty
    let optional_modules = SubsystemStatus::ok(
        "Module registry available; extension import Implemented, runtime Partial",
    )
    .with_details(serde_json::json!({
        "loaded_modules": 0,
        "compatibility_adapters": 0,
        "remote_capabilities": false,
        "extension_import": capability_truth.entry("extension_import"),
        "extension_runtime": capability_truth.entry("extension_runtime"),
        "capability_matrix": capability_truth,
    }));

    // Disk/Runtime health
    let disk_runtime = probe_disk_runtime(workspace_root);

    // Offline-safe web inspection: live backend checks are deliberately not run by doctor.
    let web_engine = crate::web_research::WebResearchEngine::production(policy.egress_policy());
    let web_report = crate::web_research::WebDoctor::inspect(
        &web_engine,
        crate::web_research::BrowserServiceStatus::absent(),
    );
    let web_research = SubsystemStatus::ok("Native web research contract available").with_details(
        serde_json::json!({
            "internet_access": policy.scope().allow_network,
            "web_outbound": policy.scope().allow_web_outbound,
            "private_network": policy.scope().allow_private_network,
            "web_fetch": true,
            "search_backends": web_report.search_backends,
            "browser_provider": web_report.browser,
            "live_probe_performed": web_report.live_probe_performed,
            "notes": web_report.notes,
        }),
    );

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
        output_optimization: probe_output_optimization(),
    }
}

fn probe_disk_runtime(workspace_root: &Path) -> crate::SubsystemStatus {
    use crate::SubsystemStatus;

    // Check workspace accessibility
    let workspace_readable = workspace_root.exists() && workspace_root.is_dir();

    // Basic runtime checks
    let temp_writable = std::env::temp_dir().exists();

    if workspace_readable && temp_writable {
        SubsystemStatus::ok("Disk and runtime healthy").with_details(serde_json::json!({
            "workspace_root": workspace_root.display().to_string(),
            "temp_dir": std::env::temp_dir().display().to_string(),
        }))
    } else {
        SubsystemStatus::unavailable("Disk or runtime issues detected").with_details(
            serde_json::json!({
                "workspace_readable": workspace_readable,
                "temp_writable": temp_writable,
            }),
        )
    }
}

fn probe_output_optimization() -> crate::SubsystemStatus {
    use crate::SubsystemStatus;
    use crate::rtk_adapter::RtkAdapter;

    let probe = RtkAdapter::probe();

    if probe.available {
        SubsystemStatus::ok("Output optimization available").with_details(serde_json::json!({
            "builtin_reducer": true,
            "rtk_available": true,
            "rtk_version": probe.version,
            "rtk_capabilities": probe.capabilities.iter().map(|c| format!("{:?}", c)).collect::<Vec<_>>(),
        }))
    } else {
        SubsystemStatus::ok("Builtin reducer only (RTK not found)").with_details(
            serde_json::json!({
                "builtin_reducer": true,
                "rtk_available": false,
                "note": "RTK is optional; builtin reducer works without it",
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
        ActionKind::WebSearch
        | ActionKind::WebFetch
        | ActionKind::WebDownload
        | ActionKind::WebBrowser
        | ActionKind::WebSubmit
        | ActionKind::WebUpload => {
            // Web operations: note target URL
            if let Some(target) = &request.action.target {
                affected_files.push(target.clone());
                estimated_scope = Some(ScopeEstimate::Operations(1));
            }
        }
    }

    Ok(crate::ApprovalDetail {
        schema_version: crate::APPROVAL_DETAIL_SCHEMA_VERSION,
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
        CredentialStrategy, EventPayload, ExecutionMode, MemoryEventStore, OpenAiProvider,
        OpenAiRetryBudget, ProviderError, ProviderProfile, mock_provider::MockStreamItem,
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
        let provider = OpenAiProvider::new(
            ProviderProfile {
                id: "local-test".into(),
                endpoint: format!("http://{address}"),
                model: "test-model".into(),
                credential_strategy: CredentialStrategy::None,
                openai_http_api: Default::default(),
            },
            OpenAiRetryBudget::default(),
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
                artifact: None,
                intent: Default::default(),
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
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
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
            artifact: None,
            intent: Default::default(),
        });
        let durable = serde_json::to_string(&store.list(session_id).unwrap()).unwrap();
        assert!(durable.contains("only user intent"));
        assert!(!durable.contains("resolved secret-free"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prompt_path_uses_token_budgeted_context_optimizer() {
        let root = tempfile::tempdir().expect("temp root");
        let workspace = root.path();
        std::fs::write(
            workspace.join("AGENTS.md"),
            "HOT project rules that must remain",
        )
        .expect("agents");
        std::fs::create_dir_all(workspace.join(".impetus/conventions")).expect("conv dir");
        // Large WARM convention (~200 tokens) dropped under tight budget.
        std::fs::write(
            workspace.join(".impetus/conventions/bulk.md"),
            format!("---\nid: bulk\n---\n{}", "C".repeat(800)),
        )
        .expect("convention");

        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace));
        let harness = Harness::new(store.clone(), policy.clone());
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.to_path_buf(),
        }) else {
            panic!("create session");
        };

        let runtime = AgentRuntime::attach(store, policy.clone(), session_id).expect("attach");
        runtime
            .submit_intent("user question for prompt")
            .expect("intent");

        let lazy = crate::ServiceBinding::Builtin {
            variant: "lazy".into(),
        };
        // Budget fits HOT (~9) + cold tool refs; not the large WARM convention.
        let messages =
            resolve_provider_messages_with_binding(workspace, &runtime, &lazy, 20, None, None)
                .expect("resolve");

        let system: Vec<_> = messages
            .iter()
            .filter(|m| m.role() == "system")
            .map(|m| m.content())
            .collect();
        assert!(
            system
                .iter()
                .any(|c| c.contains("HOT project rules that must remain")),
            "HOT instruction kept"
        );
        assert!(
            system.iter().all(|c| !c.contains(&"C".repeat(50))),
            "large WARM convention body dropped under budget"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.role() == "user" && m.content().contains("user question")),
            "conversation intent preserved"
        );

        // Optimizer does not change policy decisions.
        let denied = crate::Action {
            origin: crate::ActionOrigin::Agent,
            kind: crate::ActionKind::SshConnect,
            summary: "connect".into(),
            target: None,
        };
        let before = policy.evaluate(&denied);
        let _ = resolve_provider_messages_with_binding(workspace, &runtime, &lazy, 20, None, None);
        assert_eq!(policy.evaluate(&denied), before);
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
        let provider = OpenAiProvider::new(
            ProviderProfile {
                id: "remote-profile".into(),
                endpoint: "https://api.example.test".into(),
                model: "test-model".into(),
                credential_strategy: CredentialStrategy::KeychainReference {
                    service: "opaque-service-label".into(),
                    account: "opaque-account-label".into(),
                },
                openai_http_api: Default::default(),
            },
            OpenAiRetryBudget::default(),
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
                artifact: None,
                intent: Default::default(),
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
        assert_eq!(detail.schema_version, crate::APPROVAL_DETAIL_SCHEMA_VERSION);
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
                vec![MockStreamItem::ToolCall {
                    id: "read-evidence".into(),
                    tool: "read_file".into(),
                    arguments: r#"{"path":"evidence.txt"}"#.into(),
                }],
                vec![MockStreamItem::ToolCall {
                    id: "write-result".into(),
                    tool: "write_file".into(),
                    arguments: r#"{"path":"result.txt","content":"approved result"}"#.into(),
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
                artifact: None,
                intent: Default::default(),
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
                vec![MockStreamItem::ToolCall {
                    id: "write-blocked".into(),
                    tool: "write_file".into(),
                    arguments: r#"{"path":"blocked.txt","content":"must not write"}"#.into(),
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
            artifact: None,
            intent: Default::default(),
        });
        let mut approval_id = None;
        for _ in 0..100 {
            let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
                session_id,
                after_sequence: 0,
            }) else {
                panic!("event stream")
            };
            approval_id = events.iter().find_map(|event| match &event.payload {
                EventPayload::Approval(crate::ApprovalEvent::Requested { request }) => {
                    Some(request.id)
                }
                _ => None,
            });
            if approval_id.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let approval_id = approval_id.expect("write approval");
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
                artifact: None,
                intent: Default::default(),
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

    #[test]
    fn doctor_reports_offline_safe_web_research_contract() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let health =
            gather_subsystem_health(&store, &policy, &ProviderRegistry::new(), workspace.path());

        assert!(health.web_research.available);
        let details = health.web_research.details.expect("web details");
        assert_eq!(details["internet_access"], false);
        assert_eq!(details["web_outbound"], false);
        assert_eq!(details["private_network"], false);
        assert_eq!(details["web_fetch"], true);
        assert_eq!(details["search_backends"][0]["id"], "bing_html");
        assert_eq!(details["search_backends"][1]["id"], "duckduckgo");
    }

    #[test]
    fn doctor_subsystem_health_matches_capability_truth_matrix() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let health =
            gather_subsystem_health(&store, &policy, &ProviderRegistry::new(), workspace.path());

        let sandbox = health.sandbox.details.expect("sandbox details");
        assert_eq!(sandbox["seatbelt_process_wrap"], true);
        assert_eq!(sandbox["admission"], "path_scope");
        assert!(
            health
                .sandbox
                .message
                .contains("Seatbelt process wrap on macOS")
        );

        let artifacts = health.artifact_store.details.expect("artifact details");
        assert_eq!(artifacts["durable"], true);
        assert_eq!(artifacts["durable_artifact_store"], true);
        assert_eq!(artifacts["ephemeral_attachment_store"], true);

        let tools = health.tools_capabilities.details.expect("tools details");
        assert_eq!(tools["tool_schema_gate"], true);
        assert_eq!(tools["provider_http_tools"], true);

        let modules = health.optional_modules.details.expect("modules details");
        assert_eq!(modules["extension_runtime"]["level"], "IMPLEMENTED");
        assert_eq!(
            modules["extension_runtime"]["details"]["mcp_live_tools_in_loop"],
            true
        );
        assert_eq!(
            modules["extension_runtime"]["details"]["impetusd_autoload"],
            true
        );
        assert_eq!(modules["capability_matrix"]["schema_version"], 1);
        let caps = modules["capability_matrix"]["capabilities"]
            .as_array()
            .expect("capabilities array");
        assert!(
            caps.iter()
                .any(|c| c["id"] == "seatbelt_process_wrap" && c["level"] == "IMPLEMENTED")
        );
        assert!(
            caps.iter()
                .any(|c| c["id"] == "durable_artifact_store" && c["level"] == "IMPLEMENTED")
        );
        assert!(
            caps.iter()
                .any(|c| c["id"] == "tool_schema_validation" && c["level"] == "IMPLEMENTED")
        );
        assert!(
            caps.iter()
                .any(|c| c["id"] == "openai_native_chat_completions" && c["level"] == "PARTIAL")
        );
        assert!(
            caps.iter()
                .any(|c| c["id"] == "openai_responses_api" && c["level"] == "PARTIAL")
        );
    }

    #[test]
    fn ipc_fork_and_checkpoint_expose_branch_metadata() {
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(store.clone(), policy());
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: std::env::current_dir().unwrap().canonicalize().unwrap(),
        }) else {
            panic!("create session");
        };
        // Created + two intents via append on store for deterministic sequences.
        store
            .append_next(
                session_id,
                EventPayload::Intent(crate::IntentEvent::new("one")),
            )
            .expect("intent 1");
        store
            .append_next(
                session_id,
                EventPayload::Intent(crate::IntentEvent::new("two")),
            )
            .expect("intent 2");

        let IpcResponse::Checkpoint { checkpoint } = harness.handle(IpcRequest::CreateCheckpoint {
            session_id,
            name: "stable".into(),
            sequence: Some(2),
        }) else {
            panic!("create checkpoint");
        };
        assert_eq!(checkpoint.sequence, 2);

        let IpcResponse::Session {
            session_id: forked_id,
            ..
        } = harness.handle(IpcRequest::ForkSession {
            session_id,
            up_to_sequence: 2,
        })
        else {
            panic!("fork session");
        };
        assert_ne!(forked_id, session_id);

        let IpcResponse::Sessions { sessions } = harness.handle(IpcRequest::ListSessions) else {
            panic!("list sessions");
        };
        let forked_meta = sessions
            .iter()
            .find(|s| s.id == forked_id)
            .expect("fork meta");
        assert_eq!(forked_meta.parent_session_id, Some(session_id));
        assert_eq!(forked_meta.fork_sequence, Some(2));

        let IpcResponse::Session {
            session_id: restored_id,
            ..
        } = harness.handle(IpcRequest::RestoreCheckpoint {
            checkpoint_id: checkpoint.id,
        })
        else {
            panic!("restore checkpoint");
        };
        assert_ne!(restored_id, session_id);
        assert_ne!(restored_id, forked_id);
    }

    #[test]
    fn chunked_artifact_upload_returns_ref_without_event_body() {
        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
        use sha2::{Digest, Sha256};

        let artifact_root = tempfile::tempdir().expect("artifacts");
        let workspace = tempfile::tempdir().expect("workspace");
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(
            store.clone(),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        )
        .with_artifact_root(artifact_root.path());

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create session");
        };

        let body = b"oversized paste payload that must not enter durable events";
        let IpcResponse::ArtifactUploadBegun { upload_id, .. } =
            harness.handle(IpcRequest::BeginArtifactUpload {
                session_id,
                declared_bytes: Some(body.len()),
                content_type: Some("text/plain".into()),
            })
        else {
            panic!("begin upload");
        };

        let mid = body.len() / 2;
        for (seq, chunk) in [(0u64, &body[..mid]), (1u64, &body[mid..])] {
            let IpcResponse::ArtifactChunkAccepted {
                bytes_received,
                next_seq,
                ..
            } = harness.handle(IpcRequest::AppendArtifactChunk {
                upload_id,
                seq,
                data_b64: BASE64.encode(chunk),
            })
            else {
                panic!("append chunk {seq}");
            };
            assert_eq!(next_seq, seq + 1);
            assert!(bytes_received > 0);
        }

        let IpcResponse::ArtifactStored { artifact } =
            harness.handle(IpcRequest::FinishArtifactUpload { upload_id })
        else {
            panic!("finish upload");
        };
        assert_eq!(artifact.byte_count, body.len());
        assert_eq!(artifact.id, format!("{:x}", Sha256::digest(body)));

        let events = store.list(session_id).expect("events");
        let encoded = serde_json::to_string(&events).expect("encode events");
        assert!(
            !encoded.contains("oversized paste payload"),
            "raw paste must not appear in durable events"
        );

        let durable = DurableArtifactStore::open(artifact_root.path()).expect("reopen");
        assert_eq!(durable.read(&artifact.id).unwrap(), body);

        // #122 Context Builder hook: uploaded ref materializes without full dump path.
        let materialized = crate::ContextBuilder::new(
            &durable,
            crate::output_reducer::TokenBudget { max_tokens: 2_000 },
        )
        .materialize(&artifact)
        .expect("materialize uploaded artifact");
        assert!(materialized.content.contains("oversized paste payload"));
    }

    #[tokio::test]
    async fn prompt_with_artifact_ref_keeps_body_out_of_intent_event() {
        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

        let artifact_root = tempfile::tempdir().expect("artifacts");
        let workspace = tempfile::tempdir().expect("workspace");
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(
            store.clone(),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        )
        .with_artifact_root(artifact_root.path());

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create session");
        };

        let body = b"secret paste body for artifact-backed prompt";
        let IpcResponse::ArtifactUploadBegun { upload_id, .. } =
            harness.handle(IpcRequest::BeginArtifactUpload {
                session_id,
                declared_bytes: Some(body.len()),
                content_type: Some("text/plain".into()),
            })
        else {
            panic!("begin");
        };
        assert!(matches!(
            harness.handle(IpcRequest::AppendArtifactChunk {
                upload_id,
                seq: 0,
                data_b64: BASE64.encode(body),
            }),
            IpcResponse::ArtifactChunkAccepted { .. }
        ));
        let IpcResponse::ArtifactStored { artifact } =
            harness.handle(IpcRequest::FinishArtifactUpload { upload_id })
        else {
            panic!("finish");
        };

        let label = "[Pasted text · 1 KB · 1 lines]";
        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: label.into(),
                artifact: Some(artifact.clone()),
                intent: Default::default(),
            }),
            IpcResponse::Status { .. }
        ));

        let events = store.list(session_id).expect("events");
        let encoded = serde_json::to_string(&events).expect("encode");
        assert!(
            !encoded.contains("secret paste body"),
            "raw paste must not appear in durable events"
        );
        let intent = events
            .iter()
            .rev()
            .find_map(|event| match &event.payload {
                EventPayload::Intent(intent) => Some(intent),
                _ => None,
            })
            .expect("intent");
        assert_eq!(intent.text, label);
        assert_eq!(intent.artifact.as_ref(), Some(&artifact));
        assert_eq!(intent.intent, UserPromptIntent::Prompt);
    }

    #[tokio::test]
    async fn prompt_intent_routes_steer_and_follow_up_without_secrets() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(
            store.clone(),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        );

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create session");
        };

        // Steer without active run → Conflict (router stub).
        let steer_idle = harness.handle(IpcRequest::Prompt {
            session_id,
            text: "nudge".into(),
            artifact: None,
            intent: UserPromptIntent::Steer,
        });
        assert!(
            matches!(
                &steer_idle,
                IpcResponse::Error {
                    code: IpcErrorCode::Conflict,
                    message
                } if message.contains("steer rejected")
            ),
            "expected steer conflict, got {steer_idle:?}"
        );

        // Baseline Prompt starts a run (RunStarted recorded before spawn).
        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "do work".into(),
                artifact: None,
                intent: UserPromptIntent::Prompt,
            }),
            IpcResponse::Status { .. }
        ));

        let steer_ok = harness.handle(IpcRequest::Prompt {
            session_id,
            text: "prefer tests".into(),
            artifact: None,
            intent: UserPromptIntent::Steer,
        });
        assert!(
            matches!(steer_ok, IpcResponse::Status { .. }),
            "steer should accept while run active: {steer_ok:?}"
        );
        let events = store.list(session_id).expect("events");
        let steer_event = events
            .iter()
            .rev()
            .find_map(|event| match &event.payload {
                EventPayload::Intent(intent) if intent.intent == UserPromptIntent::Steer => {
                    Some(intent)
                }
                _ => None,
            })
            .expect("steer intent event");
        assert_eq!(steer_event.text, "prefer tests");

        let follow = harness.handle(IpcRequest::Prompt {
            session_id,
            text: "then open PR".into(),
            artifact: None,
            intent: UserPromptIntent::FollowUp,
        });
        assert!(
            matches!(follow, IpcResponse::Status { .. }),
            "follow-up should enqueue when session exists: {follow:?}"
        );
        let events = store.list(session_id).expect("events");
        let follow_event = events
            .iter()
            .rev()
            .find_map(|event| match &event.payload {
                EventPayload::Intent(intent) if intent.intent == UserPromptIntent::FollowUp => {
                    Some(intent)
                }
                _ => None,
            })
            .expect("follow-up intent event");
        assert_eq!(follow_event.text, "then open PR");
        // No secrets in payloads.
        let encoded = serde_json::to_string(&events).expect("encode");
        assert!(!encoded.contains("sk-"));
        assert!(!encoded.contains("Bearer "));
    }

    #[tokio::test]
    async fn steer_accept_calls_mock_rewrite_seam_without_network() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let mock = Arc::new(crate::MockSteerRewrite::with_fixed_fragment(
            "rewritten-nudge",
        ));
        let harness = Harness::new(
            store.clone(),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        )
        .with_steer_rewrite(mock.clone());

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create session");
        };

        // Idle: rewrite must not run (router rejects first).
        let idle = harness.handle(IpcRequest::Prompt {
            session_id,
            text: "early".into(),
            artifact: None,
            intent: UserPromptIntent::Steer,
        });
        assert!(
            matches!(
                &idle,
                IpcResponse::Error {
                    code: IpcErrorCode::Conflict,
                    ..
                }
            ),
            "expected idle steer conflict: {idle:?}"
        );
        assert_eq!(mock.call_count(), 0);

        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "baseline work".into(),
                artifact: None,
                intent: UserPromptIntent::Prompt,
            }),
            IpcResponse::Status { .. }
        ));

        let steer_ok = harness.handle(IpcRequest::Prompt {
            session_id,
            text: "prefer tests".into(),
            artifact: None,
            intent: UserPromptIntent::Steer,
        });
        assert!(
            matches!(steer_ok, IpcResponse::Status { .. }),
            "steer should accept: {steer_ok:?}"
        );
        assert_eq!(mock.call_count(), 1);
        let calls = mock.calls();
        assert_eq!(calls[0].0.session_id, session_id);
        assert!(calls[0].0.active_run_id != uuid::Uuid::nil());
        assert_eq!(calls[0].0.active_prompt.as_deref(), Some("baseline work"));
        assert_eq!(calls[0].1, "prefer tests");

        // Durable Intent keeps user steer text (origin/policy path unchanged).
        let events = store.list(session_id).expect("events");
        let steer_event = events
            .iter()
            .rev()
            .find_map(|event| match &event.payload {
                EventPayload::Intent(intent) if intent.intent == UserPromptIntent::Steer => {
                    Some(intent)
                }
                _ => None,
            })
            .expect("steer intent");
        assert_eq!(steer_event.text, "prefer tests");
    }

    #[tokio::test]
    async fn follow_up_drains_into_prompt_after_run_completes() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(
            store.clone(),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create session");
        };

        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "first turn".into(),
                artifact: None,
                intent: UserPromptIntent::Prompt,
            }),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "queued next".into(),
                artifact: None,
                intent: UserPromptIntent::FollowUp,
            }),
            IpcResponse::Status { .. }
        ));

        // Wait until drained Prompt turn finishes (two Completed runs).
        let mut completed_runs = 0usize;
        for _ in 0..50 {
            let events = store.list(session_id).expect("events");
            completed_runs = events
                .iter()
                .filter(|e| {
                    matches!(
                        e.payload,
                        EventPayload::Run(crate::RunEvent::Completed { .. })
                    )
                })
                .count();
            if completed_runs >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            completed_runs >= 2,
            "expected drained follow-up to complete a second run, got {completed_runs}"
        );

        let events = store.list(session_id).expect("events");
        let prompt_texts: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::Intent(intent) if intent.intent == UserPromptIntent::Prompt => {
                    Some(intent.text.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(
            prompt_texts.contains(&"first turn"),
            "missing first prompt: {prompt_texts:?}"
        );
        assert!(
            prompt_texts.contains(&"queued next"),
            "drained follow-up must land as Prompt: {prompt_texts:?}"
        );
        assert!(events.iter().any(|e| {
            matches!(
                &e.payload,
                EventPayload::Intent(intent) if intent.intent == UserPromptIntent::FollowUp
                    && intent.text == "queued next"
            )
        }));
    }

    #[tokio::test]
    async fn follow_up_drains_after_cancel() {
        let workspace = tempfile::tempdir().expect("workspace");
        let provider = Arc::new(MockProvider::scripted(
            "slow-scripted",
            "test-model",
            [
                vec![MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "still running ".repeat(200),
                }],
                vec![MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "drained turn".into(),
                }],
            ],
        ));
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
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

        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "long task".into(),
                artifact: None,
                intent: UserPromptIntent::Prompt,
            }),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "after cancel".into(),
                artifact: None,
                intent: UserPromptIntent::FollowUp,
            }),
            IpcResponse::Status { .. }
        ));

        let cancel = harness.handle(IpcRequest::Cancel { session_id });
        assert!(
            matches!(
                cancel,
                IpcResponse::Status {
                    status: RuntimeStatus::Running | RuntimeStatus::Cancelled,
                    ..
                }
            ),
            "cancel should finish first run and may start drained follow-up: {cancel:?}"
        );

        let mut saw_drained_prompt = false;
        for _ in 0..50 {
            let events = store.list(session_id).expect("events");
            saw_drained_prompt = events.iter().any(|e| {
                matches!(
                    &e.payload,
                    EventPayload::Intent(intent)
                        if intent.intent == UserPromptIntent::Prompt
                            && intent.text == "after cancel"
                )
            });
            if saw_drained_prompt
                && events.iter().any(|e| {
                    matches!(
                        e.payload,
                        EventPayload::Run(crate::RunEvent::Cancelled { .. })
                    )
                })
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            saw_drained_prompt,
            "cancel path must drain follow-up as Prompt"
        );
        let events = store.list(session_id).expect("events");
        assert!(events.iter().any(|e| {
            matches!(
                e.payload,
                EventPayload::Run(crate::RunEvent::Cancelled { .. })
            )
        }));
    }

    #[test]
    fn oversized_declared_upload_is_rejected() {
        let artifact_root = tempfile::tempdir().expect("artifacts");
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        )
        .with_artifact_root(artifact_root.path());
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create session");
        };
        let response = harness.handle(IpcRequest::BeginArtifactUpload {
            session_id,
            declared_bytes: Some(crate::MAX_ARTIFACT_UPLOAD_BYTES + 1),
            content_type: None,
        });
        assert!(
            matches!(response, IpcResponse::Error { .. }),
            "expected rejection, got {response:?}"
        );
    }

    #[test]
    fn goto_definition_ipc_uses_mock_coding_tools() {
        let workspace = tempfile::tempdir().expect("workspace");
        let query = crate::PositionQuery::new("src/main.rs", 1, 2);
        let location =
            crate::SourceLocation::new("src/lib.rs", crate::SourceRange::new(8, 0, 8, 4));
        let mock = Arc::new(
            crate::MockCodingToolsProvider::new().with_definition(query, vec![location.clone()]),
        );
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        )
        .with_coding_tools(Arc::new(crate::OptionalCodingToolsService::with_provider(
            mock,
        )));

        let response = harness.handle(IpcRequest::GotoDefinition {
            path: std::path::PathBuf::from("src/main.rs"),
            line: 1,
            character: 2,
        });
        match response {
            IpcResponse::Definition { locations } => {
                assert_eq!(locations, vec![location]);
            }
            other => panic!("expected Definition, got {other:?}"),
        }
    }

    #[test]
    fn goto_definition_ipc_absent_provider_fail_closed() {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        );

        let response = harness.handle(IpcRequest::GotoDefinition {
            path: std::path::PathBuf::from("src/lib.rs"),
            line: 0,
            character: 0,
        });
        match response {
            IpcResponse::Error {
                code: IpcErrorCode::Unavailable,
                message,
            } => {
                assert!(message.contains(crate::ABSENT_CODING_TOOLS_REASON));
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_agent_loop_injects_mcp_bridge_from_tool_providers() {
        use crate::mcp_adapter::McpTool;
        use crate::mcp_live::{McpLiveBridge, McpLiveCallResult, McpLiveCaller};
        use async_trait::async_trait;
        use serde_json::{Value, json};

        struct OkCaller;
        #[async_trait]
        impl McpLiveCaller for OkCaller {
            async fn call_tool(&self, _tool: &str, _arguments: Value) -> McpLiveCallResult {
                McpLiveCallResult::Ok {
                    preview: "from-runtime".into(),
                }
            }
        }

        let workspace = tempfile::tempdir().expect("workspace");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            policy.clone(),
        ));

        let tool = McpTool {
            name: "echo".into(),
            description: "Echo".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"]
            }),
            annotations: Some(json!({"readOnlyHint": true})),
        };
        let mut providers = crate::ToolProviderRuntime::new();
        providers.register_live_bridge(
            "mock",
            Arc::new(McpLiveBridge::from_tools(
                "mock",
                vec![tool],
                Arc::new(OkCaller),
            )),
        );
        let providers = Arc::new(tokio::sync::Mutex::new(providers));

        // Harness stores the runtime handle.
        let harness = Harness::new(Arc::new(MemoryEventStore::default()), policy.clone())
            .with_tool_providers(providers.clone());
        assert!(harness.tool_providers.is_some());

        // build_agent_loop connects/merges and returns a loop (MCP outside AgentLoop).
        let _agent = build_agent_loop(
            runtime.clone(),
            Some(providers.clone()),
            crate::HookPrefilter::default(),
        )
        .await
        .expect("build loop with MCP");

        // Prove the same runtime bridge still serves tools to the orchestrator.
        let bridge = providers.lock().await.bridge(None).expect("bridge");
        let orch = crate::ToolOrchestrator::new(policy, workspace.path().to_path_buf())
            .with_mcp_live(bridge);
        let observations = orch
            .process_tool_calls(
                uuid::Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "mcp-1".into(),
                    name: "mcp:mock:echo".into(),
                    arguments: json!({"text": "hi"}),
                }],
                &runtime,
            )
            .await
            .expect("mcp");
        assert_eq!(observations[0].outcome, crate::ToolOutcomeStatus::Success);
        assert_eq!(observations[0].preview, "from-runtime");
    }

    #[test]
    fn execution_mode_defaults_to_ask_on_new_session() {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create session");
        };
        let IpcResponse::ExecutionMode { mode, .. } =
            harness.handle(IpcRequest::GetExecutionMode { session_id })
        else {
            panic!("get execution mode");
        };
        assert_eq!(mode, ExecutionMode::Ask);
    }

    #[test]
    fn set_execution_mode_persists_via_durable_event() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(
            store.clone(),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create session");
        };
        let IpcResponse::ExecutionMode { mode, .. } =
            harness.handle(IpcRequest::SetExecutionMode {
                session_id,
                mode: ExecutionMode::Plan,
            })
        else {
            panic!("set execution mode");
        };
        assert_eq!(mode, ExecutionMode::Plan);

        let events = store.list(session_id).expect("events");
        assert!(events.iter().any(|event| matches!(
            &event.payload,
            EventPayload::Session(SessionEvent::ExecutionModeChanged {
                mode: ExecutionMode::Plan
            })
        )));

        let IpcResponse::ExecutionMode { mode, .. } =
            harness.handle(IpcRequest::GetExecutionMode { session_id })
        else {
            panic!("get execution mode");
        };
        assert_eq!(mode, ExecutionMode::Plan);
    }

    #[test]
    fn hello_advertises_execution_mode_capabilities() {
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(
                tempfile::tempdir().expect("workspace").path(),
            )),
        );
        let IpcResponse::Hello {
            capabilities,
            version,
        } = harness.handle(IpcRequest::Hello {
            version: IPC_VERSION,
            capabilities: vec![
                "execution_mode".into(),
                "approval_scope_file_edits".into(),
                "approval_scope_full_auto".into(),
            ],
        })
        else {
            panic!("hello");
        };
        assert_eq!(version, IPC_VERSION);
        for cap in [
            "execution_mode",
            "approval_scope_file_edits",
            "approval_scope_full_auto",
        ] {
            assert!(
                capabilities.iter().any(|advertised| advertised == cap),
                "missing capability {cap}"
            );
        }
    }

    #[test]
    fn reload_policy_config_via_ipc_applies_without_restart() {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        );
        let IpcResponse::PolicyConfig { config } = harness.handle(IpcRequest::ReloadPolicyConfig {
            path: None,
            config_json: Some(r#"{"version":1,"overrides":{"write_file":"allow"}}"#.into()),
        }) else {
            panic!("reload policy config");
        };
        assert_eq!(
            config.override_for(crate::ActionKind::WriteFile),
            Some(crate::PolicyConfigDecision::Allow)
        );
        assert_eq!(
            harness
                .policy()
                .config()
                .override_for(crate::ActionKind::WriteFile),
            Some(crate::PolicyConfigDecision::Allow)
        );
    }

    #[test]
    fn reload_policy_config_invalid_keeps_prior_and_audits_notice() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(
            store.clone(),
            PolicyEngine::with_config(
                SandboxScope::local_workspace(workspace.path()),
                PolicyConfig::parse(r#"{"version":1,"overrides":{"write_file":"allow"}}"#)
                    .expect("config"),
            ),
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create session");
        };
        let IpcResponse::Error { code, message } = harness.handle(IpcRequest::ReloadPolicyConfig {
            path: None,
            config_json: Some(r#"{"version":99}"#.into()),
        }) else {
            panic!("expected reload error");
        };
        assert_eq!(code, IpcErrorCode::InvalidRequest);
        assert!(message.contains("99"));
        assert_eq!(
            harness
                .policy()
                .config()
                .override_for(crate::ActionKind::WriteFile),
            Some(crate::PolicyConfigDecision::Allow)
        );
        let events = store.list(session_id).expect("events");
        assert!(events.iter().any(|event| matches!(
            &event.payload,
            EventPayload::Notice(crate::NoticeEvent::Runtime { message })
                if message.contains("policy config reload rejected")
        )));
    }

    #[test]
    fn reload_policy_config_rejects_path_and_inline_together() {
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(
                tempfile::tempdir().expect("workspace").path(),
            )),
        );
        let IpcResponse::Error { code, message } = harness.handle(IpcRequest::ReloadPolicyConfig {
            path: Some(std::path::PathBuf::from("/tmp/policy.json")),
            config_json: Some(r#"{"version":1}"#.into()),
        }) else {
            panic!("expected reload error");
        };
        assert_eq!(code, IpcErrorCode::InvalidRequest);
        assert!(message.contains("not both"));
    }

    #[test]
    fn hello_advertises_reload_policy_config_capability() {
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(
                tempfile::tempdir().expect("workspace").path(),
            )),
        );
        let IpcResponse::Hello { capabilities, .. } = harness.handle(IpcRequest::Hello {
            version: IPC_VERSION,
            capabilities: vec!["reload_policy_config".into()],
        }) else {
            panic!("hello");
        };
        assert!(
            capabilities
                .iter()
                .any(|capability| capability == "reload_policy_config")
        );
    }
}
