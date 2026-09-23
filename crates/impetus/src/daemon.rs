//! Thin CLI adapter over [`impetus_daemon_control`].
//!
//! Lifecycle (flock / spawn / stale recovery / readiness) lives in the shared
//! crate. This module only resolves CLI-specific binary location and maps
//! types for `main.rs` call sites.

use anyhow::{Result, bail};
use impetus_daemon_control::DaemonOptions;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Discover socket path as `String` for CLI call sites (`main` / doctor / TUI).
pub fn discover_socket_path() -> String {
    impetus_daemon_control::discover_socket_path()
        .to_string_lossy()
        .into_owned()
}

/// Platform default data root (userspace only).
pub fn default_data_root() -> PathBuf {
    impetus_daemon_control::default_data_root()
}

/// Ensure `impetusd` is running for this CLI socket path.
///
/// Resolves `data_dir` and the daemon binary (CLI PATH / next-to-exe rules),
/// then delegates lifecycle to [`impetus_daemon_control::ensure_daemon_running`].
pub async fn ensure_daemon_running(socket_path: &str) -> Result<()> {
    let data_dir = std::env::var_os("IMPETUS_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| Path::new(socket_path).parent().map(Path::to_path_buf))
        .unwrap_or_else(default_data_root);

    let daemon_binary = PathBuf::from(find_impetusd_binary()?);
    let opts = DaemonOptions::new(PathBuf::from(socket_path), data_dir, daemon_binary);

    impetus_daemon_control::ensure_daemon_running(&opts)
        .await
        .map_err(anyhow::Error::from)
}

/// Find impetusd binary: env override → debug next-to-exe → `which` → next-to-exe.
fn find_impetusd_binary() -> Result<String> {
    if let Ok(override_path) = std::env::var("IMPETUS_IMPETUSD_PATH")
        && !override_path.is_empty()
    {
        return Ok(override_path);
    }

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

    bail!(
        "impetusd not found in PATH or next to impetus binary. Install it or ensure it's in PATH."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_adapter_delegates_lifecycle_to_daemon_control() {
        let src = include_str!("daemon.rs");
        let prod_end = src.find("\n#[cfg(test)]").unwrap_or(src.len());
        let prod = &src[..prod_end];
        assert!(
            prod.contains("impetus_daemon_control::ensure_daemon_running"),
            "CLI must call shared ensure_daemon_running"
        );
        assert!(
            prod.contains("DaemonOptions::new"),
            "CLI must build DaemonOptions"
        );
        assert!(
            !prod.contains("libc::flock") && !prod.contains("struct Spawn"),
            "CLI must not duplicate flock lifecycle"
        );
        assert!(
            !prod.contains("fn spawn_impetusd") && !prod.contains("Command::new(&impetusd_path)"),
            "CLI must not duplicate spawn"
        );
        for needle in [
            "sudo",
            "osascript",
            "withAdministratorPrivileges",
            "AuthorizationCreate",
            "SMJobBless",
            "PrivilegedHelperTools",
        ] {
            assert!(
                !prod.contains(needle),
                "CLI daemon adapter must not escalate via `{needle}`"
            );
        }
    }

    #[test]
    fn find_impetusd_binary_is_userspace_resolver() {
        let src = include_str!("daemon.rs");
        let start = src
            .find("fn find_impetusd_binary")
            .expect("find_impetusd_binary present");
        let end = src[start..]
            .find("\n#[cfg(test)]")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(body.contains("IMPETUS_IMPETUSD_PATH"));
        assert!(body.contains("which") && body.contains("impetusd"));
        assert!(body.contains("current_exe"));
        assert!(!body.contains("PrivilegedHelperTools"));
        assert!(!body.contains("sudo"));
    }

    #[test]
    fn discover_socket_returns_string_for_main() {
        let path = discover_socket_path();
        assert!(!path.is_empty());
        // main.rs passes `&socket_path` into ensure / connect as &str.
        let _: &str = &path;
    }

    #[test]
    fn default_data_root_is_userspace_only() {
        let root = default_data_root();
        let s = root.to_string_lossy();
        assert!(!s.contains("/var/lib"));
        assert!(!s.contains("PrivilegedHelperTools"));
        assert!(
            s.contains("Application Support/Impetus")
                || s.contains("/impetus")
                || s.contains("Impetus"),
            "unexpected data root: {s}"
        );
    }
}
