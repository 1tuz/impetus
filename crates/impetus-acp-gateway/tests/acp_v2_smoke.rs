//! Smoke test for ACP V2 integration with real agent.
//!
//! Requires codex-acp and codex CLI installed.
//! Run with: CODEX_API_KEY=... cargo test --test acp_v2_smoke -- --nocapture

use agent_client_protocol::AcpAgentConfig;
use impetus_acp_gateway::{AcpGatewayV2, GatewayState, StreamUpdate};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
#[ignore] // Requires external agent and credentials
async fn smoke_test_codex_acp_session() {
    // Check if codex-acp is available
    let codex_path = which::which("codex-acp").expect("codex-acp not found in PATH");
    println!("Found codex-acp at: {}", codex_path.display());

    // Create config
    let config = AcpAgentConfig::new(&codex_path);
    let gateway = Arc::new(AcpGatewayV2::new(config));

    // Check initial state
    assert_eq!(gateway.state().await, GatewayState::NotStarted);

    // Start session
    let workspace = PathBuf::from("/tmp/acp-test-workspace");
    std::fs::create_dir_all(&workspace).expect("failed to create test workspace");

    let prompt = "Echo 'hello from ACP V2 test'".to_string();

    println!("Starting ACP session...");
    let session_handle = tokio::spawn({
        let gateway = Arc::clone(&gateway);
        async move { gateway.start_session(workspace, prompt).await }
    });

    // Collect updates with timeout
    let text_chunks = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let chunks_clone = Arc::clone(&text_chunks);

    let gateway_for_updates = Arc::clone(&gateway);
    let update_task = tokio::spawn(async move {
        while let Some(update) = gateway_for_updates.recv_update().await {
            match update {
                StreamUpdate::Text(text) => {
                    println!("Text: {}", text);
                    chunks_clone.lock().await.push(text);
                }
                StreamUpdate::ToolUse { tool_name, status } => {
                    println!("Tool: {} - {}", tool_name, status);
                }
                StreamUpdate::Status(status) => {
                    println!("Status: {}", status);
                }
                StreamUpdate::Completed { stop_reason } => {
                    println!("Completed: {:?}", stop_reason);
                    break;
                }
                StreamUpdate::Error(err) => {
                    eprintln!("Error: {}", err);
                    break;
                }
            }
        }
    });

    let gateway_clone = Arc::clone(&gateway);
    let perm_task = tokio::spawn(async move {
        while let Some((req, tx)) = gateway_clone.recv_permission_request().await {
            println!(
                "Permission request: {} - {}",
                req.request_id, req.description
            );
            let _ = tx.send(impetus_acp_gateway::PermissionDecision::Deny);
        }
    });

    // Run with timeout
    let result = timeout(Duration::from_secs(30), async {
        session_handle
            .await
            .map_err(|e| anyhow::anyhow!("join: {}", e))?
    })
    .await;

    match result {
        Ok(Ok(session_id)) => {
            println!("Session completed: {:?}", session_id);
            let chunks = text_chunks.lock().await;
            println!("Collected {} text chunks", chunks.len());
            assert!(!chunks.is_empty(), "should receive at least one text chunk");
        }
        Ok(Err(e)) => {
            panic!("Session failed: {}", e);
        }
        Err(_) => {
            panic!("Test timed out after 30s");
        }
    }

    // Cleanup spawned tasks
    update_task.abort();
    perm_task.abort();
}
