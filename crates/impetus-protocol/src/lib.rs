//! Shared Impetus IPC and durable-event wire types.
//!
//! This crate is intentionally runtime-free: no rusqlite, reqwest, Harness,
//! process/PTY managers, or SQLite-backed stores. Downstream crates
//! (`impetus-core`, `impetus-client`) re-export these types for compatibility.

pub mod events;
pub mod ipc;
pub mod types;

pub use events::{
    AgentEvent, ApprovalEvent, BackendEvent, BudgetEvent, ChildEvent, CommandEvent,
    CompactionStructuralState, EVENT_SCHEMA_VERSION, Event, EventPayload, IntentEvent,
    MAX_ACTIVITY_PREVIEW_CHARS, NoticeEvent, PlanEvent, PtyEvent, RetryEvent, RunEvent,
    SandboxEvent, SandboxPrepareState, SessionEvent, ToolEvent, ToolEventOutcome,
    bound_activity_preview, legacy_payload,
};
pub use ipc::{
    IPC_CAPABILITIES, IPC_EVENTS_FRAME_BUDGET, IPC_MIN_SUPPORTED, IPC_VERSION, IpcErrorCode,
    IpcRequest, IpcResponse, MAX_IPC_LINE_BYTES, capability_allows, negotiate_ipc_version,
    required_capability, trim_events_to_ipc_frame, validate_request_on_wire,
    validate_response_on_wire,
};
pub use types::*;
