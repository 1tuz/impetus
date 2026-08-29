//! Policy-centred local runtime for Impetus.
//!
//! The crate intentionally owns no native GUI or PTY state. It emits durable events,
//! makes permission decisions, and exposes small capability seams for the app.

pub mod agent_loop;
pub mod approval;
pub mod attachments;
pub mod budget;
pub mod ci;
pub mod effects;
pub mod events;
pub mod execution;
pub mod harness_api;
pub mod instruction_learning;
pub mod instructions;
pub mod ipc;
pub mod mock_provider;
pub mod openai_compat_adapter;
pub mod openai_provider;
pub mod plugins;
pub mod policy;
pub mod projection;
pub mod provider;
pub mod provider_registry;
pub mod provider_trait;
pub mod remote;
pub mod runtime;
pub mod storage;
pub mod supervisor;
pub mod tool_orchestrator;
pub mod tools;

pub use agent_loop::{AgentLoop, AgentLoopError, ToolCall};
pub use approval::{
    ApprovalDetail, ApprovalId, ApprovalRequest, ApprovalResolution, ApprovalResolver,
    ApprovalState, ScopeEstimate,
};
pub use attachments::{Attachment, AttachmentError, AttachmentStore, StoreStats};
pub use budget::{BudgetChecker, BudgetConfig, BudgetError, BudgetState, ReasoningEffort};
pub use ci::{
    CiBackend, CiError, CiProject, Job, JobStatus, LocalCiEvent, LocalGitlabBackend, LocalRun,
    Pipeline, PipelineStatus, RemoteGitlabBackend, Stage,
};
pub use effects::{
    AdmittedOperation, CapabilityVersion, DeferredEffect, EffectAdmission, EffectCapability,
    EffectDecision, EffectExecution, EffectSeam, NormalizedEffect, Sandbox,
};
pub use events::{
    AgentEvent, ApprovalEvent, BackendEvent, BudgetEvent, EVENT_SCHEMA_VERSION, Event,
    EventPayload, IntentEvent, NoticeEvent, PlanEvent, RunEvent, SessionEvent, ToolEvent,
};
pub use execution::{
    ProcessExecution, ProcessExecutionError, ProcessExecutionRequest, ProcessOutput, PtySession,
    PtySessionError, PtySessionId, PtySessionManager, PtySessionRecord, PtySessionState,
    PtySessionStore, PtySessionStoreError, SqlitePtySessionStore,
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
pub use mock_provider::{MockProvider, MockStreamItem as MockProviderItem};
pub use openai_compat_adapter::OpenAiCompatibleAdapter;
pub use openai_provider::{OpenAiProvider, RetryBudget as OpenAiRetryBudget};
pub use plugins::{CapabilityAvailability, CapabilityManifest, CapabilityRegistry};
pub use policy::{
    Action, ActionFingerprint, ActionKind, ActionOrigin, PolicyDecision, PolicyEngine,
    PolicySnapshot, PolicyVersion, SandboxScope,
};
pub use projection::{ProjectionError, SessionProjection, reduce};
pub use provider::{
    CredentialResolver, CredentialStrategy, NoCredentialResolver, OpenAiCompatibleProvider,
    ProviderError, ProviderHealth, ProviderMessage, ProviderProfile, RetryBudget,
};
pub use provider_registry::ProviderRegistry;
pub use provider_trait::ModelProvider;
pub use remote::{
    HostKeyFingerprint, HostKeyVerificationError, SSHApproval, SSHApprovalStore,
    SSHApprovalStoreError, SSHConnectionError, SSHConnectionRequest, SSHKeyReference, SSHProfile,
    SftpError, SftpFileInfo, SftpOperation, SftpOperationRequest, SftpResult, SftpSession,
    SftpSessionManager, SqliteSSHApprovalStore, SqliteTmuxSessionStore, TmuxError, TmuxSession,
    TmuxSessionId, TmuxSessionManager, TmuxSessionRecord, TmuxSessionRequest, TmuxSessionState,
    TmuxSessionStore, TmuxSessionStoreError,
};
pub use runtime::{AgentRuntime, RuntimeError, RuntimeStatus};
pub use storage::{EventStore, MemoryEventStore, SessionInfo, SqliteEventStore};
pub use supervisor::{MockStreamItem, MockStreamingProvider, SessionSupervisor, SupervisorError};
pub use tool_orchestrator::{
    OrchestratorError, ToolObservation, ToolOrchestrator, ToolOutcomeStatus, ToolRequest,
};
pub use tools::{
    ArtifactMeta, ArtifactRef, ArtifactStore, ReadOnlyTool, ReadOnlyToolKind, ReadOnlyTools,
    ToolError, ToolOutcome, ToolProvenance, ToolResult,
};
