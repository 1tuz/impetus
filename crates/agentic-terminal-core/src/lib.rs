//! Policy-centred local runtime for Agentic Terminal.
//!
//! The crate intentionally owns no GPUI or PTY state. It emits durable events,
//! makes permission decisions, and exposes small capability seams for the app.

pub mod approval;
pub mod events;
pub mod plugins;
pub mod policy;
pub mod runtime;
pub mod storage;

pub use approval::{ApprovalId, ApprovalRequest, ApprovalState};
pub use events::{Event, EventKind};
pub use plugins::{CapabilityAvailability, CapabilityManifest, CapabilityRegistry};
pub use policy::{Action, ActionKind, ActionOrigin, PolicyDecision, PolicyEngine, SandboxScope};
pub use runtime::{AgentRuntime, RuntimeError, RuntimeStatus};
pub use storage::{EventStore, MemoryEventStore, SqliteEventStore};
