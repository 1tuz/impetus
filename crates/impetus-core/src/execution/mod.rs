//! Controlled process and PTY execution with durable session state.
//!
//! This module implements v0.6 task 2: controlled process/PTY execution.
//! - ProcessExecutionRequest with policy check and sandbox admission
//! - PTY session lifecycle: spawn, attach, detach, terminate
//! - Bounded output capture with artifact storage
//! - Durable session state survives harness restart
//! - Fail-closed: execution happens only after policy Allow or exact approval

mod process;
mod pty;
mod sandbox;
mod storage;

pub use process::{
    MAX_PROCESS_OUTPUT_BYTES, MAX_PROCESS_PREVIEW_BYTES, ProcessExecution, ProcessExecutionError,
    ProcessExecutionRequest, ProcessOutput,
};
pub use pty::{
    DEFAULT_PTY_READ_BYTES, MAX_PTY_RING_BYTES, PtyOutputChunk, PtySession, PtySessionError,
    PtySessionId, PtySessionManager, PtySessionState,
};
pub use sandbox::{
    MacosSeatbeltSandbox, PreparedSandboxCommand, SandboxCommandRequest, SandboxDecision,
    SandboxDecisionState, SandboxError, SandboxProvider, UnavailableSandboxProvider,
    production_sandbox_provider,
};
pub use storage::{PtySessionRecord, PtySessionStore, PtySessionStoreError, SqlitePtySessionStore};
