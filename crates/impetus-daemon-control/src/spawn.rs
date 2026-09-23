use crate::error::DaemonError;
use std::path::Path;
use std::process::{Command, Stdio};

/// Spawn `impetusd` with socket + data-dir env. Returns the child so the
/// caller can hold the spawn lock until ready (or kill on timeout).
pub(crate) fn spawn_daemon_process(
    daemon_binary: &Path,
    socket_path: &Path,
    data_dir: &Path,
) -> Result<Option<std::process::Child>, DaemonError> {
    if !daemon_binary.exists() {
        return Err(DaemonError::BinaryMissing {
            path: daemon_binary.to_path_buf(),
        });
    }
    let child = Command::new(daemon_binary)
        .env("IMPETUS_SOCKET", socket_path)
        .env("IMPETUS_DATA_DIR", data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| DaemonError::SpawnFailed {
            path: daemon_binary.to_path_buf(),
            source,
        })?;
    Ok(Some(child))
}
