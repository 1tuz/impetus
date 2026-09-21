//! Production daemon wiring helpers (`impetusd`): Explore spawn + MCP autoload.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use thiserror::Error;

use crate::child_concurrency::ChildConcurrencyGate;
use crate::child_result_store::ChildResultStore;
use crate::explore_agent_loop::AgentLoopExploreExecutor;
use crate::explore_child::{ExploreSpawnBridge, HarnessExploreSpawn};
use crate::extension_compat::McpModule;
use crate::mcp_manifest::McpManifest;
use crate::policy::PolicyEngine;
use crate::provider_trait::ModelProvider;
use crate::storage::{EventStore, MemoryEventStore};
use crate::tool_provider_runtime::{McpServerSpec, ToolProviderRuntime};
use crate::{ProviderError, default_artifact_root};

/// Failures building daemon runtime attachments.
#[derive(Debug, Error)]
pub enum DaemonWiringError {
    #[error("provider `{provider_id}` unavailable: {source}")]
    ProviderUnavailable {
        provider_id: String,
        source: ProviderError,
    },
    #[error(transparent)]
    ChildResultStore(#[from] crate::child_result_store::ChildResultError),
    #[error("MCP autoload: {0}")]
    McpAutoload(String),
}

/// Build Explore spawn bridge for production daemon (restricted AgentLoop + durable child store).
pub fn build_explore_spawn_bridge(
    data_root: &Path,
    provider: Arc<dyn ModelProvider>,
    policy: PolicyEngine,
    artifact_root: PathBuf,
) -> Result<Arc<dyn ExploreSpawnBridge>, DaemonWiringError> {
    let child_store = ChildResultStore::open(data_root.join("child_results.sqlite3"))?;
    let store_factory = Box::new(|_child_id: &str| -> Arc<dyn EventStore> {
        Arc::new(MemoryEventStore::default())
    });
    let executor = AgentLoopExploreExecutor::new(provider, store_factory, policy, artifact_root);
    Ok(Arc::new(HarnessExploreSpawn {
        gate: Arc::new(Mutex::new(ChildConcurrencyGate::new())),
        store: Arc::new(child_store),
        executor: Arc::new(executor),
    }))
}

/// Convenience wrapper using the harness default provider id.
pub fn build_explore_spawn_bridge_for_harness(
    data_root: &Path,
    provider_registry: &crate::ProviderRegistry,
    default_provider_id: &str,
    policy: PolicyEngine,
) -> Result<Arc<dyn ExploreSpawnBridge>, DaemonWiringError> {
    let provider = provider_registry
        .get(default_provider_id)
        .map_err(|source| DaemonWiringError::ProviderUnavailable {
            provider_id: default_provider_id.to_string(),
            source,
        })?;
    build_explore_spawn_bridge(data_root, provider, policy, default_artifact_root())
}

/// Load MCP server specs from `{data_root}/mcp/*.json` into a session runtime.
///
/// Missing directory → empty runtime (not an error). Any present file must parse
/// and validate (`impetus.mcp.v1` envelope); bad config fails closed.
pub fn load_daemon_mcp_runtime(data_root: &Path) -> Result<ToolProviderRuntime, DaemonWiringError> {
    let mcp_dir = data_root.join("mcp");
    if !mcp_dir.is_dir() {
        return Ok(ToolProviderRuntime::new());
    }

    let mut runtime = ToolProviderRuntime::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&mcp_dir)
        .map_err(|e| DaemonWiringError::McpAutoload(format!("read {}: {}", mcp_dir.display(), e)))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort();

    for path in entries {
        let bytes = std::fs::read(&path).map_err(|e| {
            DaemonWiringError::McpAutoload(format!("read {}: {}", path.display(), e))
        })?;
        let module: McpModule = serde_json::from_slice(&bytes).map_err(|e| {
            DaemonWiringError::McpAutoload(format!("parse {}: {}", path.display(), e))
        })?;
        let manifest = McpManifest::from_module(&module).map_err(|e| {
            DaemonWiringError::McpAutoload(format!("validate {}: {}", path.display(), e))
        })?;
        runtime.register(McpServerSpec {
            id: manifest.id,
            module,
        });
    }
    Ok(runtime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore_child::{EXPLORE_ALLOWED_TOOLS, ExploreChildRequest};
    use crate::mock_provider::{MockProvider, MockStreamItem};
    use crate::policy::SandboxScope;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn explore_bridge_spawns_with_mock_provider() {
        let data = tempfile::tempdir().expect("data");
        let workspace = tempfile::tempdir().expect("workspace");
        let provider = Arc::new(MockProvider::scripted(
            "daemon-explore",
            "test",
            [vec![MockStreamItem::Chunk {
                chunk_id: 1,
                text: "daemon-explore-summary".into(),
            }]],
        ));
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let bridge = build_explore_spawn_bridge(
            data.path(),
            provider,
            policy,
            workspace.path().to_path_buf(),
        )
        .expect("bridge");

        let outcome = bridge
            .spawn_explore(
                ExploreChildRequest {
                    parent_session_id: "parent-1".into(),
                    child_id: "child-1".into(),
                    cwd: workspace.path().to_path_buf(),
                    allowed_tools: EXPLORE_ALLOWED_TOOLS
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    context_label: "scan module".into(),
                    max_tokens: 2_000,
                    max_time_ms: 10_000,
                    max_depth: 1,
                },
                CancellationToken::new(),
            )
            .expect("spawn");
        assert_eq!(outcome.summary_label, "daemon-explore-summary");
    }

    #[test]
    fn mcp_autoload_missing_dir_is_empty() {
        let data = tempfile::tempdir().expect("data");
        let runtime = load_daemon_mcp_runtime(data.path()).expect("empty");
        assert!(runtime.registered_ids().is_empty());
    }

    #[test]
    fn mcp_autoload_rejects_invalid_json() {
        let data = tempfile::tempdir().expect("data");
        let mcp_dir = data.path().join("mcp");
        std::fs::create_dir_all(&mcp_dir).expect("mkdir");
        std::fs::write(mcp_dir.join("bad.json"), b"{not json").expect("write");
        assert!(matches!(
            load_daemon_mcp_runtime(data.path()),
            Err(DaemonWiringError::McpAutoload(_))
        ));
    }

    #[test]
    fn mcp_autoload_registers_valid_stdio_config() {
        let data = tempfile::tempdir().expect("data");
        let mcp_dir = data.path().join("mcp");
        std::fs::create_dir_all(&mcp_dir).expect("mkdir");
        std::fs::write(
            mcp_dir.join("echo.json"),
            br#"{
                "name": "echo",
                "command": "true",
                "args": [],
                "env": {},
                "transport": "stdio",
                "capabilities": { "tools": true, "resources": false, "prompts": false, "sampling": false }
            }"#,
        )
        .expect("write");
        let runtime = load_daemon_mcp_runtime(data.path()).expect("load");
        assert_eq!(runtime.registered_ids(), vec!["echo".to_string()]);
    }

    #[test]
    fn explore_bridge_maps_provider_errors() {
        let data = tempfile::tempdir().expect("data");
        let registry = crate::ProviderRegistry::new();
        assert!(matches!(
            build_explore_spawn_bridge_for_harness(
                data.path(),
                &registry,
                "missing",
                PolicyEngine::new(SandboxScope::local_workspace(".")),
            ),
            Err(DaemonWiringError::ProviderUnavailable { .. })
        ));
    }
}
