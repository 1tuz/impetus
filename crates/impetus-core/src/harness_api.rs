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
    EventPayload, EventStore, IPC_CAPABILITIES, IPC_MIN_SUPPORTED, IPC_VERSION,
    InstructionResolver, IpcErrorCode, IpcRequest, IpcResponse, MockProvider, NoCredentialResolver,
    NoticeEvent, OpenAiNativeAdapter, OpenAiProvider, PolicyConfig, PolicyEngine, Profile,
    ProviderMessage, ProviderRegistry, QueuedFollowUp, ReadOnlyTool, ReadOnlyToolKind,
    ReadOnlyTools, ResolveRequest, RuntimeError, RuntimeStatus, SandboxScope, SessionEvent,
    SteerActiveContext, SteerPendingQueue, SteerRewrite, TokenBudget, ToolOutcome,
    UserIntentRouter, UserIntentSubmission, UserPromptIntent,
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Per-session cancel handle keyed by run so a drained follow-up cannot be
/// cleared by the previous loop's cleanup (cancel/replace race).
#[derive(Clone)]
struct ActiveCancellation {
    run_id: uuid::Uuid,
    token: CancellationToken,
}

/// Reload hook for daemon MCP SoT (`$IMPETUS_DATA_DIR/mcp/*.json`).
///
/// Returns a fresh [`ToolProviderRuntime`] (bridges disconnected). Harness
/// swaps it into the live slot on `ReloadMcpServers`.
pub type McpReloadHook = Arc<dyn Fn() -> Result<crate::ToolProviderRuntime, String> + Send + Sync>;

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
    /// Optional live reload of MCP SoT (`$IMPETUS_DATA_DIR/mcp/*.json`).
    ///
    /// When set, `ReloadMcpServers` re-reads the catalog into `tool_providers`.
    mcp_reload: Option<McpReloadHook>,
    /// Daemon MCP SoT root (`$IMPETUS_DATA_DIR`) for upsert/remove/enable/disable.
    mcp_sot_root: Option<PathBuf>,
    /// Optional session-associated contextual MemoryStore control-plane.
    memory: Option<Arc<crate::SessionMemoryRuntime>>,
    /// Optional daemon ExtensionRuntime inventory (Enabled installs; CLI SoT).
    extension_runtime: Option<Arc<Mutex<crate::ExtensionRuntime>>>,
    /// Optional ExtensionHost package registry (SDK packages → AgentLoop roots).
    extension_host: Option<Arc<Mutex<crate::ExtensionHost>>>,
    /// Optional durable session model/reasoning selection (`session_models/`).
    session_model_store: Option<Arc<crate::SessionModelStore>>,
    /// Daemon-owned hook prefilter catalog for process spawn paths.
    hook_prefilter: crate::HookPrefilter,
    /// Optional governed-instruction catalog (labels/refs only; not PolicyConfig).
    policy_store: Arc<Mutex<Option<Arc<crate::PolicyStore>>>>,
    /// Optional live WorkflowEngine runtime (schedule → child spawn).
    workflow_runtime: Option<Arc<crate::WorkflowRuntime>>,
    /// Optional managed worktrees for session-aware Git cwd resolution.
    worktree_manager: Option<Arc<crate::WorktreeManager>>,
    /// Daemon-owned PTY sessions (`portable-pty`).
    pty: Arc<crate::PtySessionManager>,
    /// Per-session model override (Provider → Model); daemon SoT for picker.
    session_models: Arc<Mutex<HashMap<uuid::Uuid, crate::SessionModelSelection>>>,
    /// Monotonic connection ids for Unix-socket peers (`serve_client`).
    next_connection_id: AtomicU64,
    /// Session → connection allowed to `ResolveApproval` (Create/Attach/Fork/Restore).
    /// In-process `handle()` leaves sessions unbound → no check (tests / legacy).
    approval_owners: Mutex<HashMap<Uuid, u64>>,
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
        // ponytail: PTY seam snapshots policy at harness build; ReloadPolicyConfig
        // does not rebuild it. Upgrade — shared Arc policy inside EffectSeam.
        let pty_seam = crate::EffectSeam::with_sandbox(
            policy.clone(),
            crate::Sandbox::workspace(workspace_root.clone()),
        );
        let artifact_root = crate::default_artifact_root();
        let pty = pty_manager_with_default_artifacts(pty_seam);
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
            uploads: crate::ArtifactUploadStore::new(artifact_root),
            intent_router: Arc::new(Mutex::new(UserIntentRouter::new())),
            coding_tools: Arc::new(crate::OptionalCodingToolsService::absent()),
            steer_rewrite: default_steer_rewrite(),
            steer_pending: SteerPendingQueue::new(),
            explore_spawn: None,
            tool_providers: None,
            mcp_reload: None,
            mcp_sot_root: None,
            memory: None,
            extension_runtime: None,
            extension_host: None,
            session_model_store: None,
            hook_prefilter: crate::HookPrefilter::default(),
            policy_store: Arc::new(Mutex::new(None)),
            workflow_runtime: None,
            worktree_manager: None,
            session_models: Arc::new(Mutex::new(HashMap::new())),
            next_connection_id: AtomicU64::new(1),
            approval_owners: Mutex::new(HashMap::new()),
            pty,
        }
    }

    /// Override durable artifact root (tests / portable installs).
    pub fn with_artifact_root(mut self, root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        self.uploads = crate::ArtifactUploadStore::new(root.clone());
        if let Ok(store) = DurableArtifactStore::open(&root) {
            self.pty.set_artifacts(Arc::new(store));
        }
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

    /// Attach MCP SoT reload hook (`ReloadMcpServers` → re-read `mcp/*.json`).
    pub fn with_mcp_reload(mut self, hook: McpReloadHook) -> Self {
        self.mcp_reload = Some(hook);
        self
    }

    /// Attach daemon data root for MCP manage CRUD (`mcp/*.json` under this path).
    pub fn with_mcp_sot_root(mut self, data_root: impl Into<PathBuf>) -> Self {
        self.mcp_sot_root = Some(data_root.into());
        self
    }

    /// Attach session-associated contextual MemoryStore control-plane.
    pub fn with_memory(mut self, memory: Arc<crate::SessionMemoryRuntime>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Attach daemon ExtensionRuntime inventory (Enabled installs from durable store).
    ///
    /// CLI remains the control plane for enable/disable/unload. This slot makes
    /// loaded inventory queryable via `ListExtensions` / `GetExtensionStatus`.
    /// AgentLoop skill-path inject is separate and not implied by this wire.
    pub fn with_extension_runtime(mut self, runtime: Arc<Mutex<crate::ExtensionRuntime>>) -> Self {
        self.extension_runtime = Some(runtime);
        self
    }

    /// Attach ExtensionHost package registry (discovery / enable / AgentLoop roots).
    pub fn with_extension_host(mut self, host: Arc<Mutex<crate::ExtensionHost>>) -> Self {
        self.extension_host = Some(host);
        self
    }

    /// Attach durable session model/reasoning store (`$IMPETUS_DATA_DIR/session_models`).
    pub fn with_session_model_store(mut self, store: Arc<crate::SessionModelStore>) -> Self {
        self.session_model_store = Some(store);
        self
    }

    /// Test-only: seed RAM session-model map without SetSessionModel validation.
    #[cfg(test)]
    fn seed_session_model_ram_for_test(
        &self,
        session_id: uuid::Uuid,
        selection: crate::SessionModelSelection,
    ) {
        self.session_models
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(session_id, selection);
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

    /// Attach WorktreeManager for session-aware Git IPC cwd resolution.
    pub fn with_worktree_manager(mut self, manager: Arc<crate::WorktreeManager>) -> Self {
        self.worktree_manager = Some(manager);
        self
    }

    /// Replace PTY manager (tests / durable SqlitePtySessionStore wire).
    pub fn with_pty_manager(mut self, pty: Arc<crate::PtySessionManager>) -> Self {
        self.pty = pty;
        self
    }

    pub fn pty_manager(&self) -> Arc<crate::PtySessionManager> {
        self.pty.clone()
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
        let pty_seam = crate::EffectSeam::with_sandbox(
            policy.clone(),
            crate::Sandbox::workspace(workspace_root.clone()),
        );
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
            mcp_reload: None,
            mcp_sot_root: None,
            memory: None,
            extension_runtime: None,
            extension_host: None,
            session_model_store: None,
            hook_prefilter: crate::HookPrefilter::default(),
            policy_store: Arc::new(Mutex::new(None)),
            workflow_runtime: None,
            worktree_manager: None,
            session_models: Arc::new(Mutex::new(HashMap::new())),
            next_connection_id: AtomicU64::new(1),
            approval_owners: Mutex::new(HashMap::new()),
            pty: pty_manager_with_default_artifacts(pty_seam),
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
        let pty_seam = crate::EffectSeam::with_sandbox(
            policy.clone(),
            crate::Sandbox::workspace(workspace_root.clone()),
        );

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
            mcp_reload: None,
            mcp_sot_root: None,
            memory: None,
            extension_runtime: None,
            extension_host: None,
            session_model_store: None,
            hook_prefilter: crate::HookPrefilter::default(),
            policy_store: Arc::new(Mutex::new(None)),
            workflow_runtime: None,
            worktree_manager: None,
            session_models: Arc::new(Mutex::new(HashMap::new())),
            next_connection_id: AtomicU64::new(1),
            approval_owners: Mutex::new(HashMap::new()),
            pty: pty_manager_with_default_artifacts(pty_seam),
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
        let pty_seam = crate::EffectSeam::with_sandbox(
            policy.clone(),
            crate::Sandbox::workspace(workspace_root.clone()),
        );

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
            mcp_reload: None,
            mcp_sot_root: None,
            memory: None,
            extension_runtime: None,
            extension_host: None,
            session_model_store: None,
            hook_prefilter: crate::HookPrefilter::default(),
            policy_store: Arc::new(Mutex::new(None)),
            workflow_runtime: None,
            worktree_manager: None,
            session_models: Arc::new(Mutex::new(HashMap::new())),
            next_connection_id: AtomicU64::new(1),
            approval_owners: Mutex::new(HashMap::new()),
            pty: pty_manager_with_default_artifacts(pty_seam),
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

    pub fn has_mcp_reload(&self) -> bool {
        self.mcp_reload.is_some()
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

    pub fn has_worktree_manager(&self) -> bool {
        self.worktree_manager.is_some()
    }

    pub fn has_extension_runtime(&self) -> bool {
        self.extension_runtime.is_some()
    }

    pub fn has_extension_host(&self) -> bool {
        self.extension_host.is_some()
    }

    /// Mint a connection-scoped id for one Unix-socket peer (`serve_client`).
    pub fn mint_connection_id(&self) -> u64 {
        self.next_connection_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Resolve a single client request into a response.
    ///
    /// No global lock: EventStore and AgentRuntime use internal coordination.
    /// Independent sessions can execute concurrently.
    ///
    /// In-process callers (tests / `InMemoryTransport`) use this path: sessions
    /// stay unbound and `ResolveApproval` is not connection-gated.
    pub fn handle(&self, request: IpcRequest) -> IpcResponse {
        self.handle_with_connection(None, request)
    }

    /// Daemon path: bind `ResolveApproval` to the connection that created the
    /// session. Attach / Fork / Restore bind only when unbound or already owned
    /// by this connection — foreign Attach must not steal. Other connections get
    /// `Unavailable`. `None` connection id keeps legacy unbound behavior.
    ///
    /// Residual: owner disconnect is not detected; a new connection cannot
    /// reclaim `ResolveApproval` via Attach (no transfer API yet — YAGNI).
    pub fn handle_with_connection(
        &self,
        connection_id: Option<u64>,
        request: IpcRequest,
    ) -> IpcResponse {
        if let Some(cid) = connection_id
            && let IpcRequest::ResolveApproval { session_id, .. } = &request
            && let Some(owner) = self.approval_owner_of(*session_id)
            && owner != cid
        {
            return IpcResponse::Error {
                code: IpcErrorCode::Unavailable,
                message: "resolve_approval is bound to another client connection".into(),
            };
        }

        let create_binds = matches!(&request, IpcRequest::CreateSession { .. });
        let soft_binds = matches!(
            &request,
            IpcRequest::Attach { .. }
                | IpcRequest::ForkSession { .. }
                | IpcRequest::RestoreCheckpoint { .. }
        );

        let response = handle_request(
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
            self.mcp_reload.clone(),
            self.mcp_sot_root.clone(),
            self.memory.clone(),
            self.extension_runtime.clone(),
            self.extension_host.clone(),
            self.session_model_store.clone(),
            self.hook_prefilter.clone(),
            self.policy_store.clone(),
            self.workflow_runtime.clone(),
            self.explore_spawn.clone(),
            self.worktree_manager.clone(),
            self.pty.clone(),
            self.session_models.clone(),
            request,
        );

        if let Some(cid) = connection_id
            && let IpcResponse::Session { session_id, .. } = &response
        {
            if create_binds {
                self.bind_approval_owner(*session_id, cid);
            } else if soft_binds {
                self.bind_approval_owner_if_unbound_or_same(*session_id, cid);
            }
        }
        response
    }

    fn bind_approval_owner(&self, session_id: Uuid, connection_id: u64) {
        self.approval_owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_id, connection_id);
    }

    /// Bind only when unbound or already this connection. Never steal.
    fn bind_approval_owner_if_unbound_or_same(&self, session_id: Uuid, connection_id: u64) {
        let mut owners = self
            .approval_owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match owners.get(&session_id) {
            None => {
                owners.insert(session_id, connection_id);
            }
            Some(&existing) if existing == connection_id => {}
            Some(_) => {}
        }
    }

    fn approval_owner_of(&self, session_id: Uuid) -> Option<u64> {
        self.approval_owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&session_id)
            .copied()
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
    mcp_reload: Option<McpReloadHook>,
    mcp_sot_root: Option<PathBuf>,
    memory: Option<Arc<crate::SessionMemoryRuntime>>,
    extension_runtime: Option<Arc<Mutex<crate::ExtensionRuntime>>>,
    extension_host: Option<Arc<Mutex<crate::ExtensionHost>>>,
    session_model_store: Option<Arc<crate::SessionModelStore>>,
    hook_prefilter: crate::HookPrefilter,
    policy_store_slot: Arc<Mutex<Option<Arc<crate::PolicyStore>>>>,
    workflow_runtime: Option<Arc<crate::WorkflowRuntime>>,
    explore_spawn: Option<Arc<dyn crate::explore_child::ExploreSpawnBridge>>,
    worktree_manager: Option<Arc<crate::WorktreeManager>>,
    pty: Arc<crate::PtySessionManager>,
    session_models: Arc<Mutex<HashMap<uuid::Uuid, crate::SessionModelSelection>>>,
    request: IpcRequest,
) -> IpcResponse {
    let policy_store = policy_store_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    match request {
        IpcRequest::Hello {
            version: client_max,
            min_version,
            capabilities,
        } => {
            let client_min = min_version.unwrap_or(client_max);
            if client_min > client_max {
                IpcResponse::Incompatible {
                    supported_version: IPC_VERSION,
                    min_supported: IPC_MIN_SUPPORTED,
                    client_version: client_max,
                    upgrade_recommendation: Some(format!(
                        "Client min_version {client_min} exceeds preferred version {client_max}"
                    )),
                }
            } else {
                let lo = client_min.max(IPC_MIN_SUPPORTED);
                let hi = client_max.min(IPC_VERSION);
                if lo > hi {
                    let upgrade_recommendation = if client_max < IPC_MIN_SUPPORTED {
                        Some(format!(
                            "Client max version {client_max} is older than min supported {IPC_MIN_SUPPORTED}. Upgrade client."
                        ))
                    } else {
                        Some(format!(
                            "Client min version {client_min} is newer than harness {IPC_VERSION}. Upgrade harness."
                        ))
                    };
                    IpcResponse::Incompatible {
                        supported_version: IPC_VERSION,
                        min_supported: IPC_MIN_SUPPORTED,
                        client_version: client_max,
                        upgrade_recommendation,
                    }
                } else {
                    // Prefer highest mutually supported version in the overlap.
                    let selected = hi;
                    IpcResponse::Hello {
                        version: selected,
                        capabilities: IPC_CAPABILITIES
                            .iter()
                            .filter(|supported| {
                                capabilities
                                    .iter()
                                    .any(|requested| requested == **supported)
                            })
                            .map(|capability| (*capability).to_owned())
                            .collect(),
                    }
                }
            }
        }
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
        } => {
            let exists = match store.list_after(session_id, 0, 1) {
                Ok(probe) => !probe.is_empty(),
                Err(error) => return store_error(error),
            };
            if !exists {
                return runtime_error(RuntimeError::MissingSession(session_id));
            }
            match store.list_after(session_id, after_sequence, usize::MAX) {
                Ok(events) => {
                    let events = crate::trim_events_to_ipc_frame(session_id, events);
                    IpcResponse::Events { session_id, events }
                }
                Err(error) => store_error(error),
            }
        }
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
                    let skill_roots = extension_skill_roots(&extension_host);
                    let mut provider_messages = resolve_provider_messages(
                        &session_workspace,
                        &runtime,
                        Some(&artifact_root),
                        policy_store.as_deref(),
                        &skill_roots,
                    )
                    .unwrap_or_else(|_| {
                        vec![ProviderMessage::user(
                            runtime_intent(&runtime).unwrap_or_default(),
                        )]
                    });
                    let memory_block = memory
                        .as_ref()
                        .and_then(|rt| rt.prompt_context_block(session_id));
                    crate::inject_memory_context(
                        &mut provider_messages,
                        memory_block.as_deref(),
                    );
                    // Hydrate durable session model into RAM (daemon restart) and
                    // fail closed if saved selection is no longer valid.
                    get_or_default_session_model(
                        &session_models,
                        session_model_store.as_ref(),
                        &store,
                        &policy,
                        &provider_registry,
                        &default_provider_id,
                        session_id,
                    )
                    .map_err(|resp| match resp {
                        IpcResponse::Error { message, .. } => {
                            RuntimeError::Denied(message)
                        }
                        other => RuntimeError::Denied(format!(
                            "session model unavailable: {other:?}"
                        )),
                    })?;
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
                        session_models.clone(),
                        session_model_store.clone(),
                        memory.clone(),
                        extension_host.clone(),
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
                    resolve_context(
                        &workspace_root,
                        policy_store.as_deref(),
                        &extension_skill_roots(&extension_host),
                    )
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
                            session_models.clone(),
                            session_model_store.clone(),
                            memory.clone(),
                            extension_host.clone(),
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
                // Tool-orchestrator path stores deferred work and needs a resume
                // launch. ACP permission broker waits in-stream with no deferred
                // tool — ResolveApproval must only settle durable state (never
                // spawn a concurrent agent run over the blocked stream).
                let resume_after_tool_approval = deferred.is_some();
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
                if resume_after_tool_approval
                    && let Some(run_id) = runtime.active_run_id()?
                {
                    let workspace_root = runtime.workspace_root()?;
                    let mut messages = resolve_provider_messages(
                        &workspace_root,
                        &runtime,
                        Some(uploads.artifact_root()),
                        policy_store.as_deref(),
                        &extension_skill_roots(&extension_host),
                    )
                    .map_err(|error| RuntimeError::Denied(error.to_string()))?;
                    let memory_block = memory
                        .as_ref()
                        .and_then(|rt| rt.prompt_context_block(session_id));
                    crate::inject_memory_context(&mut messages, memory_block.as_deref());
                    // Same fail-closed gate as Prompt before resume stream.
                    get_or_default_session_model(
                        &session_models,
                        session_model_store.as_ref(),
                        &store,
                        &policy,
                        &provider_registry,
                        &default_provider_id,
                        session_id,
                    )
                    .map_err(|resp| match resp {
                        IpcResponse::Error { message, .. } => RuntimeError::Denied(message),
                        other => RuntimeError::Denied(format!(
                            "session model unavailable: {other:?}"
                        )),
                    })?;
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
                        session_models.clone(),
                        session_model_store.clone(),
                        memory.clone(),
                        extension_host.clone(),
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
                let deferred = runtime.deferred_tool(approval_id)?;
                let detail = compute_approval_detail(
                    request,
                    &session_workspace,
                    &attachments,
                    deferred.as_ref(),
                )?;
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
            content_type,
        } => {
            match AgentRuntime::attach(store, policy_snapshot(&policy), session_id).and_then(|_| {
                uploads
                    .begin(session_id, declared_bytes, content_type)
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
        IpcRequest::ReadArtifact {
            artifact_id,
            max_bytes,
        } => read_durable_artifact(uploads.artifact_root(), &artifact_id, max_bytes),
        IpcRequest::GetArtifactMetadata { artifact_id } => {
            get_durable_artifact_metadata(uploads.artifact_root(), &artifact_id)
        }
        IpcRequest::ReadArtifactRange {
            artifact_id,
            start,
            len,
        } => read_durable_artifact_range(uploads.artifact_root(), &artifact_id, start, len),
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
            // Capability gate is the negotiated Hello set (daemon enforces).
            // Harness still fail-closes when the mode declares a required cap
            // that this build does not advertise at all.
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
        IpcRequest::CodingDiagnostics { path } => {
            let coding_tools = coding_tools.clone();
            match crate::block_on_coding_tools(async move { coding_tools.diagnostics(&path).await })
            {
                Ok(diagnostics) => IpcResponse::CodingDiagnostics { diagnostics },
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
        IpcRequest::CodingSymbols { path } => {
            let coding_tools = coding_tools.clone();
            match crate::block_on_coding_tools(async move { coding_tools.symbols(&path).await }) {
                Ok(symbols) => IpcResponse::CodingSymbols { symbols },
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
        IpcRequest::CancelCodingRequest { request_id } => {
            let coding_tools = coding_tools.clone();
            match crate::block_on_coding_tools(async move {
                coding_tools.cancel_request(request_id).await
            }) {
                Ok(()) => IpcResponse::CodingCancelAccepted { request_id },
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
            let Some(rt) = workflow_runtime.as_ref() else {
                return IpcResponse::Error {
                    code: IpcErrorCode::Unavailable,
                    message: "workflow runtime not configured".into(),
                };
            };
            rt.cancel_session(session_id);
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
        IpcRequest::ListWorkspaceDir { session_id, path } => handle_workspace_files(
            store,
            policy,
            session_id,
            &path,
            "list workspace directory",
            |root, rel| {
                crate::workspace_files::list_directory(root, rel).map(|listing| {
                    IpcResponse::WorkspaceDirListing {
                        session_id,
                        listing,
                    }
                })
            },
        ),
        IpcRequest::StatWorkspaceFile { session_id, path } => handle_workspace_files(
            store,
            policy,
            session_id,
            &path,
            "stat workspace file",
            |root, rel| {
                crate::workspace_files::stat_path(root, rel).map(|metadata| {
                    IpcResponse::WorkspaceFileStat {
                        session_id,
                        metadata,
                    }
                })
            },
        ),
        IpcRequest::ReadWorkspaceFile {
            session_id,
            path,
            max_bytes,
        } => handle_workspace_files(
            store,
            policy,
            session_id,
            &path,
            "read workspace file",
            |root, rel| {
                let content = match max_bytes {
                    Some(cap) => crate::workspace_files::read_text_file_limited(root, rel, cap),
                    None => crate::workspace_files::read_text_file(root, rel),
                }?;
                Ok(IpcResponse::WorkspaceFileContent {
                    session_id,
                    content,
                })
            },
        ),
        IpcRequest::SearchWorkspaceFiles {
            session_id,
            path,
            pattern,
        } => handle_workspace_files(
            store,
            policy,
            session_id,
            &path,
            "search workspace files",
            |root, rel| {
                crate::workspace_files::search_text(root, rel, &pattern)
                    .map(|result| IpcResponse::WorkspaceSearchResult { session_id, result })
            },
        ),
        // Daemon-owned Git IPC (WorktreeManager cwd + system git CLI).
        IpcRequest::GetRepositoryState { session_id } => {
            match session_git_cwd(&store, &policy, worktree_manager.as_deref(), session_id) {
                Ok(cwd) => match crate::get_repository_state(&cwd) {
                    Ok(state) => IpcResponse::RepositoryState { session_id, state },
                    Err(error) => git_ops_error(error),
                },
                Err(response) => response,
            }
        }
        IpcRequest::ListBranches { session_id } => {
            match session_git_cwd(&store, &policy, worktree_manager.as_deref(), session_id) {
                Ok(cwd) => match crate::list_branches(&cwd.path) {
                    Ok(branches) => IpcResponse::Branches {
                        session_id,
                        branches,
                    },
                    Err(error) => git_ops_error(error),
                },
                Err(response) => response,
            }
        }
        IpcRequest::GetCurrentBranch { session_id } => {
            match session_git_cwd(&store, &policy, worktree_manager.as_deref(), session_id) {
                Ok(cwd) => match crate::get_current_branch(&cwd.path) {
                    Ok(branch) => IpcResponse::CurrentBranch { session_id, branch },
                    Err(error) => git_ops_error(error),
                },
                Err(response) => response,
            }
        }
        IpcRequest::CreateBranch {
            session_id,
            name,
            checkout,
        } => match session_git_cwd(&store, &policy, worktree_manager.as_deref(), session_id) {
            Ok(cwd) => {
                if let (Some(mgr), Some(_)) = (worktree_manager.as_deref(), cwd.binding.as_ref()) {
                    match mgr.create_bound_branch(session_id, &name, checkout) {
                        Ok(binding) => match crate::get_current_branch(&binding.path) {
                            Ok(branch) => IpcResponse::CurrentBranch { session_id, branch },
                            Err(error) => git_ops_error(error),
                        },
                        Err(error) => worktree_switch_error(error),
                    }
                } else {
                    match crate::create_branch(&cwd.path, &name, checkout) {
                        Ok(branch) => IpcResponse::CurrentBranch { session_id, branch },
                        Err(error) => git_ops_error(error),
                    }
                }
            }
            Err(response) => response,
        },
        IpcRequest::SwitchBranch { session_id, name } => {
            match session_git_cwd(&store, &policy, worktree_manager.as_deref(), session_id) {
                Ok(cwd) => {
                    if let (Some(mgr), Some(_)) =
                        (worktree_manager.as_deref(), cwd.binding.as_ref())
                    {
                        match mgr.switch_bound_branch(session_id, &name) {
                            Ok(binding) => match crate::get_current_branch(&binding.path) {
                                Ok(branch) => IpcResponse::CurrentBranch { session_id, branch },
                                Err(error) => git_ops_error(error),
                            },
                            Err(error) => worktree_switch_error(error),
                        }
                    } else {
                        match crate::switch_branch(&cwd.path, &name) {
                            Ok(branch) => IpcResponse::CurrentBranch { session_id, branch },
                            Err(error) => git_ops_error(error),
                        }
                    }
                }
                Err(response) => response,
            }
        }
        IpcRequest::GitStatus { session_id } => {
            match session_git_cwd(&store, &policy, worktree_manager.as_deref(), session_id) {
                Ok(cwd) => match crate::git_status(&cwd.path) {
                    Ok(status) => IpcResponse::GitStatus { session_id, status },
                    Err(error) => git_ops_error(error),
                },
                Err(response) => response,
            }
        }
        IpcRequest::ListChangedFiles { session_id } => {
            match session_git_cwd(&store, &policy, worktree_manager.as_deref(), session_id) {
                Ok(cwd) => match crate::list_changed_files(&cwd.path) {
                    Ok(files) => IpcResponse::ChangedFiles { session_id, files },
                    Err(error) => git_ops_error(error),
                },
                Err(response) => response,
            }
        }
        IpcRequest::GetDiff {
            session_id,
            base_ref,
        } => match session_git_cwd(&store, &policy, worktree_manager.as_deref(), session_id) {
            Ok(cwd) => match crate::get_diff(&cwd.path, base_ref.as_deref()) {
                Ok(mut diff) => {
                    maybe_enrich_diff_counts(
                        worktree_manager.as_deref(),
                        session_id,
                        base_ref.as_deref(),
                        &mut diff,
                    );
                    IpcResponse::Diff { session_id, diff }
                }
                Err(error) => git_ops_error(error),
            },
            Err(response) => response,
        },
        IpcRequest::GetFileDiff {
            session_id,
            path,
            base_ref,
        } => match session_git_cwd(&store, &policy, worktree_manager.as_deref(), session_id) {
            Ok(cwd) => match crate::get_file_diff(&cwd.path, &path, base_ref.as_deref()) {
                Ok(mut diff) => {
                    maybe_enrich_diff_counts(
                        worktree_manager.as_deref(),
                        session_id,
                        base_ref.as_deref(),
                        &mut diff,
                    );
                    IpcResponse::Diff { session_id, diff }
                }
                Err(error) => git_ops_error(error),
            },
            Err(response) => response,
        },
        // Daemon-owned PTY IPC (portable-pty). Additive — do not touch Git/Files arms.
        IpcRequest::PtyStart {
            session_id,
            command,
            args,
            working_dir,
            cols,
            rows,
        } => handle_pty_start(
            &store,
            &policy,
            &pty,
            session_id,
            command,
            args,
            working_dir,
            cols.unwrap_or(80),
            rows.unwrap_or(24),
        ),
        IpcRequest::PtyAttach { session_id, pty_id } => {
            match pty.require_owner(crate::PtySessionId(pty_id), session_id) {
                Ok(_) => match pty.attach(crate::PtySessionId(pty_id)) {
                    Ok(session) => {
                        pty_notice(&store, session_id, format!("pty {pty_id} attached"));
                        pty_session_response(session)
                    }
                    Err(error) => pty_error(error),
                },
                Err(error) => pty_error(error),
            }
        }
        IpcRequest::PtyInput {
            session_id,
            pty_id,
            data_b64,
        } => match pty.require_owner(crate::PtySessionId(pty_id), session_id) {
            Ok(_) => match decode_pty_b64(&data_b64) {
                Ok(data) => match pty.write_input(crate::PtySessionId(pty_id), &data) {
                    Ok(()) => IpcResponse::PtyOk { pty_id },
                    Err(error) => pty_error(error),
                },
                Err(message) => IpcResponse::Error {
                    code: IpcErrorCode::InvalidRequest,
                    message,
                },
            },
            Err(error) => pty_error(error),
        },
        IpcRequest::PtyOutput {
            session_id,
            pty_id,
            max_bytes,
        } => match pty.require_owner(crate::PtySessionId(pty_id), session_id) {
            Ok(owner) => {
                let max = max_bytes.unwrap_or(crate::DEFAULT_PTY_READ_BYTES);
                match pty.read_output(crate::PtySessionId(pty_id), max) {
                    Ok(chunk) => {
                        if let Some(artifact) = chunk.spill_artifact.clone() {
                            let dropped_bytes = artifact.byte_count as u64;
                            pty_emit(
                                &store,
                                owner.owner_session_id,
                                crate::PtyEvent::Spill {
                                    pty_id,
                                    artifact,
                                    dropped_bytes,
                                },
                            );
                        }
                        if chunk.eof {
                            let preview = crate::bound_activity_preview(&String::from_utf8_lossy(
                                &chunk.data,
                            ));
                            pty_emit(
                                &store,
                                owner.owner_session_id,
                                crate::PtyEvent::Output {
                                    pty_id,
                                    preview,
                                    dropped_total: chunk.dropped_total,
                                    eof: true,
                                },
                            );
                        }
                        IpcResponse::PtyOutput {
                            pty_id,
                            data_b64: encode_pty_b64(&chunk.data),
                            dropped_total: chunk.dropped_total,
                            eof: chunk.eof,
                            spill_artifact: chunk.spill_artifact,
                        }
                    }
                    Err(error) => pty_error(error),
                }
            }
            Err(error) => pty_error(error),
        },
        IpcRequest::PtyResize {
            session_id,
            pty_id,
            cols,
            rows,
        } => match pty.require_owner(crate::PtySessionId(pty_id), session_id) {
            Ok(_) => match pty.resize(crate::PtySessionId(pty_id), cols, rows) {
                Ok(()) => IpcResponse::PtyOk { pty_id },
                Err(error) => pty_error(error),
            },
            Err(error) => pty_error(error),
        },
        IpcRequest::PtyDetach { session_id, pty_id } => {
            match pty.require_owner(crate::PtySessionId(pty_id), session_id) {
                Ok(_) => match pty.detach(crate::PtySessionId(pty_id)) {
                    Ok(()) => {
                        pty_notice(&store, session_id, format!("pty {pty_id} detached"));
                        IpcResponse::PtyOk { pty_id }
                    }
                    Err(error) => pty_error(error),
                },
                Err(error) => pty_error(error),
            }
        }
        IpcRequest::PtyTerminate { session_id, pty_id } => {
            match pty.require_owner(crate::PtySessionId(pty_id), session_id) {
                Ok(owner) => match pty.terminate(crate::PtySessionId(pty_id)) {
                    Ok(()) => {
                        let exit_code =
                            pty.get_session(crate::PtySessionId(pty_id))
                                .and_then(|session| match session.state {
                                    crate::PtySessionState::Exited { exit_code } => exit_code,
                                    _ => None,
                                });
                        pty_emit(
                            &store,
                            owner.owner_session_id,
                            crate::PtyEvent::Exited { pty_id, exit_code },
                        );
                        IpcResponse::PtyOk { pty_id }
                    }
                    Err(error) => pty_error(error),
                },
                Err(error) => pty_error(error),
            }
        }
        IpcRequest::PtyStatus { session_id, pty_id } => {
            match pty.require_owner(crate::PtySessionId(pty_id), session_id) {
                Ok(session) => pty_session_response(session),
                Err(error) => pty_error(error),
            }
        }
        // Daemon SoT catalog reads (labels/status only; never env/credentials).
        IpcRequest::ListMcpServers => {
            let servers = match tool_providers {
                Some(runtime) => {
                    crate::block_on_coding_tools(async move { runtime.lock().await.list_status() })
                }
                None => Vec::new(),
            };
            IpcResponse::McpServers { servers }
        }
        IpcRequest::ListModels | IpcRequest::ListProviders => {
            let registry = provider_registry.clone();
            let default_id = default_provider_id.clone();
            let providers = crate::block_on_coding_tools(async move {
                registry.list_status_with_discovery(&default_id).await
            });
            IpcResponse::Models { providers }
        }
        IpcRequest::GetSessionModel { session_id } => {
            match get_or_default_session_model(
                &session_models,
                session_model_store.as_ref(),
                &store,
                &policy,
                &provider_registry,
                &default_provider_id,
                session_id,
            ) {
                Ok(selection) => IpcResponse::SessionModel {
                    session_id,
                    selection,
                },
                Err(resp) => resp,
            }
        }
        IpcRequest::SetSessionModel {
            session_id,
            provider_id,
            model_id,
            reasoning_effort,
        } => match store_session_model(
            &session_models,
            session_model_store.as_ref(),
            &store,
            &policy,
            &provider_registry,
            session_id,
            provider_id,
            model_id,
            reasoning_effort,
        ) {
            Ok(selection) => IpcResponse::SessionModel {
                session_id,
                selection,
            },
            Err(resp) => resp,
        },
        IpcRequest::CreateWorktree {
            session_id,
            for_build,
        } => handle_create_worktree(worktree_manager.as_deref(), &store, &policy, session_id, for_build),
        IpcRequest::ListWorktrees { session_id } => {
            handle_list_worktrees(worktree_manager.as_deref(), session_id)
        }
        IpcRequest::GetWorktree { worktree_id } => {
            handle_get_worktree(worktree_manager.as_deref(), &worktree_id)
        }
        IpcRequest::CloseWorktree { worktree_id } => {
            handle_close_worktree(worktree_manager.as_deref(), &worktree_id)
        }
        IpcRequest::ResumeWorktree { session_id } => {
            handle_resume_worktree(worktree_manager.as_deref(), session_id)
        }
        IpcRequest::StopWorktree { session_id } => {
            handle_stop_worktree(worktree_manager.as_deref(), session_id)
        }
        IpcRequest::CheckWorktreeMergeReady {
            session_id,
            base_ref,
        } => handle_check_merge_ready(worktree_manager.as_deref(), session_id, &base_ref),
        IpcRequest::MergeWorktree {
            session_id,
            base_ref,
        } => handle_merge_worktree(worktree_manager.as_deref(), session_id, &base_ref),
        IpcRequest::ReloadMcpServers => match (tool_providers.clone(), mcp_reload.clone()) {
            (Some(slot), Some(hook)) => match hook() {
                Ok(fresh) => {
                    let servers = crate::block_on_coding_tools(async move {
                        let mut guard = slot.lock().await;
                        guard.replace_all(fresh);
                        guard.list_status()
                    });
                    IpcResponse::McpServers { servers }
                }
                Err(message) => IpcResponse::Error {
                    code: IpcErrorCode::InvalidRequest,
                    message,
                },
            },
            (None, None) => IpcResponse::McpServers {
                servers: Vec::new(),
            },
            _ => IpcResponse::Error {
                code: IpcErrorCode::Unavailable,
                message: "ReloadMcpServers requires both tool_providers slot and mcp_reload hook (daemon SoT)"
                    .into(),
            },
        },
        IpcRequest::UpsertMcpServer { server } => handle_mcp_manage_mutate(
            mcp_sot_root.as_deref(),
            tool_providers.clone(),
            mcp_reload.clone(),
            McpManageMutate::Upsert(server),
        ),
        IpcRequest::RemoveMcpServer { id } => handle_mcp_manage_mutate(
            mcp_sot_root.as_deref(),
            tool_providers.clone(),
            mcp_reload.clone(),
            McpManageMutate::Remove(id),
        ),
        IpcRequest::EnableMcpServer { id } => handle_mcp_manage_mutate(
            mcp_sot_root.as_deref(),
            tool_providers.clone(),
            mcp_reload.clone(),
            McpManageMutate::Enable(id),
        ),
        IpcRequest::DisableMcpServer { id } => handle_mcp_manage_mutate(
            mcp_sot_root.as_deref(),
            tool_providers.clone(),
            mcp_reload.clone(),
            McpManageMutate::Disable(id),
        ),
        IpcRequest::ListMemory { session_id, scope } => {
            handle_memory_list(&store, memory.as_ref(), session_id, scope)
        }
        IpcRequest::GetMemory { session_id, id } => {
            handle_memory_get(&store, memory.as_ref(), session_id, &id)
        }
        IpcRequest::AppendMemory {
            session_id,
            id,
            scope,
            content,
            provenance,
        } => handle_memory_append(
            &store,
            memory.as_ref(),
            session_id,
            id,
            scope,
            content,
            provenance,
        ),
        IpcRequest::ClearMemory { session_id, scope } => {
            handle_memory_clear(&store, memory.as_ref(), session_id, scope)
        }
        IpcRequest::ExportMemory { session_id, format } => {
            handle_memory_export(&store, memory.as_ref(), session_id, format)
        }
        IpcRequest::ListExtensions => handle_list_extensions(extension_runtime.as_ref()),
        IpcRequest::GetExtensionStatus { installation_id } => {
            handle_get_extension_status(extension_runtime.as_ref(), &installation_id)
        }
        IpcRequest::ReloadExtensionPackages => {
            let resp = handle_reload_extension_packages(
                extension_host.as_ref(),
                mcp_sot_root.as_deref(),
                &workspace_root,
            );
            if matches!(resp, IpcResponse::ExtensionPackagesReloaded { .. }) {
                refresh_mcp_runtime_best_effort(tool_providers.clone(), mcp_reload.clone());
            }
            resp
        }
        IpcRequest::ListExtensionPackages => handle_list_extension_packages(extension_host.as_ref()),
        IpcRequest::GetExtensionPackage { id } => {
            handle_get_extension_package(extension_host.as_ref(), &id)
        }
        IpcRequest::EnableExtensionPackage { id } => handle_enable_extension_package(
            extension_host.as_ref(),
            tool_providers.clone(),
            mcp_reload.clone(),
            &id,
        ),
        IpcRequest::DisableExtensionPackage { id } => handle_disable_extension_package(
            extension_host.as_ref(),
            tool_providers.clone(),
            mcp_reload.clone(),
            &id,
        ),
        IpcRequest::GetBrowserHealth => IpcResponse::BrowserHealth {
            status: impetus_protocol::BrowserHealthStatus::absent(),
        },
        IpcRequest::NegotiateBrowser { protocol_version } => IpcResponse::BrowserNegotiate {
            result: impetus_protocol::BrowserNegotiateInfo {
                protocol_version,
                compatible: false,
                reason: impetus_protocol::BrowserHealthStatus::ABSENT_REASON.into(),
            },
        },
    }
}

fn extension_status_info(state: &crate::ExtensionState) -> impetus_protocol::ExtensionStatusInfo {
    use crate::extension_compat::ExtensionSource;
    let source = match &state.resolution.source {
        ExtensionSource::Native => "native",
        ExtensionSource::AgentSkills => "agent_skills",
        ExtensionSource::Mcp => "mcp",
        ExtensionSource::AgentPlugins => "agent_plugins",
        ExtensionSource::ClaudeCode => "claude_code",
        ExtensionSource::Codex => "codex",
        ExtensionSource::Cursor => "cursor",
        ExtensionSource::DeepSeekHarness => "deepseek_harness",
        ExtensionSource::Custom(name) => name.as_str(),
    };
    let status = match state.status {
        crate::ExtensionLifecycleStatus::Enabled => "enabled",
        crate::ExtensionLifecycleStatus::Disabled => "disabled",
        crate::ExtensionLifecycleStatus::Unloaded => "unloaded",
    };
    impetus_protocol::ExtensionStatusInfo {
        installation_id: state.installation_id.clone(),
        module_id: state.resolution.module_id.clone(),
        module_name: state.resolution.module_name.clone(),
        version: state.resolution.version.clone(),
        source: source.to_string(),
        status: status.to_string(),
    }
}

fn handle_list_extensions(
    extension_runtime: Option<&Arc<Mutex<crate::ExtensionRuntime>>>,
) -> IpcResponse {
    let Some(slot) = extension_runtime else {
        return IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: "ExtensionRuntime not wired on this harness".into(),
        };
    };
    let guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut extensions: Vec<_> = guard
        .loaded_states()
        .into_iter()
        .map(extension_status_info)
        .collect();
    extensions.sort_by(|a, b| a.installation_id.cmp(&b.installation_id));
    IpcResponse::Extensions { extensions }
}

fn handle_get_extension_status(
    extension_runtime: Option<&Arc<Mutex<crate::ExtensionRuntime>>>,
    installation_id: &str,
) -> IpcResponse {
    let Some(slot) = extension_runtime else {
        return IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: "ExtensionRuntime not wired on this harness".into(),
        };
    };
    let guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard
        .loaded_states()
        .into_iter()
        .find(|state| state.installation_id == installation_id)
    {
        Some(state) => IpcResponse::ExtensionStatus {
            extension: extension_status_info(state),
        },
        None => IpcResponse::Error {
            code: IpcErrorCode::InvalidRequest,
            message: format!("extension not loaded: {installation_id}"),
        },
    }
}

fn package_info_from_loaded(
    ext: &crate::LoadedExtension,
) -> impetus_protocol::ExtensionPackageInfo {
    use crate::ExtensionHostPhase;
    let phase = match ext.phase {
        ExtensionHostPhase::Discovered => "discovered",
        ExtensionHostPhase::Validated => "validated",
        ExtensionHostPhase::Compatible => "compatible",
        ExtensionHostPhase::Loaded => "loaded",
        ExtensionHostPhase::Active => "active",
        ExtensionHostPhase::Failed => "failed",
        ExtensionHostPhase::Disabled => "disabled",
    };
    let source = match ext.source {
        crate::ExtensionPackageSource::Global => "global",
        crate::ExtensionPackageSource::Workspace => "workspace",
        crate::ExtensionPackageSource::Dev => "dev",
    };
    impetus_protocol::ExtensionPackageInfo {
        id: ext.id.as_str().to_string(),
        name: ext.manifest.name.clone(),
        version: ext.manifest.version.clone(),
        extension_api_version: ext.manifest.extension_api_version,
        source: source.to_string(),
        phase: phase.to_string(),
        capabilities: ext
            .manifest
            .capabilities
            .iter()
            .map(|c| c.as_str().to_string())
            .collect(),
        permissions: ext
            .manifest
            .permissions
            .iter()
            .map(|p| p.as_str().to_string())
            .collect(),
        last_error: ext.last_error.clone(),
        compatible: !matches!(ext.phase, ExtensionHostPhase::Failed),
    }
}

fn handle_reload_extension_packages(
    extension_host: Option<&Arc<Mutex<crate::ExtensionHost>>>,
    data_root: Option<&Path>,
    workspace_root: &Path,
) -> IpcResponse {
    let Some(slot) = extension_host else {
        return IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: "ExtensionHost not wired on this harness".into(),
        };
    };
    let Some(data_root) = data_root else {
        return IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: "Extension package reload requires daemon data root ($IMPETUS_DATA_DIR)"
                .into(),
        };
    };
    let roots =
        crate::ExtensionDiscoveryRoots::from_data_and_workspace(data_root, Some(workspace_root));
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let results = guard.reload(&roots);
    let loaded = results.iter().filter(|(_, r)| r.is_ok()).count() as u32;
    let failed = results.iter().filter(|(_, r)| r.is_err()).count() as u32;
    IpcResponse::ExtensionPackagesReloaded { loaded, failed }
}

fn handle_list_extension_packages(
    extension_host: Option<&Arc<Mutex<crate::ExtensionHost>>>,
) -> IpcResponse {
    let Some(slot) = extension_host else {
        return IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: "ExtensionHost not wired on this harness".into(),
        };
    };
    let guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut packages: Vec<_> = guard
        .list()
        .into_iter()
        .map(package_info_from_loaded)
        .collect();
    packages.sort_by(|a, b| a.id.cmp(&b.id));
    IpcResponse::ExtensionPackages { packages }
}

fn handle_get_extension_package(
    extension_host: Option<&Arc<Mutex<crate::ExtensionHost>>>,
    id: &str,
) -> IpcResponse {
    let Some(slot) = extension_host else {
        return IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: "ExtensionHost not wired on this harness".into(),
        };
    };
    let guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.get(id) {
        Some(ext) => IpcResponse::ExtensionPackage {
            package: package_info_from_loaded(ext),
        },
        None => IpcResponse::Error {
            code: IpcErrorCode::InvalidRequest,
            message: format!("extension package not found: {id}"),
        },
    }
}

fn handle_enable_extension_package(
    extension_host: Option<&Arc<Mutex<crate::ExtensionHost>>>,
    tool_providers: Option<Arc<tokio::sync::Mutex<crate::ToolProviderRuntime>>>,
    mcp_reload: Option<McpReloadHook>,
    id: &str,
) -> IpcResponse {
    let Some(slot) = extension_host else {
        return IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: "ExtensionHost not wired on this harness".into(),
        };
    };
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.enable(id) {
        Ok(()) => {
            let package = match guard.get(id) {
                Some(ext) => package_info_from_loaded(ext),
                None => {
                    return IpcResponse::Error {
                        code: IpcErrorCode::InvalidRequest,
                        message: format!("extension package not found after enable: {id}"),
                    };
                }
            };
            drop(guard);
            refresh_mcp_runtime_best_effort(tool_providers, mcp_reload);
            IpcResponse::ExtensionPackage { package }
        }
        Err(err) => IpcResponse::Error {
            code: IpcErrorCode::InvalidRequest,
            message: err.to_string(),
        },
    }
}

fn handle_disable_extension_package(
    extension_host: Option<&Arc<Mutex<crate::ExtensionHost>>>,
    tool_providers: Option<Arc<tokio::sync::Mutex<crate::ToolProviderRuntime>>>,
    mcp_reload: Option<McpReloadHook>,
    id: &str,
) -> IpcResponse {
    let Some(slot) = extension_host else {
        return IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: "ExtensionHost not wired on this harness".into(),
        };
    };
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.disable(id) {
        Ok(()) => {
            let package = match guard.get(id) {
                Some(ext) => package_info_from_loaded(ext),
                None => {
                    return IpcResponse::Error {
                        code: IpcErrorCode::InvalidRequest,
                        message: format!("extension package not found after disable: {id}"),
                    };
                }
            };
            drop(guard);
            refresh_mcp_runtime_best_effort(tool_providers, mcp_reload);
            IpcResponse::ExtensionPackage { package }
        }
        Err(err) => IpcResponse::Error {
            code: IpcErrorCode::InvalidRequest,
            message: err.to_string(),
        },
    }
}

/// After mcp_bridge enable/disable, refresh ToolProviderRuntime when wired.
fn refresh_mcp_runtime_best_effort(
    tool_providers: Option<Arc<tokio::sync::Mutex<crate::ToolProviderRuntime>>>,
    mcp_reload: Option<McpReloadHook>,
) {
    let (Some(slot), Some(hook)) = (tool_providers, mcp_reload) else {
        return;
    };
    let Ok(fresh) = hook() else {
        return;
    };
    crate::block_on_coding_tools(async move {
        let mut guard = slot.lock().await;
        guard.replace_all(fresh);
    });
}

enum McpManageMutate {
    Upsert(impetus_protocol::McpServerUpsert),
    Remove(String),
    Enable(String),
    Disable(String),
}

fn handle_mcp_manage_mutate(
    mcp_sot_root: Option<&Path>,
    tool_providers: Option<Arc<tokio::sync::Mutex<crate::ToolProviderRuntime>>>,
    mcp_reload: Option<McpReloadHook>,
    op: McpManageMutate,
) -> IpcResponse {
    let Some(root) = mcp_sot_root else {
        return IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: "MCP manage requires daemon SoT root ($IMPETUS_DATA_DIR)".into(),
        };
    };
    let write = match op {
        McpManageMutate::Upsert(server) => crate::upsert_daemon_mcp_server(root, &server),
        McpManageMutate::Remove(id) => crate::remove_daemon_mcp_server(root, &id),
        McpManageMutate::Enable(id) => crate::set_daemon_mcp_enabled(root, &id, true),
        McpManageMutate::Disable(id) => crate::set_daemon_mcp_enabled(root, &id, false),
    };
    if let Err(err) = write {
        return IpcResponse::Error {
            code: IpcErrorCode::InvalidRequest,
            message: err.to_string(),
        };
    }
    match (tool_providers, mcp_reload) {
        (Some(slot), Some(hook)) => match hook() {
            Ok(fresh) => {
                let servers = crate::block_on_coding_tools(async move {
                    let mut guard = slot.lock().await;
                    guard.replace_all(fresh);
                    guard.list_status()
                });
                IpcResponse::McpServers { servers }
            }
            Err(message) => IpcResponse::Error {
                code: IpcErrorCode::InvalidRequest,
                message,
            },
        },
        _ => IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: "MCP manage wrote SoT but reload slot/hook missing".into(),
        },
    }
}

#[allow(clippy::result_large_err)]
fn require_session(store: &Arc<dyn EventStore>, session_id: uuid::Uuid) -> Result<(), IpcResponse> {
    match store.list_sessions() {
        Ok(sessions) if sessions.iter().any(|s| s.id == session_id) => Ok(()),
        Ok(_) => Err(IpcResponse::Error {
            code: IpcErrorCode::MissingSession,
            message: format!("session {session_id} not found"),
        }),
        Err(err) => Err(IpcResponse::Error {
            code: IpcErrorCode::Internal,
            message: err.to_string(),
        }),
    }
}

#[allow(clippy::result_large_err)]
fn require_memory(
    memory: Option<&Arc<crate::SessionMemoryRuntime>>,
) -> Result<&Arc<crate::SessionMemoryRuntime>, IpcResponse> {
    memory.ok_or_else(|| IpcResponse::Error {
        code: IpcErrorCode::Unavailable,
        message: "MemoryStore control-plane not wired on this harness".into(),
    })
}

fn memory_store_error(err: crate::MemoryStoreError) -> IpcResponse {
    IpcResponse::Error {
        code: IpcErrorCode::InvalidRequest,
        message: err.to_string(),
    }
}

fn handle_memory_list(
    store: &Arc<dyn EventStore>,
    memory: Option<&Arc<crate::SessionMemoryRuntime>>,
    session_id: uuid::Uuid,
    scope: Option<impetus_protocol::MemoryEntryScope>,
) -> IpcResponse {
    if let Err(resp) = require_session(store, session_id) {
        return resp;
    }
    let runtime = match require_memory(memory) {
        Ok(runtime) => runtime,
        Err(resp) => return resp,
    };
    match runtime.list(session_id, scope) {
        Ok(entries) => IpcResponse::MemoryEntries {
            session_id,
            entries,
        },
        Err(err) => memory_store_error(err),
    }
}

fn handle_memory_get(
    store: &Arc<dyn EventStore>,
    memory: Option<&Arc<crate::SessionMemoryRuntime>>,
    session_id: uuid::Uuid,
    id: &str,
) -> IpcResponse {
    if let Err(resp) = require_session(store, session_id) {
        return resp;
    }
    let runtime = match require_memory(memory) {
        Ok(runtime) => runtime,
        Err(resp) => return resp,
    };
    match runtime.get(session_id, id) {
        Ok(Some(entry)) => IpcResponse::MemoryEntry { session_id, entry },
        Ok(None) => IpcResponse::Error {
            code: IpcErrorCode::InvalidRequest,
            message: format!("memory entry `{id}` not found"),
        },
        Err(err) => memory_store_error(err),
    }
}

fn handle_memory_append(
    store: &Arc<dyn EventStore>,
    memory: Option<&Arc<crate::SessionMemoryRuntime>>,
    session_id: uuid::Uuid,
    id: String,
    scope: impetus_protocol::MemoryEntryScope,
    content: String,
    provenance: impetus_protocol::MemoryProvenanceInfo,
) -> IpcResponse {
    if let Err(resp) = require_session(store, session_id) {
        return resp;
    }
    let runtime = match require_memory(memory) {
        Ok(runtime) => runtime,
        Err(resp) => return resp,
    };
    match runtime.append(session_id, id, scope, content, provenance) {
        Ok(entry) => IpcResponse::MemoryEntry { session_id, entry },
        Err(err) => memory_store_error(err),
    }
}

fn handle_memory_clear(
    store: &Arc<dyn EventStore>,
    memory: Option<&Arc<crate::SessionMemoryRuntime>>,
    session_id: uuid::Uuid,
    scope: Option<impetus_protocol::MemoryEntryScope>,
) -> IpcResponse {
    if let Err(resp) = require_session(store, session_id) {
        return resp;
    }
    let runtime = match require_memory(memory) {
        Ok(runtime) => runtime,
        Err(resp) => return resp,
    };
    match runtime.clear(session_id, scope) {
        Ok(removed) => IpcResponse::MemoryCleared {
            session_id,
            removed,
        },
        Err(err) => memory_store_error(err),
    }
}

fn handle_memory_export(
    store: &Arc<dyn EventStore>,
    memory: Option<&Arc<crate::SessionMemoryRuntime>>,
    session_id: uuid::Uuid,
    format: impetus_protocol::MemoryExportFormat,
) -> IpcResponse {
    if let Err(resp) = require_session(store, session_id) {
        return resp;
    }
    let runtime = match require_memory(memory) {
        Ok(runtime) => runtime,
        Err(resp) => return resp,
    };
    match runtime.export(session_id, format) {
        Ok(body) => IpcResponse::MemoryExport {
            session_id,
            format,
            body,
        },
        Err(err) => memory_store_error(err),
    }
}

/// Prefer WorktreeManager numstat counts when base_ref + managed worktree exist.
fn maybe_enrich_diff_counts(
    worktrees: Option<&crate::WorktreeManager>,
    session_id: uuid::Uuid,
    base_ref: Option<&str>,
    diff: &mut crate::GitDiffPayload,
) {
    let (Some(mgr), Some(base)) = (worktrees, base_ref) else {
        return;
    };
    if let Ok(summary) = mgr.diff_summary(session_id, base) {
        crate::apply_worktree_diff_counts(diff, &summary);
    }
}

#[allow(clippy::result_large_err)]
fn session_git_cwd(
    store: &Arc<dyn EventStore>,
    policy: &Arc<Mutex<PolicyEngine>>,
    worktrees: Option<&crate::WorktreeManager>,
    session_id: uuid::Uuid,
) -> Result<crate::GitSessionCwd, IpcResponse> {
    let runtime = AgentRuntime::attach(store.clone(), policy_snapshot(policy), session_id)
        .map_err(runtime_error)?;
    let workspace_root = runtime.workspace_root().map_err(runtime_error)?;
    crate::resolve_session_git_cwd(
        worktrees,
        session_id,
        runtime.worktree_id(),
        &workspace_root,
    )
    .map_err(git_ops_error)
}

fn git_ops_error(error: crate::GitOpsError) -> IpcResponse {
    use crate::GitOpsError;
    let code = match &error {
        GitOpsError::NotARepo(_)
        | GitOpsError::InvalidBranchName(_)
        | GitOpsError::BranchExists(_)
        | GitOpsError::UnknownBranch(_) => IpcErrorCode::InvalidRequest,
        GitOpsError::Dirty
        | GitOpsError::ConflictInProgress
        | GitOpsError::StaleWorktree
        | GitOpsError::PathMissing(_) => IpcErrorCode::Conflict,
        GitOpsError::Git(_) | GitOpsError::Io(_) => IpcErrorCode::Internal,
    };
    IpcResponse::Error {
        code,
        message: error.to_string(),
    }
}

fn worktree_switch_error(error: crate::WorktreeError) -> IpcResponse {
    use crate::WorktreeError;
    let code = match &error {
        WorktreeError::InvalidBranchName(_)
        | WorktreeError::BranchExists(_)
        | WorktreeError::UnknownBranch(_) => IpcErrorCode::InvalidRequest,
        WorktreeError::Dirty
        | WorktreeError::ConflictInProgress
        | WorktreeError::InvalidTransition { .. }
        | WorktreeError::PathMissing(_)
        | WorktreeError::NotFound(_)
        | WorktreeError::NotFoundId(_) => IpcErrorCode::Conflict,
        _ => IpcErrorCode::Internal,
    };
    IpcResponse::Error {
        code,
        message: error.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_pty_start(
    store: &Arc<dyn EventStore>,
    policy: &Arc<Mutex<PolicyEngine>>,
    pty: &Arc<crate::PtySessionManager>,
    session_id: uuid::Uuid,
    command: String,
    args: Vec<String>,
    working_dir: Option<PathBuf>,
    cols: u16,
    rows: u16,
) -> IpcResponse {
    let runtime = match AgentRuntime::attach(store.clone(), policy_snapshot(policy), session_id) {
        Ok(runtime) => runtime,
        Err(error) => return runtime_error(error),
    };
    let workspace_root = match runtime.workspace_root() {
        Ok(root) => root,
        Err(error) => return runtime_error(error),
    };
    let cwd = match crate::resolve_pty_working_dir(&workspace_root, working_dir) {
        Ok(path) => path,
        Err(error) => return pty_error(error),
    };
    // Login shells re-source profile scripts and can surprise-trigger password
    // prompts; refuse at harness admit (userspace policy — no elevation).
    if !crate::pty_argv_is_non_login(&args) {
        return IpcResponse::Error {
            code: IpcErrorCode::InvalidRequest,
            message: "PTY login-shell argv (-l/--login) is not allowed; use a non-login shell"
                .into(),
        };
    }
    match pty.start(
        session_id,
        command,
        args,
        cwd,
        crate::policy::ActionOrigin::User,
        cols,
        rows,
        1,
    ) {
        Ok(session) => {
            let working_dir = Some(session.working_dir.display().to_string());
            let _ = store.append_next(
                session_id,
                EventPayload::Pty(crate::PtyEvent::Started {
                    pty_id: session.id.0,
                    command: session.command.clone(),
                    working_dir,
                }),
            );
            pty_session_response(session)
        }
        Err(error) => pty_error(error),
    }
}

fn pty_session_response(session: crate::PtySession) -> IpcResponse {
    IpcResponse::PtySession {
        pty_id: session.id.0,
        owner_session_id: session.owner_session_id,
        state: session.state,
        command: session.command,
        cols: session.cols,
        rows: session.rows,
    }
}

fn pty_error(error: crate::PtySessionError) -> IpcResponse {
    use crate::PtySessionError;
    let code = match &error {
        PtySessionError::SessionNotFound(_) | PtySessionError::NotLive(_) => {
            IpcErrorCode::MissingSession
        }
        PtySessionError::NotOwner(_) => IpcErrorCode::Unavailable,
        PtySessionError::UnsafeWorkingDir(_) => IpcErrorCode::InvalidRequest,
        PtySessionError::AlreadyRunning(_) | PtySessionError::ApprovalRequired => {
            IpcErrorCode::Conflict
        }
        PtySessionError::PolicyDenied(_) | PtySessionError::SandboxDenied(_) => {
            IpcErrorCode::Unavailable
        }
        PtySessionError::SpawnFailed(_) | PtySessionError::Io(_) | PtySessionError::Storage(_) => {
            IpcErrorCode::Internal
        }
    };
    IpcResponse::Error {
        code,
        message: error.to_string(),
    }
}

fn pty_manager_with_default_artifacts(seam: crate::EffectSeam) -> Arc<crate::PtySessionManager> {
    let manager = crate::PtySessionManager::new(seam);
    match DurableArtifactStore::open(crate::default_artifact_root()) {
        Ok(store) => Arc::new(manager.with_artifacts(Arc::new(store))),
        Err(_) => Arc::new(manager),
    }
}

fn pty_notice(store: &Arc<dyn EventStore>, session_id: uuid::Uuid, message: String) {
    let _ = store.append_next(
        session_id,
        EventPayload::Notice(NoticeEvent::Runtime { message }),
    );
}

#[allow(clippy::result_large_err)]
fn get_or_default_session_model(
    session_models: &Arc<Mutex<HashMap<uuid::Uuid, crate::SessionModelSelection>>>,
    durable: Option<&Arc<crate::SessionModelStore>>,
    store: &Arc<dyn EventStore>,
    policy: &Arc<Mutex<PolicyEngine>>,
    provider_registry: &ProviderRegistry,
    default_provider_id: &str,
    session_id: uuid::Uuid,
) -> Result<crate::SessionModelSelection, IpcResponse> {
    let _ = AgentRuntime::attach(store.clone(), policy_snapshot(policy), session_id)
        .map_err(runtime_error)?;
    let existing = session_models
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&session_id)
        .cloned();
    // Guard must drop before re-lock on invalid (no if-let temporary hold).
    if let Some(existing) = existing {
        // Fail-closed: revalidate RAM hits (stale effort after catalog change).
        if let Err(err) = validate_session_model_selection(provider_registry, &existing) {
            session_models
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&session_id);
            return Err(err);
        }
        return Ok(existing);
    }
    if let Some(durable) = durable {
        match durable.load(session_id) {
            Ok(Some(saved)) => {
                validate_session_model_selection(provider_registry, &saved)?;
                session_models
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(session_id, saved.clone());
                return Ok(saved);
            }
            Ok(None) => {}
            Err(err) => {
                return Err(IpcResponse::Error {
                    code: IpcErrorCode::Internal,
                    message: format!(
                        "durable session model for {session_id} unreadable (fail-closed): {err}"
                    ),
                });
            }
        }
    }
    let provider = provider_registry
        .get(default_provider_id)
        .map_err(|e| IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: e.to_string(),
        })?;
    Ok(crate::SessionModelSelection {
        provider_id: default_provider_id.to_owned(),
        model_id: provider.model_id().to_owned(),
        // Honest: do not invent a global default effort (e.g. "medium").
        reasoning_effort: None,
    })
}

#[allow(clippy::result_large_err)]
fn validate_session_model_selection(
    provider_registry: &ProviderRegistry,
    selection: &crate::SessionModelSelection,
) -> Result<(), IpcResponse> {
    let _provider =
        provider_registry
            .get(&selection.provider_id)
            .map_err(|e| IpcResponse::Error {
                code: IpcErrorCode::Unavailable,
                message: format!(
                    "saved session model provider `{}` unavailable: {e}",
                    selection.provider_id
                ),
            })?;
    let (advertised, _) = {
        let registry = provider_registry.clone();
        let pid = selection.provider_id.clone();
        let mid = selection.model_id.clone();
        crate::block_on_coding_tools(async move {
            registry.advertised_reasoning_efforts(&pid, &mid).await
        })
    }
    .map_err(|e| IpcResponse::Error {
        code: IpcErrorCode::Unavailable,
        message: format!(
            "saved session model `{}/{}` unavailable: {e}",
            selection.provider_id, selection.model_id
        ),
    })?;
    if let Some(effort) = selection.reasoning_effort.as_deref() {
        if advertised.is_empty() {
            return Err(IpcResponse::Error {
                code: IpcErrorCode::InvalidRequest,
                message: format!(
                    "saved reasoning_effort `{effort}` rejected: model `{}` no longer advertises reasoning efforts",
                    selection.model_id
                ),
            });
        }
        if !advertised.iter().any(|a| a == effort) {
            return Err(IpcResponse::Error {
                code: IpcErrorCode::InvalidRequest,
                message: format!(
                    "saved reasoning_effort `{effort}` no longer advertised for `{}` (advertised: {})",
                    selection.model_id,
                    advertised.join(", ")
                ),
            });
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::result_large_err)]
fn store_session_model(
    session_models: &Arc<Mutex<HashMap<uuid::Uuid, crate::SessionModelSelection>>>,
    durable: Option<&Arc<crate::SessionModelStore>>,
    store: &Arc<dyn EventStore>,
    policy: &Arc<Mutex<PolicyEngine>>,
    provider_registry: &ProviderRegistry,
    session_id: uuid::Uuid,
    provider_id: String,
    model_id: String,
    reasoning_effort: Option<String>,
) -> Result<crate::SessionModelSelection, IpcResponse> {
    let _ = AgentRuntime::attach(store.clone(), policy_snapshot(policy), session_id)
        .map_err(runtime_error)?;
    if provider_id.is_empty() || model_id.is_empty() {
        return Err(IpcResponse::Error {
            code: IpcErrorCode::InvalidRequest,
            message: "provider_id and model_id are required".into(),
        });
    }
    let _provider = provider_registry
        .get(&provider_id)
        .map_err(|e| IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: e.to_string(),
        })?;
    let (advertised, _default_effort) = {
        let registry = provider_registry.clone();
        let pid = provider_id.clone();
        let mid = model_id.clone();
        crate::block_on_coding_tools(async move {
            registry.advertised_reasoning_efforts(&pid, &mid).await
        })
    }
    .map_err(|e| IpcResponse::Error {
        code: IpcErrorCode::Unavailable,
        message: e.to_string(),
    })?;
    if let Some(effort) = reasoning_effort.as_deref() {
        if advertised.is_empty() {
            return Err(IpcResponse::Error {
                code: IpcErrorCode::InvalidRequest,
                message: format!(
                    "reasoning_effort `{effort}` rejected: model `{model_id}` does not advertise reasoning efforts"
                ),
            });
        }
        if !advertised.iter().any(|a| a == effort) {
            return Err(IpcResponse::Error {
                code: IpcErrorCode::InvalidRequest,
                message: format!(
                    "unsupported reasoning_effort `{effort}` (advertised: {})",
                    advertised.join(", ")
                ),
            });
        }
    }
    let selection = crate::SessionModelSelection {
        provider_id,
        model_id,
        reasoning_effort,
    };
    if let Some(durable) = durable {
        durable
            .save(session_id, &selection)
            .map_err(|e| IpcResponse::Error {
                code: IpcErrorCode::Internal,
                message: format!("persist session model: {e}"),
            })?;
    }
    session_models
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(session_id, selection.clone());
    let _ = store.append_next(
        session_id,
        EventPayload::Notice(NoticeEvent::Runtime {
            message: format!(
                "session model set to {}/{} (effort={})",
                selection.provider_id,
                selection.model_id,
                selection.reasoning_effort.as_deref().unwrap_or("default")
            ),
        }),
    );
    Ok(selection)
}

fn worktree_unavailable() -> IpcResponse {
    IpcResponse::Error {
        code: IpcErrorCode::Unavailable,
        message: "WorktreeManager not wired on this harness".into(),
    }
}

fn handle_create_worktree(
    worktrees: Option<&crate::WorktreeManager>,
    store: &Arc<dyn EventStore>,
    policy: &Arc<Mutex<PolicyEngine>>,
    session_id: uuid::Uuid,
    for_build: bool,
) -> IpcResponse {
    let Some(mgr) = worktrees else {
        return worktree_unavailable();
    };
    let runtime = match AgentRuntime::attach(store.clone(), policy_snapshot(policy), session_id) {
        Ok(r) => r,
        Err(e) => return runtime_error(e),
    };
    let repo_root = match runtime.workspace_root() {
        Ok(r) => r,
        Err(e) => return runtime_error(e),
    };
    let result = if for_build {
        mgr.create_for_role(session_id, &repo_root, crate::AgentWorkRole::Build)
    } else {
        mgr.create(session_id, &repo_root)
    };
    match result {
        Ok(binding) => IpcResponse::Worktree {
            worktree: binding.to_info(),
        },
        Err(e) => worktree_switch_error(e),
    }
}

fn handle_list_worktrees(
    worktrees: Option<&crate::WorktreeManager>,
    session_id: Option<uuid::Uuid>,
) -> IpcResponse {
    let Some(mgr) = worktrees else {
        return worktree_unavailable();
    };
    let list = match session_id {
        Some(id) => match mgr.get_by_session(id) {
            Ok(Some(b)) => vec![b.to_info()],
            Ok(None) => Vec::new(),
            Err(e) => return worktree_switch_error(e),
        },
        None => match mgr.list_catalog() {
            Ok(bindings) => bindings.into_iter().map(|b| b.to_info()).collect(),
            Err(e) => return worktree_switch_error(e),
        },
    };
    IpcResponse::Worktrees { worktrees: list }
}

fn handle_get_worktree(
    worktrees: Option<&crate::WorktreeManager>,
    worktree_id: &str,
) -> IpcResponse {
    let Some(mgr) = worktrees else {
        return worktree_unavailable();
    };
    match mgr.get_by_worktree_id(worktree_id) {
        Ok(Some(b)) => IpcResponse::Worktree {
            worktree: b.to_info(),
        },
        Ok(None) => IpcResponse::Error {
            code: IpcErrorCode::MissingSession,
            message: format!("worktree not found: {worktree_id}"),
        },
        Err(e) => worktree_switch_error(e),
    }
}

fn handle_close_worktree(
    worktrees: Option<&crate::WorktreeManager>,
    worktree_id: &str,
) -> IpcResponse {
    let Some(mgr) = worktrees else {
        return worktree_unavailable();
    };
    match mgr.get_by_worktree_id(worktree_id) {
        Ok(Some(binding)) => match mgr.close(binding.session_id) {
            Ok(b) => IpcResponse::Worktree {
                worktree: b.to_info(),
            },
            Err(e) => worktree_switch_error(e),
        },
        Ok(None) => IpcResponse::Error {
            code: IpcErrorCode::MissingSession,
            message: format!("worktree not found: {worktree_id}"),
        },
        Err(e) => worktree_switch_error(e),
    }
}

fn handle_resume_worktree(
    worktrees: Option<&crate::WorktreeManager>,
    session_id: uuid::Uuid,
) -> IpcResponse {
    let Some(mgr) = worktrees else {
        return worktree_unavailable();
    };
    match mgr.resume(session_id) {
        Ok(b) => IpcResponse::Worktree {
            worktree: b.to_info(),
        },
        Err(e) => worktree_switch_error(e),
    }
}

fn handle_stop_worktree(
    worktrees: Option<&crate::WorktreeManager>,
    session_id: uuid::Uuid,
) -> IpcResponse {
    let Some(mgr) = worktrees else {
        return worktree_unavailable();
    };
    match mgr.stop(session_id) {
        Ok(b) => IpcResponse::Worktree {
            worktree: b.to_info(),
        },
        Err(e) => worktree_switch_error(e),
    }
}

fn handle_check_merge_ready(
    worktrees: Option<&crate::WorktreeManager>,
    session_id: uuid::Uuid,
    base_ref: &str,
) -> IpcResponse {
    let Some(mgr) = worktrees else {
        return worktree_unavailable();
    };
    match mgr.check_merge_ready(session_id, base_ref) {
        Ok(report) => IpcResponse::WorktreeMergeReady { report },
        Err(e) => worktree_switch_error(e),
    }
}

fn handle_merge_worktree(
    worktrees: Option<&crate::WorktreeManager>,
    session_id: uuid::Uuid,
    base_ref: &str,
) -> IpcResponse {
    let Some(mgr) = worktrees else {
        return worktree_unavailable();
    };
    match mgr.attempt_merge(session_id, base_ref) {
        Ok(b) => IpcResponse::WorktreeMerged {
            worktree: b.to_info(),
        },
        Err(e) => worktree_switch_error(e),
    }
}

fn pty_emit(store: &Arc<dyn EventStore>, session_id: uuid::Uuid, event: crate::PtyEvent) {
    let _ = store.append_next(session_id, EventPayload::Pty(event));
}

fn decode_pty_b64(data_b64: &str) -> Result<Vec<u8>, String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    BASE64
        .decode(data_b64.as_bytes())
        .map_err(|_| "invalid base64 pty input".into())
}

fn encode_pty_b64(data: &[u8]) -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    BASE64.encode(data)
}

#[allow(clippy::result_large_err)]
fn open_artifact_store(root: &Path) -> Result<DurableArtifactStore, IpcResponse> {
    DurableArtifactStore::open(root).map_err(|error| IpcResponse::Error {
        code: IpcErrorCode::Internal,
        message: format!("artifact store unavailable: {error}"),
    })
}

fn read_durable_artifact(root: &Path, artifact_id: &str, max_bytes: Option<usize>) -> IpcResponse {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

    let store = match open_artifact_store(root) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let Some(meta) = (match store.metadata(artifact_id) {
        Ok(meta) => meta,
        Err(error) => {
            return IpcResponse::Error {
                code: IpcErrorCode::Internal,
                message: format!("artifact metadata failed: {error}"),
            };
        }
    }) else {
        return IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: format!("artifact {artifact_id} not found"),
        };
    };

    let cap = max_bytes
        .unwrap_or(crate::MAX_ARTIFACT_UPLOAD_CHUNK_BYTES)
        .min(crate::MAX_ARTIFACT_UPLOAD_CHUNK_BYTES);
    let bytes = match store.read_range(artifact_id, 0, cap) {
        Ok(bytes) => bytes,
        Err(error) => {
            return IpcResponse::Error {
                code: IpcErrorCode::Internal,
                message: format!("artifact read failed: {error}"),
            };
        }
    };
    let truncated = bytes.len() < meta.byte_count;
    IpcResponse::ArtifactContent {
        artifact_id: artifact_id.to_owned(),
        content_type: meta.content_type,
        byte_count: meta.byte_count,
        returned_bytes: bytes.len(),
        data_b64: BASE64.encode(&bytes),
        truncated,
    }
}

fn get_durable_artifact_metadata(root: &Path, artifact_id: &str) -> IpcResponse {
    let store = match open_artifact_store(root) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match store.metadata(artifact_id) {
        Ok(Some(meta)) => IpcResponse::ArtifactMetadata { meta },
        Ok(None) => IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: format!("artifact {artifact_id} not found"),
        },
        Err(error) => IpcResponse::Error {
            code: IpcErrorCode::Internal,
            message: format!("artifact metadata failed: {error}"),
        },
    }
}

fn read_durable_artifact_range(
    root: &Path,
    artifact_id: &str,
    start: usize,
    len: usize,
) -> IpcResponse {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

    let store = match open_artifact_store(root) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let Some(meta) = (match store.metadata(artifact_id) {
        Ok(meta) => meta,
        Err(error) => {
            return IpcResponse::Error {
                code: IpcErrorCode::Internal,
                message: format!("artifact metadata failed: {error}"),
            };
        }
    }) else {
        return IpcResponse::Error {
            code: IpcErrorCode::Unavailable,
            message: format!("artifact {artifact_id} not found"),
        };
    };

    let want = len.min(crate::MAX_ARTIFACT_UPLOAD_CHUNK_BYTES);
    let bytes = match store.read_range(artifact_id, start, want) {
        Ok(bytes) => bytes,
        Err(error) => {
            return IpcResponse::Error {
                code: IpcErrorCode::Internal,
                message: format!("artifact range read failed: {error}"),
            };
        }
    };
    let end = start.saturating_add(bytes.len());
    let truncated = end < meta.byte_count;
    IpcResponse::ArtifactRange {
        artifact_id: artifact_id.to_owned(),
        start,
        returned_bytes: bytes.len(),
        data_b64: BASE64.encode(&bytes),
        truncated,
    }
}

fn handle_workspace_files<F>(
    store: Arc<dyn EventStore>,
    policy: Arc<Mutex<PolicyEngine>>,
    session_id: uuid::Uuid,
    path: &Path,
    summary: &str,
    op: F,
) -> IpcResponse
where
    F: FnOnce(&Path, &Path) -> Result<IpcResponse, crate::workspace_files::WorkspaceFilesError>,
{
    let relative = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    let target = relative.display().to_string();
    match AgentRuntime::attach(store, policy_snapshot(&policy), session_id) {
        Ok(runtime) => {
            let workspace_root = match runtime.workspace_root() {
                Ok(root) => root,
                Err(error) => return runtime_error(error),
            };
            let seam = match runtime.effect_seam() {
                Ok(seam) => seam,
                Err(error) => return runtime_error(error),
            };
            let effect =
                crate::NormalizedEffect::workspace_read(ActionOrigin::User, summary, target);
            match seam.execute(&effect, || op(&workspace_root, relative)) {
                Ok(crate::EffectExecution::Executed(response)) => response,
                Ok(crate::EffectExecution::Denied { reason }) => IpcResponse::Error {
                    code: IpcErrorCode::Unavailable,
                    message: reason,
                },
                Ok(crate::EffectExecution::NeedsApproval { .. }) => IpcResponse::Error {
                    code: IpcErrorCode::Unavailable,
                    message: "workspace files read needs an approval that is not granted in read-only scope"
                        .into(),
                },
                Err(error) => workspace_files_error(error),
            }
        }
        Err(error) => runtime_error(error),
    }
}

fn workspace_files_error(error: crate::workspace_files::WorkspaceFilesError) -> IpcResponse {
    use crate::workspace_files::WorkspaceFilesError;
    let code = match &error {
        WorkspaceFilesError::UnsafePath(_)
        | WorkspaceFilesError::NotFound(_)
        | WorkspaceFilesError::NotDirectory(_)
        | WorkspaceFilesError::NotFile(_)
        | WorkspaceFilesError::TooLarge { .. }
        | WorkspaceFilesError::Binary(_) => IpcErrorCode::InvalidRequest,
        WorkspaceFilesError::Io(_) => IpcErrorCode::Internal,
    };
    IpcResponse::Error {
        code,
        message: error.to_string(),
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
    stream_options: crate::StreamOptions,
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
            stream_options,
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
    session_models: Arc<Mutex<HashMap<uuid::Uuid, crate::SessionModelSelection>>>,
    session_model_store: Option<Arc<crate::SessionModelStore>>,
    memory: Option<Arc<crate::SessionMemoryRuntime>>,
    extension_host: Option<Arc<Mutex<crate::ExtensionHost>>>,
) -> Result<(), RuntimeError> {
    let runtime_session_id = runtime.session_id();
    let session_override = session_models
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&runtime_session_id)
        .cloned();

    let requirements = crate::model_router::CapabilityRequirements {
        tools: true,
        ..Default::default()
    };
    let budget = runtime.budget_config().unwrap_or_default();
    let selected = model_router.select_model(&requirements, &budget);

    let (selected_provider_id, stream_options, selection_message) =
        if let Some(ref override_sel) = session_override {
            let opts = crate::StreamOptions {
                model_id: Some(override_sel.model_id.clone()),
                reasoning_effort: override_sel.reasoning_effort.clone(),
            };
            (
                override_sel.provider_id.clone(),
                opts,
                format!(
                    "session model override {}/{} (effort={})",
                    override_sel.provider_id,
                    override_sel.model_id,
                    override_sel
                        .reasoning_effort
                        .as_deref()
                        .unwrap_or("default")
                ),
            )
        } else {
            let selected_provider_id = selected
                .as_ref()
                .map(|s| s.provider_id.clone())
                .unwrap_or_else(|| default_provider_id.to_owned());
            let message = if let Some(ref selection) = selected {
                format!(
                    "ModelRouter selected {}/{}: {}",
                    selection.provider_id, selection.model_id, selection.reasoning
                )
            } else {
                format!("ModelRouter fallback to default provider: {selected_provider_id}")
            };
            let opts = crate::StreamOptions {
                model_id: selected.as_ref().map(|s| s.model_id.clone()),
                reasoning_effort: None,
            };
            (selected_provider_id, opts, message)
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
    let task_session_models = session_models;
    let task_session_model_store = session_model_store;
    let task_stream_options = stream_options;
    let task_memory = memory;
    let task_extension_host = extension_host;

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
            task_stream_options,
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
                task_session_models,
                task_session_model_store,
                task_memory,
                task_extension_host,
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
    session_models: Arc<Mutex<HashMap<uuid::Uuid, crate::SessionModelSelection>>>,
    session_model_store: Option<Arc<crate::SessionModelStore>>,
    memory: Option<Arc<crate::SessionMemoryRuntime>>,
    extension_host: Option<Arc<Mutex<crate::ExtensionHost>>>,
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
    let mut provider_messages = resolve_provider_messages(
        &session_workspace,
        &runtime,
        Some(&artifact_root),
        policy_store.as_deref(),
        &extension_skill_roots(&extension_host),
    )
    .unwrap_or_else(|_| {
        vec![ProviderMessage::user(
            runtime_intent(&runtime).unwrap_or_default(),
        )]
    });
    let memory_block = memory
        .as_ref()
        .and_then(|rt| rt.prompt_context_block(session_id));
    crate::inject_memory_context(&mut provider_messages, memory_block.as_deref());
    // Same fail-closed gate as Prompt before launching drained FollowUp.
    let policy_arc = Arc::new(Mutex::new(policy.clone()));
    if get_or_default_session_model(
        &session_models,
        session_model_store.as_ref(),
        &store,
        &policy_arc,
        &provider_registry,
        &default_provider_id,
        session_id,
    )
    .is_err()
    {
        return;
    }
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
        session_models,
        session_model_store,
        memory,
        extension_host,
    );
}

fn extension_skill_roots(
    extension_host: &Option<Arc<Mutex<crate::ExtensionHost>>>,
) -> Vec<PathBuf> {
    let Some(slot) = extension_host else {
        return Vec::new();
    };
    let guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.capability_registry().skill_roots
}

fn resolve_context(
    workspace_root: &std::path::Path,
    policy_store: Option<&crate::PolicyStore>,
    extra_skill_roots: &[PathBuf],
) -> anyhow::Result<crate::ResolvedInstructions> {
    let mut resolver = InstructionResolver::new(workspace_root);
    resolver.set_extra_skill_roots(extra_skill_roots.iter().cloned());
    let resolved = resolver.resolve(&ResolveRequest::default())?;
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
    extra_skill_roots: &[PathBuf],
) -> anyhow::Result<Vec<ProviderMessage>> {
    resolve_provider_messages_with_binding(
        workspace_root,
        runtime,
        &Profile::Standard.default_bindings().context,
        DEFAULT_CONTEXT_BUDGET_TOKENS,
        artifact_root,
        policy_store,
        extra_skill_roots,
    )
}

fn resolve_provider_messages_with_binding(
    workspace_root: &std::path::Path,
    runtime: &AgentRuntime,
    context_binding: &crate::ServiceBinding,
    budget_tokens: usize,
    artifact_root: Option<&std::path::Path>,
    policy_store: Option<&crate::PolicyStore>,
    extra_skill_roots: &[PathBuf],
) -> anyhow::Result<Vec<ProviderMessage>> {
    let instructions = resolve_context(workspace_root, policy_store, extra_skill_roots)?;
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
            crate::EventPayload::Agent(crate::AgentEvent::Chunk { text, artifact, .. }) => {
                pending_assistant.push_str(&materialize_agent_chunk_text(
                    artifact_store.as_ref(),
                    &text,
                    artifact.as_ref(),
                    artifact_budget,
                ));
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

/// When a chunk carries an artifact, expand the bounded preview to budgeted
/// body text (same path as tool/intent materialization).
fn materialize_agent_chunk_text(
    store: Option<&DurableArtifactStore>,
    preview: &str,
    artifact: Option<&crate::DurableArtifactRef>,
    budget: TokenBudget,
) -> String {
    let Some(artifact) = artifact else {
        return preview.to_string();
    };
    let Some(store) = store else {
        return preview.to_string();
    };
    match ContextBuilder::new(store, budget).materialize(artifact) {
        Ok(materialized) => materialized.content,
        Err(_) => preview.to_string(),
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
///
/// When a deferred write tool carries proposed `content`, builds a real
/// [`crate::DiffObservation`] (before = on-disk, after = proposed). Without
/// proposed content, does not invent a delete-only fake preview; may fall back
/// to `git diff HEAD -- <path>` when the workspace has uncommitted changes.
fn compute_approval_detail(
    request: crate::ApprovalRequest,
    workspace_root: &Path,
    attachments: &crate::AttachmentStore,
    deferred: Option<&(String, String, serde_json::Value)>,
) -> Result<crate::ApprovalDetail, RuntimeError> {
    use crate::{
        ActionKind, MAX_UNIFIED_PREVIEW_LINES, ScopeEstimate, from_git_diff, from_texts,
        unified_preview,
    };

    let mut affected_files = vec![];
    let mut diff_preview = None;
    let mut diff_observation = None;
    let mut estimated_scope = None;
    let mut attachment_refs = vec![];

    match &request.action.kind {
        ActionKind::WriteFile => {
            if let Some(target) = &request.action.target {
                affected_files.push(target.clone());

                let target_path = if Path::new(target).is_absolute() {
                    PathBuf::from(target)
                } else {
                    workspace_root.join(target)
                };

                let before = if target_path.exists() {
                    std::fs::read_to_string(&target_path).unwrap_or_default()
                } else {
                    String::new()
                };
                if !before.is_empty() {
                    estimated_scope = Some(ScopeEstimate::Lines(before.lines().count() as u32));
                }

                let proposed = deferred.and_then(|(_, tool_name, args)| {
                    if matches!(tool_name.as_str(), "write_file" | "edit_file") {
                        args.get("content")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    } else {
                        None
                    }
                });

                if let Some(after) = proposed {
                    let obs = from_texts(target.as_str(), &before, &after);
                    estimated_scope = Some(ScopeEstimate::Lines(
                        (obs.insertions + obs.deletions).max(1) as u32,
                    ));
                    let preview = unified_preview(&obs, MAX_UNIFIED_PREVIEW_LINES);
                    if preview.len() < 1_000_000
                        && let Ok(attachment_id) = attachments
                            .store("text/x-diff".to_string(), preview.as_bytes().to_vec())
                    {
                        attachment_refs.push(attachment_id);
                    }
                    diff_preview = Some(preview);
                    diff_observation = Some(obs);
                } else if let Some(obs) =
                    from_git_diff(workspace_root, &["HEAD", "--", target.as_str()])
                {
                    let preview = unified_preview(&obs, MAX_UNIFIED_PREVIEW_LINES);
                    diff_preview = Some(preview);
                    diff_observation = Some(obs);
                }
            }
        }
        ActionKind::ReadFile => {
            if let Some(target) = &request.action.target {
                affected_files.push(target.clone());
            }
        }
        ActionKind::SpawnProcess => {
            if let Some(cmd) = &request.action.target {
                estimated_scope = Some(ScopeEstimate::Operations(1));
                if let Ok(attachment_id) =
                    attachments.store("text/plain".to_string(), cmd.as_bytes().to_vec())
                {
                    attachment_refs.push(attachment_id);
                }
            }
        }
        ActionKind::NetworkConnect | ActionKind::SshConnect | ActionKind::SftpTransfer => {
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
        diff_observation,
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
            resolve_provider_messages_with_binding(workspace, &runtime, &lazy, 20, None, None, &[])
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
        let _ =
            resolve_provider_messages_with_binding(workspace, &runtime, &lazy, 20, None, None, &[]);
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
    async fn resolve_approval_bound_to_owner_connection() {
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let harness = Harness::new(store.clone(), policy.clone());
        let conn_a = harness.mint_connection_id();
        let conn_b = harness.mint_connection_id();
        assert_ne!(conn_a, conn_b);

        let IpcResponse::Session { session_id, .. } = harness.handle_with_connection(
            Some(conn_a),
            IpcRequest::CreateSession {
                workspace_root: std::env::current_dir()
                    .expect("workspace")
                    .canonicalize()
                    .expect("canonical workspace"),
            },
        ) else {
            panic!("session creation response");
        };

        let runtime = AgentRuntime::attach(store.clone(), policy.clone(), session_id)
            .expect("attach to created session");
        runtime
            .submit_intent("write a test file")
            .expect("submit intent");
        runtime
            .request_action(crate::Action {
                origin: crate::ActionOrigin::Agent,
                kind: crate::ActionKind::WriteFile,
                summary: "write file".into(),
                target: Some("test.txt".into()),
            })
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

        let foreign = harness.handle_with_connection(
            Some(conn_b),
            IpcRequest::ResolveApproval {
                session_id,
                approval_id,
                accepted: true,
            },
        );
        assert!(
            matches!(
                foreign,
                IpcResponse::Error {
                    code: IpcErrorCode::Unavailable,
                    ..
                }
            ),
            "foreign connection must not resolve: {foreign:?}"
        );
        assert!(
            runtime
                .pending_approval(approval_id)
                .expect("check pending")
                .is_some(),
            "approval must still be pending after foreign resolve"
        );

        let IpcResponse::ApprovalResolved {
            approval_id: resolved_id,
            ..
        } = harness.handle_with_connection(
            Some(conn_a),
            IpcRequest::ResolveApproval {
                session_id,
                approval_id,
                accepted: true,
            },
        )
        else {
            panic!("owner connection must resolve approval");
        };
        assert_eq!(resolved_id, approval_id);
        assert!(
            runtime
                .pending_approval(approval_id)
                .expect("check pending")
                .is_none(),
            "owner resolve must clear pending approval"
        );
    }

    #[tokio::test]
    async fn attach_does_not_steal_approval_owner() {
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let harness = Harness::new(store.clone(), policy.clone());
        let conn_a = harness.mint_connection_id();
        let conn_b = harness.mint_connection_id();
        assert_ne!(conn_a, conn_b);

        let IpcResponse::Session { session_id, .. } = harness.handle_with_connection(
            Some(conn_a),
            IpcRequest::CreateSession {
                workspace_root: std::env::current_dir()
                    .expect("workspace")
                    .canonicalize()
                    .expect("canonical workspace"),
            },
        ) else {
            panic!("session creation response");
        };

        let runtime = AgentRuntime::attach(store.clone(), policy.clone(), session_id)
            .expect("attach to created session");
        runtime
            .submit_intent("write a test file")
            .expect("submit intent");
        runtime
            .request_action(crate::Action {
                origin: crate::ActionOrigin::Agent,
                kind: crate::ActionKind::WriteFile,
                summary: "write file".into(),
                target: Some("test.txt".into()),
            })
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

        // Foreign Attach succeeds for session visibility but must not rebind owner.
        assert!(
            matches!(
                harness.handle_with_connection(Some(conn_b), IpcRequest::Attach { session_id },),
                IpcResponse::Session { .. }
            ),
            "foreign Attach must still succeed"
        );

        let foreign = harness.handle_with_connection(
            Some(conn_b),
            IpcRequest::ResolveApproval {
                session_id,
                approval_id,
                accepted: true,
            },
        );
        assert!(
            matches!(
                foreign,
                IpcResponse::Error {
                    code: IpcErrorCode::Unavailable,
                    ..
                }
            ),
            "Attach must not steal ResolveApproval: {foreign:?}"
        );
        assert!(
            runtime
                .pending_approval(approval_id)
                .expect("check pending")
                .is_some(),
            "approval must still be pending after foreign Attach+Resolve"
        );

        let IpcResponse::ApprovalResolved {
            approval_id: resolved_id,
            ..
        } = harness.handle_with_connection(
            Some(conn_a),
            IpcRequest::ResolveApproval {
                session_id,
                approval_id,
                accepted: true,
            },
        )
        else {
            panic!("original owner must still resolve after foreign Attach");
        };
        assert_eq!(resolved_id, approval_id);
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
        assert_eq!(detail.affected_files, vec!["test.txt"]);
        // No deferred write content → no fake delete-only preview.
        assert!(detail.diff_preview.is_none());
        assert!(detail.diff_observation.is_none());
    }

    #[test]
    fn approval_detail_uses_the_session_workspace() {
        let root = tempfile::tempdir().expect("root");
        let daemon_workspace = root.path().join("daemon");
        let session_workspace = root.path().join("session");
        std::fs::create_dir(&daemon_workspace).expect("daemon workspace");
        std::fs::create_dir(&session_workspace).expect("session workspace");
        std::fs::write(session_workspace.join("existing.txt"), "session content\n")
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
        runtime
            .record_deferred_tool(
                approval_id,
                "write-1".into(),
                "write_file".into(),
                serde_json::json!({
                    "path": "existing.txt",
                    "content": "session content\nproposed line\n"
                }),
            )
            .expect("deferred write");

        let IpcResponse::ApprovalDetail { detail, .. } =
            harness.handle(IpcRequest::GetApprovalDetail {
                session_id,
                approval_id,
            })
        else {
            panic!("approval detail")
        };
        let preview = detail.diff_preview.expect("real proposed diff");
        assert!(preview.contains("session content") || preview.contains("+proposed"));
        assert!(preview.contains('+'), "expected addition in {preview}");
        let obs = detail.diff_observation.expect("structured observation");
        assert_eq!(obs.files_changed, 1);
        assert!(obs.insertions >= 1);
        assert!(!obs.hunks.is_empty());
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
        assert_eq!(modules["extension_runtime"]["level"], "PARTIAL");
        assert_eq!(
            modules["extension_runtime"]["details"]["mcp_live_tools_in_loop"],
            true
        );
        assert_eq!(
            modules["extension_runtime"]["details"]["impetusd_autoload"],
            true
        );
        assert_eq!(
            modules["extension_runtime"]["details"]["harness_inject"],
            true
        );
        assert_eq!(
            modules["extension_runtime"]["details"]["agent_loop_skill_inject"],
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
        let meta = durable.metadata(&artifact.id).unwrap().unwrap();
        assert_eq!(meta.content_type.as_deref(), Some("text/plain"));

        let IpcResponse::ArtifactMetadata { meta: ipc_meta } =
            harness.handle(IpcRequest::GetArtifactMetadata {
                artifact_id: artifact.id.clone(),
            })
        else {
            panic!("get artifact metadata");
        };
        assert_eq!(ipc_meta.content_type.as_deref(), Some("text/plain"));
        assert_eq!(ipc_meta.byte_count, body.len());

        let IpcResponse::ArtifactContent {
            data_b64,
            content_type,
            truncated,
            returned_bytes,
            ..
        } = harness.handle(IpcRequest::ReadArtifact {
            artifact_id: artifact.id.clone(),
            max_bytes: None,
        })
        else {
            panic!("read artifact");
        };
        assert_eq!(content_type.as_deref(), Some("text/plain"));
        assert!(!truncated);
        assert_eq!(returned_bytes, body.len());
        assert_eq!(BASE64.decode(data_b64.as_bytes()).unwrap(), body);

        let IpcResponse::ArtifactRange {
            data_b64: range_b64,
            returned_bytes: range_len,
            truncated: range_trunc,
            ..
        } = harness.handle(IpcRequest::ReadArtifactRange {
            artifact_id: artifact.id.clone(),
            start: mid,
            len: body.len(),
        })
        else {
            panic!("read artifact range");
        };
        assert_eq!(range_len, body.len() - mid);
        assert!(!range_trunc);
        assert_eq!(BASE64.decode(range_b64.as_bytes()).unwrap(), &body[mid..]);

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

    #[test]
    fn stream_paginates_large_chunk_batches_under_ipc_line_cap() {
        use crate::{
            AgentEvent, EventPayload, IPC_EVENTS_FRAME_BUDGET, MAX_AGENT_CHUNK_EVENT_BYTES,
            MAX_IPC_LINE_BYTES, MemoryEventStore,
        };

        let store = Arc::new(MemoryEventStore::default());
        let session_id = store.create_session().expect("session");
        let run_id = uuid::Uuid::nil();
        let body = "z".repeat(MAX_AGENT_CHUNK_EVENT_BYTES);
        for chunk_id in 1..=8u64 {
            store
                .append_next(
                    session_id,
                    EventPayload::Agent(AgentEvent::Chunk {
                        run_id,
                        chunk_id,
                        text: body.clone(),
                        artifact: None,
                    }),
                )
                .expect("append chunk");
        }

        let harness = Harness::new(store, PolicyEngine::new(SandboxScope::local_workspace(".")));
        let mut after_sequence = 0u64;
        let mut seen = 0usize;
        let mut pages = 0usize;
        loop {
            let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
                session_id,
                after_sequence,
            }) else {
                panic!("expected Events");
            };
            if events.is_empty() {
                break;
            }
            pages += 1;
            seen += events.len();
            let frame = serde_json::to_vec(&IpcResponse::Events {
                session_id,
                events: events.clone(),
            })
            .expect("encode");
            assert!(
                frame.len() <= IPC_EVENTS_FRAME_BUDGET,
                "page {pages} frame {} exceeds soft budget",
                frame.len()
            );
            assert!(frame.len() <= MAX_IPC_LINE_BYTES);
            after_sequence = events.last().expect("non-empty").sequence;
        }
        assert!(
            pages > 1,
            "expected pagination across multiple Stream pages"
        );
        // Created + 8 chunks
        assert_eq!(seen, 9);
        // Cursor resume: list_after still returns full unbounded tail for store API.
        let remaining = harness
            .store()
            .list_after(session_id, after_sequence, usize::MAX)
            .expect("list_after");
        assert!(remaining.is_empty());
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

    #[test]
    fn coding_diagnostics_symbols_cancel_ipc_use_mock_provider() {
        let workspace = tempfile::tempdir().expect("workspace");
        let path = std::path::PathBuf::from("src/lib.rs");
        let diag = crate::CodingDiagnostic {
            path: path.clone(),
            range: crate::SourceRange::new(1, 0, 1, 3),
            severity: crate::DiagnosticSeverity::Error,
            message: "boom".into(),
            code: Some("E0001".into()),
        };
        let sym = crate::DocumentSymbol {
            name: "main".into(),
            kind: crate::SymbolKind::Function,
            location: crate::SourceLocation::new(&path, crate::SourceRange::new(0, 0, 0, 4)),
            container_name: None,
        };
        let mock = Arc::new(
            crate::MockCodingToolsProvider::new()
                .with_diagnostics(&path, vec![diag.clone()])
                .with_symbols(&path, vec![sym.clone()])
                .with_cancelable(42),
        );
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        )
        .with_coding_tools(Arc::new(crate::OptionalCodingToolsService::with_provider(
            mock,
        )));

        match harness.handle(IpcRequest::CodingDiagnostics { path: path.clone() }) {
            IpcResponse::CodingDiagnostics { diagnostics } => {
                assert_eq!(diagnostics, vec![diag]);
            }
            other => panic!("expected CodingDiagnostics, got {other:?}"),
        }
        match harness.handle(IpcRequest::CodingSymbols { path }) {
            IpcResponse::CodingSymbols { symbols } => {
                assert_eq!(symbols, vec![sym]);
            }
            other => panic!("expected CodingSymbols, got {other:?}"),
        }
        match harness.handle(IpcRequest::CancelCodingRequest { request_id: 42 }) {
            IpcResponse::CodingCancelAccepted { request_id } => assert_eq!(request_id, 42),
            other => panic!("expected CodingCancelAccepted, got {other:?}"),
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
            min_version: Some(IPC_MIN_SUPPORTED),
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
    fn hello_negotiates_overlap_not_always_latest() {
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(
                tempfile::tempdir().expect("workspace").path(),
            )),
        );
        // Client max=12, min=12 → select 12 even though server also speaks 13.
        let IpcResponse::Hello { version, .. } = harness.handle(IpcRequest::Hello {
            version: 12,
            min_version: Some(12),
            capabilities: vec!["session_create".into()],
        }) else {
            panic!("expected Hello for overlapping v12");
        };
        assert_eq!(version, 12);

        // Client only speaks 11 → incompatible.
        let resp = harness.handle(IpcRequest::Hello {
            version: 11,
            min_version: Some(11),
            capabilities: vec![],
        });
        assert!(
            matches!(
                resp,
                IpcResponse::Incompatible {
                    client_version: 11,
                    ..
                }
            ),
            "{resp:?}"
        );

        // Legacy exact-version client on 13 (no min_version) → 13.
        let IpcResponse::Hello { version, .. } = harness.handle(IpcRequest::Hello {
            version: IPC_VERSION,
            min_version: None,
            capabilities: vec![],
        }) else {
            panic!("legacy hello");
        };
        assert_eq!(version, IPC_VERSION);
    }

    #[test]
    fn list_mcp_servers_empty_without_runtime() {
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(
                tempfile::tempdir().expect("workspace").path(),
            )),
        );
        let response = harness.handle(IpcRequest::ListMcpServers);
        match response {
            IpcResponse::McpServers { servers } => assert!(servers.is_empty()),
            other => panic!("expected empty McpServers, got {other:?}"),
        }
    }

    #[test]
    fn list_extensions_unavailable_without_runtime() {
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(
                tempfile::tempdir().expect("workspace").path(),
            )),
        );
        let response = harness.handle(IpcRequest::ListExtensions);
        match response {
            IpcResponse::Error {
                code: IpcErrorCode::Unavailable,
                message,
            } => assert!(message.contains("ExtensionRuntime")),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn list_extensions_returns_enabled_excludes_disabled() {
        use crate::extension_compat::ExtensionSource;
        use crate::{
            ExtensionLifecycleStatus, ExtensionRuntime, ExtensionState, ExtensionStateStore,
            ResolutionPlan, daemon_extension_state_db, load_daemon_extension_runtime,
        };

        let data = tempfile::tempdir().expect("data");
        let db = daemon_extension_state_db(data.path());
        std::fs::create_dir_all(db.parent().unwrap()).expect("mkdir");
        let store = ExtensionStateStore::open(&db).expect("open");

        let enabled = ExtensionState {
            installation_id: "inst-enabled".into(),
            resolution: ResolutionPlan {
                source: ExtensionSource::AgentSkills,
                module_id: "ok-skill".into(),
                module_name: "ok".into(),
                version: "0.1.0".into(),
                source_path: data.path().join("src/SKILL.md"),
            },
            created_paths: vec![],
            modified_paths: vec![],
            ownership: vec![],
            status: ExtensionLifecycleStatus::Enabled,
        };
        let disabled = ExtensionState {
            installation_id: "inst-disabled".into(),
            resolution: ResolutionPlan {
                source: ExtensionSource::AgentSkills,
                module_id: "off-skill".into(),
                module_name: "off".into(),
                version: "0.1.0".into(),
                source_path: data.path().join("src2/SKILL.md"),
            },
            created_paths: vec![],
            modified_paths: vec![],
            ownership: vec![],
            status: ExtensionLifecycleStatus::Disabled,
        };
        store.put(&enabled).expect("put enabled");
        store.put(&disabled).expect("put disabled");

        let runtime = load_daemon_extension_runtime(data.path()).expect("reload");
        assert!(runtime.is_loaded("inst-enabled"));
        assert!(!runtime.is_loaded("inst-disabled"));

        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(data.path())),
        )
        .with_extension_runtime(Arc::new(Mutex::new(runtime)));

        let IpcResponse::Extensions { extensions } = harness.handle(IpcRequest::ListExtensions)
        else {
            panic!("expected Extensions");
        };
        assert_eq!(extensions.len(), 1);
        assert_eq!(extensions[0].installation_id, "inst-enabled");
        assert_eq!(extensions[0].module_id, "ok-skill");
        assert_eq!(extensions[0].status, "enabled");
        assert_eq!(extensions[0].source, "agent_skills");

        let IpcResponse::ExtensionStatus { extension } =
            harness.handle(IpcRequest::GetExtensionStatus {
                installation_id: "inst-enabled".into(),
            })
        else {
            panic!("expected ExtensionStatus");
        };
        assert_eq!(extension.installation_id, "inst-enabled");

        let missing = harness.handle(IpcRequest::GetExtensionStatus {
            installation_id: "inst-disabled".into(),
        });
        match missing {
            IpcResponse::Error {
                code: IpcErrorCode::InvalidRequest,
                ..
            } => {}
            other => panic!("disabled must not be loaded, got {other:?}"),
        }

        // Empty runtime still wired → empty list (not Unavailable).
        let empty = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(data.path())),
        )
        .with_extension_runtime(Arc::new(Mutex::new(ExtensionRuntime::empty())));
        let IpcResponse::Extensions { extensions } = empty.handle(IpcRequest::ListExtensions)
        else {
            panic!("expected empty Extensions");
        };
        assert!(extensions.is_empty());
    }

    #[test]
    fn list_mcp_servers_returns_registered_labels_only() {
        use crate::extension_compat::{McpCapabilities, McpModule, McpTransport};
        use std::collections::HashMap;

        let mut runtime = crate::ToolProviderRuntime::new();
        runtime.register(crate::McpServerSpec {
            id: "files".into(),
            module: McpModule {
                name: "Files MCP".into(),
                command: "npx".into(),
                args: vec!["-y".into(), "secret-token-should-not-leak".into()],
                env: HashMap::from([("API_KEY".into(), "super-secret".into())]),
                transport: McpTransport::Stdio,
                capabilities: McpCapabilities {
                    tools: true,
                    ..McpCapabilities::default()
                },
            },
        });
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(
                tempfile::tempdir().expect("workspace").path(),
            )),
        )
        .with_tool_providers(Arc::new(tokio::sync::Mutex::new(runtime)));

        let response = harness.handle(IpcRequest::ListMcpServers);
        let IpcResponse::McpServers { servers } = response else {
            panic!("expected McpServers, got {response:?}");
        };
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].id, "files");
        assert_eq!(servers[0].name, "Files MCP");
        assert!(!servers[0].connected);
        assert_eq!(servers[0].transport, Some(McpTransport::Stdio));
        let encoded = serde_json::to_string(&servers[0]).expect("encode");
        assert!(!encoded.contains("super-secret"));
        assert!(!encoded.contains("secret-token"));
        assert!(!encoded.contains("API_KEY"));
        assert!(!encoded.contains("npx"));
    }

    #[test]
    fn reload_mcp_servers_swaps_catalog_and_keeps_connected_false() {
        let data = tempfile::tempdir().expect("data");
        let mcp_dir = data.path().join("mcp");
        std::fs::create_dir_all(&mcp_dir).expect("mkdir");
        let slot = Arc::new(tokio::sync::Mutex::new(crate::ToolProviderRuntime::new()));
        let data_path = data.path().to_path_buf();
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(
                tempfile::tempdir().expect("workspace").path(),
            )),
        )
        .with_tool_providers(slot)
        .with_mcp_reload(Arc::new(move || {
            crate::load_daemon_mcp_runtime(&data_path).map_err(|e| e.to_string())
        }));

        let IpcResponse::McpServers { servers } = harness.handle(IpcRequest::ReloadMcpServers)
        else {
            panic!("reload empty");
        };
        assert!(servers.is_empty());

        std::fs::write(
            mcp_dir.join("tools.json"),
            br#"{
                "name": "tools",
                "command": "true",
                "args": [],
                "env": {},
                "transport": "stdio",
                "capabilities": { "tools": true, "resources": false, "prompts": false, "sampling": false }
            }"#,
        )
        .expect("write");
        let IpcResponse::McpServers { servers } = harness.handle(IpcRequest::ReloadMcpServers)
        else {
            panic!("reload with file");
        };
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].id, "tools");
        assert!(
            !servers[0].connected,
            "list/reload must stay connected=false until first tool use"
        );
    }

    #[test]
    fn mcp_manage_upsert_disable_enable_remove_round_trip() {
        use impetus_protocol::{McpCapabilities, McpServerUpsert, McpTransport};

        let data = tempfile::tempdir().expect("data");
        let slot = Arc::new(tokio::sync::Mutex::new(crate::ToolProviderRuntime::new()));
        let data_path = data.path().to_path_buf();
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(
                tempfile::tempdir().expect("workspace").path(),
            )),
        )
        .with_tool_providers(slot)
        .with_mcp_sot_root(data.path())
        .with_mcp_reload(Arc::new(move || {
            crate::load_daemon_mcp_runtime(&data_path).map_err(|e| e.to_string())
        }));

        let IpcResponse::McpServers { servers } = harness.handle(IpcRequest::UpsertMcpServer {
            server: McpServerUpsert {
                id: "echo".into(),
                name: "echo".into(),
                command: "true".into(),
                args: vec![],
                transport: McpTransport::Stdio,
                capabilities: McpCapabilities {
                    tools: true,
                    ..McpCapabilities::default()
                },
                env_keys: vec!["LABEL_ONLY".into()],
            },
        }) else {
            panic!("upsert");
        };
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].id, "echo");
        let encoded = serde_json::to_string(&servers[0]).expect("encode");
        assert!(!encoded.contains("secret"));
        assert!(data.path().join("mcp/echo.json").is_file());

        let IpcResponse::McpServers { servers } =
            harness.handle(IpcRequest::DisableMcpServer { id: "echo".into() })
        else {
            panic!("disable");
        };
        assert!(servers.is_empty());
        assert!(data.path().join("mcp/echo.json.disabled").is_file());
        assert!(!data.path().join("mcp/echo.json").is_file());

        let IpcResponse::McpServers { servers } =
            harness.handle(IpcRequest::EnableMcpServer { id: "echo".into() })
        else {
            panic!("enable");
        };
        assert_eq!(servers.len(), 1);

        let IpcResponse::McpServers { servers } =
            harness.handle(IpcRequest::RemoveMcpServer { id: "echo".into() })
        else {
            panic!("remove");
        };
        assert!(servers.is_empty());
        assert!(!data.path().join("mcp/echo.json").exists());
        assert!(!data.path().join("mcp/echo.json.disabled").exists());
    }

    #[test]
    fn memory_control_plane_append_list_export_clear() {
        use impetus_protocol::{MemoryEntryScope, MemoryExportFormat, MemoryProvenanceInfo};

        let data = tempfile::tempdir().expect("data");
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(
                tempfile::tempdir().expect("workspace").path(),
            )),
        )
        .with_memory(crate::open_daemon_memory_runtime(data.path()));

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: tempfile::tempdir().expect("ws").path().to_path_buf(),
        }) else {
            panic!("create");
        };

        let IpcResponse::MemoryEntry { entry, .. } = harness.handle(IpcRequest::AppendMemory {
            session_id,
            id: "n1".into(),
            scope: MemoryEntryScope::Project,
            content: "API_TOKEN=fake-test-token-abc\nnote=safe".into(),
            provenance: MemoryProvenanceInfo {
                source: "user".into(),
                kind: "note".into(),
            },
        }) else {
            panic!("append");
        };
        assert_eq!(entry.id, "n1");
        assert!(
            !entry.content.contains("fake-test-token-abc"),
            "content must be redacted: {}",
            entry.content
        );
        assert!(entry.content.contains("[REDACTED]"));
        assert!(entry.content.contains("note=safe"));

        let IpcResponse::MemoryEntries { entries, .. } = harness.handle(IpcRequest::ListMemory {
            session_id,
            scope: Some(MemoryEntryScope::Project),
        }) else {
            panic!("list");
        };
        assert_eq!(entries.len(), 1);

        let IpcResponse::MemoryExport { body, .. } = harness.handle(IpcRequest::ExportMemory {
            session_id,
            format: MemoryExportFormat::Jsonl,
        }) else {
            panic!("export");
        };
        assert!(body.contains("\"id\":\"n1\""));
        assert!(!body.contains("fake-test-token-abc"));

        let IpcResponse::MemoryCleared { removed, .. } = harness.handle(IpcRequest::ClearMemory {
            session_id,
            scope: None,
        }) else {
            panic!("clear");
        };
        assert_eq!(removed, 1);
        let IpcResponse::MemoryEntries { entries, .. } = harness.handle(IpcRequest::ListMemory {
            session_id,
            scope: None,
        }) else {
            panic!("list empty");
        };
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn append_memory_then_prompt_injects_into_provider_messages() {
        use impetus_protocol::{MemoryEntryScope, MemoryProvenanceInfo};

        let workspace = tempfile::tempdir().expect("workspace");
        let data = tempfile::tempdir().expect("data");
        let provider = Arc::new(MockProvider::default_mock());
        let harness = Harness::with_test_provider(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
            provider.clone(),
        )
        .with_memory(crate::open_daemon_memory_runtime(data.path()));

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create");
        };

        let IpcResponse::MemoryEntry { .. } = harness.handle(IpcRequest::AppendMemory {
            session_id,
            id: "proj-ctx".into(),
            scope: MemoryEntryScope::Project,
            content: "widget API uses /v2/widgets".into(),
            provenance: MemoryProvenanceInfo {
                source: "user".into(),
                kind: "note".into(),
            },
        }) else {
            panic!("append");
        };

        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "what endpoint?".into(),
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
                    status: RuntimeStatus::Completed,
                    ..
                }
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let received = provider.received_messages();
        assert!(
            !received.is_empty(),
            "provider must receive at least one turn"
        );
        let first = serde_json::to_string(&received[0]).expect("serialize");
        assert!(
            first.contains("widget API uses /v2/widgets"),
            "memory content missing from provider messages: {first}"
        );
        assert!(
            first.contains(crate::MEMORY_PROMPT_CONTEXT_HEADER),
            "memory header missing: {first}"
        );
    }

    #[tokio::test]
    async fn append_memory_then_approval_resume_injects_into_provider_messages() {
        use impetus_protocol::{MemoryEntryScope, MemoryProvenanceInfo};

        let workspace = tempfile::tempdir().expect("workspace");
        let data = tempfile::tempdir().expect("data");
        std::fs::write(workspace.path().join("evidence.txt"), "confirmed evidence")
            .expect("fixture");
        let provider = Arc::new(MockProvider::scripted(
            "scripted-memory-resume",
            "test-model",
            [
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
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
            provider.clone(),
        )
        .with_memory(crate::open_daemon_memory_runtime(data.path()));

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create");
        };

        let IpcResponse::MemoryEntry { .. } = harness.handle(IpcRequest::AppendMemory {
            session_id,
            id: "proj-ctx".into(),
            scope: MemoryEntryScope::Project,
            content: "widget API uses /v2/widgets".into(),
            provenance: MemoryProvenanceInfo {
                source: "user".into(),
                kind: "note".into(),
            },
        }) else {
            panic!("append");
        };

        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "write after approval".into(),
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
            panic!("event stream");
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

        let received = provider.received_messages();
        assert!(
            received.len() >= 2,
            "expected prompt + resume turns, got {}",
            received.len()
        );
        let resume = serde_json::to_string(&received[received.len() - 1]).expect("serialize");
        assert!(
            resume.contains("widget API uses /v2/widgets"),
            "memory missing from approval-resume provider messages: {resume}"
        );
        assert!(
            resume.contains(crate::MEMORY_PROMPT_CONTEXT_HEADER),
            "memory header missing on resume: {resume}"
        );
    }

    #[tokio::test]
    async fn empty_memory_prompt_does_not_invent_context() {
        let workspace = tempfile::tempdir().expect("workspace");
        let data = tempfile::tempdir().expect("data");
        let provider = Arc::new(MockProvider::default_mock());
        let harness = Harness::with_test_provider(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
            provider.clone(),
        )
        .with_memory(crate::open_daemon_memory_runtime(data.path()));

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create");
        };

        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "hello".into(),
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
                    status: RuntimeStatus::Completed,
                    ..
                }
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let received = provider.received_messages();
        assert!(!received.is_empty());
        let first = serde_json::to_string(&received[0]).expect("serialize");
        assert!(
            !first.contains(crate::MEMORY_PROMPT_CONTEXT_HEADER),
            "empty memory must not inject header: {first}"
        );
    }

    #[test]
    fn browser_health_and_negotiate_report_absent() {
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        let IpcResponse::BrowserHealth { status } = harness.handle(IpcRequest::GetBrowserHealth)
        else {
            panic!("health");
        };
        assert_eq!(status, impetus_protocol::BrowserHealthStatus::absent());
        let IpcResponse::BrowserNegotiate { result } =
            harness.handle(IpcRequest::NegotiateBrowser {
                protocol_version: "0.1".into(),
            })
        else {
            panic!("negotiate");
        };
        assert!(!result.compatible);
        assert_eq!(result.protocol_version, "0.1");
    }

    #[test]
    fn corrupt_durable_session_model_fails_closed() {
        let data = tempfile::tempdir().expect("data");
        let root = data.path().join("session_models");
        std::fs::create_dir_all(&root).expect("mkdir");
        let store = Arc::new(crate::SessionModelStore::open(&root).expect("open"));
        let events: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(
            Arc::clone(&events),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        )
        .with_session_model_store(Arc::clone(&store));
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: PathBuf::from("."),
        }) else {
            panic!("create");
        };
        std::fs::write(root.join(format!("{session_id}.json")), b"{not-valid-json")
            .expect("corrupt");
        let resp = harness.handle(IpcRequest::GetSessionModel { session_id });
        assert!(
            matches!(
                resp,
                IpcResponse::Error {
                    code: IpcErrorCode::Internal,
                    ..
                }
            ),
            "corrupt durable must fail-closed, got {resp:?}"
        );
    }

    #[test]
    fn invalid_ram_session_model_effort_fails_closed_on_get_and_prompt() {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create");
        };
        let bad = crate::SessionModelSelection {
            provider_id: "mock".into(),
            model_id: "mock-model".into(),
            reasoning_effort: Some("not-advertised".into()),
        };
        // Seed stale effort that mock catalog no longer advertises.
        harness.seed_session_model_ram_for_test(session_id, bad.clone());
        let get = harness.handle(IpcRequest::GetSessionModel { session_id });
        assert!(
            matches!(
                get,
                IpcResponse::Error {
                    code: IpcErrorCode::InvalidRequest,
                    ..
                }
            ),
            "Get must fail-closed on invalid RAM effort, got {get:?}"
        );
        // Bad RAM entry removed; subsequent Get falls through to honest default.
        let IpcResponse::SessionModel { selection, .. } =
            harness.handle(IpcRequest::GetSessionModel { session_id })
        else {
            panic!("get after clear must return default");
        };
        assert_eq!(selection.provider_id, "mock");
        assert_eq!(selection.reasoning_effort, None);

        // Re-seed and prove Prompt also fails closed before launch.
        harness.seed_session_model_ram_for_test(session_id, bad);
        let prompt = harness.handle(IpcRequest::Prompt {
            session_id,
            text: "should not launch".into(),
            artifact: None,
            intent: Default::default(),
        });
        assert!(
            matches!(prompt, IpcResponse::Error { .. }),
            "Prompt must fail-closed on invalid RAM effort, got {prompt:?}"
        );
    }

    #[test]
    fn session_model_survives_harness_restart_via_durable_store() {
        let data = tempfile::tempdir().expect("data");
        let store = Arc::new(
            crate::SessionModelStore::open(data.path().join("session_models")).expect("open"),
        );
        let events: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(
            Arc::clone(&events),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        )
        .with_session_model_store(Arc::clone(&store));
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: PathBuf::from("."),
        }) else {
            panic!("create");
        };
        let IpcResponse::SessionModel {
            selection: before, ..
        } = harness.handle(IpcRequest::GetSessionModel { session_id })
        else {
            panic!("get default");
        };
        let set = harness.handle(IpcRequest::SetSessionModel {
            session_id,
            provider_id: before.provider_id.clone(),
            model_id: before.model_id.clone(),
            reasoning_effort: None,
        });
        assert!(
            matches!(set, IpcResponse::SessionModel { .. }),
            "set: {set:?}"
        );

        // Simulate daemon restart: fresh Harness + empty RAM map, same durable root.
        let harness2 = Harness::new(
            Arc::clone(&events),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        )
        .with_session_model_store(store);
        let IpcResponse::SessionModel { selection, .. } =
            harness2.handle(IpcRequest::GetSessionModel { session_id })
        else {
            panic!("get after restart");
        };
        assert_eq!(selection.provider_id, before.provider_id);
        assert_eq!(selection.model_id, before.model_id);
        assert_eq!(selection.reasoning_effort, None);
    }

    #[tokio::test]
    async fn protocol_path_smoke_session_model_prompt_mcp_worktree() {
        let workspace = tempfile::tempdir().expect("workspace");
        let data = tempfile::tempdir().expect("data");
        let mcp_dir = data.path().join("mcp");
        std::fs::create_dir_all(&mcp_dir).expect("mcp");
        std::fs::write(
            mcp_dir.join("echo.json"),
            br#"{
                "name": "echo",
                "command": "true",
                "args": [],
                "env": {},
                "transport": "stdio",
                "capabilities": { "tools": true, "resources": false, "prompts": false, "sampling": false }
            }"#,
        )
        .expect("mcp json");
        let worktrees = Arc::new(
            crate::WorktreeManager::open(data.path().join("wt.db"), data.path().join("worktrees"))
                .expect("wt"),
        );
        let mcp = crate::load_daemon_mcp_runtime(data.path()).expect("mcp load");
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        )
        .with_tool_providers(Arc::new(tokio::sync::Mutex::new(mcp)))
        .with_worktree_manager(worktrees);

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create session");
        };
        let IpcResponse::SessionModel { selection, .. } =
            harness.handle(IpcRequest::GetSessionModel { session_id })
        else {
            panic!("get model");
        };
        assert_eq!(selection.provider_id, "mock");

        let accepted = harness.handle(IpcRequest::Prompt {
            session_id,
            text: "smoke".into(),
            artifact: None,
            intent: Default::default(),
        });
        assert!(
            matches!(
                accepted,
                IpcResponse::Status {
                    status: RuntimeStatus::Running,
                    ..
                }
            ),
            "prompt path: {accepted:?}"
        );

        let IpcResponse::McpServers { servers } = harness.handle(IpcRequest::ListMcpServers) else {
            panic!("mcp list");
        };
        assert_eq!(servers.len(), 1);
        assert!(!servers[0].connected);

        let IpcResponse::Worktrees { worktrees } = harness.handle(IpcRequest::ListWorktrees {
            session_id: Some(session_id),
        }) else {
            panic!("worktrees");
        };
        assert!(worktrees.is_empty());

        let IpcResponse::Models { providers } = harness.handle(IpcRequest::ListModels) else {
            panic!("models");
        };
        assert!(!providers.is_empty());
    }

    #[test]
    fn list_models_returns_mock_catalog() {
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(
                tempfile::tempdir().expect("workspace").path(),
            )),
        );
        let response = harness.handle(IpcRequest::ListModels);
        let IpcResponse::Models { providers } = response else {
            panic!("expected Models, got {response:?}");
        };
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].provider_id, "mock");
        assert!(providers[0].is_default);
        assert!(!providers[0].model_id.is_empty());
        // Mock has no remote /v1/models — honest Unknown, not fake Healthy.
        assert_eq!(
            providers[0].health,
            crate::ModelProviderHealthLabel::Unknown
        );
        assert_eq!(
            providers[0].availability,
            impetus_protocol::ModelAvailability::Unknown
        );
        assert_eq!(
            providers[0].provider_options["discovery"],
            serde_json::json!("static_fallback")
        );
        assert!(providers[0].agent_capabilities.is_none());
        assert_eq!(
            providers[0].reasoning_efforts,
            vec!["low", "medium", "high"]
        );
        assert!(providers[0].capabilities.reasoning);
        assert!(!providers[0].capabilities.tools);
    }

    #[tokio::test]
    async fn set_session_model_is_used_on_prompt_stream() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mock = Arc::new(
            crate::MockProvider::default_mock()
                .with_reasoning_efforts(["low", "medium", "high"])
                .with_catalog_models(["mock-model", "session-model-x"]),
        );
        let harness = Harness::with_test_provider(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
            mock.clone(),
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: workspace.path().to_path_buf(),
        }) else {
            panic!("create session");
        };
        let set = harness.handle(IpcRequest::SetSessionModel {
            session_id,
            provider_id: "mock".into(),
            model_id: "session-model-x".into(),
            reasoning_effort: Some("high".into()),
        });
        assert!(
            matches!(set, IpcResponse::SessionModel { .. }),
            "set model: {set:?}"
        );

        let accepted = harness.handle(IpcRequest::Prompt {
            session_id,
            text: "use override".into(),
            artifact: None,
            intent: Default::default(),
        });
        assert!(
            matches!(
                accepted,
                IpcResponse::Status {
                    status: RuntimeStatus::Running,
                    ..
                }
            ),
            "prompt: {accepted:?}"
        );

        // Allow agent loop task to call provider.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let opts = mock.last_stream_options().expect("stream options recorded");
        assert_eq!(opts.model_id.as_deref(), Some("session-model-x"));
        assert_eq!(opts.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn hello_advertises_list_mcp_and_list_models() {
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(
                tempfile::tempdir().expect("workspace").path(),
            )),
        );
        let IpcResponse::Hello { capabilities, .. } = harness.handle(IpcRequest::Hello {
            version: IPC_VERSION,
            min_version: Some(IPC_MIN_SUPPORTED),
            capabilities: vec!["list_mcp".into(), "list_models".into()],
        }) else {
            panic!("hello");
        };
        assert!(capabilities.iter().any(|c| c == "list_mcp"));
        assert!(capabilities.iter().any(|c| c == "list_models"));
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
            min_version: Some(IPC_MIN_SUPPORTED),
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

    #[test]
    fn git_status_and_list_branches_ipc_on_temp_repo() {
        let workspace = tempfile::tempdir().expect("workspace");
        let root = workspace.path();
        std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(root)
            .status()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(root)
            .status()
            .expect("email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(root)
            .status()
            .expect("name");
        std::fs::write(root.join("README"), b"seed").expect("write");
        std::process::Command::new("git")
            .args(["add", "README"])
            .current_dir(root)
            .status()
            .expect("add");
        std::process::Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(root)
            .status()
            .expect("commit");

        let store = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(
            store,
            PolicyEngine::new(SandboxScope::local_workspace(root)),
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: root.to_path_buf(),
        }) else {
            panic!("create session");
        };

        let IpcResponse::Hello { capabilities, .. } = harness.handle(IpcRequest::Hello {
            version: IPC_VERSION,
            min_version: Some(IPC_MIN_SUPPORTED),
            capabilities: vec!["git".into()],
        }) else {
            panic!("hello");
        };
        assert!(capabilities.iter().any(|c| c == "git"));

        let IpcResponse::Branches { branches, .. } =
            harness.handle(IpcRequest::ListBranches { session_id })
        else {
            panic!("list branches");
        };
        assert!(
            branches.iter().any(|b| b.name == "main" && b.current),
            "{branches:?}"
        );

        let IpcResponse::GitStatus { status, .. } =
            harness.handle(IpcRequest::GitStatus { session_id })
        else {
            panic!("git status");
        };
        assert!(!status.dirty);
        assert_eq!(status.branch.name.as_deref(), Some("main"));

        std::fs::write(root.join("wip.txt"), b"x").expect("dirty");
        let IpcResponse::GitStatus { status, .. } =
            harness.handle(IpcRequest::GitStatus { session_id })
        else {
            panic!("dirty status");
        };
        assert!(status.dirty);
        assert!(status.files.iter().any(|f| f.path.ends_with("wip.txt")));
    }

    #[test]
    fn get_diff_returns_structured_observation_on_temp_repo() {
        let workspace = tempfile::tempdir().expect("workspace");
        let root = workspace.path();
        std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(root)
            .status()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(root)
            .status()
            .expect("email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(root)
            .status()
            .expect("name");
        std::fs::write(root.join("README"), b"seed\n").expect("write");
        std::process::Command::new("git")
            .args(["add", "README"])
            .current_dir(root)
            .status()
            .expect("add");
        std::process::Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(root)
            .status()
            .expect("commit");
        std::fs::write(root.join("README"), b"seed\nedited\n").expect("edit");

        let store = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(
            store,
            PolicyEngine::new(SandboxScope::local_workspace(root)),
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: root.to_path_buf(),
        }) else {
            panic!("create session");
        };

        let IpcResponse::Hello { capabilities, .. } = harness.handle(IpcRequest::Hello {
            version: IPC_VERSION,
            min_version: Some(IPC_MIN_SUPPORTED),
            capabilities: vec!["git".into(), "structured_diff".into()],
        }) else {
            panic!("hello");
        };
        assert!(capabilities.iter().any(|c| c == "structured_diff"));

        let IpcResponse::Diff { diff, .. } = harness.handle(IpcRequest::GetDiff {
            session_id,
            base_ref: None,
        }) else {
            panic!("get diff");
        };
        assert!(!diff.patch.is_empty());
        let obs = diff.observation.expect("structured DiffObservation");
        assert!(obs.files_changed >= 1);
        assert!(!obs.hunks.is_empty());
        assert!(obs.insertions >= 1 || obs.deletions >= 1);

        let IpcResponse::Diff { diff, .. } = harness.handle(IpcRequest::GetFileDiff {
            session_id,
            path: std::path::PathBuf::from("README"),
            base_ref: None,
        }) else {
            panic!("get file diff");
        };
        let file_obs = diff.observation.expect("file DiffObservation");
        assert!(!file_obs.hunks.is_empty());
        assert!(
            file_obs
                .hunks
                .iter()
                .any(|h| h.file.ends_with("README") || h.file.as_os_str() == "README")
        );
    }
}
