use agentic_terminal_client::{HarnessClient, UnixSocketTransport};
use agentic_terminal_core::{IpcRequest, ReadOnlyToolKind};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    let request = parse_request(std::env::args().skip(1).collect())?;
    let client = UnixSocketTransport::connect(socket_path()?)
        .await
        .context("connect to harness; start agentic-terminal-harness first")?;
    let response = dispatch(&client, request).await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

/// Keep the reference CLI on the same public client contract as TUI and
/// adapters. Raw IPC stays encapsulated in `UnixSocketTransport`.
async fn dispatch(
    client: &impl HarnessClient,
    request: IpcRequest,
) -> Result<agentic_terminal_core::IpcResponse> {
    match request {
        IpcRequest::CreateSession => client.create_session().await,
        IpcRequest::Attach { session_id } => client.resume_session(session_id).await,
        IpcRequest::Prompt { session_id, text } => client.send_message(session_id, text).await,
        IpcRequest::Cancel { session_id } => client.soft_interrupt(session_id).await,
        request => client.request(request).await,
    }
}

fn parse_request(arguments: Vec<String>) -> Result<IpcRequest> {
    match arguments.as_slice() {
        [command] if command == "create" => Ok(IpcRequest::CreateSession),
        [command] if command == "list" => Ok(IpcRequest::ListSessions),
        [command, session_id] if command == "stream" => Ok(IpcRequest::Stream {
            session_id: Uuid::parse_str(session_id)?,
            after_sequence: 0,
        }),
        [command, session_id, after_sequence] if command == "stream" => Ok(IpcRequest::Stream {
            session_id: Uuid::parse_str(session_id)?,
            after_sequence: after_sequence.parse()?,
        }),
        [command, session_id] if command == "attach" => Ok(IpcRequest::Attach {
            session_id: Uuid::parse_str(session_id)?,
        }),
        [command, session_id] if command == "cancel" => Ok(IpcRequest::Cancel {
            session_id: Uuid::parse_str(session_id)?,
        }),
        [command, session_id, text] if command == "prompt" => Ok(IpcRequest::Prompt {
            session_id: Uuid::parse_str(session_id)?,
            text: text.clone(),
        }),
        [command, session_id, sub, target] if command == "tool" && sub == "list" => {
            Ok(IpcRequest::Tool {
                session_id: Uuid::parse_str(session_id)?,
                kind: ReadOnlyToolKind::List,
                target: target.to_string(),
                pattern: None,
            })
        }
        [command, session_id, sub, target] if command == "tool" && sub == "read" => {
            Ok(IpcRequest::Tool {
                session_id: Uuid::parse_str(session_id)?,
                kind: ReadOnlyToolKind::Read,
                target: target.to_string(),
                pattern: None,
            })
        }
        [command, session_id, sub, target, pattern] if command == "tool" && sub == "search" => {
            Ok(IpcRequest::Tool {
                session_id: Uuid::parse_str(session_id)?,
                kind: ReadOnlyToolKind::Search,
                target: target.to_string(),
                pattern: Some(pattern.to_string()),
            })
        }
        _ => bail!(
            "usage: agentic-terminal-cli <create|list|attach SESSION_ID|stream SESSION_ID [AFTER_SEQUENCE]|prompt SESSION_ID TEXT|cancel SESSION_ID|tool SESSION_ID <list|read|search> TARGET [PATTERN]>"
        ),
    }
}

fn socket_path() -> Result<PathBuf> {
    let data_root = std::env::var_os("AGENTIC_TERMINAL_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is set on macOS"))
                .join("Library/Application Support/Agentic Terminal")
        });
    Ok(std::env::var_os("AGENTIC_TERMINAL_SOCKET")
        .map(PathBuf::from)
        .unwrap_or(data_root.join("harness.sock")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentic_terminal_client::InMemoryTransport;
    use agentic_terminal_core::{IpcResponse, MemoryEventStore, PolicyEngine, SandboxScope};
    use std::sync::Arc;

    #[tokio::test]
    async fn cli_dispatch_uses_transport_neutral_contract() {
        let client = InMemoryTransport::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        assert!(matches!(
            dispatch(&client, IpcRequest::CreateSession).await.unwrap(),
            IpcResponse::Session { .. }
        ));
    }
}
