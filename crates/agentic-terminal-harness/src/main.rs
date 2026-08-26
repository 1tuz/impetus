use agentic_terminal_core::{EventStore, IpcErrorCode, IpcRequest, IpcResponse, SqliteEventStore};
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
    let harness =
        agentic_terminal_core::Harness::new(store, agentic_terminal_core::harness_api::policy());
    harness.handle(request)
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
    use agentic_terminal_core::{
        IPC_VERSION, MemoryEventStore, ReadOnlyToolKind, RuntimeStatus, ToolOutcome,
    };

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
