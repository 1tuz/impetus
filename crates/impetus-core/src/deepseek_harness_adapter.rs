//! DeepSeek Harness / Cordis process bridge.
//!
//! Discovers an out-of-process Cordis/DeepSeek Harness bridge from a JSON
//! manifest. TypeScript/Cordis never loads inside `impetusd` — only an
//! external command + args are recorded for later Module IPC spawn.
//!
//! Manifest schema (version 1):
//! ```json
//! {
//!   "name": "deepseek-harness",
//!   "version": "0.1.0",
//!   "command": "/usr/bin/node",
//!   "args": ["./bridge.js"],
//!   "protocol": "impetus-module-ipc-v1"
//! }
//! ```

use crate::agent_plugins_adapter::slugify;
use crate::extension_compat::{CanonicalModuleKind, CanonicalModuleSpec, ExtensionSource};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Supported Module IPC protocol id for DeepSeek/Cordis bridges.
pub const DEEPSEEK_PROCESS_PROTOCOL: &str = "impetus-module-ipc-v1";

/// On-disk manifest for an out-of-process DeepSeek Harness bridge.
#[derive(Debug, Clone, Deserialize)]
pub struct DeepSeekHarnessManifest {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    /// Absolute or PATH-resolvable command that launches the bridge process.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Must be [`DEEPSEEK_PROCESS_PROTOCOL`].
    pub protocol: String,
}

/// DeepSeek Harness process adapter.
pub struct DeepSeekHarnessAdapter;

impl DeepSeekHarnessAdapter {
    /// Import a bridge manifest file into a canonical module spec.
    ///
    /// Does not spawn the Cordis/TS process. Validates protocol and that the
    /// command path exists when given as an absolute/relative filesystem path.
    pub async fn import(manifest_path: &Path) -> Result<CanonicalModuleSpec> {
        if !manifest_path.is_file() {
            anyhow::bail!(
                "{} is not a DeepSeek Harness bridge manifest file",
                manifest_path.display()
            );
        }

        let bytes = tokio::fs::read(manifest_path)
            .await
            .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
        let manifest: DeepSeekHarnessManifest = serde_json::from_slice(&bytes)
            .context("Failed to parse DeepSeek Harness bridge manifest")?;

        Self::import_manifest(&manifest, manifest_path)
    }

    /// Import an already-parsed manifest (tests / callers that load JSON themselves).
    pub fn import_manifest(
        manifest: &DeepSeekHarnessManifest,
        manifest_path: &Path,
    ) -> Result<CanonicalModuleSpec> {
        if manifest.name.trim().is_empty() {
            anyhow::bail!("DeepSeek Harness manifest name must not be empty");
        }
        if manifest.protocol != DEEPSEEK_PROCESS_PROTOCOL {
            anyhow::bail!(
                "unsupported DeepSeek Harness protocol {:?} (expected {})",
                manifest.protocol,
                DEEPSEEK_PROCESS_PROTOCOL
            );
        }
        if manifest.command.trim().is_empty() {
            anyhow::bail!("DeepSeek Harness manifest command must not be empty");
        }

        validate_command_path(&manifest.command)?;

        let mut metadata = HashMap::new();
        metadata.insert(
            "command".to_string(),
            serde_json::json!(manifest.command.clone()),
        );
        metadata.insert("args".to_string(), serde_json::json!(manifest.args.clone()));
        metadata.insert(
            "protocol".to_string(),
            serde_json::json!(manifest.protocol.clone()),
        );
        metadata.insert(
            "manifest_path".to_string(),
            serde_json::json!(manifest_path.display().to_string()),
        );
        metadata.insert(
            "isolation".to_string(),
            serde_json::json!("external_process"),
        );

        Ok(CanonicalModuleSpec {
            id: slugify(&manifest.name),
            name: manifest.name.clone(),
            version: manifest
                .version
                .clone()
                .unwrap_or_else(|| "0.0.0".to_string()),
            source: ExtensionSource::DeepSeekHarness,
            kind: CanonicalModuleKind::Extension,
            capabilities: vec!["process_adapter".to_string()],
            metadata,
        })
    }
}

fn validate_command_path(command: &str) -> Result<()> {
    let path = PathBuf::from(command);
    // Absolute or explicit relative paths must exist; bare names resolve via PATH later.
    if (path.is_absolute() || command.contains('/') || command.contains('\\')) && !path.exists() {
        anyhow::bail!(
            "DeepSeek Harness bridge command not found at {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn write_file(dir: &Path, rel: &str, content: &str) -> PathBuf {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[tokio::test]
    async fn import_valid_manifest_with_absolute_command() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = write_file(dir.path(), "bridge.sh", "#!/bin/sh\necho ok\n");
        #[cfg(unix)]
        {
            let mut perms = std::fs::metadata(&bridge).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bridge, perms).unwrap();
        }

        let manifest = format!(
            r#"{{
                "name": "deepseek-harness",
                "version": "1.2.3",
                "command": "{}",
                "args": ["--socket"],
                "protocol": "impetus-module-ipc-v1"
            }}"#,
            bridge.display()
        );
        let path = write_file(dir.path(), "bridge.json", &manifest);

        let spec = DeepSeekHarnessAdapter::import(&path).await.unwrap();
        assert_eq!(spec.source, ExtensionSource::DeepSeekHarness);
        assert_eq!(spec.kind, CanonicalModuleKind::Extension);
        assert_eq!(spec.id, "deepseek-harness");
        assert_eq!(spec.version, "1.2.3");
        assert_eq!(spec.capabilities, vec!["process_adapter"]);
        assert_eq!(spec.metadata.get("isolation").unwrap(), "external_process");
        assert_eq!(
            spec.metadata.get("protocol").unwrap(),
            DEEPSEEK_PROCESS_PROTOCOL
        );
    }

    #[tokio::test]
    async fn import_rejects_unknown_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(
            dir.path(),
            "bad.json",
            r#"{
                "name": "x",
                "command": "node",
                "protocol": "cordis-inproc"
            }"#,
        );
        let err = DeepSeekHarnessAdapter::import(&path).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported DeepSeek Harness protocol")
        );
    }

    #[tokio::test]
    async fn import_rejects_missing_absolute_command() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-bridge");
        let manifest = format!(
            r#"{{
                "name": "x",
                "command": "{}",
                "protocol": "impetus-module-ipc-v1"
            }}"#,
            missing.display()
        );
        let path = write_file(dir.path(), "missing.json", &manifest);
        let err = DeepSeekHarnessAdapter::import(&path).await.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn import_allows_path_command_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(
            dir.path(),
            "ok.json",
            r#"{
                "name": "cordis-bridge",
                "command": "node",
                "args": ["bridge.js"],
                "protocol": "impetus-module-ipc-v1"
            }"#,
        );
        let spec = DeepSeekHarnessAdapter::import(&path).await.unwrap();
        assert_eq!(spec.id, "cordis-bridge");
        assert_eq!(spec.metadata.get("command").unwrap(), "node");
    }

    #[tokio::test]
    async fn import_non_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = DeepSeekHarnessAdapter::import(dir.path())
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("not a DeepSeek Harness bridge manifest")
        );
    }
}
