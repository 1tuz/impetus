//! Zap structured adapter: subscribes to harness IPC and renders Blocks.
//!
//! This binary connects to the harness Unix socket, streams events, and renders
//! structured Blocks to stdout. It translates typed events (Intent, Plan, Tool,
//! Approval, Agent) into visual Blocks with OSC sequences for Zap terminal.

use agentic_terminal_client::{HarnessClient, UnixSocketTransport};
use agentic_terminal_core::{Event, EventPayload, IpcRequest, IpcResponse, RuntimeStatus};
use anyhow::{Context, Result};
use std::io::{self, Write};
use std::path::PathBuf;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: agentic-terminal-zap-adapter <session-id>");
        eprintln!("       agentic-terminal-zap-adapter create <prompt>");
        std::process::exit(1);
    }

    let socket_path = std::env::var_os("AGENTIC_TERMINAL_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is set on macOS"))
                .join("Library/Application Support/Agentic Terminal")
                .join("harness.sock")
        });

    let transport = UnixSocketTransport::connect(socket_path).await?;

    match args[1].as_str() {
        "create" => {
            let prompt = args[2..].join(" ");
            create_and_attach(&transport, &prompt).await?;
        }
        session_id => {
            let session_id = Uuid::parse_str(session_id).context("Invalid session ID")?;

            // Poll for events using incremental Stream requests
            let mut last_sequence = 0u64;
            loop {
                let response = transport
                    .request(IpcRequest::Stream {
                        session_id,
                        after_sequence: last_sequence,
                    })
                    .await
                    .context("Failed to stream events")?;

                match response {
                    IpcResponse::Events { events, .. } => {
                        for event in &events {
                            render_event(event);
                            last_sequence = event.sequence;

                            if let EventPayload::Run(
                                agentic_terminal_core::RunEvent::Completed { .. }
                                | agentic_terminal_core::RunEvent::Failed { .. }
                                | agentic_terminal_core::RunEvent::Cancelled { .. },
                            ) = &event.payload
                            {
                                return Ok(());
                            }
                        }

                        if events.is_empty() {
                            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                        }
                    }
                    IpcResponse::Error { message, .. } => {
                        anyhow::bail!("Stream error: {}", message);
                    }
                    _ => anyhow::bail!("Unexpected response: {:?}", response),
                }
            }
        }
    }

    Ok(())
}

async fn create_and_attach(transport: &UnixSocketTransport, prompt: &str) -> Result<()> {
    // Create session
    let response = transport
        .request(IpcRequest::CreateSession)
        .await
        .context("Failed to create session")?;

    let session_id = match response {
        IpcResponse::Session { session_id, status } => {
            render_block("Session", &format!("Created: {}", session_id));
            render_status(&status);
            session_id
        }
        _ => anyhow::bail!("Unexpected response: {:?}", response),
    };

    // Send prompt to start the session
    let _response = transport
        .request(IpcRequest::Prompt {
            session_id,
            text: prompt.to_string(),
        })
        .await
        .context("Failed to send prompt")?;

    // Poll for events with incremental stream requests
    let mut last_sequence = 0u64;
    loop {
        let response = transport
            .request(IpcRequest::Stream {
                session_id,
                after_sequence: last_sequence,
            })
            .await
            .context("Failed to stream events")?;

        match response {
            IpcResponse::Events { events, .. } => {
                for event in &events {
                    render_event(event);
                    last_sequence = event.sequence;

                    // Check for terminal status
                    if let EventPayload::Run(
                        agentic_terminal_core::RunEvent::Completed { .. }
                        | agentic_terminal_core::RunEvent::Failed { .. }
                        | agentic_terminal_core::RunEvent::Cancelled { .. },
                    ) = &event.payload
                    {
                        return Ok(());
                    }
                }

                // If no new events, wait a bit before polling again
                if events.is_empty() {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }
            IpcResponse::Error { message, .. } => {
                anyhow::bail!("Stream error: {}", message);
            }
            _ => anyhow::bail!("Unexpected response: {:?}", response),
        }
    }
}

fn render_event(event: &Event) {
    match &event.payload {
        EventPayload::Session(session_event) => {
            render_block("Session", &format!("{:?}", session_event));
        }
        EventPayload::Run(run_event) => {
            render_block("Run", &format!("{:?}", run_event));
        }
        EventPayload::Intent(intent) => {
            render_block("Intent", &intent.text);
        }
        EventPayload::Plan(plan) => {
            render_block("Plan", &plan.summary);
        }
        EventPayload::Tool(tool) => {
            render_block("Tool", &format!("{:?}", tool));
        }
        EventPayload::Agent(agent) => match agent {
            agentic_terminal_core::AgentEvent::Chunk { text, .. } => {
                render_block("Agent", text);
            }
            agentic_terminal_core::AgentEvent::Final { text, .. } => {
                render_block("Agent [Final]", text);
            }
        },
        EventPayload::Approval(approval) => {
            render_block("Approval", &format!("{:?}", approval));
        }
        EventPayload::Backend(backend) => {
            render_block("Backend", &format!("{:?}", backend));
        }
        EventPayload::Notice(notice) => {
            render_block("Notice", &format!("{:?}", notice));
        }
    }
}

fn render_block(kind: &str, content: &str) {
    // Simple block rendering with visual separators
    println!("┌─ {} ─────────────────────────────────────", kind);
    for line in content.lines() {
        println!("│ {}", line);
    }
    println!("└─────────────────────────────────────────────");
    io::stdout().flush().unwrap();
}

fn render_status(status: &RuntimeStatus) {
    let status_symbol = match status {
        RuntimeStatus::Idle => "○",
        RuntimeStatus::Running => "●",
        RuntimeStatus::AwaitingApproval => "⏸",
        RuntimeStatus::Completed => "✓",
        RuntimeStatus::Failed => "✗",
        RuntimeStatus::Cancelled => "⊗",
        RuntimeStatus::InterruptedUnknown => "?",
    };

    println!("[{}] Status: {:?}", status_symbol, status);
    io::stdout().flush().unwrap();
}
