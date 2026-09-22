//! Prove Impetus ACP gateway applies model/reasoning via `session/set_config_option`.

use agent_client_protocol::AcpAgentConfig;
use impetus_acp_gateway::{AcpGatewayV2, SessionLaunchOptions};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;
use tokio::time::timeout;

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

#[tokio::test]
async fn set_session_model_and_reasoning_reach_acp_agent() {
    let agent_bin = build_mock_agent();
    let tmp = tempdir().expect("tmp");
    let record = tmp.path().join("applied.json");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let config = AcpAgentConfig::new(&agent_bin)
        .env("IMPETUS_ACP_MOCK_RECORD", record.display().to_string());
    let gateway = AcpGatewayV2::new(config);

    let launch = SessionLaunchOptions {
        model_id: Some("mock-smart".into()),
        reasoning_effort: Some("xhigh".into()),
        strict: true,
    };

    let sid = timeout(
        Duration::from_secs(30),
        gateway.start_session_with_options(workspace, "ping".into(), launch),
    )
    .await
    .expect("timeout")
    .expect("session");
    assert!(!sid.0.is_empty());

    let body = std::fs::read_to_string(&record).expect("record written by mock agent");
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    let sets = parsed["sets"].as_array().expect("sets array");
    let pairs: Vec<(String, String)> = sets
        .iter()
        .map(|row| {
            (
                row["config_id"].as_str().unwrap().to_owned(),
                row["value"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(
        pairs.contains(&("model".into(), "mock-smart".into())),
        "model set missing in {pairs:?}"
    );
    assert!(
        pairs.contains(&("effort".into(), "xhigh".into())),
        "reasoning set missing in {pairs:?}"
    );
}
