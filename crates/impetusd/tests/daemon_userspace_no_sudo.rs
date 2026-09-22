//! macOS / Unix userspace daemon path — no sudo / no admin password.
//!
//! Spawns real `impetusd`, asserts runtime paths are user-owned and that the
//! daemon binary source path contains no privilege-escalation helpers.
//! Does **not** claim Instruments proof that Authorization Services never fires.

mod common;

use common::DaemonFixture;
use impetus_client::{HarnessClient, UnixSocketTransport};
use std::os::unix::fs::MetadataExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_userspace_no_sudo_paths_and_session() {
    let mut daemon = DaemonFixture::spawn();
    // Baseline uid = creator of a fresh user-writable temp path (not root).
    let baseline = tempfile::tempdir().expect("baseline");
    let uid = std::fs::metadata(baseline.path())
        .expect("baseline meta")
        .uid();

    let data_meta = std::fs::metadata(daemon.data_dir.path()).expect("data dir meta");
    assert_eq!(
        data_meta.uid(),
        uid,
        "IMPETUS_DATA_DIR must be user-owned, not root"
    );
    assert!(
        !daemon.data_dir.path().starts_with("/Library")
            && !daemon.data_dir.path().starts_with("/var/root"),
        "data dir must not be system/root path: {}",
        daemon.data_dir.path().display()
    );

    assert!(
        daemon.socket.starts_with(daemon.data_dir.path()),
        "socket {} not under data dir {}",
        daemon.socket.display(),
        daemon.data_dir.path().display()
    );
    let sock_meta = std::fs::metadata(&daemon.socket).expect("socket meta");
    assert_eq!(sock_meta.uid(), uid, "socket must be user-owned");
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = sock_meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket mode must be 0600, got {mode:o}");
    }

    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");
    let session_id = client
        .create_session(common::workspace_root())
        .await
        .expect("create session");
    let _ = client.get_session_model(session_id).await.expect("model");

    let daemon_src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"));
    let start = daemon_src.find("fn main(").expect("main present");
    let end = daemon_src[start..]
        .find("\n#[cfg(test)]")
        .map(|i| start + i)
        .unwrap_or(daemon_src.len());
    let production = &daemon_src[start..end];
    for needle in [
        "Command::new(\"sudo\")",
        "AuthorizationCreate",
        "AuthorizationExecuteWithPrivileges",
        "SMJobBless",
        "osascript",
        "with administrator privileges",
        "withAdministratorPrivileges",
        "SFAuthorization",
        "PrivilegedHelperTools",
    ] {
        assert!(
            !production.contains(needle),
            "impetusd production path must not contain privilege helper `{needle}`"
        );
    }

    daemon.kill_in_place();
}
