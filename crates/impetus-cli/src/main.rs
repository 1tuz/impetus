use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use impetus_client::HarnessClient;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "impetus-cli")]
#[command(about = "Impetus CLI - interact with headless harness")]
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
    /// Show current profile and configuration
    Profile,
    /// Show service bindings and module status
    Components {
        #[command(subcommand)]
        action: Option<ComponentsAction>,
    },
}

#[derive(Subcommand)]
enum ComponentsAction {
    /// List all registered modules
    List,
    /// Show service bindings
    Bindings,
    /// Show module status and health
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let socket_path = std::env::var("IMPETUS_SOCKET").unwrap_or_else(|_| {
        let home = std::env::var("HOME").expect("HOME not set");
        format!("{home}/Library/Application Support/Impetus/harness.sock")
    });

    let client = impetus_client::UnixSocketTransport::connect(&socket_path)
        .await
        .context("connect to harness socket")?;

    match cli.command {
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
        Commands::Profile => {
            // For now, show default profile configuration
            // In future, this will query harness for actual runtime profile
            let profile = impetus_core::Profile::default();
            let bindings = profile.default_bindings();

            println!("Profile: {:?}", profile);
            println!("Description: {}", profile.description());
            println!("\nDefault service bindings:");
            println!("  agent_loop:      {:?}", bindings.agent_loop);
            println!("  scheduler:       {:?}", bindings.scheduler);
            println!("  model_router:    {:?}", bindings.model_router);
            println!("  context:         {:?}", bindings.context);
            println!("  reference:       {:?}", bindings.reference);
            println!("  memory:          {:?}", bindings.memory);
            println!("  policy:          {:?}", bindings.policy);
            println!("  tools:           {:?}", bindings.tools);
            println!("  output_reducer:  {:?}", bindings.output_reducer);

            if !bindings.custom.is_empty() {
                println!("\nCustom bindings:");
                for (name, binding) in &bindings.custom {
                    println!("  {}: {:?}", name, binding);
                }
            }
        }
        Commands::Components { action } => {
            match action {
                None | Some(ComponentsAction::List) => {
                    // TODO: Query harness for registered modules
                    println!("Registered modules:");
                    println!("  (Module registry query not yet implemented via IPC)");
                    println!("\nBuiltin services:");
                    println!("  agent_loop (standard)");
                    println!("  scheduler (standard)");
                    println!("  model_router (balanced)");
                    println!("  context (lazy)");
                    println!("  reference (yaml)");
                    println!("  memory (standard)");
                    println!("  policy (standard)");
                    println!("  tools (standard)");
                    println!("  output_reducer (standard)");
                }
                Some(ComponentsAction::Bindings) => {
                    let profile = impetus_core::Profile::default();
                    let bindings = profile.default_bindings();

                    println!("Service bindings:");
                    println!("  AgentLoop        → {:?}", bindings.agent_loop);
                    println!("  Scheduler        → {:?}", bindings.scheduler);
                    println!("  ModelRouter      → {:?}", bindings.model_router);
                    println!("  ContextService   → {:?}", bindings.context);
                    println!("  ReferenceService → {:?}", bindings.reference);
                    println!("  MemoryService    → {:?}", bindings.memory);
                    println!("  Policy           → {:?}", bindings.policy);
                    println!("  ToolProvider     → {:?}", bindings.tools);
                    println!("  OutputReducer    → {:?}", bindings.output_reducer);
                }
                Some(ComponentsAction::Status) => {
                    // TODO: Query harness for module health
                    println!("Module health status:");
                    println!("  (Module health query not yet implemented via IPC)");
                    println!("\nCurrent state:");
                    println!("  All builtin services: Ready");
                }
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
        let cli = Cli::try_parse_from(["impetus-cli", "context", &id.to_string()]).unwrap();
        assert!(matches!(cli.command, Commands::Context { session_id } if session_id == id));
    }

    #[test]
    fn parses_rejected_approval() {
        let session_id = Uuid::new_v4();
        let approval_id = Uuid::new_v4();
        let cli = Cli::try_parse_from([
            "impetus-cli",
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
