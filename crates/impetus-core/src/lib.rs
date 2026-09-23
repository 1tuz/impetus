//! Policy-centred local runtime for Impetus.
//!
//! The crate intentionally owns no native GUI or PTY state. It emits durable events,
//! makes permission decisions, and exposes small capability seams for the app.

pub mod acp_adapter;
pub mod agent_loop;
pub mod agent_plugins_adapter;
pub mod agent_scheduler;
pub mod agent_skills_adapter;
pub mod anthropic_provider;
pub mod approval;
pub mod artifact_upload;
pub mod attachments;
pub mod audit_log;
pub mod budget;
pub mod builtin_ids;
pub mod capability_truth;
pub mod child_concurrency;
pub mod child_result_store;
pub mod ci;
pub mod claude_code_adapter;
pub mod codex_adapter;
pub mod coding_tools;
pub mod compaction;
pub mod context_builder;
pub mod context_optimizer;
pub mod cost_estimation;
pub mod cursor_adapter;
pub mod daemon_wiring;
pub mod deepseek_harness_adapter;
pub mod diagnostics;
pub mod diff_observation;
pub mod durable_artifacts;
pub mod effects;
pub mod events;
pub mod execution;
pub mod execution_mode;
pub mod explore_agent_loop;
pub mod explore_child;
pub mod extension_adapter;
pub mod extension_capability_registry;
pub mod extension_compat;
pub mod extension_host;
pub mod extension_host_process;
pub mod extension_id;
pub mod extension_lifecycle;
pub mod extension_manifest;
pub mod extension_policy;
pub mod git_ops;
pub mod harness_api;
pub mod hook_prefilter;
pub mod instruction_learning;
pub mod instructions;
pub mod ipc;
pub mod lsp_backend;
pub mod lsp_process;
pub mod mcp_adapter;
pub mod mcp_live;
pub mod mcp_manifest;
pub mod memory_runtime;
pub mod memory_store;
pub mod mock_provider;
pub mod model_router;
pub mod module;
pub mod module_fallback;
pub mod module_ipc;
pub mod module_lifecycle;
pub mod module_registry;
pub mod observations;
pub mod openai_compat_adapter;
pub mod openai_native_adapter;
pub mod openai_provider;
pub mod openai_responses;
pub mod output_reducer;
pub mod ownership;
pub mod plugins;
pub mod policy;
pub mod policy_config;
pub mod policy_store;
pub mod privilege_boundaries;
pub mod profile;
pub mod projection;
pub mod provider;
pub mod provider_protocol_adapter;
pub mod provider_registry;
pub mod provider_trait;
pub mod reference_store;
pub mod reference_tools;
pub mod remote;
pub mod risk_gate;
pub mod role_agent_loop;
pub mod role_child;
pub mod rtk_adapter;
pub mod runtime;
pub mod schema;
#[cfg(test)]
mod security_runtime_pr;
pub mod service_contract;
pub mod service_provider;
pub mod session_model_store;
pub mod steer_rewrite;
pub mod storage;
pub mod subagent_metadata;
pub mod supervisor;
pub mod tempo_importer;
pub mod tool_orchestrator;
pub mod tool_provider_runtime;
pub mod tool_schema;
pub mod tools;
pub mod user_intent;
pub mod web_research;
pub mod workflow_engine;
pub mod workflow_runtime;
pub mod workspace_files;
pub mod worktree_manager;

pub use acp_adapter::AcpAdapter;
pub use agent_loop::{AgentLoop, AgentLoopError, ToolCall, inject_memory_context};
pub use agent_plugins_adapter::{AgentPluginsAdapter, PluginCommandEntry};
pub use agent_scheduler::{
    AgentSchedulerError, InMemoryAgentScheduler, RoleScheduleTask, ScheduleAdmission,
    ScheduleRecord, ScheduleStatus, parse_step_role,
};
pub use agent_skills_adapter::AgentSkillsAdapter;
pub use anthropic_provider::AnthropicProvider;
pub use approval::{
    APPROVAL_DETAIL_SCHEMA_ID, APPROVAL_DETAIL_SCHEMA_VERSION, ApprovalDetail, ApprovalId,
    ApprovalRequest, ApprovalResolution, ApprovalResolver, ApprovalState, ScopeEstimate,
};
pub use artifact_upload::{
    ArtifactUploadError, ArtifactUploadStore, MAX_ARTIFACT_UPLOAD_BYTES,
    MAX_ARTIFACT_UPLOAD_CHUNK_BYTES, upload_error_message,
};
pub use attachments::{Attachment, AttachmentError, AttachmentStore, StoreStats};
pub use audit_log::{AuditEntry, AuditLog, AuditQuery};
pub use budget::{
    BudgetChecker, BudgetConfig, BudgetError, BudgetState, CompactionPolicy, ReasoningEffort,
};
pub use builtin_ids::{
    BuiltinIdAudit, BuiltinIdEntry, BuiltinKind, DuplicateBuiltinId, audit_builtin_ids,
    audit_shipped_builtin_ids, find_duplicate_ids, shipped_builtin_ids, unused_builtin_ids_stub,
};
pub use capability_truth::{
    CAPABILITIES_SCHEMA_ID, CAPABILITIES_SCHEMA_VERSION, CapabilityEntry, CapabilityLevel,
    CapabilityTruthReport,
};
pub use child_concurrency::{
    ChildConcurrencyConfig, ChildConcurrencyError, ChildConcurrencyGate,
    DEFAULT_CHILD_CONCURRENCY_CAP, DEFAULT_PER_PARENT_CHILD_CAP,
};
pub use child_result_store::{
    ChildResult, ChildResultError, ChildResultStatus, ChildResultStore, child_result_from_metadata,
    child_result_role,
};
pub use ci::{
    CiBackend, CiError, CiProject, Job, JobStatus, LocalCiEvent, LocalGitlabBackend, LocalRun,
    Pipeline, PipelineStatus, RemoteGitlabBackend, Stage,
};
pub use claude_code_adapter::ClaudeCodeAdapter;
pub use codex_adapter::CodexAdapter;
pub use coding_tools::{
    ABSENT_CODING_TOOLS_REASON, AbsentCodingToolsService, CodingDiagnostic, CodingToolsError,
    CodingToolsProvider, CodingToolsService, DiagnosticSeverity, DocumentSymbol, HoverInfo,
    MockCodingToolsProvider, OptionalCodingToolsService, PositionQuery,
    ProviderBackedCodingToolsService, SourceLocation, SourcePosition, SourceRange, SymbolKind,
    block_on_coding_tools,
};
pub use compaction::{
    compact_provider_messages, estimate_tokens as estimate_compaction_tokens, summarize_messages,
};
pub use context_builder::{
    ArtifactRangeSource, ContextBuilder, ContextBuilderError, MaterializedArtifact,
};
pub use context_optimizer::{
    BuiltinContextOptimizer, BuiltinContextService, ContextCatalogEntry, ContextItem,
    ContextPayload, ContextService, ContextTier, DEFAULT_CONTEXT_BUDGET_TOKENS, DescriptionSource,
    MemoryDescriptionSource, ToolStub, default_tool_stubs, system_messages_for_binding,
};
pub use cursor_adapter::CursorAdapter;
pub use daemon_wiring::{
    DaemonWiringError, build_agent_loop_explore_executor,
    build_agent_loop_explore_executor_for_harness, build_agent_loop_role_executor,
    build_agent_loop_role_executor_for_harness, build_explore_spawn_bridge,
    build_explore_spawn_bridge_for_harness, daemon_extension_state_db, daemon_mcp_dir,
    default_pty_session_store_path, default_worktree_store_path, default_worktrees_root,
    load_daemon_extension_runtime, load_daemon_hook_prefilter, load_daemon_mcp_runtime,
    load_daemon_policy_store, open_daemon_pty_session_store, open_daemon_worktree_manager,
    remove_daemon_mcp_server, set_daemon_mcp_enabled, upsert_daemon_mcp_server,
};
pub use deepseek_harness_adapter::{
    DEEPSEEK_PROCESS_PROTOCOL, DeepSeekHarnessAdapter, DeepSeekHarnessManifest,
};
pub use diagnostics::{SubsystemHealth, SubsystemStatus};
pub use diff_observation::{
    MAX_HUNK_PREVIEW_LINES, MAX_HUNKS, MAX_UNIFIED_PREVIEW_LINES, from_git_diff, from_texts,
    from_unified, unified_preview,
};
pub use durable_artifacts::{
    ARTIFACT_GC_INTERVAL, ARTIFACT_GC_RETENTION, ArtifactMeta as DurableArtifactMeta,
    ArtifactRef as DurableArtifactRef, DurableArtifactStore, default_artifact_root,
    run_artifact_gc,
};
pub use effects::{
    AdmittedOperation, CapabilityVersion, DeferredEffect, EffectAdmission, EffectCapability,
    EffectDecision, EffectExecution, EffectSeam, NormalizedEffect, Sandbox,
    normalized_effect_from_action,
};
pub use events::{
    AgentEvent, ApprovalEvent, BackendEvent, BudgetEvent, ChildEvent, CommandEvent,
    CompactionStructuralState, EVENT_SCHEMA_VERSION, Event, EventPayload, IntentEvent,
    MAX_ACTIVITY_PREVIEW_CHARS, NoticeEvent, PlanEvent, PtyEvent, RetryEvent, RunEvent,
    SandboxEvent, SandboxPrepareState, SessionEvent, ToolEvent, ToolEventOutcome,
    bound_activity_preview,
};
pub use execution::{
    DEFAULT_PTY_READ_BYTES, MAX_PROCESS_OUTPUT_BYTES, MAX_PROCESS_PREVIEW_BYTES,
    MAX_PTY_PENDING_SPILLS, MAX_PTY_RING_BYTES, MacosSeatbeltSandbox, PTY_SPILL_COALESCE_BYTES,
    PreparedPtySandbox, PreparedSandboxCommand, ProcessExecution, ProcessExecutionError,
    ProcessExecutionRequest, ProcessOutput, PtyOutputChunk, PtySandboxKeepAlive, PtySession,
    PtySessionError, PtySessionId, PtySessionManager, PtySessionRecord, PtySessionState,
    PtySessionStore, PtySessionStoreError, SandboxCommandRequest, SandboxDecision,
    SandboxDecisionState, SandboxError, SandboxProvider, SqlitePtySessionStore,
    UnavailableSandboxProvider, prepare_pty_sandbox, production_sandbox_provider,
    resolve_pty_working_dir,
};
pub use execution_mode::ExecutionMode;
pub use explore_agent_loop::{
    AgentLoopExploreExecutor, explore_provider_tool_names, explore_provider_tool_schemas,
};
pub use explore_child::{
    EXPLORE_ALLOWED_TOOLS, ExploreChildEnv, ExploreChildError, ExploreChildExecutor,
    ExploreChildOutcome, ExploreChildRequest, ExploreChildRunner, ExploreExecutorError,
    ExploreExecutorOutput, ExploreSpawnBridge, HarnessExploreSpawn, MockExploreExecutor,
    ReadOnlyExploreExecutor, intersect_explore_tools, is_explore_forbidden_tool,
    resume_parent_after_explore, validate_explore_allowed_tools,
};
pub use extension_adapter::{ExtensionAdapter, ExtensionRegistry};
pub use extension_capability_registry::ExtensionCapabilityRegistry;
pub use extension_compat::{
    AgentProfile, CanonicalModuleKind, CanonicalModuleSpec, CanonicalSkill, Command,
    CommandArgument, CommandHandler, CompatibilityMatrix, ExtensionSource, ImportCapability,
    ImportResult, Instruction, InstructionContext, InstructionPriority, McpCapabilities, McpModule,
    McpTransport, ToolHandler, ToolProvider as ExtensionToolProvider,
};
pub use extension_host::{
    DiscoveredPackage, ExtensionDiscoveryRoots, ExtensionHost, ExtensionHostError,
    ExtensionHostPhase, ExtensionPackageSource, LoadedExtension,
};
pub use extension_host_process::{HostProcessError, HostProcessSession, spawn_and_initialize};
pub use extension_id::{
    ExtensionIdError, ExtensionTypeDir, ensure_owned_extension_path, extension_type_root,
    is_valid_extension_id, join_under_extension_root, mcp_install_path, normalize_extension_id,
    skill_install_path,
};
pub use extension_lifecycle::{
    ApplyError, DoctorError, DoctorReport, ExtensionInstallIntent, ExtensionLifecycleStatus,
    ExtensionRuntime, ExtensionState, ExtensionStateStore, InstallHealthReport, InstallPlan,
    LifecycleError, LifecycleResult, PathHealthReport, PathHealthStatus, PlanError, RemoveError,
    RemoveResult, RepairError, RepairResult, ResolutionPlan, apply_install, disable_install,
    doctor_install, enable_install, plan_install, remove_install, repair_install, unload_install,
};
pub use extension_manifest::{
    EXTENSION_SCHEMA_ID, EXTENSION_SCHEMA_VERSION, ExtensionManifest, ExtensionManifestError,
    ExtensionManifestKind, validate_capabilities as validate_extension_capabilities,
    validate_digest as validate_extension_digest,
};
pub use extension_policy::{
    action_kinds_for_permission, evaluate_permission_against_scope, permission_eval,
};
pub use git_ops::{
    GIT_DIFF_MAX_BYTES, GitBranchInfo, GitChangeKind, GitChangedFile, GitCurrentBranch,
    GitDiffPayload, GitOpsError, GitRepositoryState, GitSessionCwd, GitStatusSnapshot,
    apply_worktree_diff_counts, create_branch, get_current_branch, get_diff, get_file_diff,
    get_repository_state, git_status, list_branches, list_changed_files, resolve_session_git_cwd,
    switch_branch,
};
pub use harness_api::{Harness, McpReloadHook, redact_tool_outcome};
pub use hook_prefilter::HookCatalogLoadError;
pub use hook_prefilter::{
    HookAction, HookCatalogError, HookPrefilter, HookRule, HookTrustLevel, PrefilterDecision,
    PrefilterError, SpawnStubError, SpawnStubOutcome, spawn_stub,
};
pub use impetus_protocol::SessionModelSelection;
pub use instruction_learning::{
    InstructionLearning, LearningEvidence, ObservationKind, Proposal, ProposalLifecycle,
    ProposalTarget,
};
pub use instructions::{
    InstructionKind, InstructionReference, InstructionResolveError, InstructionResolver,
    InstructionScope, InstructionTokenEstimate, ResolveRequest, ResolvedInstructions,
    governed_instruction_ids,
};
pub use ipc::{
    IPC_CAPABILITIES, IPC_EVENTS_FRAME_BUDGET, IPC_MIN_SUPPORTED, IPC_VERSION, IpcErrorCode,
    IpcRequest, IpcResponse, MAX_IPC_LINE_BYTES, capability_allows, negotiate_ipc_version,
    required_capability, trim_events_to_ipc_frame, validate_request_on_wire,
    validate_response_on_wire,
};
pub use lsp_backend::{
    LSP_BACKEND_NOT_IMPLEMENTED, LspBackendFamily, LspBackendHandshake, LspBackendLaunchHint,
    LspBackendModule, optional_coding_tools_with_lsp,
};
pub use lsp_process::ProcessLspBackend;
pub use mcp_adapter::McpAdapter;
pub use mcp_live::{McpLiveBridge, McpLiveCallResult, McpLiveToolEntry};
pub use mcp_manifest::{
    MCP_SCHEMA_ID, MCP_SCHEMA_VERSION, McpManifest, McpManifestError,
    validate_env_keys as validate_mcp_env_keys,
};
pub use memory_runtime::{
    MEMORY_PROMPT_CONTEXT_HEADER, SessionMemoryRuntime, daemon_memory_dir,
    format_prompt_context_block, open_daemon_memory_runtime,
};
pub use memory_store::{
    DERIVED_INDEX_DIR, MemoryDerivedIndex, MemoryEntry, MemoryPromotionTarget, MemoryProvenance,
    MemoryScope, MemoryStore, MemoryStoreError, MemoryTrustError, evaluate_with_memory_context,
    granted_effect_capabilities, refuse_auto_promote, resolve_index_path,
    sandbox_scope_after_memory,
};
pub use mock_provider::{MockProvider, MockStreamItem as MockProviderItem};
pub use observations::{
    DiffHunk, DiffObservation, DiffSource, PipelineJob, PipelineObservation, SearchMatch,
    SearchObservation, TestFailure, TestObservation,
};
pub use openai_compat_adapter::OpenAiCompatibleAdapter;
pub use openai_native_adapter::OpenAiNativeAdapter;
pub use openai_provider::{OpenAiProvider, RetryBudget as OpenAiRetryBudget};
pub use output_reducer::{OutputReducer, ReducedOutput, ReductionStrategy, TokenBudget};
pub use ownership::{OwnershipError, OwnershipRecord, OwnershipStore, content_digest, path_key};
pub use plugins::{CapabilityAvailability, CapabilityManifest, CapabilityRegistry};
pub use policy::{
    Action, ActionFingerprint, ActionKind, ActionOrigin, PolicyDecision, PolicyEngine,
    PolicySnapshot, PolicyVersion, SandboxScope,
};
pub use policy_config::{
    POLICY_CONFIG_VERSION, PolicyConfig, PolicyConfigDecision, PolicyConfigError,
};
pub use policy_store::{
    GovernedInstructionRef, POLICY_STORE_VERSION, PolicyStore, PolicyStoreError,
    default_policy_store_path,
};
pub use privilege_boundaries::{
    command_requests_privilege_escalation, pty_argv_is_non_login, risk_gate_denies_sudo,
};
pub use profile::{Profile, ProfileConfig, ServiceBinding, ServiceBindings};
pub use projection::{ProjectionError, SessionProjection, reduce};
pub use provider::{
    CredentialResolver, CredentialStrategy, NoCredentialResolver, OpenAiCompatibleProvider,
    OpenAiHttpApi, ProviderError, ProviderHealth, ProviderMessage, ProviderProfile, RetryBudget,
};
pub use provider_protocol_adapter::{ProviderProtocolAdapter, ToolCallAssembler};
pub use provider_registry::{ModelProviderHealthLabel, ModelProviderStatus, ProviderRegistry};
pub use provider_trait::{
    FinishReason, ModelCatalogEntry, ModelCatalogResult, ModelProvider, StreamEvent, StreamOptions,
};
pub use reference_store::{
    DatasetManifest, DatasetScope, ImportResult as ReferenceImportResult, PartitionStrategy,
    RecordProvenance, RecordSource, ReferenceRecord, ReferenceService, SearchFilters, SearchResult,
    Sensitivity, YamlReferenceService,
};
pub use reference_tools::{
    ReferenceGetRequest, ReferenceGetResponse, ReferenceListDatasetsResponse,
    ReferenceSearchRequest, ReferenceSearchResponse, ReferenceToolError, ReferenceToolKind,
    ReferenceTools,
};
pub use remote::{
    HostKeyFingerprint, HostKeyVerificationError, SSHApproval, SSHApprovalStore,
    SSHApprovalStoreError, SSHConnectionError, SSHConnectionRequest, SSHKeyReference, SSHProfile,
    SftpError, SftpFileInfo, SftpOperation, SftpOperationRequest, SftpResult, SftpSession,
    SftpSessionManager, SqliteSSHApprovalStore, SqliteTmuxSessionStore, TmuxError, TmuxSession,
    TmuxSessionId, TmuxSessionManager, TmuxSessionRecord, TmuxSessionRequest, TmuxSessionState,
    TmuxSessionStore, TmuxSessionStoreError,
};
pub use risk_gate::{
    DeterministicRiskGate, RiskContext, RiskGate, RiskGateDecision, command_text,
    default_risk_gate, is_mutating_effect, is_opaque_shell, is_read_only_effect,
    is_safe_readonly_command,
};
pub use role_agent_loop::{
    AgentLoopRoleExecutor, role_provider_tool_names, role_provider_tool_schemas,
};
pub use role_child::{
    BUILD_ALLOWED_TOOLS, HarnessRoleSpawn, MockRoleExecutor, ProcessRoleChildExecutor,
    RESEARCH_ALLOWED_TOOLS, REVIEW_ALLOWED_TOOLS, RoleChildEnv, RoleChildError, RoleChildExecutor,
    RoleChildOutcome, RoleChildRequest, RoleChildRunner, RoleExecutorError, RoleExecutorOutput,
    RoleSpawnBridge, allowed_tools_for, default_echo_program,
};
pub use runtime::{
    AGENT_CHUNK_COALESCE_BYTES, AGENT_CHUNK_PREVIEW_BYTES, AgentRuntime,
    MAX_AGENT_CHUNK_EVENT_BYTES, RuntimeError, RuntimeStatus, bound_agent_chunk_text,
};
pub use schema::{
    HARNESS_NEST_KEYS, KNOWN_SCHEMAS, NEST_HARNESS, NEST_PROVIDER, PROVIDER_NEST_KEYS,
    SCHEMA_APPROVAL_DETAIL, SCHEMA_CAPABILITIES, SCHEMA_EXTENSION, SCHEMA_MCP, SCHEMA_SESSION,
    SchemaSpec, SchemaValidationError, WIRE_RESPONSE_SCHEMAS, lookup as lookup_schema,
    reject_leaked_nested_fields, reject_unknown_critical_fields, require_nest_objects,
    require_version as require_schema_version, validate_approval_detail_wire,
    validate_envelope as validate_schema_envelope, validate_session_wire,
};
pub use service_provider::{
    ExternalServiceHandle, ResolvedService, ServiceProvider, ServiceProviderKind, ServiceTrait,
};
pub use session_model_store::{
    SessionModelStore, daemon_session_models_dir, open_daemon_session_model_store,
};
pub use steer_rewrite::{
    MockSteerRewrite, PassthroughSteerRewrite, ProviderSteerRewrite, SteerActiveContext,
    SteerPendingQueue, SteerRewrite, SteerRewriteError, SteerRewriteOutput, default_steer_rewrite,
    provider_steer_rewrite,
};
pub use storage::{
    CheckpointInfo, EventStore, MemoryEventStore, SessionInfo, SqliteEventStore, StoreError,
};
pub use subagent_metadata::{
    ChildRunMetadata, ChildRunMetadataError, SubagentCapabilityIntent, SubagentRole,
};
pub use supervisor::{MockStreamingProvider, SessionSupervisor, SupervisorError};
pub use tempo_importer::{TempoImporter, TempoImporterConfig, TempoWorklog};
pub use tool_orchestrator::{
    OrchestratorError, ToolObservation, ToolOrchestrator, ToolOutcomeStatus, ToolRequest,
};
pub use tool_provider_runtime::{
    McpServerSpec, McpServerStatus, ToolProviderRuntime, filter_mcp_catalog, parse_mcp_catalog_name,
};
pub use tool_schema::{
    BuiltinToolSchema, ToolArgError, builtin_tool_schemas, canonical_tool_name, schema_for_tool,
    validate_arguments_with_schema, validate_tool_arguments,
};
pub use tools::{
    ARTIFACT_CHUNK_SIZE, ReadOnlyTool, ReadOnlyToolKind, ReadOnlyTools, ToolError, ToolOutcome,
    ToolProvenance, ToolResult,
};
pub use user_intent::{
    FanoutResults, QueuedFollowUp, UserIntentAccepted, UserIntentError, UserIntentFanout,
    UserIntentRouter, UserIntentSubmission, UserPromptIntent,
};
pub use workflow_engine::{
    StepCheckpoint, StepStatus, WorkflowBudget, WorkflowEngine, WorkflowError, WorkflowRecipe,
    WorkflowStatus, WorkflowStep,
};
pub use workflow_runtime::{WorkflowRuntime, WorkflowRuntimeError};
pub use workspace_files::{
    BINARY_PROBE_BYTES, IGNORED_DIR_NAMES, MAX_WORKSPACE_FILE_BYTES, MAX_WORKSPACE_SEARCH_FILES,
    MAX_WORKSPACE_SEARCH_HITS, WorkspaceDirEntry, WorkspaceDirListing, WorkspaceFileContent,
    WorkspaceFileMetadata, WorkspaceFilesError, WorkspaceSearchHit, WorkspaceSearchResult,
};
pub use worktree_manager::{
    AgentWorkRole, MergeReadyReport, StaleReason, StaleReport, WorktreeAttachedPermissions,
    WorktreeBinding, WorktreeDiffSummary, WorktreeError, WorktreeLifecycleState, WorktreeManager,
};
