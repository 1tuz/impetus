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

    // Initialize
    let init_result = gateway.initialize().await.expect("initialize");
    assert_eq!(gateway.status(), AgentStatus::Connected);
    assert!(init_result.get("protocolVersion").is_some());

    // Stop
    gateway.stop().await.expect("stop agent");
    assert_eq!(gateway.status(), AgentStatus::NotStarted);
}

#[tokio::test]
#[ignore]
async fn integration_agent_owned_credential_flow() {
    let bin_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/examples/mock_agent_bin");

    if !bin_path.exists() {
        eprintln!("Skipping: binary not found");
        return;
    }

    let profile = AcpProfile::manual_executable(
        "test-auth",
        "Test Auth Agent",
        bin_path.canonicalize().unwrap(),
    );

    let mut gateway = AcpGateway::new(profile).unwrap();
    gateway.start().await.unwrap();
    gateway.initialize().await.unwrap();

    // Симуляция: agent запрашивает credential через notification
    // (в реальном сценарии это происходит асинхронно)

    // Harness forwards prompt и получает ответ от пользователя
    let credential = Some("test-api-key-12345".to_string());

    // Harness отправляет credential обратно agent
    gateway
        .respond_credential("mock-request-id", credential)
        .await
        .expect("respond credential");

    gateway.stop().await.unwrap();
}
