//! Minimal read-only observer example.
//!
//! Demonstrates connecting to the harness daemon, listing sessions,
//! and streaming events without sending any prompts or modifying state.
//!
//! Run with:
//!   cargo run -p impetus-client --example read_only_observer
//!
//! Prerequisites:
//!   impetusd must be running with at least one active session.

use anyhow::Result;
use impetus_client::{HarnessClient, UnixSocketTransport};
use impetus_core::{IpcRequest, IpcResponse};

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = socket_path()?;
    println!("Connecting to impetusd at {}", socket_path);

    let client = UnixSocketTransport::connect(&socket_path).await?;
    println!("Connected.");

    // Hello handshake
    match client.hello().await? {
        IpcResponse::Hello {
            version,
            capabilities,
        } => {
            println!("Protocol version: {}", version);
            println!("Capabilities: {:?}", capabilities);
        }
        other => {
            anyhow::bail!("Unexpected hello response: {:?}", other);
        }
    }

    // List all sessions
    let sessions = client.list_sessions().await?;
    println!("\nSessions ({}):", sessions.len());
    for session_id in &sessions {
        println!("  - {}", session_id);
    }

    if sessions.is_empty() {
        println!("\nNo sessions found. Create one with `impetus` client.");
        return Ok(());
    }

    // Observe the first session (read-only)
    let session_id = sessions[0];
    println!("\nObserving session: {}", session_id);

    // Stream events from the beginning
    match client
        .request(IpcRequest::Stream {
            session_id,
            after_sequence: 0,
        })
        .await?
    {
        IpcResponse::Events { events, .. } => {
            println!(
                "
Events ({}):",
                events.len()
            );
            for event in events {
                println!("  [{:04}] {:?}", event.sequence, event.payload);
            }
        }
        other => {
            anyhow::bail!("Unexpected stream response: {:?}", other);
        }
    }

    // Query context (read-only instruction resolution)
    match client.request(IpcRequest::Context { session_id }).await? {
        IpcResponse::Context { context, .. } => {
            println!("\nResolved instructions:");
            println!("  Instruction references: {}", context.references.len());
            println!("  Estimated tokens: {:?}", context.estimated_tokens);
        }
        other => {
            anyhow::bail!("Unexpected context response: {:?}", other);
        }
    }

    println!("\nRead-only observation complete.");
    Ok(())
}

fn socket_path() -> Result<String> {
    Ok(std::env::var("IMPETUS_SOCKET").unwrap_or_else(|_| {
        let home = std::env::var("HOME").expect("HOME is set on macOS");
        format!("{}/Library/Application Support/Impetus/harness.sock", home)
    }))
}
