use std::path::PathBuf;

/// Discover socket path from environment or default data-dir location.
///
/// Order: `IMPETUS_SOCKET` → `$IMPETUS_DATA_DIR/harness.sock` → platform
/// default under `$HOME` (same rules as `impetusd`).
pub fn discover_socket_path() -> PathBuf {
    if let Ok(socket) = std::env::var("IMPETUS_SOCKET") {
        return PathBuf::from(socket);
    }
    default_data_root().join("harness.sock")
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
