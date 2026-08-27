//! Zap structured adapter: subscribes to harness IPC and renders Blocks.
//!
//! This binary connects to the harness Unix socket, streams events, and renders
//! structured Blocks to stdout. It translates typed events (Intent, Plan, Tool,
//! Approval, Agent) into visual Blocks with OSC sequences for Zap terminal.
//!
//! ## v0.3 step 2/4 implementation
//!
//! - OSC escape sequences for Zap notification hooks
//! - Structured blocks protocol: diff, approval, output, attachment, status, error
//! - Live session status bar: Running / Idle / NeedsApproval with current action
//!
//! ## TODO for next iteration
//!
//! - Syntax highlighting metadata in diff blocks
//! - Interactive approval buttons (bidirectional IPC)
//! - Attachment preview links
//! - Parse affected files from approval payload

mod blocks;
mod osc;
mod status_bar;

use agentic_terminal_client::{HarnessClient, UnixSocketTransport};
use agentic_terminal_core::{Event, EventPayload, IpcRequest, IpcResponse, RuntimeStatus};
use anyhow::{Context, Result};
use blocks::{render_block_text, render_event_block};
use status_bar::StatusBar;
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
    let status_bar = StatusBar::new();

    match args[1].as_str() {
        "create" => {
            let prompt = args[2..].join(" ");
            create_and_attach(&transport, &prompt, status_bar).await?;
        }
        session_id => {
            let session_id = Uuid::parse_str(session_id).context("Invalid session ID")?;
            stream_session(&transport, session_id, status_bar).await?;
        }
    }

    Ok(())
}

async fn stream_session(
    transport: &UnixSocketTransport,
    session_id: Uuid,
    status_bar: StatusBar,
) -> Result<()> {
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
                    render_event(event, &status_bar);
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

async fn create_and_attach(
    transport: &UnixSocketTransport,
    prompt: &str,
    status_bar: StatusBar,
) -> Result<()> {
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

    let _response = transport
        .request(IpcRequest::Prompt {
            session_id,
            text: prompt.to_string(),
        })
        .await
        .context("Failed to send prompt")?;

    stream_session(transport, session_id, status_bar).await
}

fn render_event(event: &Event, status_bar: &StatusBar) {
    status_bar.update_from_event(event);
    status_bar.send_update();

    if let Some(block) = render_event_block(event) {
        println!("{}", render_block_text(&block));
        io::stdout().flush().unwrap();
        return;
    }

    match &event.payload {
        EventPayload::Session(session_event) => {
            render_block("Session", &format!("{:?}", session_event));
            osc::send_notification(
                osc::NotificationType::StateChange,
                &format!("{:?}", session_event),
            );
        }
        EventPayload::Run(run_event) => {
            render_block("Run", &format!("{:?}", run_event));
            use agentic_terminal_core::RunEvent;
            match run_event {
                RunEvent::Started { .. } => osc::send_state("Running", None),
                RunEvent::Completed { .. } => osc::send_state("Completed", None),
                RunEvent::Failed { reason, .. } => osc::send_error(reason),
                RunEvent::Cancelled { .. } => osc::send_state("Cancelled", None),
                RunEvent::InterruptedUnknown { .. } => osc::send_error("Interrupted"),
            }
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
                print!("{}", text);
                io::stdout().flush().unwrap();
                osc::send_output_chunk(text);
            }
            agentic_terminal_core::AgentEvent::Final { text, .. } => {
                render_block("Agent [Final]", text);
            }
        },
        EventPayload::Approval(approval) => {
            render_block("Approval", &format!("{:?}", approval));
            if let agentic_terminal_core::ApprovalEvent::Requested { request } = approval {
                osc::send_approval_request(&request.id.to_string(), &request.action.summary, &[]);
            }
        }
        EventPayload::Backend(backend) => {
            render_block("Backend", &format!("{:?}", backend));
            use agentic_terminal_core::BackendEvent;
            match backend {
                BackendEvent::ProviderUnavailable { reason, .. } => osc::send_error(reason),
                BackendEvent::KeychainUnavailable { reason } => osc::send_error(reason),
                BackendEvent::TokenExpiryWarning { .. } => {
                    osc::send_warning("Token expiry warning")
                }
                _ => {}
            }
        }
        EventPayload::Notice(notice) => {
            render_block("Notice", &format!("{:?}", notice));
        }
    }
}

fn render_block(kind: &str, content: &str) {
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

    let (state_str, detail) = match status {
        RuntimeStatus::Idle => ("Idle", "Ready"),
        RuntimeStatus::Running => ("Running", "Processing"),
        RuntimeStatus::AwaitingApproval => ("NeedsApproval", "Waiting for approval"),
        RuntimeStatus::Completed => ("Completed", "Done"),
        RuntimeStatus::Failed => ("Failed", "Error"),
        RuntimeStatus::Cancelled => ("Cancelled", "Cancelled"),
        RuntimeStatus::InterruptedUnknown => ("Interrupted", "Interrupted"),
    };
    osc::send_state(state_str, Some(detail));
}
