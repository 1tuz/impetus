//! Интеграционный тест: запускает mock_agent_bin через AcpGateway.

use agentic_terminal_acp_gateway::{AcpGateway, AcpProfile, AgentStatus};
use std::path::PathBuf;

#[tokio::test]
#[ignore] // Требует скомпилированный example binary
async fn integration_mock_agent_lifecycle() {
    // Путь к скомпилированному mock_agent_bin
    let bin_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/examples/mock_agent_bin");

    if !bin_path.exists() {
        eprintln!(
            "Skipping: binary not found at {}. Run: cargo build --example mock_agent_bin",
            bin_path.display()
        );
        return;
    }

    let profile = AcpProfile::manual_executable(
        "test-mock",
        "Test Mock Agent",
        bin_path.canonicalize().expect("canonical path"),
    );

    let mut gateway = AcpGateway::new(profile).expect("create gateway");
    assert_eq!(gateway.status(), AgentStatus::NotStarted);

    // Start
    gateway.start().await.expect("start agent");
    assert_eq!(gateway.status(), AgentStatus::Initializing);

    // Дать агенту время на startup
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // TODO: отправить JSON-RPC initialize через stdin

    // Stop
    gateway.stop().await.expect("stop agent");
    assert_eq!(gateway.status(), AgentStatus::NotStarted);
}
