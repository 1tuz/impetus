use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;

/// Check if impetusd is running by attempting to connect to the socket.
pub async fn is_daemon_running(socket_path: &str) -> bool {
    impetus_client::UnixSocketTransport::connect(socket_path)
        .await
        .is_ok()
}

/// Discover socket path from environment or default data-dir location.
///
/// Order: `IMPETUS_SOCKET` → `$IMPETUS_DATA_DIR/harness.sock` → platform
/// default under `$HOME` (same rules as `impetusd`).
pub fn discover_socket_path() -> String {
    if let Ok(socket) = std::env::var("IMPETUS_SOCKET") {
        return socket;
    }
    default_data_root()
        .join("harness.sock")
        .to_string_lossy()
        .into_owned()
}

/// Platform default data root (userspace only — never `/var` / PrivilegedHelper).
pub fn default_data_root() -> PathBuf {
    if let Ok(dir) = std::env::var("IMPETUS_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = PathBuf::from(std::env::var("HOME").expect("HOME not set"));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Impetus")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("impetus")
    }
}

/// Attempt to spawn `impetusd` if not already accepting on `socket_path`.
///
/// Product UX: ordinary `impetus` commands lazy-start the daemon. Manual
/// `impetusd` is for development / debugging / advanced administration.
pub async fn ensure_daemon_running(socket_path: &str) -> Result<()> {
    if is_daemon_running(socket_path).await {
        return Ok(());
    }

    // Stale socket: path exists but nothing accepts.
    let socket = Path::new(socket_path);
    if socket.exists() {
        std::fs::remove_file(socket).context("failed to remove stale socket")?;
    }

    let impetusd_path = find_impetusd_binary()?;
    let data_dir = std::env::var_os("IMPETUS_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| socket.parent().map(Path::to_path_buf))
        .unwrap_or_else(default_data_root);

    if let Some(parent) = data_dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::create_dir_all(&data_dir);

    Command::new(&impetusd_path)
        .env("IMPETUS_SOCKET", socket_path)
        .env("IMPETUS_DATA_DIR", &data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn impetusd")?;

    // Readiness: connect succeeds (Hello negotiation happens on first request).
    for attempt in 1..=10 {
        sleep(Duration::from_millis(300)).await;
        if is_daemon_running(socket_path).await {
            return Ok(());
        }
        if attempt == 10 {
            anyhow::bail!(
                "impetusd did not become ready within ~3s at {socket_path}. \
                 For debugging run manually: {impetusd_path}"
            );
        }
    }

    Ok(())
}

/// Find impetusd binary in PATH or next to impetus binary.
fn find_impetusd_binary() -> Result<String> {
    // Development mode: look for target/debug/impetusd next to current exe.
    if cfg!(debug_assertions)
        && let Ok(current_exe) = std::env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        let impetusd = parent.join("impetusd");
        if impetusd.exists() {
            return Ok(impetusd.to_string_lossy().to_string());
        }
    }

    if let Ok(output) = Command::new("which").arg("impetusd").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Ok(path);
        }
    }

    if let Ok(current_exe) = std::env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        let impetusd = parent.join("impetusd");
        if impetusd.exists() {
            return Ok(impetusd.to_string_lossy().to_string());
        }
    }

    anyhow::bail!(
        "impetusd not found in PATH or next to impetus binary. Install it or ensure it's in PATH."
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn ensure_daemon_spawn_has_no_privilege_escalation() {
        let src = include_str!("daemon.rs");
        let start = src
            .find("pub async fn ensure_daemon_running")
            .expect("ensure_daemon_running present");
        let end = src[start..]
            .find("\nfn find_impetusd_binary")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let spawn_path = &src[start..end];
        for needle in [
            "sudo",
            "osascript",
            "withAdministratorPrivileges",
            "AuthorizationCreate",
            "SMJobBless",
            "PrivilegedHelperTools",
        ] {
            assert!(
                !spawn_path.contains(needle),
                "CLI daemon spawn must not escalate via `{needle}`"
            );
        }
        assert!(
            spawn_path.contains("Command::new(&impetusd_path)"),
            "CLI must spawn impetusd directly as the current user"
        );
        assert!(
            spawn_path.contains("IMPETUS_SOCKET"),
            "lazy-start must pass IMPETUS_SOCKET so client and daemon agree"
        );
        assert!(
            spawn_path.contains("IMPETUS_DATA_DIR"),
            "lazy-start must pass IMPETUS_DATA_DIR for durable state colocation"
        );
    }

    #[test]
    fn discover_socket_defaults_under_user_data_dir() {
        let src = include_str!("daemon.rs");
        let start = src
            .find("pub fn discover_socket_path")
            .expect("discover_socket_path present");
        let end = src[start..]
            .find("\npub async fn ensure_daemon_running")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let discover = &src[start..end];
        assert!(discover.contains("IMPETUS_SOCKET"));
        assert!(discover.contains("harness.sock"));
        assert!(!discover.contains("/var/lib"));
        assert!(!discover.contains("PrivilegedHelperTools"));
    }

    #[test]
    fn default_data_root_is_userspace_only() {
        let src = include_str!("daemon.rs");
        let start = src
            .find("pub fn default_data_root")
            .expect("default_data_root present");
        let end = src[start..]
            .find("\npub async fn ensure_daemon_running")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("Library/Application Support/Impetus") || body.contains("XDG_DATA_HOME")
        );
        assert!(!body.contains("/var/lib"));
        assert!(!body.contains("PrivilegedHelperTools"));
    }
}
