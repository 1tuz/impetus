//! Smoke test for ACP V2 with real Codex ACP (`codex-acp`).
//!
//! Agent-owned auth: inherits the user's Codex home (`~/.codex`) and credentials.
//! The Impetus profile must select an advertised auth method (`api-key` when
//! Codex is configured for API-key / custom provider auth).
//!
//! Prerequisites:
//! - `codex-acp` on PATH
//! - Working Codex login (`codex exec 'pong'` succeeds) — ChatGPT or API key
//!   with a matching `model_provider` in `~/.codex/config.toml`
//!
//! Run:
//! `cargo test -p impetus-acp-gateway --test acp_v2_smoke -- --ignored --nocapture`

use agent_client_protocol::AcpAgentConfig;
use impetus_acp_gateway::{AcpGatewayV2, GatewayState, StreamUpdate};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
#[ignore] // Requires external agent and credentials
async fn smoke_test_codex_acp_session() {
    let codex_path = which::which("codex-acp").expect("codex-acp not found in PATH");
    println!("Found codex-acp at: {}", codex_path.display());

    // Inherit ~/.codex (do not override CODEX_HOME). TinyCast-style: agent owns login.
    let config = AcpAgentConfig::new(&codex_path).env("NO_BROWSER", "1");
    let gateway = Arc::new(AcpGatewayV2::new(config).with_auth_method(Some("api-key".into())));

    assert_eq!(gateway.state().await, GatewayState::NotStarted);

    let workspace = PathBuf::from("/tmp/acp-test-workspace");
    std::fs::create_dir_all(&workspace).expect("failed to create test workspace");

    let prompt = "Reply with exactly: hello from ACP V2 test".to_string();

    println!("Starting ACP session (inherits user Codex auth)...");
    let session_handle = tokio::spawn({
        let gateway = Arc::clone(&gateway);
        async move { gateway.start_session(workspace, prompt).await }
    });

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
                StreamUpdate::ToolUse {
                    tool_name,
                    status,
                    tool_call_id,
                    ..
                } => {
                    println!("Tool: {} ({}) - {}", tool_name, tool_call_id, status);
                }
                StreamUpdate::Status(status) => {
                    println!("Status: {}", status);
                }
                StreamUpdate::Completed { stop_reason } => {
                    println!("Completed: {:?}", stop_reason);
                    break;
                }
                StreamUpdate::Interrupted { reason } => {
                    eprintln!("Interrupted: {}", reason);
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

    let result = timeout(Duration::from_secs(90), async {
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
        Ok(Err(e)) => panic!("Session failed: {e}"),
        Err(_) => panic!("Test timed out after 90s"),
    }

    update_task.abort();
    perm_task.abort();
}
