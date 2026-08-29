use anyhow::{Context, Result};
use impetus_client::HarnessClient;
use impetus_core::{IpcRequest, IpcResponse};
use std::io::{self, Write};
use uuid::Uuid;

/// MVP TUI: minimal interactive interface
pub async fn run(socket_path: &str) -> Result<()> {
    let client = impetus_client::UnixSocketTransport::connect(socket_path)
        .await
        .context("Failed to connect to impetusd")?;

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║           Impetus MVP UI - Terminal Interface           ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();

    let mut current_session: Option<Uuid> = None;

    loop {
        print_prompt(&current_session);

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        let parts: Vec<&str> = input.split_whitespace().collect();
        let cmd = parts.first().copied().unwrap_or("");

        match cmd {
            "help" | "h" => print_help(),
            "quit" | "q" | "exit" => {
                println!("Goodbye!");
                break;
            }
            "new" | "create" => match create_session(&client).await {
                Ok(session_id) => {
                    println!("✓ Created session: {}", session_id);
                    current_session = Some(session_id);
                }
                Err(e) => eprintln!("✗ Error: {}", e),
            },
            "list" | "ls" => {
                if let Err(e) = list_sessions(&client).await {
                    eprintln!("✗ Error: {}", e);
                }
            }
            "use" | "switch" => {
                if parts.len() < 2 {
                    eprintln!("Usage: use <session-id>");
                    continue;
                }
                match parts[1].parse::<Uuid>() {
                    Ok(id) => {
                        current_session = Some(id);
                        println!("✓ Switched to session: {}", id);
                    }
                    Err(_) => eprintln!("✗ Invalid session ID"),
                }
            }
            "prompt" | "p" => {
                if let Some(sid) = current_session {
                    let prompt_text = parts[1..].join(" ");
                    if prompt_text.is_empty() {
                        eprintln!("Usage: prompt <text>");
                        continue;
                    }
                    if let Err(e) = send_prompt(&client, sid, &prompt_text).await {
                        eprintln!("✗ Error: {}", e);
                    }
                } else {
                    eprintln!("✗ No active session");
                }
            }
            "stream" | "s" => {
                if let Some(sid) = current_session {
                    if let Err(e) = stream_events(&client, sid).await {
                        eprintln!("✗ Error: {}", e);
                    }
                } else {
                    eprintln!("✗ No active session");
                }
            }
            "cancel" | "c" => {
                if let Some(sid) = current_session {
                    if let Err(e) = cancel_session(&client, sid).await {
                        eprintln!("✗ Error: {}", e);
                    }
                } else {
                    eprintln!("✗ No active session");
                }
            }
            _ => {
                eprintln!("Unknown command: '{}'. Type 'help' for commands.", cmd);
            }
        }
    }

    Ok(())
}

fn print_prompt(session: &Option<Uuid>) {
    if let Some(id) = session {
        print!("impetus [{}]> ", &id.to_string()[..8]);
    } else {
        print!("impetus> ");
    }
    io::stdout().flush().unwrap();
}

fn print_help() {
    println!();
    println!("Available commands:");
    println!("  help, h           - Show this help");
    println!("  new, create       - Create a new session");
    println!("  list, ls          - List all sessions");
    println!("  use <id>          - Switch to session");
    println!("  prompt <text>     - Send prompt to current session");
    println!("  stream, s         - Stream events from current session");
    println!("  cancel, c         - Cancel current session");
    println!("  quit, q, exit     - Exit TUI");
    println!();
}

async fn create_session(client: &impetus_client::UnixSocketTransport) -> Result<Uuid> {
    let response = client.request(IpcRequest::CreateSession).await?;

    match response {
        IpcResponse::Session { session_id, .. } => Ok(session_id),
        IpcResponse::Error { message, .. } => {
            anyhow::bail!("Server error: {}", message)
        }
        other => anyhow::bail!("Unexpected response: {:?}", other),
    }
}

async fn list_sessions(client: &impetus_client::UnixSocketTransport) -> Result<()> {
    let response = client.request(IpcRequest::ListSessions).await?;

    match response {
        IpcResponse::Sessions { sessions } => {
            if sessions.is_empty() {
                println!("No sessions found.");
            } else {
                println!("\nSessions:");
                for id in sessions {
                    println!("  {}", id);
                }
                println!();
            }
            Ok(())
        }
        IpcResponse::Error { message, .. } => {
            anyhow::bail!("Server error: {}", message)
        }
        other => anyhow::bail!("Unexpected response: {:?}", other),
    }
}

async fn send_prompt(
    client: &impetus_client::UnixSocketTransport,
    session_id: Uuid,
    prompt: &str,
) -> Result<()> {
    let response = client
        .request(IpcRequest::Prompt {
            session_id,
            text: prompt.to_string(),
        })
        .await?;

    match response {
        IpcResponse::Status { status, .. } => {
            println!(
                "✓ Prompt sent (status: {:?}). Use 'stream' to see output.",
                status
            );
            Ok(())
        }
        IpcResponse::Error { message, .. } => {
            anyhow::bail!("Server error: {}", message)
        }
        other => anyhow::bail!("Unexpected response: {:?}", other),
    }
}

async fn stream_events(
    client: &impetus_client::UnixSocketTransport,
    session_id: Uuid,
) -> Result<()> {
    println!("Streaming events (Ctrl+C to stop)...\n");

    let response = client
        .request(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        })
        .await?;

    match response {
        IpcResponse::Events { events, .. } => {
            if events.is_empty() {
                println!("(no events yet)");
            } else {
                for evt in events {
                    println!("[{}] {:?}", evt.sequence, evt.payload);
                }
            }
            Ok(())
        }
        IpcResponse::Error { message, .. } => {
            anyhow::bail!("Server error: {}", message)
        }
        other => anyhow::bail!("Unexpected response: {:?}", other),
    }
}

async fn cancel_session(
    client: &impetus_client::UnixSocketTransport,
    session_id: Uuid,
) -> Result<()> {
    let response = client.request(IpcRequest::Cancel { session_id }).await?;

    match response {
        IpcResponse::Status { status, .. } => {
            println!("✓ Session cancelled (status: {:?}).", status);
            Ok(())
        }
        IpcResponse::Error { message, .. } => {
            anyhow::bail!("Server error: {}", message)
        }
        other => anyhow::bail!("Unexpected response: {:?}", other),
    }
}
