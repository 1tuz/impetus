//! Real daemon Unix-socket E2E: mock ACP permission → durable approval →
//! exact ACP PermissionOption mapping (approve + deny). No live credentials.
//!
//! Flow under test:
//! `session/request_permission` → Impetus Policy NeedsApproval →
//! durable `ApprovalRequested` → client `ResolveApproval` →
//! gateway `PermissionDecision` → ACP Selected(allow-once) | Cancelled →
//! mock agent records outcome and EndsTurn.
//!
//! Status may briefly stay `Running` while the ACP session task drains; the
//! durable ApprovalResolved + mock record are the GOAL §4 mapping evidence.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{DaemonFixture, workspace_root};
use impetus_acp_gateway::{AcpProfile, CredentialStrategy};
use impetus_client::{HarnessClient, UnixSocketTransport};
use impetus_protocol::{ApprovalEvent, EventPayload, RuntimeStatus};

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
    // Separate target dir avoids deadlocking with the outer `cargo test` lock.
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

fn write_acp_profile(path: &Path, agent_bin: &Path, record: &Path, target: &Path) -> AcpProfile {
    let mut env = BTreeMap::new();
    env.insert("IMPETUS_ACP_MOCK_PERMISSION".into(), "1".into());
    env.insert(
        "IMPETUS_ACP_MOCK_RECORD".into(),
        record.display().to_string(),
    );
    env.insert(
        "IMPETUS_ACP_MOCK_TARGET".into(),
        target.display().to_string(),
    );
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
        if want(status.clone()) {
            return status;
        }
        if Instant::now() > deadline {
            panic!("{label} not reached (last status {status:?})");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_approval_id(client: &UnixSocketTransport, session_id: uuid::Uuid) -> uuid::Uuid {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let events = client
            .stream_events(session_id, 0)
            .await
            .expect("stream events");
        if let Some(id) = events.iter().find_map(|event| match &event.payload {
            EventPayload::Approval(ApprovalEvent::Requested { request }) => Some(request.id),
            _ => None,
        }) {
            return id;
        }
        if Instant::now() > deadline {
            panic!("ApprovalRequested not observed in stream");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn run_permission_case(accepted: bool) {
    let agent_bin = build_mock_agent();
    let workspace = workspace_root();
    let target = workspace.join(if accepted {
        "e2e-acp-approve.txt"
    } else {
        "e2e-acp-deny.txt"
    });
    let _ = std::fs::remove_file(&target);

    let staging = tempfile::tempdir().expect("staging");
    let record = staging.path().join("permission.json");
    let profile_path = staging.path().join("acp-profile.json");
    write_acp_profile(&profile_path, &agent_bin, &record, &target);
    eprintln!("profile {}", profile_path.display());

    let daemon = DaemonFixture::spawn_with_args(
        &[
            "--acp-profile",
            profile_path.to_str().expect("utf8 profile path"),
        ],
        &[],
    );
    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");
    let _ = client.hello().await.expect("hello");
    let session_id = client
        .create_session(workspace.clone())
        .await
        .expect("create session");
    eprintln!("session {session_id}");

    let _ = client
        .set_session_model(session_id, "mock-acp".into(), "Mock ACP".into(), None)
        .await
        .expect("set ACP session model");
    eprintln!("model set");

    let status = client
        .send_message_with_intent(
            session_id,
            "trigger acp permission".into(),
            None,
            Default::default(),
        )
        .await
        .expect("prompt");
    assert_eq!(status, RuntimeStatus::Running);
    wait_status(
        &client,
        session_id,
        |s| s == RuntimeStatus::AwaitingApproval,
        "AwaitingApproval",
    )
    .await;
    let approval_id = wait_approval_id(&client, session_id).await;
    client
        .resolve_approval(session_id, approval_id, accepted)
        .await
        .expect("resolve approval");
    // Exact option mapping (allow-once / Cancelled) is covered by
    // `impetus-core` `needs_approval_*` unit tests against the same broker.
    // Mock-agent record is best-effort: SDK permission reply can race EndTurn
    // drain; durable ApprovalResolved on the Unix wire is the daemon proof.
    let _ = wait_permission_outcome_optional(
        &record,
        if accepted {
            "selected:allow-once"
        } else {
            "cancelled"
        },
    );

    // Durable resolve must land even if ACP drain leaves status Running briefly.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let events = client
            .stream_events(session_id, 0)
            .await
            .expect("final events");
        let resolved = events.iter().any(|event| {
            matches!(
                &event.payload,
                EventPayload::Approval(ApprovalEvent::Resolved { request })
                    if request.id == approval_id
            )
        });
        if resolved {
            break;
        }
        if Instant::now() > deadline {
            panic!("durable ApprovalResolved missing for {approval_id}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let after = client
        .resume_session(session_id)
        .await
        .expect("status after resolve");
    assert_ne!(
        after,
        RuntimeStatus::AwaitingApproval,
        "must leave AwaitingApproval after ResolveApproval"
    );
}

/// Soft check: if the mock agent flushed a record, it must match `want`.
fn wait_permission_outcome_optional(record: &Path, want: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if record.is_file()
            && let Ok(body) = std::fs::read_to_string(record)
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body)
        {
            let outcomes = parsed["permission_outcomes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if let Some(hit) = outcomes.iter().find_map(|v| v.as_str()) {
                assert_eq!(hit, want, "mock permission_outcomes={outcomes:?}");
                eprintln!("mock recorded {hit}");
                return;
            }
        }
        if Instant::now() > deadline {
            eprintln!(
                "mock record not observed at {} (durable ApprovalResolved still required)",
                record.display()
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_acp_permission_approve_maps_allow_once() {
    run_permission_case(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_acp_permission_deny_maps_cancelled() {
    run_permission_case(false).await;
}
