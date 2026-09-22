//! Real daemon-boundary E2E over Unix socket + `UnixSocketTransport`.
//!
//! Spawns production `impetusd` (mock provider, no API keys). Does **not**
//! mock the transport.

mod common;

use std::time::{Duration, Instant};

use common::{DaemonFixture, workspace_root};
use impetus_client::{HarnessClient, UnixSocketTransport};
use impetus_protocol::{IpcResponse, RuntimeStatus};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_e2e_handshake_session_model_prompt_mcp_git_pty() {
    let mut daemon = DaemonFixture::spawn();
    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect + Hello handshake");

    // Explicit Hello after connect (connect already negotiated; re-hello OK).
    let hello = client.hello().await.expect("hello");
    let IpcResponse::Hello {
        version,
        capabilities,
        ..
    } = hello
    else {
        panic!("expected Hello, got {hello:?}");
    };
    assert!(version >= 12, "negotiated version {version}");
    assert!(
        capabilities.iter().any(|c| c == "session_create"),
        "missing session_create in {capabilities:?}"
    );

    let session_id = client
        .create_session(workspace_root())
        .await
        .expect("create session");

    let providers = client.list_providers().await.expect("list providers");
    assert!(!providers.is_empty(), "mock provider must appear");
    let mock = providers
        .iter()
        .find(|p| p.provider_id == "mock")
        .expect("mock provider");
    assert_eq!(mock.model_id, "mock-model");
    assert!(
        mock.reasoning_efforts.contains(&"high".into()),
        "default mock must advertise efforts: {:?}",
        mock.reasoning_efforts
    );

    let selection = client
        .set_session_model(
            session_id,
            "mock".into(),
            "mock-model".into(),
            Some("high".into()),
        )
        .await
        .expect("set session model + reasoning");
    assert_eq!(selection.provider_id, "mock");
    assert_eq!(selection.model_id, "mock-model");
    assert_eq!(selection.reasoning_effort.as_deref(), Some("high"));

    let status = client
        .send_message_with_intent(
            session_id,
            "e2e mock prompt".into(),
            None,
            Default::default(),
        )
        .await
        .expect("prompt");
    assert_eq!(status, RuntimeStatus::Running);
    let terminal = wait_until_prompt_settled(&client, session_id).await;
    assert!(
        matches!(terminal, RuntimeStatus::Completed | RuntimeStatus::Idle),
        "mock prompt must succeed (Completed|Idle), got {terminal:?}"
    );

    let mcp = client.list_mcp_servers().await.expect("list mcp");
    assert!(mcp.is_empty() || mcp.iter().all(|s| !s.connected));
    let reloaded = client.reload_mcp_servers().await.expect("reload mcp");
    assert_eq!(reloaded.len(), mcp.len());

    let git = client.git_status(session_id).await.expect("git status");
    // Repo workspace: porcelain may be dirty under parallel agents — only require parse OK.
    let _ = git.branch;
    let _ = git.files;

    let pty = client
        .pty_start(
            session_id,
            "/bin/sleep".into(),
            vec!["30".into()],
            Some(workspace_root()),
            Some(80),
            Some(24),
        )
        .await
        .expect("pty start");
    assert_eq!(pty.owner_session_id, session_id);
    client
        .pty_detach(session_id, pty.pty_id)
        .await
        .expect("pty detach");
    let reattached = client
        .pty_attach(session_id, pty.pty_id)
        .await
        .expect("pty re-attach while daemon live");
    assert_eq!(reattached.pty_id, pty.pty_id);
    client
        .pty_terminate(session_id, pty.pty_id)
        .await
        .expect("pty terminate");

    // Client detach / reconnect pattern (same live daemon).
    drop(client);
    let client2 = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("reconnect");
    let resumed = client2
        .resume_session(session_id)
        .await
        .expect("resume after client reconnect");
    assert!(
        matches!(resumed, RuntimeStatus::Completed | RuntimeStatus::Idle),
        "unexpected status after reconnect: {resumed:?}"
    );

    // Kill daemon + restart: durable session recovers; live PTY does not.
    let pty_before_restart = client2
        .pty_start(
            session_id,
            "/bin/sleep".into(),
            vec!["60".into()],
            Some(workspace_root()),
            None,
            None,
        )
        .await
        .expect("pty before restart");
    let pty_id = pty_before_restart.pty_id;
    drop(client2);

    daemon.restart_after_kill();
    let client3 = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect after daemon restart");
    let after_restart = client3
        .resume_session(session_id)
        .await
        .expect("session durable across daemon restart");
    assert!(
        matches!(
            after_restart,
            RuntimeStatus::Completed | RuntimeStatus::Idle | RuntimeStatus::InterruptedUnknown
        ),
        "session attach after restart: {after_restart:?}"
    );
    let restored = client3
        .get_session_model(session_id)
        .await
        .expect("session model durable across daemon restart");
    assert_eq!(restored.provider_id, "mock");
    assert_eq!(restored.model_id, "mock-model");
    assert_eq!(restored.reasoning_effort.as_deref(), Some("high"));

    let pty_gone = client3.pty_attach(session_id, pty_id).await;
    assert!(
        pty_gone.is_err(),
        "live PTY handles must not survive daemon restart (got {pty_gone:?})"
    );
}

async fn wait_until_prompt_settled(
    client: &UnixSocketTransport,
    session_id: uuid::Uuid,
) -> RuntimeStatus {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let status = client
            .resume_session(session_id)
            .await
            .expect("attach while waiting for prompt");
        if status != RuntimeStatus::Running {
            return status;
        }
        if Instant::now() > deadline {
            panic!("prompt still Running after timeout");
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}
