//! Real daemon Unix-socket E2E: mid-turn ACP cancel / crash, then second Prompt.
//! Deterministic SDK mock only — no secrets, no live agent CLI.
//!
//! Coverage:
//! - Cancel via harness (`IpcRequest::Cancel`) while mock is in
//!   `IMPETUS_ACP_MOCK_SLOW_ONCE` wait → run ≠ Completed; Cancelled durable;
//!   second Prompt succeeds (gateway relaunch).
//! - Agent crash (`IMPETUS_ACP_MOCK_CRASH_ONCE`) → InterruptedUnknown (or
//!   Cancelled); never Completed; second Prompt succeeds.
//!
//! Library reconnect already covered by `acp_reconnect_cancel_crash`; this
//! file proves the daemon / Unix IPC layer.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{DaemonFixture, workspace_root};
use impetus_acp_gateway::{AcpProfile, CredentialStrategy};
use impetus_client::{HarnessClient, UnixSocketTransport};
use impetus_protocol::{EventPayload, RunEvent, RuntimeStatus};

fn build_mock_agent() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.join("../..");
    // Prefer already-built debug example. Nested `cargo build` under `cargo test`
    // deadlocks on the package lock — never invoke cargo when the binary exists.
    let candidates = [
        workspace.join("target/debug/examples/acp_sdk_mock_agent"),
        workspace.join("target/debug/examples/acp_sdk_mock_agent.exe"),
    ];
    for path in &candidates {
        if path.exists() {
            return path.canonicalize().expect("canonical mock agent");
        }
    }
    let nested_target = workspace.join("target/acp-mock-agent");
    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "-p",
            "impetus-acp-gateway",
            "--example",
            "acp_sdk_mock_agent",
            "--quiet",
        ])
        .env("CARGO_TARGET_DIR", &nested_target)
        .current_dir(&workspace)
        .status()
        .expect("cargo build mock agent");
    assert!(status.success(), "mock agent build failed");
    let mut path = nested_target.join("debug/examples/acp_sdk_mock_agent");
    if !path.exists() {
        path = nested_target.join("debug/examples/acp_sdk_mock_agent.exe");
    }
    assert!(path.exists(), "missing mock agent at {}", path.display());
    path.canonicalize().expect("canonical mock agent")
}

fn write_acp_profile(path: &Path, agent_bin: &Path, env: BTreeMap<String, String>) -> AcpProfile {
    let profile = AcpProfile {
        id: "mock-acp".into(),
        display_name: "Mock ACP".into(),
        command: agent_bin.to_path_buf(),
        args: Vec::new(),
        env,
        auth_method_id: None,
        credential_strategy: CredentialStrategy::AgentOwned,
        credential_ref: None,
    };
    profile.validate().expect("profile valid");
    std::fs::write(path, serde_json::to_vec_pretty(&profile).expect("encode")).expect("write");
    profile
}

async fn wait_status(
    client: &UnixSocketTransport,
    session_id: uuid::Uuid,
    want: impl Fn(RuntimeStatus) -> bool,
    label: &str,
) -> RuntimeStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = client
            .resume_session(session_id)
            .await
            .unwrap_or_else(|e| panic!("attach while waiting for {label}: {e}"));
        if want(status) {
            return status;
        }
        if Instant::now() > deadline {
            panic!("{label} not reached (last status {status:?})");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_flag_gone(flag: &Path, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if !flag.exists() {
            return;
        }
        if Instant::now() > deadline {
            panic!("{label}: flag still present at {}", flag.display());
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

fn assert_no_completed(events: &[impetus_protocol::Event]) {
    assert!(
        !events.iter().any(|event| {
            matches!(
                &event.payload,
                EventPayload::Run(RunEvent::Completed { .. })
            )
        }),
        "must never report Completed; events={events:?}"
    );
}

async fn setup_acp_session(
    daemon: &DaemonFixture,
    workspace: PathBuf,
) -> (UnixSocketTransport, uuid::Uuid) {
    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");
    let _ = client.hello().await.expect("hello");
    let session_id = client
        .create_session(workspace)
        .await
        .expect("create session");
    let _ = client
        .set_session_model(
            session_id,
            "mock-acp".into(),
            "Mock ACP".into(),
            None,
            None,
            serde_json::Value::Null,
        )
        .await
        .expect("set ACP session model");
    (client, session_id)
}

async fn second_prompt_succeeds(client: &UnixSocketTransport, session_id: uuid::Uuid) {
    // After first ACP session, agent-advertised model ids populate the catalog
    // (`mock-default` / …). Profile display-name is no longer listed — rebind.
    let _ = client
        .set_session_model(
            session_id,
            "mock-acp".into(),
            "mock-default".into(),
            None,
            None,
            serde_json::Value::Null,
        )
        .await
        .expect("rebind to advertised ACP model");

    let status = client
        .send_message_with_intent(
            session_id,
            "after interrupt".into(),
            None,
            Default::default(),
        )
        .await
        .expect("second prompt");
    assert_eq!(status, RuntimeStatus::Running, "second prompt must start");
    let terminal = wait_status(
        client,
        session_id,
        |s| s != RuntimeStatus::Running,
        "second prompt terminal",
    )
    .await;
    assert!(
        matches!(terminal, RuntimeStatus::Completed | RuntimeStatus::Idle),
        "second prompt must complete honestly, got {terminal:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_acp_cancel_mid_turn_then_second_prompt() {
    let agent_bin = build_mock_agent();
    let workspace = workspace_root();
    let staging = tempfile::tempdir().expect("staging");
    let slow_flag = staging.path().join("slow-once");
    std::fs::write(&slow_flag, b"1").expect("slow flag");

    let mut env = BTreeMap::new();
    env.insert(
        "IMPETUS_ACP_MOCK_SLOW_ONCE".into(),
        slow_flag.display().to_string(),
    );
    let profile_path = staging.path().join("acp-profile.json");
    write_acp_profile(&profile_path, &agent_bin, env);

    let daemon = DaemonFixture::spawn_with_args(
        &[
            "--acp-profile",
            profile_path.to_str().expect("utf8 profile path"),
        ],
        &[],
    );
    let (client, session_id) = setup_acp_session(&daemon, workspace).await;

    let status = client
        .send_message_with_intent(
            session_id,
            "slow cancel me".into(),
            None,
            Default::default(),
        )
        .await
        .expect("first prompt");
    assert_eq!(status, RuntimeStatus::Running);

    // Flag deleted once mock enters slow wait — cancel channel is armed.
    wait_flag_gone(&slow_flag, "slow-once entered").await;

    let cancel_status = client.cancel(session_id).await.expect("Cancel IPC");
    assert_ne!(
        cancel_status,
        RuntimeStatus::Completed,
        "Cancel must not report Completed"
    );
    assert!(
        matches!(
            cancel_status,
            RuntimeStatus::Cancelled | RuntimeStatus::Running
        ),
        "Cancel IPC status unexpected: {cancel_status:?}"
    );

    let terminal = wait_status(
        &client,
        session_id,
        |s| s != RuntimeStatus::Running,
        "post-cancel terminal",
    )
    .await;
    assert_ne!(terminal, RuntimeStatus::Completed);
    assert!(
        matches!(
            terminal,
            RuntimeStatus::Cancelled | RuntimeStatus::InterruptedUnknown
        ),
        "cancel mid-turn expected Cancelled/InterruptedUnknown, got {terminal:?}"
    );

    let events = client
        .stream_events(session_id, 0)
        .await
        .expect("events after cancel");
    assert_no_completed(&events);
    assert!(
        events.iter().any(|event| {
            matches!(
                &event.payload,
                EventPayload::Run(RunEvent::Cancelled { .. })
                    | EventPayload::Run(RunEvent::InterruptedUnknown { .. })
            )
        }),
        "durable cancel/interrupt event missing: {events:?}"
    );

    second_prompt_succeeds(&client, session_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_acp_crash_mid_turn_then_second_prompt() {
    let agent_bin = build_mock_agent();
    let workspace = workspace_root();
    let staging = tempfile::tempdir().expect("staging");
    let crash_flag = staging.path().join("crash-once");
    std::fs::write(&crash_flag, b"1").expect("crash flag");

    let mut env = BTreeMap::new();
    env.insert(
        "IMPETUS_ACP_MOCK_CRASH_ONCE".into(),
        crash_flag.display().to_string(),
    );
    let profile_path = staging.path().join("acp-profile.json");
    write_acp_profile(&profile_path, &agent_bin, env);

    let daemon = DaemonFixture::spawn_with_args(
        &[
            "--acp-profile",
            profile_path.to_str().expect("utf8 profile path"),
        ],
        &[],
    );
    let (client, session_id) = setup_acp_session(&daemon, workspace).await;

    let status = client
        .send_message_with_intent(session_id, "crash me".into(), None, Default::default())
        .await
        .expect("crash prompt");
    assert_eq!(status, RuntimeStatus::Running);

    let terminal = wait_status(
        &client,
        session_id,
        |s| s != RuntimeStatus::Running,
        "post-crash terminal",
    )
    .await;
    assert_ne!(terminal, RuntimeStatus::Completed);
    assert!(
        matches!(
            terminal,
            RuntimeStatus::InterruptedUnknown | RuntimeStatus::Cancelled
        ),
        "crash mid-turn expected InterruptedUnknown/Cancelled, got {terminal:?}"
    );

    let events = client
        .stream_events(session_id, 0)
        .await
        .expect("events after crash");
    assert_no_completed(&events);
    assert!(
        events.iter().any(|event| {
            matches!(
                &event.payload,
                EventPayload::Run(RunEvent::InterruptedUnknown { .. })
                    | EventPayload::Run(RunEvent::Cancelled { .. })
            )
        }),
        "durable interrupt event missing: {events:?}"
    );

    second_prompt_succeeds(&client, session_id).await;
}
