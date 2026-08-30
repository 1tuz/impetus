//! Versioned protocol DTOs exposed through the client boundary.
//!
//! Presentation crates should import these types from `impetus-client`, not
//! depend on `impetus-core` directly. The daemon remains the sole authority;
//! these are transport/event data shapes only.

pub use impetus_core::{
    AgentEvent, ApprovalEvent, ApprovalState, BackendEvent, BudgetEvent, Event, EventPayload,
    IpcRequest, IpcResponse, NoticeEvent, RetryEvent, RunEvent, SessionEvent, ToolEvent,
};
