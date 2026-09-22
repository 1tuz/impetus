//! Production daemon wiring helpers (`impetusd`): Explore/role spawn + MCP autoload.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use thiserror::Error;

use crate::child_concurrency::ChildConcurrencyGate;
use crate::child_result_store::ChildResultStore;
use crate::explore_agent_loop::AgentLoopExploreExecutor;
use crate::explore_child::{ExploreChildExecutor, ExploreSpawnBridge, HarnessExploreSpawn};
use crate::extension_compat::McpModule;
use crate::hook_prefilter::HookPrefilter;
use crate::mcp_manifest::McpManifest;
use crate::policy::PolicyEngine;
use crate::policy_store::{PolicyStore, default_policy_store_path};
use crate::provider_trait::ModelProvider;
use crate::role_agent_loop::AgentLoopRoleExecutor;
use crate::role_child::RoleChildExecutor;
use crate::storage::{EventStore, MemoryEventStore};
use crate::tool_provider_runtime::{McpServerSpec, ToolProviderRuntime};
use crate::worktree_manager::WorktreeManager;
use crate::{ProviderError, SqlitePtySessionStore, default_artifact_root};

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
    #[error("hook catalog load: {0}")]
    HookCatalog(String),
    #[error("policy store load: {0}")]
    PolicyStore(String),
    /// WorktreeManager open failed — fail closed so Git IPC does not silently
    /// fall back to workspace root without managed-worktree preference.
    #[error("worktree manager open: {0}")]
    WorktreeManager(String),
    #[error("PTY session store open: {0}")]
    PtySessionStore(String),
    #[error("MCP manage: {0}")]
    McpManage(String),
    #[error("extension runtime reload: {0}")]
    ExtensionReload(String),
}

/// Binding SQLite under the daemon data root (`worktrees.sqlite3`).
pub fn default_worktree_store_path(data_root: &Path) -> PathBuf {
    data_root.join("worktrees.sqlite3")
}

/// Checkout directory root for managed worktrees (`worktrees/`).
pub fn default_worktrees_root(data_root: &Path) -> PathBuf {
    data_root.join("worktrees")
}

/// Open or create [`WorktreeManager`] under `data_root`.
///
/// Paths: `{data_root}/worktrees.sqlite3` (bindings) + `{data_root}/worktrees/`
/// (checkouts). Fail closed on open/IO errors — production daemon must attach
/// the manager so session Git cwd prefers managed worktrees; without it
/// `resolve_session_git_cwd` would silently use the workspace root.
pub fn open_daemon_worktree_manager(
    data_root: &Path,
) -> Result<Arc<WorktreeManager>, DaemonWiringError> {
    let manager = WorktreeManager::open(
        default_worktree_store_path(data_root),
        default_worktrees_root(data_root),
    )
    .map_err(|error| DaemonWiringError::WorktreeManager(error.to_string()))?;
    Ok(Arc::new(manager))
}

/// Build Explore spawn bridge for production daemon (restricted AgentLoop + durable child store).
pub fn build_explore_spawn_bridge(
    data_root: &Path,
    provider: Arc<dyn ModelProvider>,
    policy: PolicyEngine,
    artifact_root: PathBuf,
    parent_events: Arc<dyn EventStore>,
) -> Result<Arc<dyn ExploreSpawnBridge>, DaemonWiringError> {
    let child_store = ChildResultStore::open(data_root.join("child_results.sqlite3"))?;
    let executor = build_agent_loop_explore_executor(provider, policy, artifact_root);
    Ok(Arc::new(HarnessExploreSpawn {
        gate: Arc::new(Mutex::new(ChildConcurrencyGate::new())),
        store: Arc::new(child_store),
        executor,
        parent_events: Some(parent_events),
    }))
}

/// Restricted AgentLoop Explore executor (shared by `explore_spawn` + Workflow Explore).
pub fn build_agent_loop_explore_executor(
    provider: Arc<dyn ModelProvider>,
    policy: PolicyEngine,
    artifact_root: PathBuf,
) -> Arc<dyn ExploreChildExecutor> {
    let store_factory = Box::new(|_child_id: &str| -> Arc<dyn EventStore> {
        Arc::new(MemoryEventStore::default())
    });
    Arc::new(AgentLoopExploreExecutor::new(
        provider,
        store_factory,
        policy,
        artifact_root,
    ))
}

/// Restricted AgentLoop role executor (Workflow Research/Build/Review).
pub fn build_agent_loop_role_executor(
    provider: Arc<dyn ModelProvider>,
    policy: PolicyEngine,
    artifact_root: PathBuf,
) -> Arc<dyn RoleChildExecutor> {
    let store_factory = Box::new(|_child_id: &str| -> Arc<dyn EventStore> {
        Arc::new(MemoryEventStore::default())
    });
    Arc::new(AgentLoopRoleExecutor::new(
        provider,
        store_factory,
        policy,
        artifact_root,
    ))
}

/// Convenience: AgentLoop Explore executor from harness default provider.
pub fn build_agent_loop_explore_executor_for_harness(
    provider_registry: &crate::ProviderRegistry,
    default_provider_id: &str,
    policy: PolicyEngine,
) -> Result<Arc<dyn ExploreChildExecutor>, DaemonWiringError> {
    let provider = provider_registry
        .get(default_provider_id)
        .map_err(|source| DaemonWiringError::ProviderUnavailable {
            provider_id: default_provider_id.to_string(),
            source,
        })?;
    Ok(build_agent_loop_explore_executor(
        provider,
        policy,
        default_artifact_root(),
    ))
}

/// Convenience: AgentLoop role executor from harness default provider.
pub fn build_agent_loop_role_executor_for_harness(
    provider_registry: &crate::ProviderRegistry,
    default_provider_id: &str,
    policy: PolicyEngine,
) -> Result<Arc<dyn RoleChildExecutor>, DaemonWiringError> {
    let provider = provider_registry
        .get(default_provider_id)
        .map_err(|source| DaemonWiringError::ProviderUnavailable {
            provider_id: default_provider_id.to_string(),
            source,
        })?;
    Ok(build_agent_loop_role_executor(
        provider,
        policy,
        default_artifact_root(),
    ))
}

/// Convenience wrapper using the harness default provider id.
pub fn build_explore_spawn_bridge_for_harness(
    data_root: &Path,
    provider_registry: &crate::ProviderRegistry,
    default_provider_id: &str,
    policy: PolicyEngine,
    parent_events: Arc<dyn EventStore>,
) -> Result<Arc<dyn ExploreSpawnBridge>, DaemonWiringError> {
    let provider = provider_registry
        .get(default_provider_id)
        .map_err(|source| DaemonWiringError::ProviderUnavailable {
            provider_id: default_provider_id.to_string(),
            source,
        })?;
    build_explore_spawn_bridge(
        data_root,
        provider,
        policy,
        default_artifact_root(),
        parent_events,
    )
}

/// Canonical daemon MCP SoT directory: `{data_root}/mcp/`.
///
/// Only this path is autoloaded / reloaded by `impetusd`. Workspace
/// `{repo}/.impetus/mcp/` (extension lifecycle) is not daemon SoT.
pub fn daemon_mcp_dir(data_root: &Path) -> PathBuf {
    data_root.join("mcp")
}

/// Optional daemon-scoped extension install-state DB:
/// `{data_root}/extensions/install_state.db`.
///
/// Project installs under `{repo}/.impetus/` stay CLI SoT. When this DB exists,
/// daemon restart reloads Enabled rows into [`ExtensionRuntime`].
pub fn daemon_extension_state_db(data_root: &Path) -> PathBuf {
    data_root.join("extensions").join("install_state.db")
}

/// Reload extension runtime from durable daemon extension store (Enabled only).
///
/// Missing DB → empty runtime (not an error). CLI remains the control plane for
/// enable/disable/unload; no marketplace IPC.
pub fn load_daemon_extension_runtime(
    data_root: &Path,
) -> Result<crate::ExtensionRuntime, DaemonWiringError> {
    let db = daemon_extension_state_db(data_root);
    if !db.is_file() {
        return Ok(crate::ExtensionRuntime::empty());
    }
    let store = crate::ExtensionStateStore::open(&db)
        .map_err(|e| DaemonWiringError::ExtensionReload(format!("open {}: {e}", db.display())))?;
    crate::ExtensionRuntime::reload_from_store(&store)
        .map_err(|e| DaemonWiringError::ExtensionReload(format!("reload {}: {e}", db.display())))
}

/// Load MCP server specs from `{data_root}/mcp/*.json` into a session runtime.
///
/// Missing directory → empty runtime (not an error). Any present file must parse
/// and validate (`impetus.mcp.v1` envelope); bad config fails closed.
///
/// `ListMcpServers` reports `connected=false` until first tool use
/// (`ensure_connected`) — honest lazy health, not a live probe on list.
pub fn load_daemon_mcp_runtime(data_root: &Path) -> Result<ToolProviderRuntime, DaemonWiringError> {
    let mcp_dir = daemon_mcp_dir(data_root);
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

fn mcp_enabled_path(mcp_dir: &Path, id: &str) -> PathBuf {
    mcp_dir.join(format!("{id}.json"))
}

fn mcp_disabled_path(mcp_dir: &Path, id: &str) -> PathBuf {
    mcp_dir.join(format!("{id}.json.disabled"))
}

fn mcp_module_from_upsert(
    upsert: &impetus_protocol::McpServerUpsert,
) -> Result<(String, McpModule), DaemonWiringError> {
    use crate::extension_id::normalize_extension_id;
    use std::collections::HashMap;

    let id = normalize_extension_id(&upsert.id)
        .map_err(|e| DaemonWiringError::McpManage(e.to_string()))?;
    let name = if upsert.name.trim().is_empty() {
        id.clone()
    } else {
        upsert.name.clone()
    };
    // Autoload derives id from normalized `name`; keep them aligned.
    let name_id =
        normalize_extension_id(&name).map_err(|e| DaemonWiringError::McpManage(e.to_string()))?;
    if name_id != id {
        return Err(DaemonWiringError::McpManage(format!(
            "mcp upsert id `{id}` must match normalized name `{name_id}`"
        )));
    }
    let mut env = HashMap::new();
    for key in &upsert.env_keys {
        if key.trim().is_empty() {
            return Err(DaemonWiringError::McpManage(
                "mcp env_keys entry must be non-empty".into(),
            ));
        }
        // Labels only — never secret values on disk via manage API.
        env.insert(key.clone(), String::new());
    }
    let module = McpModule {
        name,
        command: upsert.command.clone(),
        args: upsert.args.clone(),
        env,
        transport: upsert.transport,
        capabilities: upsert.capabilities.clone(),
    };
    let manifest = McpManifest::from_module(&module)
        .map_err(|e| DaemonWiringError::McpManage(e.to_string()))?;
    if manifest.id != id {
        return Err(DaemonWiringError::McpManage(format!(
            "manifest id `{}` diverged from upsert id `{id}`",
            manifest.id
        )));
    }
    Ok((id, module))
}

/// Write/replace `{data_root}/mcp/{id}.json` and clear any disabled twin.
pub fn upsert_daemon_mcp_server(
    data_root: &Path,
    upsert: &impetus_protocol::McpServerUpsert,
) -> Result<(), DaemonWiringError> {
    let (id, module) = mcp_module_from_upsert(upsert)?;
    let mcp_dir = daemon_mcp_dir(data_root);
    std::fs::create_dir_all(&mcp_dir)
        .map_err(|e| DaemonWiringError::McpManage(format!("create {}: {e}", mcp_dir.display())))?;
    let enabled = mcp_enabled_path(&mcp_dir, &id);
    let disabled = mcp_disabled_path(&mcp_dir, &id);
    let bytes = serde_json::to_vec_pretty(&module)
        .map_err(|e| DaemonWiringError::McpManage(format!("serialize {id}: {e}")))?;
    std::fs::write(&enabled, bytes)
        .map_err(|e| DaemonWiringError::McpManage(format!("write {}: {e}", enabled.display())))?;
    let _ = std::fs::remove_file(&disabled);
    Ok(())
}

/// Delete enabled and disabled forms of `{id}` under daemon MCP SoT.
pub fn remove_daemon_mcp_server(data_root: &Path, id: &str) -> Result<(), DaemonWiringError> {
    let id = crate::extension_id::normalize_extension_id(id)
        .map_err(|e| DaemonWiringError::McpManage(e.to_string()))?;
    let mcp_dir = daemon_mcp_dir(data_root);
    let enabled = mcp_enabled_path(&mcp_dir, &id);
    let disabled = mcp_disabled_path(&mcp_dir, &id);
    let mut removed = false;
    for path in [&enabled, &disabled] {
        match std::fs::remove_file(path) {
            Ok(()) => removed = true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(DaemonWiringError::McpManage(format!(
                    "remove {}: {err}",
                    path.display()
                )));
            }
        }
    }
    if !removed {
        return Err(DaemonWiringError::McpManage(format!(
            "mcp server `{id}` not found"
        )));
    }
    Ok(())
}

/// Enable (`*.json.disabled` → `*.json`) or disable (`*.json` → `*.json.disabled`).
pub fn set_daemon_mcp_enabled(
    data_root: &Path,
    id: &str,
    enabled: bool,
) -> Result<(), DaemonWiringError> {
    let id = crate::extension_id::normalize_extension_id(id)
        .map_err(|e| DaemonWiringError::McpManage(e.to_string()))?;
    let mcp_dir = daemon_mcp_dir(data_root);
    let on = mcp_enabled_path(&mcp_dir, &id);
    let off = mcp_disabled_path(&mcp_dir, &id);
    if enabled {
        if on.is_file() {
            return Ok(());
        }
        if !off.is_file() {
            return Err(DaemonWiringError::McpManage(format!(
                "mcp server `{id}` not found"
            )));
        }
        std::fs::rename(&off, &on)
            .map_err(|e| DaemonWiringError::McpManage(format!("enable {}: {e}", off.display())))?;
    } else {
        if off.is_file() {
            let _ = std::fs::remove_file(&on);
            return Ok(());
        }
        if !on.is_file() {
            return Err(DaemonWiringError::McpManage(format!(
                "mcp server `{id}` not found"
            )));
        }
        std::fs::rename(&on, &off)
            .map_err(|e| DaemonWiringError::McpManage(format!("disable {}: {e}", on.display())))?;
    }
    Ok(())
}

/// Durable PTY metadata DB under the daemon data root.
pub fn default_pty_session_store_path(data_root: &Path) -> PathBuf {
    data_root.join("pty_sessions.sqlite3")
}

/// Open SqlitePtySessionStore under `data_root` (fail closed).
pub fn open_daemon_pty_session_store(
    data_root: &Path,
) -> Result<Arc<SqlitePtySessionStore>, DaemonWiringError> {
    let store = SqlitePtySessionStore::new(default_pty_session_store_path(data_root))
        .map_err(|error| DaemonWiringError::PtySessionStore(error.to_string()))?;
    Ok(Arc::new(store))
}

/// Load hook prefilter catalog from `{data_root}/hooks.json` and/or `hooks/*.json`.
///
/// Missing paths → empty catalog. Any present file must parse; bad config fails closed.
pub fn load_daemon_hook_prefilter(data_root: &Path) -> Result<HookPrefilter, DaemonWiringError> {
    HookPrefilter::load_daemon_catalog(data_root)
        .map_err(|error| DaemonWiringError::HookCatalog(error.to_string()))
}

/// Load optional governed-instruction catalog from `{data_root}/policy_store.json`.
pub fn load_daemon_policy_store(
    data_root: &Path,
) -> Result<Option<PolicyStore>, DaemonWiringError> {
    PolicyStore::load_optional(default_policy_store_path(data_root))
        .map_err(|error| DaemonWiringError::PolicyStore(error.to_string()))
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
        let parent_events: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let bridge = build_explore_spawn_bridge(
            data.path(),
            provider,
            policy,
            workspace.path().to_path_buf(),
            parent_events,
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
    fn extension_reload_missing_db_is_empty() {
        let data = tempfile::tempdir().expect("data");
        let runtime = load_daemon_extension_runtime(data.path()).expect("empty");
        assert!(runtime.loaded_ids().is_empty());
    }

    #[test]
    fn extension_reload_skips_disabled_after_restart() {
        use crate::extension_compat::ExtensionSource;
        use crate::{
            ExtensionLifecycleStatus, ExtensionState, ExtensionStateStore, ResolutionPlan,
        };

        let data = tempfile::tempdir().expect("data");
        let db = daemon_extension_state_db(data.path());
        std::fs::create_dir_all(db.parent().unwrap()).expect("mkdir");
        let store = ExtensionStateStore::open(&db).expect("open");

        let enabled = ExtensionState {
            installation_id: "inst-enabled".into(),
            resolution: ResolutionPlan {
                source: ExtensionSource::AgentSkills,
                module_id: "ok-skill".into(),
                module_name: "ok".into(),
                version: "0.1.0".into(),
                source_path: data.path().join("src/SKILL.md"),
            },
            created_paths: vec![],
            modified_paths: vec![],
            ownership: vec![],
            status: ExtensionLifecycleStatus::Enabled,
        };
        let disabled = ExtensionState {
            installation_id: "inst-disabled".into(),
            resolution: ResolutionPlan {
                source: ExtensionSource::AgentSkills,
                module_id: "off-skill".into(),
                module_name: "off".into(),
                version: "0.1.0".into(),
                source_path: data.path().join("src2/SKILL.md"),
            },
            created_paths: vec![],
            modified_paths: vec![],
            ownership: vec![],
            status: ExtensionLifecycleStatus::Disabled,
        };
        store.put(&enabled).expect("put enabled");
        store.put(&disabled).expect("put disabled");

        let runtime = load_daemon_extension_runtime(data.path()).expect("reload");
        assert!(runtime.is_loaded("inst-enabled"));
        assert!(!runtime.is_loaded("inst-disabled"));
        assert_eq!(runtime.loaded_ids(), vec!["inst-enabled".to_string()]);
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
        let status = runtime.list_status();
        assert!(!status[0].connected);
    }

    #[test]
    fn mcp_manage_upsert_disable_skips_autoload() {
        use impetus_protocol::{McpCapabilities, McpServerUpsert, McpTransport};

        let data = tempfile::tempdir().expect("data");
        upsert_daemon_mcp_server(
            data.path(),
            &McpServerUpsert {
                id: "echo".into(),
                name: "echo".into(),
                command: "true".into(),
                args: vec![],
                transport: McpTransport::Stdio,
                capabilities: McpCapabilities {
                    tools: true,
                    ..McpCapabilities::default()
                },
                env_keys: vec![],
            },
        )
        .expect("upsert");
        let runtime = load_daemon_mcp_runtime(data.path()).expect("load");
        assert_eq!(runtime.registered_ids(), vec!["echo".to_string()]);

        set_daemon_mcp_enabled(data.path(), "echo", false).expect("disable");
        let runtime = load_daemon_mcp_runtime(data.path()).expect("load disabled");
        assert!(runtime.registered_ids().is_empty());

        set_daemon_mcp_enabled(data.path(), "echo", true).expect("enable");
        remove_daemon_mcp_server(data.path(), "echo").expect("remove");
        let runtime = load_daemon_mcp_runtime(data.path()).expect("load removed");
        assert!(runtime.registered_ids().is_empty());
    }

    #[test]
    fn pty_session_store_opens_under_data_root() {
        let data = tempfile::tempdir().expect("data");
        let store = open_daemon_pty_session_store(data.path()).expect("open");
        assert!(default_pty_session_store_path(data.path()).is_file());
        let _ = store;
    }

    #[test]
    fn agent_loop_explore_executor_builds_for_mock_provider() {
        let registry = crate::ProviderRegistry::new();
        let mock = Arc::new(MockProvider::default_mock());
        registry.register(mock).expect("register");
        let exec = build_agent_loop_explore_executor_for_harness(
            &registry,
            "mock",
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        )
        .expect("executor");
        let _ = exec;
    }

    #[test]
    fn agent_loop_role_executor_builds_for_mock_provider() {
        let registry = crate::ProviderRegistry::new();
        let mock = Arc::new(MockProvider::default_mock());
        registry.register(mock).expect("register");
        let exec = build_agent_loop_role_executor_for_harness(
            &registry,
            "mock",
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        )
        .expect("executor");
        let _ = exec;
    }

    #[test]
    fn hook_autoload_missing_paths_is_empty() {
        let data = tempfile::tempdir().expect("data");
        let catalog = load_daemon_hook_prefilter(data.path()).expect("empty");
        assert!(catalog.rules().is_empty());
    }

    #[test]
    fn hook_autoload_rejects_invalid_json() {
        let data = tempfile::tempdir().expect("data");
        std::fs::write(data.path().join("hooks.json"), b"{").expect("write");
        assert!(matches!(
            load_daemon_hook_prefilter(data.path()),
            Err(DaemonWiringError::HookCatalog(_))
        ));
    }

    #[test]
    fn policy_store_autoload_missing_is_none() {
        let data = tempfile::tempdir().expect("data");
        assert_eq!(load_daemon_policy_store(data.path()).expect("none"), None);
    }

    #[test]
    fn worktree_manager_opens_under_data_root() {
        let data = tempfile::tempdir().expect("data");
        let mgr = open_daemon_worktree_manager(data.path()).expect("open");
        assert!(default_worktree_store_path(data.path()).is_file());
        assert!(default_worktrees_root(data.path()).is_dir());
        // Re-open same store (daemon restart).
        let _again = open_daemon_worktree_manager(data.path()).expect("reopen");
        let _ = mgr;
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
                Arc::new(MemoryEventStore::default()),
            ),
            Err(DaemonWiringError::ProviderUnavailable { .. })
        ));
    }
}
