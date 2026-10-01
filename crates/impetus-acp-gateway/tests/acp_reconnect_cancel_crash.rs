//! Deterministic cancel / crash → reconnect against spawned SDK mock agent.
//!
//! CI-safe: no secrets, no live Codex. Proves mid-turn `session/cancel` and
//! agent crash never report false Completed, and the same `AcpGatewayV2` can
//! start a second session afterward.

use agent_client_protocol::AcpAgentConfig;
use agent_client_protocol::schema::v1::StopReason;
use impetus_acp_gateway::{AcpGatewayV2, GatewayState, StreamUpdate};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use tokio::time::{sleep, timeout};

fn build_mock_agent() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "-p",
            "impetus-acp-gateway",
            "--example",
            "acp_sdk_mock_agent",
            "--quiet",
        ])
        .current_dir(manifest.join("../.."))
        .status()
        .expect("cargo build mock agent");
    assert!(status.success(), "mock agent build failed");
    let mut path = manifest.join("../../target/debug/examples/acp_sdk_mock_agent");
    if !path.exists() {
        path = manifest.join("../../target/debug/examples/acp_sdk_mock_agent.exe");
    }
    assert!(path.exists(), "missing mock agent at {}", path.display());
    path.canonicalize().expect("canonical mock agent")
}

async fn drain_until_terminal(gateway: &AcpGatewayV2) -> (Option<StreamUpdate>, Vec<StreamUpdate>) {
    let mut seen = Vec::new();
    let terminal = timeout(Duration::from_secs(20), async {
        while let Some(update) = gateway.recv_update().await {
            let is_terminal = matches!(
                update,
                StreamUpdate::Completed { .. }
                    | StreamUpdate::Interrupted { .. }
                    | StreamUpdate::Error(_)
            );
            seen.push(update.clone());
            if is_terminal {
                return Some(update);
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    (terminal, seen)
}

#[tokio::test]
async fn cancel_mid_turn_then_second_prompt_reuses_gateway() {
    let agent_bin = build_mock_agent();
    let tmp = tempdir().expect("tmp");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let slow_flag = tmp.path().join("slow-once");
    std::fs::write(&slow_flag, b"1").unwrap();

    let config = AcpAgentConfig::new(&agent_bin).env(
        "IMPETUS_ACP_MOCK_SLOW_ONCE",
        slow_flag.display().to_string(),
    );
    let gateway = Arc::new(AcpGatewayV2::new(config));

    let session = {
        let gateway = Arc::clone(&gateway);
        let workspace = workspace.clone();
        tokio::spawn(async move { gateway.start_session(workspace, "slow-1".into()).await })
    };

    // Wait until cancel channel is armed (session active).
    timeout(Duration::from_secs(15), async {
        loop {
            if gateway.cancel_active_session().await.is_ok() {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("cancel never armed");

    let (terminal, seen) = drain_until_terminal(&gateway).await;
    let session_result = timeout(Duration::from_secs(15), session)
        .await
        .expect("session join timeout")
        .expect("session join");

    assert!(
        session_result.is_ok(),
        "cancel should complete prompt with Cancelled, got {session_result:?}"
    );
    match terminal {
        Some(StreamUpdate::Completed {
            stop_reason: StopReason::Cancelled,
        }) => {}
        other => panic!("expected Completed(Cancelled), got {other:?}; seen={seen:?}"),
    }
    assert!(
        !seen.iter().any(|u| matches!(
            u,
            StreamUpdate::Completed {
                stop_reason: StopReason::EndTurn
            }
        )),
        "cancel must not report EndTurn Completed; seen={seen:?}"
    );
    assert_eq!(gateway.state().await, GatewayState::NotStarted);

    // Second prompt on the same gateway must succeed.
    let sid2 = timeout(
        Duration::from_secs(20),
        gateway.start_session(workspace, "after-cancel".into()),
    )
    .await
    .expect("second timeout")
    .expect("second session");
    assert!(!sid2.0.is_empty());
    assert_eq!(gateway.state().await, GatewayState::NotStarted);
}

#[tokio::test]
async fn crash_mid_turn_interrupted_then_second_prompt_reuses_gateway() {
    let agent_bin = build_mock_agent();
    let tmp = tempdir().expect("tmp");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let crash_flag = tmp.path().join("crash-once");
    std::fs::write(&crash_flag, b"1").unwrap();

    let config = AcpAgentConfig::new(&agent_bin).env(
        "IMPETUS_ACP_MOCK_CRASH_ONCE",
        crash_flag.display().to_string(),
    );
    let gateway = Arc::new(AcpGatewayV2::new(config));

    let first = timeout(
        Duration::from_secs(20),
        gateway.start_session(workspace.clone(), "crash-me".into()),
    )
    .await
    .expect("first timeout");
    assert!(first.is_err(), "crash must fail session: {first:?}");

    let (terminal, seen) = drain_until_terminal(&gateway).await;
    match terminal {
        Some(StreamUpdate::Interrupted { .. }) => {}
        other => panic!("expected Interrupted, got {other:?}; seen={seen:?}"),
    }
    assert!(
        !seen
            .iter()
            .any(|u| matches!(u, StreamUpdate::Completed { .. })),
        "crash must never emit Completed; seen={seen:?}"
    );
    // Health may still report Crashed; relaunch must recover.
    assert!(matches!(
        gateway.state().await,
        GatewayState::Crashed | GatewayState::NotStarted
    ));

    let sid2 = timeout(
        Duration::from_secs(20),
        gateway.start_session(workspace, "after-crash".into()),
    )
    .await
    .expect("second timeout")
    .expect("second session after crash");
    assert!(!sid2.0.is_empty());
    assert_eq!(gateway.state().await, GatewayState::NotStarted);
}
