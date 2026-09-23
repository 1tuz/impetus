//! Real Unix-socket daemon E2E for GOAL §4 gaps: durable approvals, MCP
//! mutation, and Files/Diff — fixtures/mocks only (no live API keys).

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::{DaemonFixture, workspace_root};
use impetus_client::{HarnessClient, UnixSocketTransport};
use impetus_protocol::{
    ApprovalEvent, EventPayload, McpCapabilities, McpServerUpsert, McpTransport, RuntimeStatus,
};

const APPROVAL_FIXTURE_ENV: &[(&str, &str)] = &[("IMPETUS_MOCK_APPROVAL_FIXTURE", "1")];

async fn wait_status(
    client: &UnixSocketTransport,
    session_id: uuid::Uuid,
    want: impl Fn(RuntimeStatus) -> bool,
    label: &str,
) -> RuntimeStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
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
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

async fn wait_approval_id(client: &UnixSocketTransport, session_id: uuid::Uuid) -> uuid::Uuid {
    let deadline = Instant::now() + Duration::from_secs(10);
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
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

async fn session_with_mock(client: &UnixSocketTransport, workspace: PathBuf) -> uuid::Uuid {
    let _ = client.hello().await.expect("hello");
    let session_id = client
        .create_session(workspace)
        .await
        .expect("create session");
    let _ = client
        .set_session_model(
            session_id,
            "mock".into(),
            "mock-model".into(),
            Some("high".into()),
            None,
            serde_json::Value::Null,
        )
        .await
        .expect("set mock session model");
    session_id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_durable_approval_approve() {
    let daemon = DaemonFixture::spawn_with_env(APPROVAL_FIXTURE_ENV);
    let workspace = daemon.data_dir.path().join("ws-approve");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let written = workspace.join("e2e-approval.txt");

    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");
    let session_id = session_with_mock(&client, workspace.clone()).await;

    let status = client
        .send_message_with_intent(
            session_id,
            "write the approval file".into(),
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
        .resolve_approval(session_id, approval_id, true)
        .await
        .expect("approve");

    wait_status(
        &client,
        session_id,
        |s| matches!(s, RuntimeStatus::Completed | RuntimeStatus::Idle),
        "Completed after approve",
    )
    .await;

    assert_eq!(
        std::fs::read_to_string(&written).expect("approved write must land"),
        "from-approval"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_durable_approval_deny() {
    let daemon = DaemonFixture::spawn_with_env(APPROVAL_FIXTURE_ENV);
    let workspace = daemon.data_dir.path().join("ws-deny");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let blocked = workspace.join("e2e-approval.txt");

    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");
    let session_id = session_with_mock(&client, workspace).await;

    client
        .send_message_with_intent(session_id, "try the write".into(), None, Default::default())
        .await
        .expect("prompt");

    wait_status(
        &client,
        session_id,
        |s| s == RuntimeStatus::AwaitingApproval,
        "AwaitingApproval",
    )
    .await;
    let approval_id = wait_approval_id(&client, session_id).await;

    client
        .resolve_approval(session_id, approval_id, false)
        .await
        .expect("deny");

    wait_status(
        &client,
        session_id,
        |s| matches!(s, RuntimeStatus::Completed | RuntimeStatus::Idle),
        "Completed after deny",
    )
    .await;

    assert!(
        !blocked.exists(),
        "deny must not execute write_file ({})",
        blocked.display()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_mcp_upsert_disable_enable_remove_survives_restart() {
    let mut daemon = DaemonFixture::spawn();
    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");
    let _ = client.hello().await.expect("hello");

    let servers = client
        .upsert_mcp_server(McpServerUpsert {
            id: "e2e-echo".into(),
            name: "e2e-echo".into(),
            command: "true".into(),
            args: vec![],
            transport: McpTransport::Stdio,
            capabilities: McpCapabilities {
                tools: true,
                ..McpCapabilities::default()
            },
            env_keys: vec!["LABEL_ONLY".into()],
        })
        .await
        .expect("upsert");
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].id, "e2e-echo");
    assert!(
        !servers[0].connected,
        "list stays connected=false until first tool use"
    );
    assert!(daemon.data_dir.path().join("mcp/e2e-echo.json").is_file());

    let listed = client.list_mcp_servers().await.expect("list after upsert");
    assert!(listed.iter().any(|s| s.id == "e2e-echo"));

    let after_disable = client
        .disable_mcp_server("e2e-echo")
        .await
        .expect("disable");
    assert!(after_disable.iter().all(|s| s.id != "e2e-echo"));
    assert!(
        daemon
            .data_dir
            .path()
            .join("mcp/e2e-echo.json.disabled")
            .is_file()
    );

    let after_enable = client.enable_mcp_server("e2e-echo").await.expect("enable");
    assert_eq!(after_enable.len(), 1);
    assert_eq!(after_enable[0].id, "e2e-echo");

    drop(client);
    daemon.restart_after_kill();
    let client2 = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("reconnect after restart");
    let durable = client2
        .list_mcp_servers()
        .await
        .expect("list after restart");
    assert!(
        durable.iter().any(|s| s.id == "e2e-echo"),
        "MCP SoT must survive daemon restart: {durable:?}"
    );

    let after_remove = client2.remove_mcp_server("e2e-echo").await.expect("remove");
    assert!(after_remove.iter().all(|s| s.id != "e2e-echo"));
    assert!(!daemon.data_dir.path().join("mcp/e2e-echo.json").exists());
    assert!(
        !daemon
            .data_dir
            .path()
            .join("mcp/e2e-echo.json.disabled")
            .exists()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_list_changed_files_and_diff_parse() {
    let daemon = DaemonFixture::spawn();
    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");
    let session_id = session_with_mock(&client, workspace_root()).await;

    let files = client
        .list_changed_files(session_id)
        .await
        .expect("ListChangedFiles must parse (dirty workspace ok)");
    let _ = files.len();

    let diff = client
        .get_diff(session_id, None)
        .await
        .expect("GetDiff must parse");
    let _ = diff.patch;
    let _ = diff.observation;

    let path = files
        .first()
        .map(|f| f.path.clone())
        .unwrap_or_else(|| PathBuf::from("Cargo.toml"));
    let file_diff = client
        .get_file_diff(session_id, path, None)
        .await
        .expect("GetFileDiff must parse");
    let _ = file_diff.patch;
    let _ = file_diff.observation;
}
