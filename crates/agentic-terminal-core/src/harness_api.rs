//! Transport-neutral harness dispatch.
//!
//! The same `Harness` drives both the Unix-socket server (harness daemon) and
//! in-memory transports used by client tests and future TUI clients. It owns no
//! transport: it takes a normalized [`IpcRequest`] and returns an
//! [`IpcResponse`], deriving view DTOs from the durable event store and the
//! runtime projection. State lives in the store and the running agent, never in
//! the client.

use crate::{
    AgentRuntime, ArtifactStore, CredentialResolver, EventStore, IPC_CAPABILITIES, IPC_VERSION,
    IpcErrorCode, IpcRequest, IpcResponse, MockStreamItem, MockStreamingProvider,
    NoCredentialResolver, OpenAiCompatibleProvider, PolicyEngine, ProviderError, ReadOnlySandbox,
    ReadOnlyTool, ReadOnlyToolKind, ReadOnlyTools, RuntimeError, RuntimeStatus, SandboxScope,
    SessionSupervisor, SupervisorError, ToolOutcome,
};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
enum ProviderBackend {
    Mock,
    OpenAi {
        provider: Arc<OpenAiCompatibleProvider>,
        credential_resolver: Arc<dyn CredentialResolver>,
    },
}

/// A reusable harness request dispatcher.
///
/// Wraps the durable [`EventStore`] and the [`PolicyEngine`]. Every client
/// command is resolved here so that the Unix socket server and in-memory
/// transports share one implementation.
pub struct Harness {
    store: Arc<dyn EventStore>,
    policy: PolicyEngine,
    request_lock: Mutex<()>,
    provider: ProviderBackend,
    cancellations: Arc<Mutex<HashMap<uuid::Uuid, CancellationToken>>>,
}

impl Harness {
    pub fn new(store: Arc<dyn EventStore>, policy: PolicyEngine) -> Self {
        Self {
            store,
            policy,
            request_lock: Mutex::new(()),
            provider: ProviderBackend::Mock,
            cancellations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Use a user-selected direct-provider profile. The profile is supplied by
    /// daemon startup, not client IPC; credentials remain outside this type.
    pub fn with_openai_provider(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        provider: OpenAiCompatibleProvider,
    ) -> Self {
        Self::with_openai_provider_and_resolver(
            store,
            policy,
            provider,
            Arc::new(NoCredentialResolver),
        )
    }

    /// The resolver is injected by the harness and is called only while a
    /// provider request is active. It never enters SQLite, events, or IPC.
    pub fn with_openai_provider_and_resolver(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        provider: OpenAiCompatibleProvider,
        credential_resolver: Arc<dyn CredentialResolver>,
    ) -> Self {
        Self {
            store,
            policy,
            request_lock: Mutex::new(()),
            provider: ProviderBackend::OpenAi {
                provider: Arc::new(provider),
                credential_resolver,
            },
            cancellations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn policy(&self) -> PolicyEngine {
        self.policy.clone()
    }

    /// Resolve a single client request into a response.
    pub fn handle(&self, request: IpcRequest) -> IpcResponse {
        let Ok(_guard) = self.request_lock.lock() else {
            return IpcResponse::Error {
                code: IpcErrorCode::Internal,
                message: "harness request lock poisoned".into(),
            };
        };
        handle_request(
            self.store.clone(),
            self.policy.clone(),
            self.provider.clone(),
            self.cancellations.clone(),
            request,
        )
    }
}

fn handle_request(
    store: Arc<dyn EventStore>,
    policy: PolicyEngine,
    provider: ProviderBackend,
    cancellations: Arc<Mutex<HashMap<uuid::Uuid, CancellationToken>>>,
    request: IpcRequest,
) -> IpcResponse {
    match request {
        IpcRequest::Hello { version, .. } if version != IPC_VERSION => IpcResponse::Incompatible {
            supported_version: IPC_VERSION,
        },
        IpcRequest::Hello { capabilities, .. } => IpcResponse::Hello {
            version: IPC_VERSION,
            capabilities: IPC_CAPABILITIES
                .iter()
                .filter(|supported| {
                    capabilities
                        .iter()
                        .any(|requested| requested == **supported)
                })
                .map(|capability| (*capability).to_owned())
                .collect(),
        },
        IpcRequest::CreateSession => match AgentRuntime::create(store, policy) {
            Ok(runtime) => IpcResponse::Session {
                session_id: runtime.session_id(),
                status: RuntimeStatus::Idle,
            },
            Err(error) => runtime_error(error),
        },
        IpcRequest::Attach { session_id } => {
            match AgentRuntime::attach(store, policy, session_id)
                .and_then(|runtime| Ok((runtime.session_id(), runtime.status()?)))
            {
                Ok((session_id, status)) => IpcResponse::Session { session_id, status },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::ListSessions => match store.list_sessions() {
            Ok(sessions) => IpcResponse::Sessions {
                sessions: sessions.into_iter().map(|session| session.id).collect(),
            },
            Err(error) => IpcResponse::Error {
                code: IpcErrorCode::Internal,
                message: error.to_string(),
            },
        },
        IpcRequest::Stream {
            session_id,
            after_sequence,
        } => match AgentRuntime::attach(store, policy, session_id).and_then(|runtime| {
            Ok(runtime
                .events()?
                .into_iter()
                .filter(|event| event.sequence > after_sequence)
                .collect())
        }) {
            Ok(events) => IpcResponse::Events { session_id, events },
            Err(error) => runtime_error(error),
        },
        IpcRequest::Prompt { session_id, text } => {
            match AgentRuntime::attach(store, policy, session_id).and_then(|runtime| {
                let runtime = Arc::new(runtime);
                let provider_message = text.clone();
                let run_id = runtime.submit_intent_and_start_run(text)?;
                let runtime_session_id = runtime.session_id();
                let task_runtime = runtime.clone();
                let cancellation = CancellationToken::new();
                if let Ok(mut active) = cancellations.lock() {
                    active.insert(runtime_session_id, cancellation.clone());
                }
                let task_cancellations = cancellations.clone();
                tokio::spawn(async move {
                    match provider {
                        ProviderBackend::Mock => run_mock_stream(task_runtime, run_id).await,
                        ProviderBackend::OpenAi {
                            provider,
                            credential_resolver,
                        } => {
                            run_openai_stream(
                                task_runtime,
                                run_id,
                                provider,
                                credential_resolver,
                                provider_message,
                                cancellation,
                            )
                            .await;
                        }
                    }
                    if let Ok(mut active) = task_cancellations.lock() {
                        active.remove(&runtime_session_id);
                    }
                });
                runtime.status()
            }) {
                Ok(status) => IpcResponse::Status { session_id, status },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::Cancel { session_id } => {
            if let Ok(active) = cancellations.lock()
                && let Some(cancellation) = active.get(&session_id)
            {
                // A session has at most one active run; cancellation is kept
                // outside durable events so no handle leaks through SQLite.
                cancellation.cancel();
            }
            match AgentRuntime::attach(store, policy, session_id)
                .and_then(|runtime| runtime.cancel())
            {
                Ok(status) => IpcResponse::Status { session_id, status },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::Tool {
            session_id,
            kind,
            target,
            pattern,
        } => {
            let artifact_store = ArtifactStore::open(artifact_root().join("artifacts"))
                .expect("open artifact store");
            // The IPC effect scope is owned by the harness policy, not by the
            // daemon's current directory or an independently constructed tool
            // policy.  This keeps the fixed effect path coherent all the way
            // to capability execution.
            let workspace_root = policy.scope().workspace_root.clone();
            let tools = ReadOnlyTools::new(&workspace_root);
            let effect_seam = crate::EffectSeam::with_sandbox(
                policy.clone(),
                ReadOnlySandbox::workspace(&workspace_root),
            );
            let tool = match kind {
                ReadOnlyToolKind::List => ReadOnlyTool::List {
                    target: target.into(),
                },
                ReadOnlyToolKind::Read => ReadOnlyTool::Read {
                    target: target.into(),
                },
                ReadOnlyToolKind::Search => ReadOnlyTool::Search {
                    target: target.into(),
                    pattern: pattern.unwrap_or_default(),
                },
            };
            match AgentRuntime::attach(store, policy, session_id).and_then(|runtime| {
                tools
                    .run_with_seam(
                        tool,
                        crate::ActionOrigin::User,
                        &artifact_store,
                        &effect_seam,
                    )
                    .map_err(|e| RuntimeError::Denied(e.to_string()))
                    .and_then(|outcome| {
                        crate::tools::record_tool_outcome(&runtime, &outcome)?;
                        Ok(outcome)
                    })
            }) {
                Ok(outcome) => IpcResponse::ToolResult {
                    session_id,
                    outcome: redact_tool_outcome(outcome),
                },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::Subscribe {
            session_id,
            after_sequence: _,
        } => match AgentRuntime::attach(store, policy, session_id)
            .map(|runtime| runtime.session_id())
        {
            Ok(_) => IpcResponse::Subscribed { session_id },
            Err(error) => runtime_error(error),
        },
        IpcRequest::ResolveApproval {
            session_id,
            approval_id,
            accepted,
        } => match AgentRuntime::attach(store, policy, session_id).and_then(|runtime| {
            let request = runtime
                .pending_approval(approval_id)?
                .ok_or(RuntimeError::MissingApproval(approval_id))?;
            let resolution = crate::ApprovalResolution {
                id: approval_id,
                resolver: crate::ApprovalResolver::User,
                action_fingerprint: request.action_fingerprint.clone(),
                intent_revision: request.intent_revision,
                accepted,
            };
            runtime.resolve_approval(resolution)?;
            Ok(session_id)
        }) {
            Ok(session_id) => IpcResponse::ApprovalResolved {
                session_id,
                approval_id,
            },
            Err(error) => runtime_error(error),
        },
        IpcRequest::GetAttachment {
            session_id: _,
            attachment_id,
        } => {
            // Placeholder: attachment storage not yet implemented
            IpcResponse::Error {
                code: IpcErrorCode::Unavailable,
                message: format!("attachment {attachment_id} not found"),
            }
        }
        IpcRequest::GetApprovalDetail {
            session_id,
            approval_id,
        } => match AgentRuntime::attach(store, policy, session_id).and_then(|runtime| {
            let request = runtime
                .pending_approval(approval_id)?
                .ok_or(RuntimeError::MissingApproval(approval_id))?;
            // Placeholder: diff/scope computation not yet implemented
            let detail = crate::ApprovalDetail {
                request,
                diff_preview: None,
                affected_files: vec![],
                estimated_scope: None,
                attachment_refs: vec![],
            };
            Ok((session_id, detail))
        }) {
            Ok((session_id, detail)) => IpcResponse::ApprovalDetail { session_id, detail },
            Err(error) => runtime_error(error),
        },
    }
}

async fn run_openai_stream(
    runtime: Arc<AgentRuntime>,
    run_id: uuid::Uuid,
    provider: Arc<OpenAiCompatibleProvider>,
    credential_resolver: Arc<dyn CredentialResolver>,
    message: String,
    cancellation: CancellationToken,
) {
    let mut next_chunk_id = runtime
        .events()
        .ok()
        .and_then(|events| {
            events.iter().rev().find_map(|event| match &event.payload {
                crate::EventPayload::Agent(crate::AgentEvent::Chunk {
                    run_id: event_run,
                    chunk_id,
                    ..
                }) if *event_run == run_id => Some(*chunk_id + 1),
                _ => None,
            })
        })
        .unwrap_or(1);
    let result = match credential_resolver.resolve(provider.profile()) {
        Ok(credential) => {
            provider
                .stream_user_message(
                    &message,
                    credential.as_deref(),
                    cancellation.clone(),
                    |chunk| {
                        let chunk_id = next_chunk_id;
                        next_chunk_id += 1;
                        runtime
                            .record_agent_chunk(run_id, chunk_id, chunk)
                            .map(|_| ())
                            .map_err(|error| ProviderError::RequestFailed(error.to_string()))
                    },
                )
                .await
        }
        // Resolver implementations are platform adapters. Their diagnostic
        // details, including a Keychain service/account or OS error, are not
        // safe to persist in a run event.
        Err(_) => Err(ProviderError::MissingCredential),
    };
    match result {
        Ok(()) if matches!(runtime.status(), Ok(RuntimeStatus::Running)) => {
            let _ = runtime.finish_run(crate::RunEvent::Completed { run_id });
        }
        Err(ProviderError::Cancelled) => {}
        Err(error) if matches!(runtime.status(), Ok(RuntimeStatus::Running)) => {
            let _ = runtime.finish_run(crate::RunEvent::Failed {
                run_id,
                reason: format!("provider stream failed: {error}"),
            });
        }
        _ => {}
    }
}

/// Apply defense-in-depth redaction before tool data crosses client IPC. Full
/// file bytes and artifact filesystem paths never enter the response DTO.
pub fn redact_tool_outcome(mut outcome: ToolOutcome) -> ToolOutcome {
    if let ToolOutcome::Allowed { result } = &mut outcome {
        result.preview = crate::tools::redact_text(&result.preview);
    }
    outcome
}

async fn run_mock_stream(runtime: Arc<AgentRuntime>, run_id: uuid::Uuid) {
    let supervisor = SessionSupervisor::new(runtime.clone());
    let first_attempt = MockStreamingProvider::new([
        MockStreamItem::Chunk {
            chunk_id: 1,
            text: "Mock harness response: ".into(),
        },
        MockStreamItem::Delay(std::time::Duration::from_millis(80)),
        MockStreamItem::Disconnect,
    ]);
    let restarted_attempt = MockStreamingProvider::new([
        MockStreamItem::Chunk {
            chunk_id: 1,
            text: "Mock harness response: ".into(),
        },
        MockStreamItem::Chunk {
            chunk_id: 2,
            text: "durable stream recovered after provider restart.".into(),
        },
        MockStreamItem::Complete,
    ]);

    match supervisor.resume_mock(run_id, &first_attempt).await {
        Err(SupervisorError::ProviderDisconnected)
            if matches!(runtime.status(), Ok(RuntimeStatus::Running)) =>
        {
            if let Err(error) = supervisor.resume_mock(run_id, &restarted_attempt).await
                && matches!(runtime.status(), Ok(RuntimeStatus::Running))
            {
                let _ = runtime.finish_run(crate::RunEvent::Failed {
                    run_id,
                    reason: format!("provider restart failed: {error}"),
                });
            }
        }
        Err(error) if matches!(runtime.status(), Ok(RuntimeStatus::Running)) => {
            let _ = runtime.finish_run(crate::RunEvent::Failed {
                run_id,
                reason: format!("provider stream failed: {error}"),
            });
        }
        Ok(_) | Err(_) => {}
    }
}

fn runtime_error(error: RuntimeError) -> IpcResponse {
    let code = match &error {
        RuntimeError::MissingSession(_) => IpcErrorCode::MissingSession,
        RuntimeError::ActiveRun(_) => IpcErrorCode::Conflict,
        _ => IpcErrorCode::Internal,
    };
    IpcResponse::Error {
        code,
        message: error.to_string(),
    }
}

pub fn policy() -> PolicyEngine {
    PolicyEngine::new(SandboxScope::local_workspace("."))
}

fn data_root() -> Result<PathBuf> {
    Ok(std::env::var_os("AGENTIC_TERMINAL_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is set on macOS"))
                .join("Library/Application Support/Agentic Terminal")
        }))
}

fn artifact_root() -> PathBuf {
    data_root()
        .map(|root| root.join("artifacts"))
        .unwrap_or_else(|_| PathBuf::from("artifacts"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CredentialStrategy, EventPayload, MemoryEventStore, ProviderProfile, RetryBudget};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn explicit_local_profile_streams_durable_chunks_through_harness() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).await.unwrap();
            let request = std::str::from_utf8(&request[..read]).unwrap();
            assert!(request.contains("POST /v1/chat/completions HTTP/1.1"));
            assert!(request.contains("user question"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"evidence \"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\ndata: [DONE]\n\n")
                .await
                .unwrap();
        });
        let provider = OpenAiCompatibleProvider::new(
            ProviderProfile {
                id: "local-test".into(),
                endpoint: format!("http://{address}"),
                model: "test-model".into(),
                credential_strategy: CredentialStrategy::None,
            },
            RetryBudget::default(),
        )
        .unwrap();
        let harness = Harness::with_openai_provider(
            Arc::new(MemoryEventStore::default()),
            policy(),
            provider,
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession)
        else {
            panic!("session creation response")
        };
        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "user question".into(),
            }),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        for _ in 0..20 {
            if matches!(
                harness.handle(IpcRequest::Attach { session_id }),
                IpcResponse::Session {
                    status: RuntimeStatus::Completed,
                    ..
                }
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        }) else {
            panic!("stream response")
        };
        assert!(matches!(
            events.last().map(|event| &event.payload),
            Some(EventPayload::Run(crate::RunEvent::Completed { .. }))
        ));
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match &event.payload {
                    EventPayload::Agent(crate::AgentEvent::Chunk { text, .. }) =>
                        Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
            "evidence answer"
        );
        server.await.unwrap();
    }

    #[test]
    fn ipc_tool_uses_the_harness_owned_policy_scope() {
        let root =
            std::env::temp_dir().join(format!("harness-tool-scope-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create workspace");
        std::fs::write(root.join("evidence.txt"), "scoped evidence").expect("write fixture");
        let harness = Harness::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(&root)),
        );
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession)
        else {
            panic!("session creation response");
        };

        let response = harness.handle(IpcRequest::Tool {
            session_id,
            kind: ReadOnlyToolKind::Read,
            target: "evidence.txt".into(),
            pattern: None,
        });
        assert!(matches!(
            response,
            IpcResponse::ToolResult {
                outcome: ToolOutcome::Allowed { .. },
                ..
            }
        ));
    }

    struct CountingKeychainCredential {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl CredentialResolver for CountingKeychainCredential {
        fn resolve(&self, _profile: &ProviderProfile) -> Result<Option<String>, ProviderError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(ProviderError::RequestFailed(
                "Keychain unavailable for opaque-service-label/opaque-account-label".into(),
            ))
        }
    }

    #[tokio::test]
    async fn keychain_lookup_is_lazy_and_missing_or_unavailable_results_are_redacted() {
        let store = Arc::new(MemoryEventStore::default());
        let provider = OpenAiCompatibleProvider::new(
            ProviderProfile {
                id: "remote-profile".into(),
                endpoint: "https://api.example.test".into(),
                model: "test-model".into(),
                credential_strategy: CredentialStrategy::KeychainReference {
                    service: "opaque-service-label".into(),
                    account: "opaque-account-label".into(),
                },
            },
            RetryBudget::default(),
        )
        .unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resolver = CountingKeychainCredential {
            calls: calls.clone(),
        };
        let harness = Harness::with_openai_provider_and_resolver(
            store.clone(),
            policy(),
            provider,
            Arc::new(resolver),
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession)
        else {
            panic!("session creation response")
        };
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "question without credential".into(),
            }),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        for _ in 0..20 {
            if matches!(
                harness.handle(IpcRequest::Attach { session_id }),
                IpcResponse::Session {
                    status: RuntimeStatus::Failed,
                    ..
                }
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        }) else {
            panic!("stream response")
        };
        let exported = serde_json::to_string(&events).unwrap();
        assert!(!exported.contains("opaque-service-label"));
        assert!(!exported.contains("opaque-account-label"));
        assert!(!exported.contains("Keychain unavailable"));
        assert!(exported.contains("provider credential is required but unavailable"));
    }

    #[tokio::test]
    async fn ipc_resolve_approval_requires_exact_pending_approval() {
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let harness = Harness::new(store.clone(), policy.clone());

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession)
        else {
            panic!("session creation response");
        };

        let runtime = AgentRuntime::attach(store.clone(), policy.clone(), session_id)
            .expect("attach to created session");

        // Submit intent to establish revision
        runtime
            .submit_intent("write a test file")
            .expect("submit intent");

        // Request an action that needs approval
        let action = crate::Action {
            origin: crate::ActionOrigin::Agent,
            kind: crate::ActionKind::WriteFile,
            summary: "write file".into(),
            target: Some("test.txt".into()),
        };
        runtime
            .request_action(action)
            .expect("request action that needs approval");

        // Get the approval ID from events
        let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        }) else {
            panic!("stream response");
        };

        let approval_id = events
            .iter()
            .find_map(|e| {
                if let crate::EventPayload::Approval(crate::ApprovalEvent::Requested { request }) =
                    &e.payload
                {
                    Some(request.id)
                } else {
                    None
                }
            })
            .expect("approval request in events");

        let IpcResponse::ApprovalResolved {
            approval_id: resolved_id,
            ..
        } = harness.handle(IpcRequest::ResolveApproval {
            session_id,
            approval_id,
            accepted: true,
        })
        else {
            panic!("approval resolution response");
        };
        assert_eq!(resolved_id, approval_id);

        let remaining = runtime
            .pending_approval(approval_id)
            .expect("check pending approval");
        assert!(remaining.is_none(), "approval must be resolved");

        let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        }) else {
            panic!("stream response");
        };
        let resolved_event = events.iter().find(|e| {
            matches!(
                e.payload,
                crate::EventPayload::Approval(crate::ApprovalEvent::Resolved { .. })
            )
        });
        assert!(
            resolved_event.is_some(),
            "resolved event must be in the stream"
        );
    }

    #[tokio::test]
    async fn ipc_get_approval_detail_returns_extended_payload() {
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let harness = Harness::new(store.clone(), policy.clone());

        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession)
        else {
            panic!("session creation response");
        };

        let runtime = AgentRuntime::attach(store.clone(), policy.clone(), session_id)
            .expect("attach to created session");

        runtime
            .submit_intent("write a test file")
            .expect("submit intent");

        let action = crate::Action {
            origin: crate::ActionOrigin::Agent,
            kind: crate::ActionKind::WriteFile,
            summary: "write file".into(),
            target: Some("test.txt".into()),
        };
        runtime
            .request_action(action)
            .expect("request action that needs approval");

        let IpcResponse::Events { events, .. } = harness.handle(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        }) else {
            panic!("stream response");
        };

        let approval_id = events
            .iter()
            .find_map(|e| {
                if let crate::EventPayload::Approval(crate::ApprovalEvent::Requested { request }) =
                    &e.payload
                {
                    Some(request.id)
                } else {
                    None
                }
            })
            .expect("approval request in events");

        let IpcResponse::ApprovalDetail { detail, .. } = harness.handle(
            IpcRequest::GetApprovalDetail {
                session_id,
                approval_id,
            },
        ) else {
            panic!("approval detail response");
        };

        assert_eq!(detail.request.id, approval_id);
        assert_eq!(detail.request.action.kind, crate::ActionKind::WriteFile);
        // Placeholders until diff/scope computation implemented
        assert!(detail.diff_preview.is_none());
        assert!(detail.affected_files.is_empty());
        assert!(detail.estimated_scope.is_none());
        assert!(detail.attachment_refs.is_empty());
    }
}
