//! Control-plane peer isolation: ACP-child marker blocked; normal clients OK.

mod common;

use common::DaemonFixture;
use impetus_client::{HarnessClient, UnixSocketTransport};
use std::process::{Command, Stdio};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normal_client_still_connects_after_peer_gate() {
    let daemon = DaemonFixture::spawn();
    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("CLI/TUI-equivalent client must connect");
    let session = client
        .create_session(common::workspace_root())
        .await
        .expect("create session");
    let _ = client.get_session_model(session).await.expect("model");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_child_marker_is_rejected_without_control_ok() {
    let daemon = DaemonFixture::spawn();
    let status = spawn_python_peer(&daemon.socket, true, false).expect("spawn acp-child connector");
    assert_eq!(
        status.code(),
        Some(3),
        "ACP-marked peer must be dropped before Hello (empty recv), got {status:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_child_with_control_ok_receives_server_error_frame() {
    let daemon = DaemonFixture::spawn();
    let status = spawn_python_peer(&daemon.socket, true, true).expect("spawn authorized connector");
    assert_eq!(
        status.code(),
        Some(0),
        "authorized ACP child must be admitted (non-empty InvalidRequest), got {status:?}"
    );

    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("normal client after authorized ACP peer");
    let _ = client
        .create_session(common::workspace_root())
        .await
        .expect("session");
}

/// Python peer: send junk line, exit 3 on empty recv (rejected), 0 on any bytes (admitted).
fn spawn_python_peer(
    socket: &std::path::Path,
    acp_child: bool,
    control_ok: bool,
) -> std::io::Result<std::process::ExitStatus> {
    let socket_lit =
        serde_json::to_string(&socket.to_string_lossy()).expect("socket path JSON string");
    let mut cmd = Command::new("python3");
    cmd.args([
        "-c",
        &format!(
            r#"
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(3)
s.connect({socket_lit})
s.sendall(b'not-json\n')
try:
    data = s.recv(4096)
except Exception:
    sys.exit(2)
sys.exit(0 if data else 3)
"#
        ),
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    if acp_child {
        cmd.env("IMPETUS_ACP_CHILD", "1");
    } else {
        cmd.env_remove("IMPETUS_ACP_CHILD");
    }
    if control_ok {
        cmd.env("IMPETUS_ACP_CHILD_CONTROL_OK", "1");
    } else {
        cmd.env_remove("IMPETUS_ACP_CHILD_CONTROL_OK");
    }
    cmd.status()
}
