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
mod storage;

pub use process::{
    ProcessExecution, ProcessExecutionError, ProcessExecutionRequest, ProcessOutput,
};
pub use pty::{PtySession, PtySessionError, PtySessionId, PtySessionManager, PtySessionState};
pub use storage::{PtySessionRecord, PtySessionStore, PtySessionStoreError, SqlitePtySessionStore};
