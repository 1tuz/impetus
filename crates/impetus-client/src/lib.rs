//! Client contract for the Impetus harness.
//!
//! The client never owns session history, agent state, policy or durable
//! events: those live in the harness daemon. A client is a *view + command*
//! surface. After it closes, the running agent keeps going; relaunching
//! reconnects through [`HarnessClient`].
//!
//! Two transports implement the same [`HarnessClient`] trait:
//! - [`InMemoryTransport`] drives a [`Harness`] directly (tests, embedded use).
//! - [`UnixSocketTransport`] speaks the versioned line-JSON IPC over a Unix
//!   socket (the real daemon).

use anyhow::{Result, bail};
use impetus_core::{
    Event, EventStore, Harness, IPC_CAPABILITIES, IPC_VERSION, IpcRequest, IpcResponse,
    PolicyEngine,
};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

pub mod protocol;
pub mod unix;

pub use unix::UnixSocketTransport;

/// A dedicated event connection. It owns only its sequence cursor; durable
/// history remains in the harness event store.
pub trait EventSubscription: Send {
    /// Wait for the next non-empty batch of durable events.
    fn next_events(&mut self) -> Pin<Box<dyn Future<Output = Result<Vec<Event>>> + Send + '_>>;
}

/// Transport-neutral client contract.
///
/// Every method is a single request/response round-trip except where the
/// harness streams events. Implementors decide how bytes move; the contract
/// stays stable so TUI and CLI share one surface.
#[allow(async_fn_in_trait)]
pub trait HarnessClient: Send + Sync {
    /// Negotiate the protocol version. Returns the harness capabilities or an
    /// [`IpcResponse::Incompatible`] the caller must treat as a hard stop.
    async fn hello(&self) -> Result<IpcResponse>;

    /// Send a typed request and await its response (low-level).
    async fn request(&self, request: IpcRequest) -> Result<IpcResponse>;

    /// Create a durable session owned by the harness.
    async fn create_session(&self, workspace_root: PathBuf) -> Result<uuid::Uuid> {
        match self
            .request(IpcRequest::CreateSession { workspace_root })
            .await?
        {
            IpcResponse::Session { session_id, .. } => Ok(session_id),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// Reattach to an existing durable session after a client restart.
    async fn resume_session(&self, session_id: uuid::Uuid) -> Result<impetus_core::RuntimeStatus> {
        match self.request(IpcRequest::Attach { session_id }).await? {
            IpcResponse::Session { status, .. } => Ok(status),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// List all durable sessions with branch metadata.
    async fn list_sessions(&self) -> Result<Vec<impetus_core::SessionInfo>> {
        match self.request(IpcRequest::ListSessions).await? {
            IpcResponse::Sessions { sessions } => Ok(sessions),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// Fork a session at a logical sequence into a new shared-prefix branch.
    async fn fork_session(
        &self,
        session_id: uuid::Uuid,
        up_to_sequence: u64,
    ) -> Result<uuid::Uuid> {
        match self
            .request(IpcRequest::ForkSession {
                session_id,
                up_to_sequence,
            })
            .await?
        {
            IpcResponse::Session { session_id, .. } => Ok(session_id),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// Create a durable named checkpoint (restore creates a new branch).
    async fn create_checkpoint(
        &self,
        session_id: uuid::Uuid,
        name: String,
        sequence: Option<u64>,
    ) -> Result<impetus_core::CheckpointInfo> {
        match self
            .request(IpcRequest::CreateCheckpoint {
                session_id,
                name,
                sequence,
            })
            .await?
        {
            IpcResponse::Checkpoint { checkpoint } => Ok(checkpoint),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// List durable checkpoints for a session.
    async fn list_checkpoints(
        &self,
        session_id: uuid::Uuid,
    ) -> Result<Vec<impetus_core::CheckpointInfo>> {
        match self
            .request(IpcRequest::ListCheckpoints { session_id })
            .await?
        {
            IpcResponse::Checkpoints { checkpoints } => Ok(checkpoints),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// Restore a checkpoint as a new shared-prefix session branch.
    async fn restore_checkpoint(&self, checkpoint_id: uuid::Uuid) -> Result<uuid::Uuid> {
        match self
            .request(IpcRequest::RestoreCheckpoint { checkpoint_id })
            .await?
        {
            IpcResponse::Session { session_id, .. } => Ok(session_id),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// Submit a user message. The harness, not the client, starts the run.
    async fn send_message(
        &self,
        session_id: uuid::Uuid,
        text: String,
    ) -> Result<impetus_core::RuntimeStatus> {
        self.send_message_with_artifact(session_id, text, None)
            .await
    }

    /// Submit a user message with an optional durable artifact (large paste).
    ///
    /// When `artifact` is set, only the compact `text` label and the ref enter
    /// durable events — never the raw pasted body.
    async fn send_message_with_artifact(
        &self,
        session_id: uuid::Uuid,
        text: String,
        artifact: Option<impetus_core::DurableArtifactRef>,
    ) -> Result<impetus_core::RuntimeStatus> {
        self.send_message_with_intent(
            session_id,
            text,
            artifact,
            impetus_core::UserPromptIntent::Prompt,
        )
        .await
    }

    /// Submit typed Prompt / Steer / FollowUp (origin stays user; no policy bypass).
    async fn send_message_with_intent(
        &self,
        session_id: uuid::Uuid,
        text: String,
        artifact: Option<impetus_core::DurableArtifactRef>,
        intent: impetus_core::UserPromptIntent,
    ) -> Result<impetus_core::RuntimeStatus> {
        match self
            .request(IpcRequest::Prompt {
                session_id,
                text,
                artifact,
                intent,
            })
            .await?
        {
            IpcResponse::Status { status, .. } => Ok(status),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// Inspect the harness-owned transient instruction projection.
    async fn get_context(
        &self,
        session_id: uuid::Uuid,
    ) -> Result<impetus_core::ResolvedInstructions> {
        match self.request(IpcRequest::Context { session_id }).await? {
            IpcResponse::Context { context, .. } => Ok(context),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// Set the daemon-owned execution mode for a session (durable event).
    async fn set_execution_mode(
        &self,
        session_id: uuid::Uuid,
        mode: impetus_core::ExecutionMode,
    ) -> Result<impetus_core::ExecutionMode> {
        match self
            .request(IpcRequest::SetExecutionMode { session_id, mode })
            .await?
        {
            IpcResponse::ExecutionMode { mode, .. } => Ok(mode),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// Read the confirmed daemon execution mode (defaults to Ask).
    async fn get_execution_mode(
        &self,
        session_id: uuid::Uuid,
    ) -> Result<impetus_core::ExecutionMode> {
        match self
            .request(IpcRequest::GetExecutionMode { session_id })
            .await?
        {
            IpcResponse::ExecutionMode { mode, .. } => Ok(mode),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// Stop at the next safe runtime boundary.
    async fn cancel(&self, session_id: uuid::Uuid) -> Result<impetus_core::RuntimeStatus> {
        match self.request(IpcRequest::Cancel { session_id }).await? {
            IpcResponse::Status { status, .. } => Ok(status),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// Execute a read-only tool (bounded filesystem operation).
    async fn run_tool(
        &self,
        session_id: uuid::Uuid,
        kind: impetus_core::ReadOnlyToolKind,
        target: String,
        pattern: Option<String>,
    ) -> Result<impetus_core::ToolOutcome> {
        match self
            .request(IpcRequest::Tool {
                session_id,
                kind,
                target,
                pattern,
            })
            .await?
        {
            IpcResponse::ToolResult { outcome, .. } => Ok(outcome),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// Resolve a pending approval.
    async fn resolve_approval(
        &self,
        session_id: uuid::Uuid,
        approval_id: uuid::Uuid,
        accepted: bool,
    ) -> Result<()> {
        match self
            .request(IpcRequest::ResolveApproval {
                session_id,
                approval_id,
                accepted,
            })
            .await?
        {
            IpcResponse::ApprovalResolved { .. } => Ok(()),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected response: {response:?}"),
        }
    }

    /// Upload bytes via chunked IPC into the durable artifact store.
    ///
    /// Returns only an [`impetus_core::DurableArtifactRef`]; the raw body never
    /// enters durable session events.
    async fn upload_artifact(
        &self,
        session_id: uuid::Uuid,
        bytes: &[u8],
        content_type: Option<String>,
    ) -> Result<impetus_core::DurableArtifactRef> {
        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
        use impetus_core::MAX_ARTIFACT_UPLOAD_CHUNK_BYTES;

        let begun = self
            .request(IpcRequest::BeginArtifactUpload {
                session_id,
                declared_bytes: Some(bytes.len()),
                content_type,
            })
            .await?;
        let upload_id = match begun {
            IpcResponse::ArtifactUploadBegun { upload_id, .. } => upload_id,
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected begin response: {response:?}"),
        };

        for (seq, chunk) in bytes.chunks(MAX_ARTIFACT_UPLOAD_CHUNK_BYTES).enumerate() {
            match self
                .request(IpcRequest::AppendArtifactChunk {
                    upload_id,
                    seq: seq as u64,
                    data_b64: BASE64.encode(chunk),
                })
                .await?
            {
                IpcResponse::ArtifactChunkAccepted { .. } => {}
                IpcResponse::Error { message, .. } => {
                    let _ = self
                        .request(IpcRequest::AbortArtifactUpload { upload_id })
                        .await;
                    bail!(message);
                }
                response => bail!("unexpected append response: {response:?}"),
            }
        }

        match self
            .request(IpcRequest::FinishArtifactUpload { upload_id })
            .await?
        {
            IpcResponse::ArtifactStored { artifact } => Ok(artifact),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected finish response: {response:?}"),
        }
    }

    /// Open a dedicated event connection. Reconnect uses the last rendered
    /// sequence, so it receives a durable backfill without duplicate history.
    async fn subscribe_live(
        &self,
        session_id: uuid::Uuid,
        after_sequence: u64,
    ) -> Result<Box<dyn EventSubscription>>;
}

/// In-process transport backed by a [`Harness`].
///
/// Used by client tests and embedded front-ends that run the harness in the
/// same process. No socket, no serialization; the contract is exercised exactly
/// as the Unix transport would, with the same `Harness` dispatch path.
pub struct InMemoryTransport {
    harness: Arc<Harness>,
    store: Arc<dyn EventStore>,
}

impl InMemoryTransport {
    pub fn new(store: Arc<dyn EventStore>, policy: PolicyEngine) -> Self {
        Self {
            harness: Arc::new(Harness::new(store.clone(), policy)),
            store,
        }
    }

    /// Build transport with a custom durable artifact root (tests).
    pub fn with_artifact_root(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        artifact_root: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            harness: Arc::new(
                Harness::new(store.clone(), policy).with_artifact_root(artifact_root),
            ),
            store,
        }
    }

    /// Access the underlying harness (shared ownership with the transport).
    pub fn harness(&self) -> Arc<Harness> {
        self.harness.clone()
    }
}

impl HarnessClient for InMemoryTransport {
    async fn hello(&self) -> Result<IpcResponse> {
        Ok(self.harness.handle(IpcRequest::Hello {
            version: IPC_VERSION,
            capabilities: IPC_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect(),
        }))
    }

    async fn request(&self, request: IpcRequest) -> Result<IpcResponse> {
        Ok(self.harness.handle(request))
    }

    async fn subscribe_live(
        &self,
        session_id: uuid::Uuid,
        after_sequence: u64,
    ) -> Result<Box<dyn EventSubscription>> {
        match self.harness.handle(IpcRequest::Attach { session_id }) {
            IpcResponse::Session { .. } => {}
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected attach response: {response:?}"),
        }
        Ok(Box::new(InMemoryEventSubscription {
            store: self.store.clone(),
            session_id,
            after_sequence,
            notification_receiver: self.store.subscribe_notifications(),
        }))
    }
}

struct InMemoryEventSubscription {
    store: Arc<dyn EventStore>,
    session_id: uuid::Uuid,
    after_sequence: u64,
    notification_receiver: tokio::sync::broadcast::Receiver<(uuid::Uuid, u64)>,
}

impl EventSubscription for InMemoryEventSubscription {
    fn next_events(&mut self) -> Pin<Box<dyn Future<Output = Result<Vec<Event>>> + Send + '_>> {
        Box::pin(async move {
            loop {
                let events = self
                    .store
                    .list(self.session_id)?
                    .into_iter()
                    .filter(|event| event.sequence > self.after_sequence)
                    .collect::<Vec<_>>();
                if let Some(last) = events.last() {
                    self.after_sequence = last.sequence;
                    return Ok(events);
                }
                // Wait for notification for our session
                loop {
                    match self.notification_receiver.recv().await {
                        Ok((notified_session_id, _sequence))
                            if notified_session_id == self.session_id =>
                        {
                            break;
                        }
                        Ok(_) => continue, // Different session
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // Missed some notifications, check store immediately
                            break;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            bail!("notification channel closed");
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impetus_core::{MemoryEventStore, PolicyEngine, ReadOnlyToolKind, SandboxScope};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    fn harness_policy() -> PolicyEngine {
        PolicyEngine::new(SandboxScope::local_workspace("."))
    }

    #[tokio::test]
    async fn in_memory_transport_round_trips_contract() {
        let store = Arc::new(MemoryEventStore::default());
        let client = InMemoryTransport::new(store, harness_policy());

        let IpcResponse::Hello { capabilities, .. } = client.hello().await.unwrap() else {
            panic!("hello response");
        };
        assert!(
            capabilities
                .iter()
                .any(|capability| capability == "context")
        );
        assert!(
            capabilities
                .iter()
                .any(|capability| capability == "artifact_upload")
        );

        let session_id = client
            .create_session(std::env::current_dir().unwrap().canonicalize().unwrap())
            .await
            .unwrap();

        let context = client.get_context(session_id).await.unwrap();
        assert!(
            context.references.is_empty(),
            "fresh session has no instruction references"
        );

        let mut subscription = client.subscribe_live(session_id, 0).await.unwrap();
        let events = subscription.next_events().await.unwrap();
        assert_eq!(
            events.len(),
            2,
            "session creation and workspace are backfilled"
        );

        // Read-only tool path still denies escaping the workspace.
        let outcome = client
            .run_tool(
                session_id,
                ReadOnlyToolKind::Read,
                "/etc/passwd".into(),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, impetus_core::ToolOutcome::Denied { .. }));

        assert!(
            client
                .subscribe_live(uuid::Uuid::new_v4(), 0)
                .await
                .is_err(),
            "both transports reject subscriptions to missing sessions"
        );
    }

    #[tokio::test]
    async fn upload_artifact_chunks_into_durable_store() {
        let artifact_root = tempfile::tempdir().expect("artifacts");
        let workspace = tempfile::tempdir().expect("workspace");
        let store = Arc::new(MemoryEventStore::default());
        let client = InMemoryTransport::with_artifact_root(
            store.clone(),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
            artifact_root.path(),
        );

        let session_id = client
            .create_session(workspace.path().to_path_buf())
            .await
            .unwrap();
        let body = b"client helper multi-chunk paste body";
        let art = client
            .upload_artifact(session_id, body, Some("text/plain".into()))
            .await
            .unwrap();
        assert_eq!(art.byte_count, body.len());

        let durable =
            impetus_core::DurableArtifactStore::open(artifact_root.path()).expect("open store");
        assert_eq!(durable.read(&art.id).unwrap(), body);

        let events = store.list(session_id).expect("events");
        let dump = serde_json::to_string(&events).unwrap();
        assert!(!dump.contains("multi-chunk paste body"));
    }

    #[tokio::test]
    async fn unix_transport_handshake_and_incompatible() {
        // Spin up a minimal socket fixture around the real Harness dispatcher.
        // Daemon handshake state and capability gates are covered by harness tests.
        let dir = std::env::temp_dir().join(format!("at-client-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let socket = dir.clone();

        let store = Arc::new(MemoryEventStore::default());
        let policy = harness_policy();
        let listen = tokio::spawn(async move {
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            // The transport handshake and raw incompatible handshake use two
            // connections. Serve both so this smoke cannot deadlock waiting on
            // a listener that accepted only the first client.
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let store = store.clone();
                let policy = policy.clone();
                tokio::spawn(async move { serve_one(stream, store, policy).await });
            }
        });

        // Give the listener a moment to bind.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let client = UnixSocketTransport::connect(&dir).await.unwrap();
        let IpcResponse::Hello { capabilities, .. } = client.hello().await.unwrap() else {
            panic!("hello response");
        };
        assert!(
            capabilities
                .iter()
                .any(|capability| capability == "context")
        );
        let response = client.request(IpcRequest::ListSessions).await.unwrap();
        assert!(matches!(response, IpcResponse::Sessions { .. }));

        // A too-new client version must be rejected with Incompatible.
        let mut raw = UnixStream::connect(&dir).await.unwrap();
        let hello = serde_json::to_string(&IpcRequest::Hello {
            version: IPC_VERSION + 1,
            capabilities: vec![],
        })
        .unwrap();
        raw.write_all(format!("{hello}\n").as_bytes())
            .await
            .unwrap();
        let mut reader = BufReader::new(raw);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let response: IpcResponse = serde_json::from_str(line.trim()).unwrap();
        assert!(matches!(response, IpcResponse::Incompatible { .. }));

        listen.abort();
        let _ = std::fs::remove_file(&dir);
    }

    async fn serve_one(
        stream: tokio::net::UnixStream,
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let harness = Harness::new(store, policy);
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await.unwrap();
            if n == 0 {
                break;
            }
            let request: IpcRequest = match serde_json::from_str(line.trim()) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let response = harness.handle(request);
            writer
                .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            writer.write_all(b"\n").await.unwrap();
            writer.flush().await.unwrap();
        }
    }
}
