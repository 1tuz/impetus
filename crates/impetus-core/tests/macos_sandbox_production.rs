//! Production macOS Seatbelt provider: workspace write allowed, sibling denied.

#[cfg(target_os = "macos")]
mod macos {
    use impetus_core::execution::{SandboxCommandRequest, production_sandbox_provider};
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn unique_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "impetus-seatbelt-prod-{nonce}-{}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn production_seatbelt_allows_workspace_write_and_denies_sibling() {
        let provider = production_sandbox_provider();
        provider.probe().expect("sandbox-exec must be available");

        let root = unique_directory();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("create workspace");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let allowed = workspace.join("allowed.txt");
        let blocked = root.join("blocked.txt");

        let allowed_args = vec![allowed.display().to_string()];
        let mut prepared_ok = provider
            .prepare(&SandboxCommandRequest {
                executable: "/usr/bin/touch",
                args: &allowed_args,
                workspace_root: &workspace,
                working_dir: &workspace,
                explicit_env: &[],
                allow_network: false,
            })
            .expect("prepare allowed write");
        let status_ok = prepared_ok
            .command_mut()
            .status()
            .await
            .expect("spawn allowed write");
        assert!(status_ok.success(), "workspace write must be allowed");
        assert!(allowed.is_file(), "allowed write must create its file");

        let blocked_args = vec![blocked.display().to_string()];
        let mut prepared_deny = provider
            .prepare(&SandboxCommandRequest {
                executable: "/usr/bin/touch",
                args: &blocked_args,
                workspace_root: &workspace,
                working_dir: &workspace,
                explicit_env: &[],
                allow_network: false,
            })
            .expect("prepare blocked write");
        let status_deny = prepared_deny
            .command_mut()
            .status()
            .await
            .expect("spawn blocked write");
        assert!(
            !status_deny.success(),
            "sibling write must be rejected by Seatbelt"
        );
        assert!(!blocked.exists(), "blocked write must not create a file");

        fs::remove_dir_all(root).expect("remove production sandbox workspace");
    }
}
