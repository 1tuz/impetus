use agentic_terminal_client::HarnessClient;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "agentic-terminal")]
#[command(about = "Agentic Terminal CLI - interact with headless harness")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new session
    Create,
    /// Stream events from a session
    Stream {
        /// Session ID to stream from
        session_id: Uuid,
    },
    /// Cancel a running session
    Cancel {
        /// Session ID to cancel
        session_id: Uuid,
    },
    /// Send a prompt to a session
    Prompt {
        /// Session ID to prompt
        session_id: Uuid,
        /// The prompt text
        text: String,
    },
    /// List all sessions
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let socket_path = std::env::var("AGENTIC_TERMINAL_SOCKET").unwrap_or_else(|_| {
        let home = std::env::var("HOME").expect("HOME not set");
        format!("{home}/Library/Application Support/Agentic Terminal/harness.sock")
    });

    let client = agentic_terminal_client::UnixSocketTransport::connect(&socket_path)
        .await
        .context("connect to harness socket")?;

    match cli.command {
        Commands::Create => {
            let response = client
                .request(orbit_core::IpcRequest::CreateSession)
                .await?;
            match response {
                orbit_core::IpcResponse::Session { session_id, status } => {
                    println!("Created session: {session_id}");
                    println!("Status: {status:?}");
                }
                orbit_core::IpcResponse::Error { message, .. } => {
                    bail!("Error creating session: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Stream { session_id } => {
            let response = client
                .request(orbit_core::IpcRequest::Stream {
                    session_id,
                    after_sequence: 0,
                })
                .await?;
            match response {
                orbit_core::IpcResponse::Events { events, .. } => {
                    println!("Events from session {session_id}:");
                    for event in events {
                        println!("  [{}] {:?}", event.sequence, event.payload);
                    }
                }
                orbit_core::IpcResponse::Error { message, .. } => {
                    bail!("Error streaming: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Cancel { session_id } => {
            let response = client
                .request(orbit_core::IpcRequest::Cancel { session_id })
                .await?;
            match response {
                orbit_core::IpcResponse::Status { status, .. } => {
                    println!("Session {session_id} cancelled, status: {status:?}");
                }
                orbit_core::IpcResponse::Error { message, .. } => {
                    bail!("Error cancelling: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Prompt { session_id, text } => {
            let response = client
                .request(orbit_core::IpcRequest::Prompt { session_id, text })
                .await?;
            match response {
                orbit_core::IpcResponse::Status { status, .. } => {
                    println!("Prompt sent to {session_id}, status: {status:?}");
                }
                orbit_core::IpcResponse::Error { message, .. } => {
                    bail!("Error sending prompt: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::List => {
            let response = client
                .request(orbit_core::IpcRequest::ListSessions)
                .await?;
            match response {
                orbit_core::IpcResponse::Sessions { sessions } => {
                    if sessions.is_empty() {
                        println!("No sessions found.");
                    } else {
                        println!("Sessions:");
                        for session_id in sessions {
                            println!("  {session_id}");
                        }
                    }
                }
                orbit_core::IpcResponse::Error { message, .. } => {
                    bail!("Error listing sessions: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
    }

    Ok(())
}
