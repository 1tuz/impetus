//! Test module binary for Module Runtime integration tests
//!
//! Simple external module that responds to IPC commands:
//! - Ping/Pong for health checks
//! - ProbeCapabilities returns configured capabilities
//! - Graceful shutdown on Shutdown message

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ModuleMessage {
    Request {
        id: String,
        method: String,
        params: serde_json::Value,
    },
    Response {
        id: String,
        result: Option<serde_json::Value>,
        error: Option<String>,
    },
    StateChange {
        state: String,
        reason: Option<String>,
    },
    ProbeCapabilities,
    Capabilities {
        probes: Vec<CapabilityProbe>,
    },
    Ping,
    Pong,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CapabilityProbe {
    capability: String,
    available: bool,
    version: Option<String>,
    details: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let socket_path = parse_socket_arg(&args)?;

    eprintln!("[test-module] Connecting to socket: {:?}", socket_path);

    // Connect to harness socket
    let stream = UnixStream::connect(&socket_path)
        .await
        .context("Failed to connect to harness socket")?;

    eprintln!("[test-module] Connected successfully");

    let (read_half, mut write_half) = stream.into_split();

    // Send initial state
    send_state_change(&mut write_half, "ready", None).await?;

    // Message loop
    run_message_loop(read_half, write_half).await?;

    eprintln!("[test-module] Shutting down");
    Ok(())
}

fn parse_socket_arg(args: &[String]) -> Result<PathBuf> {
    for i in 0..args.len() {
        if args[i] == "--socket" && i + 1 < args.len() {
            return Ok(PathBuf::from(&args[i + 1]));
        }
    }
    anyhow::bail!("Missing --socket argument");
}

async fn run_message_loop(
    read_half: tokio::net::unix::OwnedReadHalf,
    mut write_half: tokio::net::unix::OwnedWriteHalf,
) -> Result<()> {
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader
            .read_line(&mut line)
            .await
            .context("Failed to read from socket")?;

        if bytes_read == 0 {
            eprintln!("[test-module] Socket closed by harness");
            break;
        }

        let message: ModuleMessage =
            serde_json::from_str(line.trim()).context("Failed to parse message")?;

        eprintln!("[test-module] Received: {:?}", message);

        match message {
            ModuleMessage::Ping => {
                send_message(&mut write_half, ModuleMessage::Pong).await?;
            }
            ModuleMessage::ProbeCapabilities => {
                let capabilities = vec![
                    CapabilityProbe {
                        capability: "echo".to_string(),
                        available: true,
                        version: Some("1.0.0".to_string()),
                        details: Some("Echo capability".to_string()),
                    },
                    CapabilityProbe {
                        capability: "ping".to_string(),
                        available: true,
                        version: Some("1.0.0".to_string()),
                        details: Some("Health check".to_string()),
                    },
                ];
                send_message(
                    &mut write_half,
                    ModuleMessage::Capabilities {
                        probes: capabilities,
                    },
                )
                .await?;
            }
            ModuleMessage::Request { id, method, params } => {
                let result = if method == "echo" { Some(params) } else { None };
                let error = if result.is_none() {
                    Some(format!("Unknown method: {}", method))
                } else {
                    None
                };
                send_message(
                    &mut write_half,
                    ModuleMessage::Response { id, result, error },
                )
                .await?;
            }
            ModuleMessage::Shutdown => {
                eprintln!("[test-module] Received shutdown request");
                break;
            }
            _ => {
                eprintln!("[test-module] Ignoring message: {:?}", message);
            }
        }
    }

    Ok(())
}

async fn send_message<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    message: ModuleMessage,
) -> Result<()> {
    let json = serde_json::to_string(&message)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

async fn send_state_change<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    state: &str,
    reason: Option<String>,
) -> Result<()> {
    let message = ModuleMessage::StateChange {
        state: state.to_string(),
        reason,
    };
    let json = serde_json::to_string(&message)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}
