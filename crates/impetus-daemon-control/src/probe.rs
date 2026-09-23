use std::path::Path;

/// True when something accepts TCP-style connections on the Unix socket
/// (bind is live). Does **not** run Hello — use before deciding to unlink.
pub fn socket_listening(socket_path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket_path).is_ok()
}

/// True when daemon answers harness Hello (negotiated transport ready).
pub async fn is_daemon_running(socket_path: &Path) -> bool {
    impetus_client::UnixSocketTransport::connect(socket_path)
        .await
        .is_ok()
}
