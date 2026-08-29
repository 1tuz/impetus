use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use impetus_client::HarnessClient;
use uuid::Uuid;

mod daemon;
mod doctor;
mod tui;

#[derive(Parser)]
#[command(name = "impetus")]
#[command(version)]
#[command(about = "Impetus - terminal-first agent harness")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run diagnostics and health checks
    Doctor {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Launch interactive TUI (MVP UI)
    Ui,
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
    /// Approve or reject a pending agent action
    Approve {
        /// Session that owns the approval
        session_id: Uuid,
        /// Pending approval ID
        approval_id: Uuid,
        /// Reject the pending action instead of approving it
        #[arg(long)]
        reject: bool,
    },
    /// Send a prompt to a session
    Prompt {
        /// Session ID to prompt
        session_id: Uuid,
        /// The prompt text
        text: String,
    },
    /// Show transient resolved instruction context for a session
    Context {
        /// Session ID to inspect
        session_id: Uuid,
    },
    /// List all sessions
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let socket_path = daemon::discover_socket_path();

    match cli.command {
        Commands::Doctor { json } => {
            doctor::run_diagnostics(&socket_path, json).await?;
            return Ok(());
        }
        Commands::Ui => {
            daemon::ensure_daemon_running(&socket_path).await?;
            tui::run(&socket_path).await?;
            return Ok(());
        }
        _ => {}
    }

    // Auto-spawn daemon if not running
    daemon::ensure_daemon_running(&socket_path).await?;

    let client = impetus_client::UnixSocketTransport::connect(&socket_path)
        .await
        .context("Failed to connect to impetusd after ensuring it's running")?;

    match cli.command {
        Commands::Doctor { .. } => unreachable!("handled above"),
        Commands::Ui => unreachable!("handled above"),
        Commands::Create => {
            let workspace_root = std::env::current_dir()?.canonicalize()?;
            let response = client
                .request(impetus_core::IpcRequest::CreateSession { workspace_root })
                .await?;
            match response {
                impetus_core::IpcResponse::Session { session_id, status } => {
                    println!("Created session: {session_id}");
                    println!("Status: {status:?}");
                }
                impetus_core::IpcResponse::Error { message, .. } => {
                    bail!("Error creating session: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Stream { session_id } => {
            let response = client
                .request(impetus_core::IpcRequest::Stream {
                    session_id,
                    after_sequence: 0,
                })
                .await?;
            match response {
                impetus_core::IpcResponse::Events { events, .. } => {
                    println!("Events from session {session_id}:");
                    for event in events {
                        println!("  [{}] {:?}", event.sequence, event.payload);
                    }
                }
                impetus_core::IpcResponse::Error { message, .. } => {
                    bail!("Error streaming: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Cancel { session_id } => {
            let response = client
                .request(impetus_core::IpcRequest::Cancel { session_id })
                .await?;
            match response {
                impetus_core::IpcResponse::Status { status, .. } => {
                    println!("Session {session_id} cancelled, status: {status:?}");
                }
                impetus_core::IpcResponse::Error { message, .. } => {
                    bail!("Error cancelling: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Approve {
            session_id,
            approval_id,
            reject,
        } => {
            let response = client
                .request(impetus_core::IpcRequest::ResolveApproval {
                    session_id,
                    approval_id,
                    accepted: !reject,
                })
                .await?;
            match response {
                impetus_core::IpcResponse::ApprovalResolved { .. } => {
                    println!("Approval {approval_id} resolved for session {session_id}");
                }
                impetus_core::IpcResponse::Error { message, .. } => {
                    bail!("Error resolving approval: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Prompt { session_id, text } => {
            let response = client
                .request(impetus_core::IpcRequest::Prompt { session_id, text })
                .await?;
            match response {
                impetus_core::IpcResponse::Status { status, .. } => {
                    println!("Prompt sent to {session_id}, status: {status:?}");
                }
                impetus_core::IpcResponse::Error { message, .. } => {
                    bail!("Error sending prompt: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Context { session_id } => {
            let context = client.get_context(session_id).await?;
            for reference in context.references {
                println!(
                    "{:?}: {}",
                    reference.kind,
                    reference.relative_path.display()
                );
            }
            println!("Estimated tokens: {}", context.estimated_tokens.total());
        }
        Commands::List => {
            let response = client
                .request(impetus_core::IpcRequest::ListSessions)
                .await?;
            match response {
                impetus_core::IpcResponse::Sessions { sessions } => {
                    if sessions.is_empty() {
                        println!("No sessions found.");
                    } else {
                        println!("Sessions:");
                        for session_id in sessions {
                            println!("  {session_id}");
                        }
                    }
                }
                impetus_core::IpcResponse::Error { message, .. } => {
                    bail!("Error listing sessions: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_context_session_id() {
        let id = Uuid::new_v4();
        let cli = Cli::try_parse_from(["impetus", "context", &id.to_string()]).unwrap();
        assert!(matches!(cli.command, Commands::Context { session_id } if session_id == id));
    }

    #[test]
    fn parses_rejected_approval() {
        let session_id = Uuid::new_v4();
        let approval_id = Uuid::new_v4();
        let cli = Cli::try_parse_from([
            "impetus",
            "approve",
            &session_id.to_string(),
            &approval_id.to_string(),
            "--reject",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Approve { reject: true, .. }
        ));
    }
}
