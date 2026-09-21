//! Policy-centred local runtime for Impetus.
//!
//! The crate intentionally owns no native GUI or PTY state. It emits durable events,
//! makes permission decisions, and exposes small capability seams for the app.

pub mod acp_adapter;
pub mod agent_loop;
pub mod agent_plugins_adapter;
pub mod agent_skills_adapter;
pub mod anthropic_provider;
pub mod approval;
pub mod artifact_upload;
pub mod attachments;
pub mod audit_log;
pub mod budget;
pub mod capability_truth;
pub mod ci;
pub mod claude_code_adapter;
pub mod codex_adapter;
pub mod compaction;
pub mod context_builder;
pub mod context_optimizer;
pub mod cost_estimation;
pub mod cursor_adapter;
pub mod deepseek_harness_adapter;
pub mod diagnostics;
pub mod durable_artifacts;
pub mod effects;
pub mod events;
pub mod execution;
pub mod extension_adapter;
pub mod extension_compat;
pub mod harness_api;
pub mod instruction_learning;
pub mod instructions;
pub mod ipc;
pub mod mcp_adapter;
pub mod mcp_live;
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
pub mod output_reducer;
pub mod plugins;
pub mod policy;
pub mod profile;
pub mod projection;
pub mod provider;
pub mod provider_protocol_adapter;
pub mod provider_registry;
pub mod provider_trait;
pub mod reference_store;
pub mod reference_tools;
pub mod remote;
pub mod rtk_adapter;
pub mod runtime;
#[cfg(test)]
mod security_runtime_pr;
pub mod service_contract;
pub mod service_provider;
pub mod storage;
pub mod supervisor;
pub mod tempo_importer;
pub mod tool_orchestrator;
pub mod tool_schema;
pub mod tools;
pub mod web_research;

pub use acp_adapter::AcpAdapter;
pub use agent_loop::{AgentLoop, AgentLoopError, ToolCall};
pub use agent_plugins_adapter::{AgentPluginsAdapter, PluginCommandEntry};
pub use agent_skills_adapter::AgentSkillsAdapter;
pub use anthropic_provider::AnthropicProvider;
pub use approval::{
    ApprovalDetail, ApprovalId, ApprovalRequest, ApprovalResolution, ApprovalResolver,
    ApprovalState, ScopeEstimate,
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
pub use capability_truth::{CapabilityEntry, CapabilityLevel, CapabilityTruthReport};
pub use ci::{
    CiBackend, CiError, CiProject, Job, JobStatus, LocalCiEvent, LocalGitlabBackend, LocalRun,
    Pipeline, PipelineStatus, RemoteGitlabBackend, Stage,
};
pub use claude_code_adapter::ClaudeCodeAdapter;
pub use codex_adapter::CodexAdapter;
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
pub use deepseek_harness_adapter::{
    DEEPSEEK_PROCESS_PROTOCOL, DeepSeekHarnessAdapter, DeepSeekHarnessManifest,
};
pub use diagnostics::{SubsystemHealth, SubsystemStatus};
pub use durable_artifacts::{
    ArtifactMeta as DurableArtifactMeta, ArtifactRef as DurableArtifactRef, DurableArtifactStore,
    default_artifact_root,
};
pub use effects::{
    AdmittedOperation, CapabilityVersion, DeferredEffect, EffectAdmission, EffectCapability,
    EffectDecision, EffectExecution, EffectSeam, NormalizedEffect, Sandbox,
};
pub use events::{
    AgentEvent, ApprovalEvent, BackendEvent, BudgetEvent, CompactionStructuralState,
    EVENT_SCHEMA_VERSION, Event, EventPayload, IntentEvent, NoticeEvent, PlanEvent, RetryEvent,
    RunEvent, SessionEvent, ToolEvent, ToolEventOutcome,
};
pub use execution::{
    MAX_PROCESS_OUTPUT_BYTES, MAX_PROCESS_PREVIEW_BYTES, ProcessExecution, ProcessExecutionError,
    ProcessExecutionRequest, ProcessOutput, PtySession, PtySessionError, PtySessionId,
    PtySessionManager, PtySessionRecord, PtySessionState, PtySessionStore, PtySessionStoreError,
    SqlitePtySessionStore,
};
pub use extension_adapter::{ExtensionAdapter, ExtensionRegistry};
pub use extension_compat::{
    AgentProfile, CanonicalModuleKind, CanonicalModuleSpec, CanonicalSkill, Command,
    CommandArgument, CommandHandler, CompatibilityMatrix, ExtensionSource, ImportCapability,
    ImportResult, Instruction, InstructionContext, InstructionPriority, McpCapabilities, McpModule,
    McpTransport, ToolHandler, ToolProvider as ExtensionToolProvider,
};
pub use harness_api::{Harness, redact_tool_outcome};
pub use instruction_learning::{
    InstructionLearning, LearningEvidence, ObservationKind, Proposal, ProposalLifecycle,
    ProposalTarget,
};
pub use instructions::{
    InstructionKind, InstructionReference, InstructionResolveError, InstructionResolver,
    InstructionScope, InstructionTokenEstimate, ResolveRequest, ResolvedInstructions,
};
pub use ipc::{IPC_CAPABILITIES, IPC_VERSION, IpcErrorCode, IpcRequest, IpcResponse};
pub use mcp_adapter::McpAdapter;
pub use mcp_live::{McpLiveBridge, McpLiveCallResult, McpLiveToolEntry};
pub use mock_provider::{MockProvider, MockStreamItem as MockProviderItem};
pub use observations::{
    DiffHunk, DiffObservation, DiffSource, PipelineJob, PipelineObservation, SearchMatch,
    SearchObservation, TestFailure, TestObservation,
};
pub use openai_compat_adapter::OpenAiCompatibleAdapter;
pub use openai_native_adapter::OpenAiNativeAdapter;
pub use openai_provider::{OpenAiProvider, RetryBudget as OpenAiRetryBudget};
pub use output_reducer::{OutputReducer, ReducedOutput, ReductionStrategy, TokenBudget};
pub use plugins::{CapabilityAvailability, CapabilityManifest, CapabilityRegistry};
pub use policy::{
    Action, ActionFingerprint, ActionKind, ActionOrigin, PolicyDecision, PolicyEngine,
    PolicySnapshot, PolicyVersion, SandboxScope,
};
pub use profile::{Profile, ProfileConfig, ServiceBinding, ServiceBindings};
pub use projection::{ProjectionError, SessionProjection, reduce};
pub use provider::{
    CredentialResolver, CredentialStrategy, NoCredentialResolver, OpenAiCompatibleProvider,
    ProviderError, ProviderHealth, ProviderMessage, ProviderProfile, RetryBudget,
};
pub use provider_protocol_adapter::{ProviderProtocolAdapter, ToolCallAssembler};
pub use provider_registry::ProviderRegistry;
pub use provider_trait::{FinishReason, ModelProvider, StreamEvent};
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
pub use runtime::{AgentRuntime, RuntimeError, RuntimeStatus};
pub use service_provider::{
    ExternalServiceHandle, ResolvedService, ServiceProvider, ServiceProviderKind, ServiceTrait,
};
pub use storage::{
    CheckpointInfo, EventStore, MemoryEventStore, SessionInfo, SqliteEventStore, StoreError,
};
pub use supervisor::{MockStreamingProvider, SessionSupervisor, SupervisorError};
pub use tempo_importer::{TempoImporter, TempoImporterConfig, TempoWorklog};
pub use tool_orchestrator::{
    OrchestratorError, ToolObservation, ToolOrchestrator, ToolOutcomeStatus, ToolRequest,
};
pub use tool_schema::{
    BuiltinToolSchema, ToolArgError, builtin_tool_schemas, canonical_tool_name, schema_for_tool,
    validate_arguments_with_schema, validate_tool_arguments,
};
pub use tools::{
    ARTIFACT_CHUNK_SIZE, ReadOnlyTool, ReadOnlyToolKind, ReadOnlyTools, ToolError, ToolOutcome,
    ToolProvenance, ToolResult,
};
