//! Client contract for the agentic-terminal harness.
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
use std::pin::Pin;
use std::sync::Arc;

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

    /// Send a typed request and await its response.
    async fn request(&self, request: IpcRequest) -> Result<IpcResponse>;

    /// Create a durable session owned by the harness.
    async fn create_session(&self) -> Result<IpcResponse> {
        self.request(IpcRequest::CreateSession).await
    }

    /// Reattach to an existing durable session after a client restart.
    async fn resume_session(&self, session_id: uuid::Uuid) -> Result<IpcResponse> {
        self.request(IpcRequest::Attach { session_id }).await
    }

    /// Submit a user message. The harness, not the client, starts the run.
    async fn send_message(&self, session_id: uuid::Uuid, text: String) -> Result<IpcResponse> {
        self.request(IpcRequest::Prompt { session_id, text }).await
    }

    /// Stop at the next safe runtime boundary.
    async fn soft_interrupt(&self, session_id: uuid::Uuid) -> Result<IpcResponse> {
        self.request(IpcRequest::Cancel { session_id }).await
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
        }))
    }
}

struct InMemoryEventSubscription {
    store: Arc<dyn EventStore>,
    session_id: uuid::Uuid,
    after_sequence: u64,
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
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
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

        assert!(matches!(
            client.hello().await.unwrap(),
            IpcResponse::Hello { .. }
        ));

        let IpcResponse::Session { session_id, .. } = client.create_session().await.unwrap() else {
            panic!("create session")
        };

        let mut subscription = client.subscribe_live(session_id, 0).await.unwrap();
        let events = subscription.next_events().await.unwrap();
        assert_eq!(events.len(), 1, "session-created event is backfilled once");

        // Read-only tool path still denies escaping the workspace.
        let response = client
            .request(IpcRequest::Tool {
                session_id,
                kind: ReadOnlyToolKind::Read,
                target: "/etc/passwd".into(),
                pattern: None,
            })
            .await
            .unwrap();
        assert!(matches!(
            response,
            IpcResponse::ToolResult {
                outcome: impetus_core::ToolOutcome::Denied { .. },
                ..
            }
        ));

        assert!(
            client
                .subscribe_live(uuid::Uuid::new_v4(), 0)
                .await
                .is_err(),
            "both transports reject subscriptions to missing sessions"
        );
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
