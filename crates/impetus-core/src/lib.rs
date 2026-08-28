//! Policy-centred local runtime for Agentic Terminal.
//!
//! The crate intentionally owns no GPUI or PTY state. It emits durable events,
//! makes permission decisions, and exposes small capability seams for the app.

pub mod approval;
pub mod budget;
pub mod ci;
pub mod effects;
pub mod events;
pub mod execution;
pub mod harness_api;
pub mod ipc;
pub mod plugins;
pub mod policy;
pub mod projection;
pub mod provider;
pub mod remote;
pub mod runtime;
pub mod storage;
pub mod supervisor;
pub mod tools;

pub use approval::{
    ApprovalDetail, ApprovalId, ApprovalRequest, ApprovalResolution, ApprovalResolver,
    ApprovalState, ScopeEstimate,
};
pub use budget::{BudgetChecker, BudgetConfig, BudgetError, BudgetState, ReasoningEffort};
pub use ci::{
    CiBackend, CiError, CiProject, Job, JobStatus, LocalCiEvent, LocalGitlabBackend, LocalRun,
    Pipeline, PipelineStatus, RemoteGitlabBackend, Stage,
};
pub use effects::{
    CapabilityVersion, DeferredEffect, EffectAdmission, EffectCapability, EffectDecision,
    EffectExecution, EffectSeam, NormalizedEffect, Sandbox,
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
pub use ipc::{IPC_CAPABILITIES, IPC_VERSION, IpcErrorCode, IpcRequest, IpcResponse};
pub use plugins::{CapabilityAvailability, CapabilityManifest, CapabilityRegistry};
pub use policy::{
    Action, ActionFingerprint, ActionKind, ActionOrigin, PolicyDecision, PolicyEngine,
    PolicySnapshot, PolicyVersion, SandboxScope,
};
pub use projection::{ProjectionError, SessionProjection, reduce};
pub use provider::{
    CredentialResolver, CredentialStrategy, NoCredentialResolver, OpenAiCompatibleProvider,
    ProviderError, ProviderHealth, ProviderProfile, RetryBudget,
};
pub use remote::{
    HostKeyFingerprint, HostKeyVerificationError, SSHApproval, SSHApprovalStore,
    SSHApprovalStoreError, SSHConnectionError, SSHConnectionRequest, SSHKeyReference, SSHProfile,
    SqliteSSHApprovalStore, SqliteTmuxSessionStore, TmuxError, TmuxSession, TmuxSessionId,
    TmuxSessionManager, TmuxSessionRecord, TmuxSessionRequest, TmuxSessionState, TmuxSessionStore,
    TmuxSessionStoreError,
};
pub use runtime::{AgentRuntime, RuntimeError, RuntimeStatus};
pub use storage::{EventStore, MemoryEventStore, SessionInfo, SqliteEventStore};
pub use supervisor::{MockStreamItem, MockStreamingProvider, SessionSupervisor, SupervisorError};
pub use tools::{
    ArtifactMeta, ArtifactRef, ArtifactStore, ReadOnlyTool, ReadOnlyToolKind, ReadOnlyTools,
    ToolError, ToolOutcome, ToolProvenance, ToolResult,
};
