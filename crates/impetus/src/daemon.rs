use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;

/// Check if impetusd is running by attempting to connect to the socket
pub async fn is_daemon_running(socket_path: &str) -> bool {
    impetus_client::UnixSocketTransport::connect(socket_path)
        .await
        .is_ok()
}

/// Discover socket path from environment or default location
pub fn discover_socket_path() -> String {
    std::env::var("IMPETUS_SOCKET").unwrap_or_else(|_| {
        let home = std::env::var("HOME").expect("HOME not set");
        format!("{home}/Library/Application Support/Impetus/harness.sock")
    })
}

/// Attempt to spawn impetusd daemon if not already running
pub async fn ensure_daemon_running(socket_path: &str) -> Result<()> {
    // Check if already running
    if is_daemon_running(socket_path).await {
        return Ok(());
    }

    // Remove stale socket if it exists
    let socket = Path::new(socket_path);
    if socket.exists() {
        std::fs::remove_file(socket).context("failed to remove stale socket")?;
    }

    // Spawn daemon
    let impetusd_path = find_impetusd_binary()?;

    Command::new(&impetusd_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn impetusd")?;

    // Wait for daemon to start (up to 3 seconds)
    for attempt in 1..=6 {
        sleep(Duration::from_millis(500)).await;
        if is_daemon_running(socket_path).await {
            return Ok(());
        }
        if attempt == 6 {
            anyhow::bail!(
                "impetusd did not start within 3 seconds. Check logs or run manually: {}",
                impetusd_path
            );
        }
    }

    Ok(())
}

/// Find impetusd binary in PATH or next to impetus binary
fn find_impetusd_binary() -> Result<String> {
    // Development mode: look for target/debug/impetusd
    if cfg!(debug_assertions)
        && let Ok(current_exe) = std::env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        let impetusd = parent.join("impetusd");
        if impetusd.exists() {
            return Ok(impetusd.to_string_lossy().to_string());
        }
    }

    // Try PATH
    if let Ok(output) = Command::new("which").arg("impetusd").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Ok(path);
        }
    }

    // Try next to current binary (release mode)
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
    }

    #[test]
    fn discover_socket_defaults_under_user_application_support() {
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
        assert!(discover.contains("Library/Application Support/Impetus/harness.sock"));
        assert!(!discover.contains("/var/lib"));
        assert!(!discover.contains("PrivilegedHelperTools"));
    }
}
