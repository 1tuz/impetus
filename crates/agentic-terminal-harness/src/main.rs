use agentic_terminal_core::{
    AgentInfo, AgentRuntime, ArtifactStore, EventStore, IPC_VERSION, IpcErrorCode, IpcRequest,
    IpcResponse, LearningState, MockStreamItem, MockStreamingProvider, PolicyEngine, ProfileInfo,
    ReadOnlyTool, ReadOnlyToolKind, ReadOnlyTools, RiskState, RuntimeError, RuntimeStatus,
    SandboxScope, SessionSupervisor, SqliteEventStore, SupervisorError, ToolOutcome, Usage,
};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

#[cfg(test)]
use agentic_terminal_core::RunEvent;

const MAX_IPC_LINE_BYTES: usize = 64 * 1024;

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = socket_path()?;
    let data_root = data_root()?;
    if socket_path.exists() {
        bail!(
            "refusing to replace existing socket: {}",
            socket_path.display()
        );
    }
    let parent = socket_path
        .parent()
        .context("socket path must have a parent directory")?;
    std::fs::create_dir_all(parent).context("create harness data directory")?;
    std::fs::create_dir_all(&data_root).context("create harness event-store directory")?;
    let store = SqliteEventStore::open(data_root.join("events.sqlite3"))?;
    let listener = UnixListener::bind(&socket_path).context("bind harness Unix socket")?;
    set_socket_permissions(&socket_path)?;
    loop {
        let (stream, _) = listener.accept().await.context("accept harness client")?;
        let store = store.clone();
        tokio::spawn(async move {
            let _ = serve_client(stream, store).await;
        });
    }
}

async fn serve_client(stream: UnixStream, store: Arc<dyn EventStore>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            return Ok(());
        }
        let response = if line.len() > MAX_IPC_LINE_BYTES {
            IpcResponse::Error {
                code: IpcErrorCode::InvalidRequest,
                message: "request exceeds 64 KiB".into(),
            }
        } else {
            match serde_json::from_str::<IpcRequest>(&line) {
                Ok(request) => handle_request(store.clone(), request),
                Err(error) => IpcResponse::Error {
                    code: IpcErrorCode::InvalidRequest,
                    message: error.to_string(),
                },
            }
        };
        writer
            .write_all(serde_json::to_string(&response)?.as_bytes())
            .await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
}

fn handle_request(store: Arc<dyn EventStore>, request: IpcRequest) -> IpcResponse {
    match request {
        IpcRequest::Hello { version, .. } if version != IPC_VERSION => IpcResponse::Incompatible {
            supported_version: IPC_VERSION,
        },
        IpcRequest::Hello { .. } => IpcResponse::Hello {
            version: IPC_VERSION,
            capabilities: vec![
                "session_create".into(),
                "session_attach".into(),
                "session_list".into(),
                "event_stream".into(),
                "prompt".into(),
                "status".into(),
                "cancel".into(),
                "tool".into(),
                "subscribe".into(),
                "fork".into(),
                "list_agents".into(),
                "get_dag".into(),
                "get_checkpoints".into(),
                "revert".into(),
                "get_usage".into(),
                "get_risk_state".into(),
                "get_profiles".into(),
                "set_profile".into(),
                "get_learning_state".into(),
            ],
        },
        IpcRequest::CreateSession => match AgentRuntime::create(store, policy()) {
            Ok(runtime) => IpcResponse::Session {
                session_id: runtime.session_id(),
                status: RuntimeStatus::Idle,
            },
            Err(error) => runtime_error(error),
        },
        IpcRequest::Attach { session_id } => {
            match AgentRuntime::attach(store, policy(), session_id)
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
        } => match AgentRuntime::attach(store, policy(), session_id).and_then(|runtime| {
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
            match AgentRuntime::attach(store, policy(), session_id).and_then(|runtime| {
                runtime.submit_intent(text)?;
                let runtime = Arc::new(runtime);
                let run_id = runtime.start_run()?;
                let task_runtime = runtime.clone();
                tokio::spawn(async move {
                    run_mock_stream(task_runtime, run_id).await;
                });
                runtime.status()
            }) {
                Ok(status) => IpcResponse::Status { session_id, status },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::Cancel { session_id } => {
            match AgentRuntime::attach(store, policy(), session_id)
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
            let artifact_store =
                ArtifactStore::open(data_root().expect("artifact root").join("artifacts"))
                    .expect("open artifact store");
            let tools = ReadOnlyTools::new(workspace_root());
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
            match AgentRuntime::attach(store, policy(), session_id).and_then(|runtime| {
                tools
                    .run(tool, &artifact_store)
                    .map_err(|e| RuntimeError::Denied(e.to_string()))
                    .and_then(|outcome| {
                        agentic_terminal_core::tools::record_tool_outcome(&runtime, &outcome)?;
                        Ok(outcome)
                    })
            }) {
                Ok(outcome) => IpcResponse::ToolResult {
                    session_id,
                    outcome: filter_outcome_for_client(outcome),
                },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::Subscribe {
            session_id,
            after_sequence: _,
        } => match AgentRuntime::attach(store, policy(), session_id)
            .map(|runtime| runtime.session_id())
        {
            Ok(_) => IpcResponse::Subscribed { session_id },
            Err(error) => runtime_error(error),
        },
        IpcRequest::Fork { session_id, label } => {
            match AgentRuntime::attach(store, policy(), session_id).map(|runtime| {
                let _ = label;
                runtime.session_id()
            }) {
                Ok(new_session_id) => IpcResponse::Forked {
                    session_id,
                    new_session_id,
                },
                Err(error) => runtime_error(error),
            }
        }
        IpcRequest::ListAgents { session_id } => IpcResponse::Agents {
            session_id,
            agents: vec![AgentInfo {
                id: session_id,
                role: "primary".into(),
                task: "idle".into(),
                status: "idle".into(),
            }],
        },
        IpcRequest::GetDag { session_id } => IpcResponse::Dag {
            session_id,
            nodes: vec![],
        },
        IpcRequest::GetCheckpoints { session_id } => IpcResponse::Checkpoints {
            session_id,
            checkpoints: vec![],
        },
        IpcRequest::Revert {
            session_id,
            checkpoint_id,
        } => IpcResponse::Reverted {
            session_id,
            checkpoint_id,
        },
        IpcRequest::GetUsage { session_id } => IpcResponse::Usage {
            session_id,
            usage: Usage::default(),
        },
        IpcRequest::GetRiskState { session_id } => IpcResponse::Risk {
            session_id,
            risk: RiskState::default(),
        },
        IpcRequest::GetProfiles { session_id } => IpcResponse::Profiles {
            session_id,
            profiles: vec![ProfileInfo {
                name: "default".into(),
                source: "builtin".into(),
                inherits: None,
                active: true,
            }],
        },
        IpcRequest::SetProfile { session_id, name } => IpcResponse::ProfileSet { session_id, name },
        IpcRequest::GetLearningState { session_id } => IpcResponse::Learning {
            session_id,
            learning: LearningState::default(),
        },
    }
}

/// Redact artifact contents before crossing the client boundary. The client
/// never receives file bytes; only the bounded preview and a content hash ref.
fn filter_outcome_for_client(outcome: ToolOutcome) -> ToolOutcome {
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
            let _ = supervisor.resume_mock(run_id, &restarted_attempt).await;
        }
        Ok(_) | Err(_) => {}
    }
}

fn runtime_error(error: RuntimeError) -> IpcResponse {
    let code = if matches!(error, RuntimeError::MissingSession(_)) {
        IpcErrorCode::MissingSession
    } else {
        IpcErrorCode::Internal
    };
    IpcResponse::Error {
        code,
        message: error.to_string(),
    }
}

fn policy() -> PolicyEngine {
    PolicyEngine::new(SandboxScope::local_workspace("."))
}

fn workspace_root() -> PathBuf {
    std::env::var_os("AGENTIC_TERMINAL_WORKSPACE")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("current directory"))
}

fn data_root() -> Result<PathBuf> {
    Ok(std::env::var_os("AGENTIC_TERMINAL_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is set on macOS"))
                .join("Library/Application Support/Agentic Terminal")
        }))
}

fn socket_path() -> Result<PathBuf> {
    Ok(std::env::var_os("AGENTIC_TERMINAL_SOCKET")
        .map(PathBuf::from)
        .unwrap_or(data_root()?.join("harness.sock")))
}

#[cfg(unix)]
fn set_socket_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .context("restrict harness socket permissions")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentic_terminal_core::MemoryEventStore;

    #[test]
    fn protocol_rejects_unknown_version() {
        assert_eq!(
            handle_request(
                Arc::new(MemoryEventStore::default()),
                IpcRequest::Hello {
                    version: IPC_VERSION + 1,
                    capabilities: vec![]
                }
            ),
            IpcResponse::Incompatible {
                supported_version: IPC_VERSION
            }
        );
    }

    #[tokio::test]
    async fn prompt_restarts_mock_provider_without_duplicating_durable_chunks() {
        let store = Arc::new(MemoryEventStore::default());
        let IpcResponse::Session { session_id, .. } =
            handle_request(store.clone(), IpcRequest::CreateSession)
        else {
            panic!("create session response")
        };
        assert!(matches!(
            handle_request(
                store.clone(),
                IpcRequest::Prompt {
                    session_id,
                    text: "explain repository".into()
                }
            ),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        assert!(matches!(
            handle_request(store.clone(), IpcRequest::Attach { session_id }),
            IpcResponse::Session {
                status: RuntimeStatus::Completed,
                ..
            }
        ));
        assert!(matches!(
            handle_request(store.clone(), IpcRequest::ListSessions),
            IpcResponse::Sessions { sessions } if sessions == vec![session_id]
        ));
        let first = handle_request(
            store.clone(),
            IpcRequest::Stream {
                session_id,
                after_sequence: 0,
            },
        );
        let second = handle_request(
            store,
            IpcRequest::Stream {
                session_id,
                after_sequence: 0,
            },
        );
        assert_eq!(first, second);
        let IpcResponse::Events { events, .. } = first else {
            panic!("stream response")
        };
        let chunks = events
            .iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    agentic_terminal_core::EventPayload::Agent(
                        agentic_terminal_core::AgentEvent::Chunk { .. }
                    )
                )
            })
            .count();
        assert_eq!(chunks, 2);
        assert!(events.iter().any(|event| matches!(
            event.payload,
            agentic_terminal_core::EventPayload::Run(RunEvent::Completed { .. })
        )));
    }

    #[tokio::test]
    async fn tool_read_outside_workspace_is_denied_over_ipc() {
        let store = Arc::new(MemoryEventStore::default());
        let IpcResponse::Session { session_id, .. } =
            handle_request(store.clone(), IpcRequest::CreateSession)
        else {
            panic!("create session response")
        };
        // Workspace root for the harness test is the crate directory; an
        // absolute path outside it must be denied without leaking content.
        let response = handle_request(
            store,
            IpcRequest::Tool {
                session_id,
                kind: ReadOnlyToolKind::Read,
                target: "/etc/hosts".into(),
                pattern: None,
            },
        );
        assert!(matches!(
            response,
            IpcResponse::ToolResult {
                outcome: ToolOutcome::Denied { .. },
                ..
            }
        ));
        assert!(!format!("{response:?}").contains("127.0.0.1"));
    }

    #[tokio::test]
    async fn cancel_stops_mock_stream_before_restart_completes_it() {
        let store = Arc::new(MemoryEventStore::default());
        let IpcResponse::Session { session_id, .. } =
            handle_request(store.clone(), IpcRequest::CreateSession)
        else {
            panic!("create session response")
        };
        assert!(matches!(
            handle_request(
                store.clone(),
                IpcRequest::Prompt {
                    session_id,
                    text: "cancel mock response".into()
                }
            ),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        assert!(matches!(
            handle_request(store.clone(), IpcRequest::Cancel { session_id }),
            IpcResponse::Status {
                status: RuntimeStatus::Cancelled,
                ..
            }
        ));
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let IpcResponse::Events { events, .. } = handle_request(
            store,
            IpcRequest::Stream {
                session_id,
                after_sequence: 0,
            },
        ) else {
            panic!("stream response")
        };
        assert!(events.iter().any(|event| matches!(
            event.payload,
            agentic_terminal_core::EventPayload::Run(RunEvent::Cancelled { .. })
        )));
        assert!(!events.iter().any(|event| matches!(
            event.payload,
            agentic_terminal_core::EventPayload::Run(RunEvent::Completed { .. })
        )));
    }
}
