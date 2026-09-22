//! PTY session management with durable state and bounded output.
//!
//! Daemon owns the PTY (`portable-pty`). Clients attach via IPC; no terminal
//! emulator lives here.
//!
//! ## Bounds
//! - Ring: [`MAX_PTY_RING_BYTES`] (256 KiB). Oldest bytes leave the ring first.
//! - Spill coalesce: overflow accumulates until [`PTY_SPILL_COALESCE_BYTES`]
//!   then one DurableArtifact is queued (not one artifact per tiny read).
//! - Pending spill refs: at most [`MAX_PTY_PENDING_SPILLS`]; oldest ref dropped
//!   when full (artifact bytes remain on disk until GC).
//!
//! ## Ownership
//! Every live PTY has a stable [`PtySession::owner_session_id`]. IPC ops must
//! present that session; cross-session input/terminate is denied.
//!
//! ## Security
//! - **User terminal** (IPC `PtyStart`, `ActionOrigin::User`): cwd must stay
//!   inside the session workspace (canonicalize + symlink-safe containment).
//! - **Agent PTY** (`ActionOrigin::Agent`): same cwd rule **plus** macOS
//!   Seatbelt wrap via the shared sandbox prepare path. Non-macOS agent spawn
//!   is fail-closed.

use crate::{
    Action, ActionKind, ActionOrigin, DurableArtifactRef, DurableArtifactStore, EffectAdmission,
    EffectSeam, NormalizedEffect,
};
use crate::{PtySessionRecord, PtySessionStore};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use thiserror::Error;
use uuid::Uuid;

/// Ring capacity for PTY output (bytes). Oldest bytes spill (or drop) on overflow.
pub const MAX_PTY_RING_BYTES: usize = 256 * 1024;

/// Coalesce overflow into one artifact once this many bytes accumulate.
pub const PTY_SPILL_COALESCE_BYTES: usize = 16 * 1024;

/// Max unread spill ArtifactRefs queued for `read_output` consumers.
pub const MAX_PTY_PENDING_SPILLS: usize = 4;

/// MIME for PTY overflow spill bodies (raw PTY bytes, not necessarily UTF-8).
const PTY_SPILL_CONTENT_TYPE: &str = "application/octet-stream";

/// Default max bytes returned by one [`PtySessionManager::read_output`] call.
pub const DEFAULT_PTY_READ_BYTES: usize = 16 * 1024;

/// Unique identifier for a PTY session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PtySessionId(pub u64);

pub use impetus_protocol::PtySessionState;

#[derive(Debug, Error)]
pub enum PtySessionError {
    #[error("policy denied PTY session: {0}")]
    PolicyDenied(String),
    #[error("approval required but not granted")]
    ApprovalRequired,
    #[error("session not found: {0:?}")]
    SessionNotFound(PtySessionId),
    #[error("pty {0:?} is owned by another session")]
    NotOwner(PtySessionId),
    #[error("session already running: {0:?}")]
    AlreadyRunning(PtySessionId),
    #[error("session not live (detached metadata only): {0:?}")]
    NotLive(PtySessionId),
    #[error("PTY working_dir escapes workspace: {0}")]
    UnsafeWorkingDir(String),
    #[error("PTY sandbox denied: {0}")]
    SandboxDenied(String),
    #[error("PTY spawn failed: {0}")]
    SpawnFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage error: {0}")]
    Storage(String),
}

/// PTY session lifecycle: spawn → attach/detach → terminate.
#[derive(Debug, Clone)]
pub struct PtySession {
    pub id: PtySessionId,
    /// Durable harness session that owns this PTY (event routing + ACL).
    pub owner_session_id: Uuid,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    pub env: Vec<(String, String)>,
    pub state: PtySessionState,
    pub origin: ActionOrigin,
    pub created_at_unix_ms: u64,
    pub cols: u16,
    pub rows: u16,
}

impl PtySession {
    pub fn new(
        id: PtySessionId,
        owner_session_id: Uuid,
        command: impl Into<String>,
        args: Vec<String>,
        working_dir: PathBuf,
        origin: ActionOrigin,
    ) -> Self {
        Self {
            id,
            owner_session_id,
            command: command.into(),
            args,
            working_dir,
            env: Vec::new(),
            state: PtySessionState::Starting,
            origin,
            created_at_unix_ms: now_unix_ms(),
            cols: 80,
            rows: 24,
        }
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn with_size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols.max(1);
        self.rows = rows.max(1);
        self
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.state,
            PtySessionState::Running { .. } | PtySessionState::Detached { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyOutputChunk {
    pub data: Vec<u8>,
    /// Bytes dropped from the ring since this session started (overflow).
    pub dropped_total: u64,
    pub eof: bool,
    /// Oldest unread overflow spill since the previous drain (when an artifact
    /// store is wired). Ring stays bounded regardless.
    pub spill_artifact: Option<DurableArtifactRef>,
}

struct OutputRing {
    data: VecDeque<u8>,
    capacity: usize,
    dropped_total: u64,
    /// Bytes awaiting coalesce into a DurableArtifactRef.
    spill_buffer: Vec<u8>,
    /// Overflow batches spilled to DurableArtifactStore, FIFO for read_output.
    pending_spills: VecDeque<DurableArtifactRef>,
}

impl OutputRing {
    fn new(capacity: usize) -> Self {
        Self {
            data: VecDeque::with_capacity(capacity.min(4096)),
            capacity,
            dropped_total: 0,
            spill_buffer: Vec::new(),
            pending_spills: VecDeque::new(),
        }
    }

    /// Push bytes; return any oldest bytes displaced by this call (may be empty).
    fn push_slice(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut spilled = Vec::new();
        for &byte in chunk {
            if self.data.len() >= self.capacity
                && let Some(old) = self.data.pop_front()
            {
                spilled.push(old);
                self.dropped_total = self.dropped_total.saturating_add(1);
            }
            self.data.push_back(byte);
        }
        spilled
    }

    /// Buffer overflow bytes; flush coalesced artifacts into pending queue.
    fn note_overflow(
        &mut self,
        spilled: &[u8],
        artifacts: Option<&DurableArtifactStore>,
        force: bool,
    ) {
        if !spilled.is_empty() {
            self.spill_buffer.extend_from_slice(spilled);
        }
        if self.spill_buffer.is_empty() {
            return;
        }
        loop {
            let threshold = PTY_SPILL_COALESCE_BYTES.min(self.capacity.max(1));
            let ready = force || self.spill_buffer.len() >= threshold;
            if !ready {
                break;
            }
            let take = if force {
                self.spill_buffer.len()
            } else {
                threshold.min(self.spill_buffer.len())
            };
            if take == 0 {
                break;
            }
            let chunk: Vec<u8> = self.spill_buffer.drain(..take).collect();
            if let Some(artifact) = spill_overflow_bytes(artifacts, &chunk) {
                self.record_spill(artifact);
            }
            if force {
                break;
            }
        }
    }

    fn record_spill(&mut self, artifact: DurableArtifactRef) {
        while self.pending_spills.len() >= MAX_PTY_PENDING_SPILLS {
            let _ = self.pending_spills.pop_front();
        }
        self.pending_spills.push_back(artifact);
    }

    fn take_spill(&mut self) -> Option<DurableArtifactRef> {
        self.pending_spills.pop_front()
    }

    fn drain(&mut self, max: usize) -> Vec<u8> {
        let n = max.min(self.data.len());
        self.data.drain(..n).collect()
    }
}

struct LivePty {
    master: Box<dyn MasterPty + Send>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    ring: Arc<Mutex<OutputRing>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    eof: Arc<AtomicBool>,
    /// Keeps Seatbelt session temp alive for agent-origin PTYs (macOS).
    _sandbox_keepalive: Option<super::sandbox::PtySandboxKeepAlive>,
}

impl LivePty {
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.killer.kill();
        if let Ok(mut child) = self.child.lock() {
            let _ = child.wait();
        }
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for LivePty {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.killer.kill();
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

/// PTY session manager coordinating policy, spawn, attach/detach, and durable storage.
pub struct PtySessionManager {
    seam: EffectSeam,
    sessions: Mutex<std::collections::HashMap<PtySessionId, PtySession>>,
    live: Mutex<std::collections::HashMap<PtySessionId, LivePty>>,
    next_id: Mutex<u64>,
    store: Mutex<Option<Arc<dyn PtySessionStore>>>,
    /// Optional durable store for ring-overflow spill. Absent → drop-only.
    artifacts: Mutex<Option<Arc<DurableArtifactStore>>>,
    /// Ring capacity for new sessions (tests may shrink; capped at MAX).
    ring_capacity: usize,
}

impl PtySessionManager {
    pub fn new(seam: EffectSeam) -> Self {
        Self {
            seam,
            sessions: Mutex::new(std::collections::HashMap::new()),
            live: Mutex::new(std::collections::HashMap::new()),
            next_id: Mutex::new(1),
            store: Mutex::new(None),
            artifacts: Mutex::new(None),
            ring_capacity: MAX_PTY_RING_BYTES,
        }
    }

    pub fn with_store(self, store: Arc<dyn PtySessionStore>) -> Self {
        self.set_store(store);
        self
    }

    /// Attach durable metadata store after construction (daemon data-root wire).
    ///
    /// Restart/resume: rows survive process restart; live PTY handles do not —
    /// client must re-`PtyStart` / attach after daemon restart.
    pub fn set_store(&self, store: Arc<dyn PtySessionStore>) {
        *self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(store);
    }

    /// True when a durable [`PtySessionStore`] is attached.
    pub fn has_store(&self) -> bool {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    /// Wire overflow spill into a durable artifact store (keeps ring bounded).
    pub fn with_artifacts(self, store: Arc<DurableArtifactStore>) -> Self {
        *self
            .artifacts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(store);
        self
    }

    /// Replace spill store after construction (Harness `with_artifact_root`).
    pub fn set_artifacts(&self, store: Arc<DurableArtifactStore>) {
        *self
            .artifacts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(store);
    }

    /// Shrink (or set) ring capacity for new spawns. Clamped to `1..=MAX_PTY_RING_BYTES`.
    pub fn with_ring_capacity(mut self, capacity: usize) -> Self {
        self.ring_capacity = capacity.clamp(1, MAX_PTY_RING_BYTES);
        self
    }

    /// Request a new PTY session through policy and effect seam (metadata only).
    pub fn request(
        &self,
        owner_session_id: Uuid,
        command: impl Into<String>,
        args: Vec<String>,
        working_dir: PathBuf,
        origin: ActionOrigin,
        intent_revision: u64,
    ) -> Result<(PtySessionId, EffectAdmission), PtySessionError> {
        let command = command.into();
        let summary = format!("PTY: {} {}", command, args.join(" "));
        let target = working_dir.display().to_string();

        let _action = Action {
            origin,
            kind: ActionKind::SpawnProcess,
            summary: summary.clone(),
            target: Some(target.clone()),
        };

        let effect = NormalizedEffect::process_spawn(origin, summary, target);
        let admission = self.seam.request(effect, intent_revision);

        let mut next_id = self
            .next_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session_id = PtySessionId(*next_id);
        *next_id += 1;

        let session = PtySession::new(
            session_id,
            owner_session_id,
            command,
            args,
            working_dir,
            origin,
        );
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_id, session.clone());
        self.persist(&session);

        Ok((session_id, admission))
    }

    /// Policy-gated start: request + spawn on Allow. Rolls back on deny/approval.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &self,
        owner_session_id: Uuid,
        command: impl Into<String>,
        args: Vec<String>,
        working_dir: PathBuf,
        origin: ActionOrigin,
        cols: u16,
        rows: u16,
        intent_revision: u64,
    ) -> Result<PtySession, PtySessionError> {
        let (session_id, admission) = self.request(
            owner_session_id,
            command,
            args,
            working_dir,
            origin,
            intent_revision,
        )?;

        {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(session) = sessions.get_mut(&session_id) {
                *session = session.clone().with_size(cols, rows);
            }
        }

        match admission {
            EffectAdmission::Allow(_) => {
                self.spawn(session_id)?;
                self.get_session(session_id)
                    .ok_or(PtySessionError::SessionNotFound(session_id))
            }
            EffectAdmission::NeedsApproval(_) => {
                self.forget(session_id);
                Err(PtySessionError::ApprovalRequired)
            }
            EffectAdmission::Deny { reason } => {
                self.forget(session_id);
                Err(PtySessionError::PolicyDenied(reason))
            }
        }
    }

    /// Deny ops when caller session is not the PTY owner.
    pub fn require_owner(
        &self,
        pty_id: PtySessionId,
        session_id: Uuid,
    ) -> Result<PtySession, PtySessionError> {
        let session = self
            .get_session(pty_id)
            .ok_or(PtySessionError::SessionNotFound(pty_id))?;
        if session.owner_session_id != session_id {
            return Err(PtySessionError::NotOwner(pty_id));
        }
        Ok(session)
    }

    /// Spawn PTY session after approval (or immediate Allow).
    pub fn spawn(&self, session_id: PtySessionId) -> Result<(), PtySessionError> {
        let (command, args, working_dir, env, cols, rows, origin, owner_workspace) = {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let session = sessions
                .get_mut(&session_id)
                .ok_or(PtySessionError::SessionNotFound(session_id))?;

            if session.is_running() {
                return Err(PtySessionError::AlreadyRunning(session_id));
            }

            (
                session.command.clone(),
                session.args.clone(),
                session.working_dir.clone(),
                session.env.clone(),
                session.cols,
                session.rows,
                session.origin,
                session.working_dir.clone(),
            )
        };

        if self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(&session_id)
        {
            return Err(PtySessionError::AlreadyRunning(session_id));
        }

        let artifacts = self
            .artifacts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let live = match open_live_pty(
            &command,
            &args,
            &working_dir,
            &env,
            cols,
            rows,
            self.ring_capacity,
            artifacts,
            origin,
            &owner_workspace,
        ) {
            Ok(live) => live,
            Err(error) => {
                self.set_state(
                    session_id,
                    PtySessionState::Failed {
                        reason: error.to_string(),
                    },
                );
                return Err(error);
            }
        };

        let pid = live
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .process_id()
            .unwrap_or(0);

        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_id, live);

        self.set_state(session_id, PtySessionState::Running { pid });
        Ok(())
    }

    pub fn get_session(&self, session_id: PtySessionId) -> Option<PtySession> {
        self.refresh_exit(session_id);
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&session_id)
            .cloned()
    }

    pub fn list_sessions(&self) -> Vec<PtySession> {
        let ids: Vec<_> = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .copied()
            .collect();
        for id in ids {
            self.refresh_exit(id);
        }
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// Mark session attached (Detached → Running). Live handle must still exist.
    pub fn attach(&self, session_id: PtySessionId) -> Result<PtySession, PtySessionError> {
        self.refresh_exit(session_id);
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = sessions
            .get_mut(&session_id)
            .ok_or(PtySessionError::SessionNotFound(session_id))?;

        match session.state {
            PtySessionState::Detached { pid } => {
                if !self
                    .live
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .contains_key(&session_id)
                {
                    return Err(PtySessionError::NotLive(session_id));
                }
                session.state = PtySessionState::Running { pid };
                let out = session.clone();
                drop(sessions);
                self.persist(&out);
                Ok(out)
            }
            PtySessionState::Running { .. } => Ok(session.clone()),
            _ => Err(PtySessionError::SessionNotFound(session_id)),
        }
    }

    pub fn write_input(
        &self,
        session_id: PtySessionId,
        data: &[u8],
    ) -> Result<(), PtySessionError> {
        self.refresh_exit(session_id);
        let live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let handle = live
            .get(&session_id)
            .ok_or(PtySessionError::NotLive(session_id))?;
        let mut writer = handle
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }

    pub fn read_output(
        &self,
        session_id: PtySessionId,
        max_bytes: usize,
    ) -> Result<PtyOutputChunk, PtySessionError> {
        self.refresh_exit(session_id);
        let max_bytes = max_bytes.clamp(1, MAX_PTY_RING_BYTES);
        let live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let handle = live
            .get(&session_id)
            .ok_or(PtySessionError::NotLive(session_id))?;
        let mut ring = handle
            .ring
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let data = ring.drain(max_bytes);
        let dropped_total = ring.dropped_total;
        let spill_artifact = ring.take_spill();
        let eof = handle.eof.load(Ordering::SeqCst) && ring.data.is_empty();
        Ok(PtyOutputChunk {
            data,
            dropped_total,
            eof,
            spill_artifact,
        })
    }

    pub fn resize(
        &self,
        session_id: PtySessionId,
        cols: u16,
        rows: u16,
    ) -> Result<(), PtySessionError> {
        let cols = cols.max(1);
        let rows = rows.max(1);
        {
            let live = self
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let handle = live
                .get(&session_id)
                .ok_or(PtySessionError::NotLive(session_id))?;
            handle
                .master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|error| PtySessionError::SpawnFailed(error.to_string()))?;
        }
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(session) = sessions.get_mut(&session_id) {
            session.cols = cols;
            session.rows = rows;
            let out = session.clone();
            drop(sessions);
            self.persist(&out);
        }
        Ok(())
    }

    /// Detach from session (keeps process running).
    pub fn detach(&self, session_id: PtySessionId) -> Result<(), PtySessionError> {
        self.refresh_exit(session_id);
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = sessions
            .get_mut(&session_id)
            .ok_or(PtySessionError::SessionNotFound(session_id))?;

        if let PtySessionState::Running { pid } = session.state {
            session.state = PtySessionState::Detached { pid };
            let out = session.clone();
            drop(sessions);
            self.persist(&out);
            Ok(())
        } else {
            Err(PtySessionError::SessionNotFound(session_id))
        }
    }

    /// Terminate session and drop live PTY.
    pub fn terminate(&self, session_id: PtySessionId) -> Result<(), PtySessionError> {
        let exit_code = {
            let mut live = self
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(mut handle) = live.remove(&session_id) {
                handle.shutdown();
                handle
                    .child
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .try_wait()
                    .ok()
                    .flatten()
                    .map(|status| {
                        if status.success() {
                            0
                        } else {
                            status.exit_code() as i32
                        }
                    })
            } else {
                None
            }
        };

        self.set_state(session_id, PtySessionState::Exited { exit_code });
        Ok(())
    }

    fn forget(&self, session_id: PtySessionId) {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&session_id);
        if let Some(mut live) = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&session_id)
        {
            live.shutdown();
        }
        if let Some(store) = self.store_snapshot() {
            let _ = block_on_pty_store(async move { store.delete_session(session_id).await });
        }
    }

    fn store_snapshot(&self) -> Option<Arc<dyn PtySessionStore>> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_state(&self, session_id: PtySessionId, state: PtySessionState) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(session) = sessions.get_mut(&session_id) {
            session.state = state.clone();
            let out = session.clone();
            drop(sessions);
            self.persist(&out);
            if let Some(store) = self.store_snapshot() {
                let _ =
                    block_on_pty_store(async move { store.update_state(session_id, &state).await });
            }
        }
    }

    fn persist(&self, session: &PtySession) {
        let Some(store) = self.store_snapshot() else {
            return;
        };
        let record = PtySessionRecord::from(session.clone());
        let _ = block_on_pty_store(async move { store.save_session(&record).await });
    }

    fn refresh_exit(&self, session_id: PtySessionId) {
        let finished = {
            let live = self
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(handle) = live.get(&session_id) else {
                return;
            };
            let mut child = handle
                .child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match child.try_wait() {
                Ok(Some(status)) => {
                    let code = if status.success() {
                        Some(0)
                    } else {
                        Some(status.exit_code() as i32)
                    };
                    Some(code)
                }
                _ => None,
            }
        };
        if let Some(exit_code) = finished {
            let mut live = self
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(mut handle) = live.remove(&session_id) {
                handle.stop.store(true, Ordering::SeqCst);
                if let Some(reader) = handle.reader.take() {
                    let _ = reader.join();
                }
            }
            self.set_state(session_id, PtySessionState::Exited { exit_code });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn open_live_pty(
    command: &str,
    args: &[String],
    working_dir: &Path,
    env: &[(String, String)],
    cols: u16,
    rows: u16,
    ring_capacity: usize,
    artifacts: Option<Arc<DurableArtifactStore>>,
    origin: ActionOrigin,
    workspace_root: &Path,
) -> Result<LivePty, PtySessionError> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| PtySessionError::SpawnFailed(error.to_string()))?;

    let (cmd, sandbox_keepalive) =
        build_pty_command(command, args, working_dir, env, origin, workspace_root)?;

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|error| PtySessionError::SpawnFailed(error.to_string()))?;
    let killer = child.clone_killer();

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| PtySessionError::SpawnFailed(error.to_string()))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| PtySessionError::SpawnFailed(error.to_string()))?;

    let capacity = ring_capacity.clamp(1, MAX_PTY_RING_BYTES);
    let ring = Arc::new(Mutex::new(OutputRing::new(capacity)));
    let stop = Arc::new(AtomicBool::new(false));
    let eof = Arc::new(AtomicBool::new(false));
    let ring_reader = Arc::clone(&ring);
    let stop_reader = Arc::clone(&stop);
    let eof_reader = Arc::clone(&eof);

    let reader_handle = std::thread::Builder::new()
        .name(format!("pty-reader-{command}"))
        .spawn(move || {
            let mut buf = [0u8; 4096];
            while !stop_reader.load(Ordering::Relaxed) {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut ring) = ring_reader.lock() {
                            let spilled = ring.push_slice(&buf[..n]);
                            ring.note_overflow(&spilled, artifacts.as_deref(), false);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            if let Ok(mut ring) = ring_reader.lock() {
                ring.note_overflow(&[], artifacts.as_deref(), true);
            }
            eof_reader.store(true, Ordering::SeqCst);
        })
        .map_err(|error| PtySessionError::SpawnFailed(error.to_string()))?;

    Ok(LivePty {
        master: pair.master,
        writer: Mutex::new(writer),
        child: Mutex::new(child),
        killer,
        ring,
        stop,
        reader: Some(reader_handle),
        eof,
        _sandbox_keepalive: sandbox_keepalive,
    })
}

fn build_pty_command(
    command: &str,
    args: &[String],
    working_dir: &Path,
    env: &[(String, String)],
    origin: ActionOrigin,
    workspace_root: &Path,
) -> Result<(CommandBuilder, Option<super::sandbox::PtySandboxKeepAlive>), PtySessionError> {
    match origin {
        ActionOrigin::User => {
            let mut cmd = CommandBuilder::new(command);
            for arg in args {
                cmd.arg(arg);
            }
            cmd.cwd(working_dir);
            for (key, value) in env {
                cmd.env(key, value);
            }
            Ok((cmd, None))
        }
        ActionOrigin::Agent => {
            #[cfg(target_os = "macos")]
            {
                let prepared = super::sandbox::prepare_pty_sandbox(
                    command,
                    args,
                    workspace_root,
                    working_dir,
                    env,
                    false,
                )
                .map_err(|e| PtySessionError::SandboxDenied(e.to_string()))?;
                let mut cmd = CommandBuilder::new(prepared.executable);
                for arg in &prepared.args {
                    cmd.arg(arg);
                }
                cmd.cwd(&prepared.working_dir);
                for (key, value) in &prepared.env {
                    cmd.env(key, value);
                }
                Ok((cmd, Some(prepared.keepalive)))
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (command, args, working_dir, env, workspace_root);
                Err(PtySessionError::SandboxDenied(
                    "agent PTY requires macOS Seatbelt; fail-closed on this platform".into(),
                ))
            }
        }
    }
}

/// Resolve PTY cwd under workspace with symlink-escape protection.
pub fn resolve_pty_working_dir(
    workspace_root: &Path,
    working_dir: Option<PathBuf>,
) -> Result<PathBuf, PtySessionError> {
    let root = workspace_root.canonicalize().map_err(|err| {
        PtySessionError::UnsafeWorkingDir(format!("canonicalize workspace: {err}"))
    })?;
    let Some(requested) = working_dir else {
        return Ok(root);
    };
    if requested.is_absolute() {
        let resolved = requested.canonicalize().map_err(|err| {
            PtySessionError::UnsafeWorkingDir(format!("{}: {err}", requested.display()))
        })?;
        if !resolved.starts_with(&root) {
            return Err(PtySessionError::UnsafeWorkingDir(
                requested.display().to_string(),
            ));
        }
        return Ok(resolved);
    }
    crate::workspace_files::resolve_workspace_path(&root, &requested)
        .map_err(|err| PtySessionError::UnsafeWorkingDir(err.to_string()))
}

/// Store overflow bytes when an artifact store is wired. No-op / None otherwise.
fn spill_overflow_bytes(
    artifacts: Option<&DurableArtifactStore>,
    spilled: &[u8],
) -> Option<DurableArtifactRef> {
    if spilled.is_empty() {
        return None;
    }
    let store = artifacts?;
    store
        .store_with_content_type(spilled, Some(PTY_SPILL_CONTENT_TYPE))
        .ok()
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after epoch")
        .as_millis() as u64
}

/// ponytail: PtySessionStore is async_trait over sync SQLite. Ceiling — sync
/// trait; upgrade when store grows real await points.
fn block_on_pty_store<T, F>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("pty store runtime")
            .block_on(fut),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyEngine, Sandbox, SandboxScope};
    use std::time::Duration;

    fn test_seam() -> EffectSeam {
        let workspace = std::env::temp_dir();
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.clone()));
        EffectSeam::with_sandbox(policy, Sandbox::workspace(workspace))
    }

    fn wait_output(manager: &PtySessionManager, id: PtySessionId, needle: &[u8]) -> Vec<u8> {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut collected = Vec::new();
        while std::time::Instant::now() < deadline {
            if let Ok(chunk) = manager.read_output(id, DEFAULT_PTY_READ_BYTES) {
                collected.extend_from_slice(&chunk.data);
                if collected.windows(needle.len()).any(|w| w == needle) {
                    return collected;
                }
                if chunk.eof {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        collected
    }

    #[test]
    fn pty_session_request_creates_session() {
        let manager = PtySessionManager::new(test_seam());

        let result = manager.request(
            Uuid::nil(),
            "bash",
            vec![],
            std::env::temp_dir(),
            ActionOrigin::Agent,
            1,
        );

        assert!(result.is_ok());
        let (session_id, admission) = result.unwrap();
        assert!(matches!(admission, EffectAdmission::NeedsApproval(_)));
        let session = manager.get_session(session_id);
        assert!(session.is_some());
        assert_eq!(session.unwrap().command, "bash");
    }

    #[test]
    fn pty_real_cat_echo_roundtrip() {
        let manager = PtySessionManager::new(test_seam());
        let session = manager
            .start(
                Uuid::nil(),
                "cat",
                vec![],
                std::env::temp_dir(),
                ActionOrigin::User,
                80,
                24,
                1,
            )
            .expect("start cat");

        assert!(matches!(
            session.state,
            PtySessionState::Running { pid } if pid != 0 && pid != 12345
        ));

        manager
            .write_input(session.id, b"hello-pty\n")
            .expect("write");
        let out = wait_output(&manager, session.id, b"hello-pty");
        assert!(
            out.windows(b"hello-pty".len())
                .any(|window| window == b"hello-pty"),
            "expected echo in {out:?}"
        );

        manager.terminate(session.id).expect("terminate");
        let ended = manager.get_session(session.id).expect("session");
        assert!(matches!(ended.state, PtySessionState::Exited { .. }));
        assert!(!ended.is_running());
    }

    #[test]
    fn pty_sleep_terminate() {
        let manager = PtySessionManager::new(test_seam());
        let session = manager
            .start(
                Uuid::nil(),
                "sleep",
                vec!["30".into()],
                std::env::temp_dir(),
                ActionOrigin::User,
                40,
                12,
                1,
            )
            .expect("start sleep");

        assert!(session.is_running());
        manager.resize(session.id, 100, 30).expect("resize");
        manager.detach(session.id).expect("detach");
        let detached = manager.get_session(session.id).expect("session");
        assert!(matches!(detached.state, PtySessionState::Detached { .. }));

        manager.attach(session.id).expect("reattach");
        manager.terminate(session.id).expect("terminate");
        let ended = manager.get_session(session.id).expect("session");
        assert!(matches!(ended.state, PtySessionState::Exited { .. }));
    }

    #[test]
    fn pty_output_ring_is_bounded() {
        let mut ring = OutputRing::new(8);
        let spilled = ring.push_slice(b"abcdefghij");
        assert_eq!(spilled, b"ab");
        assert_eq!(ring.data.len(), 8);
        assert_eq!(ring.dropped_total, 2);
        let drained = ring.drain(4);
        assert_eq!(drained, b"cdef");
        assert_eq!(ring.data.len(), 4);
    }

    #[test]
    fn pty_ring_overflow_spills_to_artifact_store() {
        let dir = tempfile::tempdir().expect("temp");
        let store = DurableArtifactStore::open(dir.path()).expect("open");
        let mut ring = OutputRing::new(8);

        let spilled = ring.push_slice(b"abcdefghij");
        assert_eq!(spilled, b"ab");
        let artifact = spill_overflow_bytes(Some(&store), &spilled).expect("spill");
        ring.record_spill(artifact.clone());

        assert_eq!(ring.data.len(), 8);
        assert_eq!(ring.dropped_total, 2);
        let taken = ring.take_spill().expect("pending spill");
        assert_eq!(taken, artifact);
        assert_eq!(store.read(&artifact.id).expect("read"), b"ab");
        assert!(ring.take_spill().is_none());
    }

    #[test]
    fn pty_ring_overflow_without_store_still_drops() {
        let mut ring = OutputRing::new(4);
        let spilled = ring.push_slice(b"01234567");
        assert_eq!(spilled.len(), 4);
        assert!(spill_overflow_bytes(None, &spilled).is_none());
        assert_eq!(ring.data.len(), 4);
        assert_eq!(ring.dropped_total, 4);
        assert!(ring.take_spill().is_none());
    }

    #[test]
    fn pty_live_overflow_spills_and_keeps_ring_bounded() {
        let dir = tempfile::tempdir().expect("temp");
        let store = Arc::new(DurableArtifactStore::open(dir.path()).expect("open"));
        // Tiny ring so a short cat echo overflows without PTY flow-control stalls.
        let manager = PtySessionManager::new(test_seam())
            .with_artifacts(Arc::clone(&store))
            .with_ring_capacity(32);

        let session = manager
            .start(
                Uuid::nil(),
                "cat",
                vec![],
                std::env::temp_dir(),
                ActionOrigin::User,
                80,
                24,
                1,
            )
            .expect("start cat");

        // 80 bytes of payload → ring 32 → spill of oldest bytes.
        let payload = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz!!!!\n";
        manager
            .write_input(session.id, payload)
            .expect("write payload");
        // Let the reader thread fill the ring and spill before we drain.
        std::thread::sleep(Duration::from_millis(200));

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut saw_spill = false;
        let mut ring_bytes = 0usize;
        while std::time::Instant::now() < deadline && !saw_spill {
            if let Ok(chunk) = manager.read_output(session.id, DEFAULT_PTY_READ_BYTES) {
                ring_bytes += chunk.data.len();
                if let Some(artifact) = chunk.spill_artifact {
                    let body = store.read(&artifact.id).expect("spill body");
                    assert!(!body.is_empty());
                    assert!(chunk.dropped_total > 0);
                    assert!(
                        body.iter().all(|b| payload.contains(b)),
                        "spill should be prefix of payload"
                    );
                    saw_spill = true;
                }
                if chunk.eof {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(saw_spill, "expected overflow spill ArtifactRef");
        // Drain remaining ring; collected live + spilled must cover payload.
        while let Ok(chunk) = manager.read_output(session.id, DEFAULT_PTY_READ_BYTES) {
            ring_bytes += chunk.data.len();
            if let Some(artifact) = &chunk.spill_artifact {
                let _ = store.read(&artifact.id);
            }
            if chunk.data.is_empty() && chunk.spill_artifact.is_none() {
                break;
            }
        }
        assert!(
            ring_bytes <= payload.len(),
            "collected ring bytes must not exceed payload"
        );

        manager.terminate(session.id).expect("terminate");
    }

    #[test]
    fn sentinel_pty_cross_session_input_denied() {
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let manager = PtySessionManager::new(test_seam());
        let session = manager
            .start(
                owner,
                "cat",
                vec![],
                std::env::temp_dir(),
                ActionOrigin::User,
                80,
                24,
                1,
            )
            .expect("start");
        assert!(matches!(
            manager.require_owner(session.id, other),
            Err(PtySessionError::NotOwner(_))
        ));
        assert!(manager.require_owner(session.id, owner).is_ok());
        manager.terminate(session.id).ok();
    }

    #[test]
    fn sentinel_pty_cwd_rejects_escape() {
        let dir = tempfile::tempdir().expect("temp");
        let root = dir.path().canonicalize().unwrap();
        let outside = std::env::temp_dir().join(format!("pty-escape-{}", Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&outside);
        let err = resolve_pty_working_dir(&root, Some(outside)).expect_err("outside");
        assert!(matches!(err, PtySessionError::UnsafeWorkingDir(_)));
        let err = resolve_pty_working_dir(&root, Some(PathBuf::from("../.."))).expect_err("dotdot");
        assert!(matches!(err, PtySessionError::UnsafeWorkingDir(_)));
        let ok = resolve_pty_working_dir(&root, None).expect("default");
        assert_eq!(ok, root);
    }

    #[test]
    fn sentinel_pty_reattach_preserves_owner_and_output() {
        let owner = Uuid::new_v4();
        let manager = PtySessionManager::new(test_seam());
        let session = manager
            .start(
                owner,
                "cat",
                vec![],
                std::env::temp_dir(),
                ActionOrigin::User,
                80,
                24,
                1,
            )
            .expect("start");
        manager
            .write_input(session.id, b"reattach-ok\n")
            .expect("write");
        manager.detach(session.id).expect("detach");
        let again = manager.attach(session.id).expect("attach");
        assert_eq!(again.owner_session_id, owner);
        let out = wait_output(&manager, session.id, b"reattach-ok");
        assert!(
            out.windows(b"reattach-ok".len())
                .any(|w| w == b"reattach-ok")
        );
        manager.terminate(session.id).ok();
    }

    #[test]
    fn sentinel_pty_pending_spills_bounded() {
        let mut ring = OutputRing::new(4);
        for i in 0..20 {
            let art = DurableArtifactRef {
                id: format!("art-{i}"),
                byte_count: 1,
            };
            ring.record_spill(art);
        }
        assert!(ring.pending_spills.len() <= MAX_PTY_PENDING_SPILLS);
    }

    #[test]
    fn pty_agent_start_requires_approval() {
        let manager = PtySessionManager::new(test_seam());
        let err = manager
            .start(
                Uuid::nil(),
                "cat",
                vec![],
                std::env::temp_dir(),
                ActionOrigin::Agent,
                80,
                24,
                1,
            )
            .expect_err("agent must need approval");
        assert!(matches!(err, PtySessionError::ApprovalRequired));
    }
}
