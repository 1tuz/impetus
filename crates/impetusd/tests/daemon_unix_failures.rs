//! Failure-path coverage for real `impetusd` Unix IPC (no transport mocks).

mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use common::{DaemonFixture, impetusd_bin, wait_for_socket, workspace_root};
use impetus_client::{HarnessClient, UnixSocketTransport};
use impetus_protocol::{IPC_VERSION, IpcRequest, IpcResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_socket_blocks_daemon_bind() {
    let data_dir = tempfile::tempdir().expect("temp");
    let socket = data_dir.path().join("harness.sock");
    // Stale file (no listener) — daemon must refuse replace.
    std::fs::write(&socket, b"").expect("stale sock file");

    let output = Command::new(impetusd_bin())
        .env("IMPETUS_DATA_DIR", data_dir.path())
        .env("IMPETUS_SOCKET", &socket)
        .env("IMPETUS_NONINTERACTIVE", "1")
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("run impetusd against stale socket");

    assert!(
        !output.status.success(),
        "daemon must exit non-zero on existing socket"
    );
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("refusing to replace existing socket") || err.contains("refusing"),
        "unexpected stderr: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_to_stale_socket_file_fails_fast() {
    let data_dir = tempfile::tempdir().expect("temp");
    let socket = data_dir.path().join("dead.sock");
    std::fs::write(&socket, b"").expect("stale");
    let err = UnixSocketTransport::connect(&socket).await;
    assert!(err.is_err(), "connect must fail on non-listening sock file");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incompatible_protocol_version_returns_incompatible() {
    let daemon = DaemonFixture::spawn();
    let mut raw = UnixStream::connect(&daemon.socket)
        .await
        .expect("raw connect");
    let hello = serde_json::to_string(&IpcRequest::Hello {
        version: IPC_VERSION + 1,
        min_version: None,
        capabilities: vec![],
    })
    .unwrap();
    raw.write_all(format!("{hello}\n").as_bytes())
        .await
        .unwrap();
    raw.flush().await.unwrap();

    let mut reader = BufReader::new(raw);
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read response");
    let response: IpcResponse = serde_json::from_str(line.trim()).expect("parse");
    match response {
        IpcResponse::Incompatible {
            supported_version,
            client_version,
            ..
        } => {
            assert_eq!(supported_version, IPC_VERSION);
            assert_eq!(client_version, IPC_VERSION + 1);
        }
        other => panic!("expected Incompatible, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_reasoning_effort_is_rejected() {
    let daemon = DaemonFixture::spawn();
    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");
    let session_id = client
        .create_session(workspace_root())
        .await
        .expect("session");

    let err = client
        .set_session_model(
            session_id,
            "mock".into(),
            "mock-model".into(),
            Some("not-a-real-effort".into()),
        )
        .await;
    let msg = format!("{}", err.expect_err("invalid reasoning must fail"));
    assert!(
        msg.contains("unsupported reasoning_effort") || msg.contains("not-a-real-effort"),
        "unexpected error: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_model_is_rejected() {
    let daemon = DaemonFixture::spawn();
    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");
    let session_id = client
        .create_session(workspace_root())
        .await
        .expect("session");

    let err = client
        .set_session_model(
            session_id,
            "mock".into(),
            "definitely-not-in-mock-catalog".into(),
            None,
        )
        .await;
    let msg = format!("{}", err.expect_err("unknown model must fail"));
    assert!(
        msg.contains("not in provider") || msg.contains("definitely-not-in-mock-catalog"),
        "unexpected error: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_provider_is_rejected() {
    let daemon = DaemonFixture::spawn();
    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");
    let session_id = client
        .create_session(workspace_root())
        .await
        .expect("session");

    let err = client
        .set_session_model(
            session_id,
            "no-such-provider".into(),
            "mock-model".into(),
            None,
        )
        .await;
    let msg = format!("{}", err.expect_err("unknown provider must fail"));
    assert!(
        msg.contains("no-such-provider") || msg.to_ascii_lowercase().contains("unavailable"),
        "unexpected error: {msg}"
    );
}

/// Second daemon on same socket path after unclean kill without unlink fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unclean_kill_leaves_stale_socket_blocking_respawn() {
    let data_dir = tempfile::tempdir().expect("temp");
    let socket = data_dir.path().join("harness.sock");
    let bin = impetusd_bin();
    let mut child = Command::new(&bin)
        .env("IMPETUS_DATA_DIR", data_dir.path())
        .env("IMPETUS_SOCKET", &socket)
        .env("IMPETUS_NONINTERACTIVE", "1")
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    wait_for_socket(&socket, Duration::from_secs(10));
    let _ = child.kill();
    let _ = child.wait();
    assert!(socket.exists(), "sock file remains after kill");

    let second = Command::new(&bin)
        .env("IMPETUS_DATA_DIR", data_dir.path())
        .env("IMPETUS_SOCKET", &socket)
        .env("IMPETUS_NONINTERACTIVE", "1")
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("second spawn");
    assert!(!second.status.success(), "respawn without unlink must fail");
}
