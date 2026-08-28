//! Evidence that the currently available macOS Seatbelt mechanism can confine
//! a child process to one canonical workspace directory.
//!
//! This is intentionally a spike, not the production execution capability.
//! See `docs/MACOS_SANDBOX_SPIKE.md` for its boundary.

#[cfg(target_os = "macos")]
mod macos {
    use std::{
        fs,
        path::PathBuf,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn unique_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("impetus-seatbelt-{nonce}-{}", std::process::id()))
    }

    fn seatbelt_profile(workspace: &std::path::Path) -> String {
        format!(
            "(version 1)\n(deny default)\n(allow process-exec)\n(allow file-read*)\n(allow file-write* (subpath \"{}\"))",
            workspace.display()
        )
    }

    #[test]
    fn seatbelt_allows_workspace_write_and_denies_sibling_write() {
        assert!(
            std::path::Path::new("/usr/bin/sandbox-exec").is_file(),
            "macOS sandbox spike requires /usr/bin/sandbox-exec"
        );

        let root = unique_directory();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("create spike workspace");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let allowed = workspace.join("allowed.txt");
        let blocked = root.join("blocked.txt");
        let profile = seatbelt_profile(&workspace);

        let allowed_status = Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile, "/usr/bin/touch"])
            .arg(&allowed)
            .status()
            .expect("launch sandboxed allowed write");
        assert!(allowed_status.success(), "workspace write must be allowed");
        assert!(allowed.is_file(), "allowed write must create its file");

        let blocked_status = Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile, "/usr/bin/touch"])
            .arg(&blocked)
            .status()
            .expect("launch sandboxed blocked write");
        assert!(
            !blocked_status.success(),
            "sibling write must be rejected by Seatbelt"
        );
        assert!(!blocked.exists(), "blocked write must not create a file");

        fs::remove_dir_all(root).expect("remove spike workspace");
    }
}
