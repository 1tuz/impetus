//! Policy-centred local runtime for Agentic Terminal.
//!
//! The crate intentionally owns no GPUI or PTY state. It emits durable events,
//! makes permission decisions, and exposes small capability seams for the app.

pub mod approval;
pub mod ci;
pub mod events;
pub mod ipc;
pub mod plugins;
pub mod policy;
pub mod projection;
pub mod runtime;
pub mod storage;
pub mod supervisor;

pub use approval::{ApprovalId, ApprovalRequest, ApprovalState};
pub use ci::{
    CiBackend, CiError, CiProject, Job, JobStatus, LocalCiEvent, LocalGitlabBackend, LocalRun,
    Pipeline, PipelineStatus, RemoteGitlabBackend, Stage,
};
pub use events::{
    AgentEvent, ApprovalEvent, EVENT_SCHEMA_VERSION, Event, EventPayload, IntentEvent, NoticeEvent,
    PlanEvent, RunEvent, SessionEvent, ToolEvent,
};
pub use ipc::{IPC_VERSION, IpcErrorCode, IpcRequest, IpcResponse};
pub use plugins::{CapabilityAvailability, CapabilityManifest, CapabilityRegistry};
pub use policy::{Action, ActionKind, ActionOrigin, PolicyDecision, PolicyEngine, SandboxScope};
pub use projection::{ProjectionError, SessionProjection, reduce};
pub use runtime::{AgentRuntime, RuntimeError, RuntimeStatus};
pub use storage::{EventStore, MemoryEventStore, SessionInfo, SqliteEventStore};
pub use supervisor::{MockStreamItem, MockStreamingProvider, SessionSupervisor, SupervisorError};
