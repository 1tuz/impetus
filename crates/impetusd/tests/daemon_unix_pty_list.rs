//! Real `impetusd` E2E for owner-scoped `PtyList` (#395 / #420).

mod common;

use common::{DaemonFixture, workspace_root};
use impetus_client::{HarnessClient, UnixSocketTransport};
use impetus_protocol::IpcResponse;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_pty_list_owner_scoped_inventory() {
    let daemon = DaemonFixture::spawn();
    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");

    let hello = client.hello().await.expect("hello");
    let IpcResponse::Hello { version, .. } = hello else {
        panic!("expected Hello, got {hello:?}");
    };
    assert!(version >= 15, "PtyList needs IPC v15+, got {version}");

    let session_a = client
        .create_session(workspace_root())
        .await
        .expect("session a");
    let pty = client
        .pty_start(
            session_a,
            "/bin/sleep".into(),
            vec!["60".into()],
            Some(workspace_root()),
            Some(80),
            Some(24),
        )
        .await
        .expect("pty start");
    assert_eq!(pty.owner_session_id, session_a);

    let all = client
        .pty_list(session_a, false)
        .await
        .expect("pty list all");
    assert!(
        all.iter().any(|s| s.pty_id == pty.pty_id),
        "inventory must include live PTY: {all:?}"
    );
    assert!(
        all.iter().all(|s| s.owner_session_id == session_a),
        "rows must be owner-scoped: {all:?}"
    );

    let live = client
        .pty_list(session_a, true)
        .await
        .expect("pty list live");
    assert!(
        live.iter().any(|s| s.pty_id == pty.pty_id),
        "live_only must include running PTY: {live:?}"
    );

    let session_b = client
        .create_session(workspace_root())
        .await
        .expect("session b");
    let foreign = client
        .pty_list(session_b, false)
        .await
        .expect("pty list foreign session");
    assert!(
        !foreign.iter().any(|s| s.pty_id == pty.pty_id),
        "other session must not see owner A PTY: {foreign:?}"
    );

    client
        .pty_terminate(session_a, pty.pty_id)
        .await
        .expect("pty terminate");

    let live_after = client
        .pty_list(session_a, true)
        .await
        .expect("pty list live after terminate");
    assert!(
        !live_after.iter().any(|s| s.pty_id == pty.pty_id),
        "live_only must drop terminated PTY: {live_after:?}"
    );

    let all_after = client
        .pty_list(session_a, false)
        .await
        .expect("pty list all after terminate");
    assert!(
        all_after.iter().any(|s| s.pty_id == pty.pty_id),
        "metadata row may remain after terminate: {all_after:?}"
    );
}
