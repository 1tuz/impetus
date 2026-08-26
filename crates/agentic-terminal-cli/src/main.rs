use agentic_terminal_core::{IPC_VERSION, IpcRequest, IpcResponse, ReadOnlyToolKind};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    let request = parse_request(std::env::args().skip(1).collect())?;
    let stream = UnixStream::connect(socket_path()?)
        .await
        .context("connect to harness; start agentic-terminal-harness first")?;
    let (reader, mut writer) = stream.into_split();
    writer
        .write_all(
            serde_json::to_string(&IpcRequest::Hello {
                version: IPC_VERSION,
                capabilities: vec!["cli".into()],
            })?
            .as_bytes(),
        )
        .await?;
    writer.write_all(b"\n").await?;
    writer
        .write_all(serde_json::to_string(&request)?.as_bytes())
        .await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    let mut lines = BufReader::new(reader).lines();
    let hello = lines
        .next_line()
        .await?
        .context("harness closed before handshake")?;
    let response = lines
        .next_line()
        .await?
        .context("harness closed before response")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::from_str::<IpcResponse>(&hello)?)?
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::from_str::<IpcResponse>(&response)?)?
    );
    Ok(())
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
