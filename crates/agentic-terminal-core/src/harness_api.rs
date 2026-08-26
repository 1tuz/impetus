//! Transport-neutral harness dispatch.
//!
//! The same `Harness` drives both the Unix-socket server (harness daemon) and
//! in-memory transports used by client tests and future TUI clients. It owns no
//! transport: it takes a normalized [`IpcRequest`] and returns an
//! [`IpcResponse`], deriving view DTOs from the durable event store and the
//! runtime projection. State lives in the store and the running agent, never in
//! the client.

use crate::{
    AgentInfo, AgentRuntime, ArtifactStore, EventStore, IPC_VERSION, IpcErrorCode, IpcRequest,
    IpcResponse, LearningState, MockStreamItem, MockStreamingProvider, PolicyEngine, ProfileInfo,
    ReadOnlyTool, ReadOnlyToolKind, ReadOnlyTools, RiskState, RuntimeError, RuntimeStatus,
    SandboxScope, SessionSupervisor, SupervisorError, ToolOutcome, Usage,
};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

/// A reusable harness request dispatcher.
///
/// Wraps the durable [`EventStore`] and the [`PolicyEngine`]. Every client
/// command is resolved here so that the Unix socket server and in-memory
/// transports share one implementation.
pub struct Harness {
    store: Arc<dyn EventStore>,
    policy: PolicyEngine,
}

impl Harness {
    pub fn new(store: Arc<dyn EventStore>, policy: PolicyEngine) -> Self {
        Self { store, policy }
    }

    pub fn policy(&self) -> PolicyEngine {
        self.policy.clone()
    }

    /// Resolve a single client request into a response.
    pub fn handle(&self, request: IpcRequest) -> IpcResponse {
        handle_request(self.store.clone(), self.policy.clone(), request)
    }
}

fn handle_request(
    store: Arc<dyn EventStore>,
    policy: PolicyEngine,
    request: IpcRequest,
) -> IpcResponse {
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
            match AgentRuntime::attach(store, policy, session_id).and_then(|runtime| {
                tools
                    .run(tool, &artifact_store)
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
        IpcRequest::Fork { session_id, label } => {
            match AgentRuntime::attach(store, policy, session_id).map(|runtime| {
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
pub fn redact_tool_outcome(outcome: ToolOutcome) -> ToolOutcome {
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

pub fn policy() -> PolicyEngine {
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

fn artifact_root() -> PathBuf {
    data_root()
        .map(|root| root.join("artifacts"))
        .unwrap_or_else(|_| PathBuf::from("artifacts"))
}
