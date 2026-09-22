//! Session-owned MCP tool provider manager (outside AgentLoop).
//!
//! Lazy: `register` only stores specs; connect/discover happens on
//! `ensure_connected` / `ensure_tool`. Harness injects a merged
//! [`McpLiveBridge`] into [`crate::ToolOrchestrator`] when configured.
//! Explore children must not receive this runtime.

use crate::extension_compat::{McpCapabilities, McpModule};
use crate::mcp_adapter::McpAdapter;
use crate::mcp_live::{McpLiveBridge, McpLiveToolEntry};
use std::collections::HashMap;
use std::sync::Arc;

/// One MCP server the session may connect to.
///
/// `module.env` holds labels only — never put secrets into events/logs.
#[derive(Debug, Clone)]
pub struct McpServerSpec {
    /// Catalog server name (the `server` segment in `mcp:{server}:{tool}`).
    pub id: String,
    pub module: McpModule,
}

pub use impetus_protocol::McpServerStatus;

struct ServerSlot {
    module: Option<McpModule>,
    bridge: Option<Arc<McpLiveBridge>>,
}

/// Session-scoped MCP connection manager.
#[derive(Default)]
pub struct ToolProviderRuntime {
    servers: HashMap<String, ServerSlot>,
}

impl ToolProviderRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a server without connecting (lazy).
    pub fn register(&mut self, spec: McpServerSpec) {
        self.servers.insert(
            spec.id,
            ServerSlot {
                module: Some(spec.module),
                bridge: None,
            },
        );
    }

    /// Replace the full registry (daemon `ReloadMcpServers`).
    ///
    /// Drops cached bridges — `connected=false` until next `ensure_connected`.
    pub fn replace_all(&mut self, other: ToolProviderRuntime) {
        *self = other;
    }

    /// Test/inject hook: attach a prebuilt live bridge (no process spawn).
    #[cfg(test)]
    pub fn register_live_bridge(
        &mut self,
        server_id: impl Into<String>,
        bridge: Arc<McpLiveBridge>,
    ) {
        self.servers.insert(
            server_id.into(),
            ServerSlot {
                module: None,
                bridge: Some(bridge),
            },
        );
    }

    pub fn is_connected(&self, server_id: &str) -> bool {
        self.servers
            .get(server_id)
            .and_then(|s| s.bridge.as_ref())
            .is_some()
    }

    pub fn registered_ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.servers.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Labels + connection flag for every registered server (no secrets/env/args).
    pub fn list_status(&self) -> Vec<McpServerStatus> {
        self.registered_ids()
            .into_iter()
            .filter_map(|id| {
                let slot = self.servers.get(&id)?;
                let connected = slot.bridge.is_some();
                let (name, transport, capabilities) = match &slot.module {
                    Some(module) => (
                        module.name.clone(),
                        Some(module.transport),
                        module.capabilities.clone(),
                    ),
                    None => (id.clone(), None, McpCapabilities::default()),
                };
                Some(McpServerStatus {
                    id,
                    name,
                    transport,
                    connected,
                    capabilities,
                })
            })
            .collect()
    }

    /// Connect + discover for one registered server; cache the bridge.
    pub async fn ensure_connected(&mut self, server_id: &str) -> anyhow::Result<()> {
        let slot = self
            .servers
            .get_mut(server_id)
            .ok_or_else(|| anyhow::anyhow!("unknown MCP server `{server_id}`"))?;
        if slot.bridge.is_some() {
            return Ok(());
        }
        let module = slot
            .module
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "MCP server `{server_id}` has no module to connect (test bridge missing?)"
                )
            })?
            .clone();
        let adapter = McpAdapter::connect(&module).await?;
        let bridge = McpLiveBridge::discover(server_id, adapter).await?;
        slot.bridge = Some(Arc::new(bridge));
        Ok(())
    }

    /// Drop the live bridge for a server (keeps registration).
    pub async fn disconnect(&mut self, server_id: &str) {
        if let Some(slot) = self.servers.get_mut(server_id) {
            slot.bridge = None;
        }
    }

    /// Disconnect then connect again.
    pub async fn reconnect(&mut self, server_id: &str) -> anyhow::Result<()> {
        self.disconnect(server_id).await;
        self.ensure_connected(server_id).await
    }

    /// Disconnect every server (explicit; async Drop is not used).
    pub async fn cleanup(&mut self) {
        for slot in self.servers.values_mut() {
            slot.bridge = None;
        }
    }

    /// Ensure the server named in `mcp:server:tool` is connected.
    pub async fn ensure_tool(&mut self, catalog_tool_name: &str) -> anyhow::Result<()> {
        let (server, _) = parse_mcp_catalog_name(catalog_tool_name)
            .ok_or_else(|| anyhow::anyhow!("not an MCP catalog name: `{catalog_tool_name}`"))?;
        self.ensure_connected(server).await
    }

    /// Connect every registered module-backed server once.
    pub async fn ensure_all_registered(&mut self) -> anyhow::Result<()> {
        let ids = self.registered_ids();
        for id in ids {
            let needs_connect = self
                .servers
                .get(&id)
                .is_some_and(|s| s.bridge.is_none() && s.module.is_some());
            if needs_connect {
                self.ensure_connected(&id).await?;
            }
        }
        Ok(())
    }

    /// Merged catalog for orchestrator. Only connected servers. Optional child ⊆ parent filter.
    pub fn bridge(&self, allowed_catalog_names: Option<&[String]>) -> Option<Arc<McpLiveBridge>> {
        let connected: HashMap<String, Arc<McpLiveBridge>> = self
            .servers
            .iter()
            .filter_map(|(id, slot)| slot.bridge.as_ref().map(|b| (id.clone(), Arc::clone(b))))
            .collect();
        if connected.is_empty() {
            return None;
        }
        let merged = McpLiveBridge::merge(connected);
        let filtered = match allowed_catalog_names {
            Some(allowed) => merged.with_allowlist(allowed),
            None => merged,
        };
        Some(Arc::new(filtered))
    }
}

/// Keep only tools whose catalog name is in `parent_allowed` (child ⊆ parent).
/// `None` means no extra filter (all tools kept).
pub fn filter_mcp_catalog(
    parent_allowed: Option<&[String]>,
    tools: impl IntoIterator<Item = McpLiveToolEntry>,
) -> Vec<McpLiveToolEntry> {
    let tools: Vec<_> = tools.into_iter().collect();
    match parent_allowed {
        None => tools,
        Some(allowed) => tools
            .into_iter()
            .filter(|t| allowed.iter().any(|name| name == &t.catalog_name))
            .collect(),
    }
}

/// Parse `mcp:{server}:{tool}` → (server, tool).
pub fn parse_mcp_catalog_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp:")?;
    let (server, tool) = rest.split_once(':')?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_adapter::McpTool;
    use crate::mcp_live::{McpLiveCallResult, McpLiveCaller, catalog_name};
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingCaller {
        calls: AtomicUsize,
        result: McpLiveCallResult,
    }

    #[async_trait]
    impl McpLiveCaller for CountingCaller {
        async fn call_tool(&self, _tool: &str, _arguments: Value) -> McpLiveCallResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    fn echo_bridge(server: &str) -> Arc<McpLiveBridge> {
        let tool = McpTool {
            name: "echo".into(),
            description: "Echo".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"]
            }),
            annotations: Some(json!({"readOnlyHint": true})),
        };
        let caller = Arc::new(CountingCaller {
            calls: AtomicUsize::new(0),
            result: McpLiveCallResult::Ok {
                preview: format!("echo:{server}"),
            },
        });
        Arc::new(McpLiveBridge::from_tools(server, vec![tool], caller))
    }

    #[test]
    fn register_does_not_connect() {
        let mut runtime = ToolProviderRuntime::new();
        runtime.register(McpServerSpec {
            id: "mock".into(),
            module: McpModule {
                name: "mock".into(),
                command: "false".into(),
                args: vec![],
                env: HashMap::new(),
                transport: crate::extension_compat::McpTransport::Stdio,
                capabilities: crate::extension_compat::McpCapabilities {
                    tools: true,
                    ..Default::default()
                },
            },
        });
        assert!(!runtime.is_connected("mock"));
        assert!(runtime.bridge(None).is_none());
        let status = runtime.list_status();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].id, "mock");
        assert!(!status[0].connected);
        assert_eq!(
            status[0].transport,
            Some(crate::extension_compat::McpTransport::Stdio)
        );
        // Catalog status must not expose env/args/command secrets surface.
        let encoded = serde_json::to_string(&status[0]).expect("encode");
        assert!(!encoded.contains("command"));
        assert!(!encoded.contains("args"));
        assert!(!encoded.contains("\"env\""));
        assert!(!encoded.contains("API_KEY"));
    }

    #[tokio::test]
    async fn ensure_connected_via_live_bridge_lists_tools() {
        let mut runtime = ToolProviderRuntime::new();
        runtime.register_live_bridge("mock", echo_bridge("mock"));
        assert!(runtime.is_connected("mock"));
        runtime
            .ensure_connected("mock")
            .await
            .expect("already live");
        let bridge = runtime.bridge(None).expect("bridge");
        assert!(bridge.contains("mcp:mock:echo"));
    }

    #[tokio::test]
    async fn reconnect_clears_and_rediscovers() {
        let mut runtime = ToolProviderRuntime::new();
        let (adapter, server_task) = crate::mcp_adapter::tests::duplex_adapter().await;
        let bridge = Arc::new(
            McpLiveBridge::discover("mock", adapter)
                .await
                .expect("discover"),
        );
        runtime.register_live_bridge("mock", bridge);
        assert!(runtime.is_connected("mock"));
        runtime.disconnect("mock").await;
        assert!(!runtime.is_connected("mock"));
        // Re-inject after clear (process-backed reconnect needs a module; inject proves clear).
        let (adapter2, server_task2) = crate::mcp_adapter::tests::duplex_adapter().await;
        let bridge2 = Arc::new(
            McpLiveBridge::discover("mock", adapter2)
                .await
                .expect("rediscover"),
        );
        runtime.register_live_bridge("mock", bridge2);
        assert!(runtime.bridge(None).unwrap().contains("mcp:mock:echo"));
        server_task.abort();
        server_task2.abort();
    }

    #[tokio::test]
    async fn cleanup_empties_bridges() {
        let mut runtime = ToolProviderRuntime::new();
        runtime.register_live_bridge("a", echo_bridge("a"));
        runtime.register_live_bridge("b", echo_bridge("b"));
        assert!(runtime.bridge(None).is_some());
        runtime.cleanup().await;
        assert!(runtime.bridge(None).is_none());
        assert!(!runtime.is_connected("a"));
        assert!(!runtime.is_connected("b"));
    }

    #[test]
    fn filter_allowlist_is_subset_of_parent() {
        let tools = vec![
            McpLiveToolEntry::from_mcp_tool(
                "mock",
                &McpTool {
                    name: "echo".into(),
                    description: "Echo".into(),
                    input_schema: json!({"type": "object"}),
                    annotations: Some(json!({"readOnlyHint": true})),
                },
            ),
            McpLiveToolEntry::from_mcp_tool(
                "mock",
                &McpTool {
                    name: "sum".into(),
                    description: "Sum".into(),
                    input_schema: json!({"type": "object"}),
                    annotations: None,
                },
            ),
        ];
        let parent = vec![catalog_name("mock", "echo")];
        let filtered = filter_mcp_catalog(Some(&parent), tools);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].catalog_name, "mcp:mock:echo");
    }

    #[tokio::test]
    async fn merged_bridge_routes_call_to_owning_server() {
        let mut runtime = ToolProviderRuntime::new();
        let a = echo_bridge("a");
        let b = echo_bridge("b");
        runtime.register_live_bridge("a", a);
        runtime.register_live_bridge("b", b);
        let bridge = runtime.bridge(None).expect("merged");
        assert!(bridge.contains("mcp:a:echo"));
        assert!(bridge.contains("mcp:b:echo"));
        let result = bridge.call("mcp:a:echo", json!({"text": "x"})).await;
        assert_eq!(
            result,
            McpLiveCallResult::Ok {
                preview: "echo:a".into()
            }
        );
    }

    #[tokio::test]
    async fn bridge_allowlist_filters_child_subset() {
        let mut runtime = ToolProviderRuntime::new();
        runtime.register_live_bridge("mock", echo_bridge("mock"));
        let allowed = vec!["mcp:mock:echo".to_string()];
        let bridge = runtime.bridge(Some(&allowed)).expect("bridge");
        assert!(bridge.contains("mcp:mock:echo"));
        let empty = runtime.bridge(Some(&[]));
        assert_eq!(empty.expect("empty filter").tool_count(), 0);
    }

    #[tokio::test]
    async fn ensure_tool_parses_catalog_name() {
        let mut runtime = ToolProviderRuntime::new();
        runtime.register_live_bridge("mock", echo_bridge("mock"));
        runtime
            .ensure_tool("mcp:mock:echo")
            .await
            .expect("ensure tool");
        assert!(parse_mcp_catalog_name("mcp:mock:echo") == Some(("mock", "echo")));
        assert!(parse_mcp_catalog_name("echo").is_none());
    }

    #[tokio::test]
    async fn orchestrator_processes_mcp_tool_from_runtime_bridge() {
        use crate::{
            AgentRuntime, MemoryEventStore, PolicyEngine, SandboxScope, ToolOrchestrator,
            ToolOutcomeStatus,
        };
        use uuid::Uuid;

        let workspace = tempfile::tempdir().expect("temp workspace");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let agent = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            policy.clone(),
        ));

        let mut runtime = ToolProviderRuntime::new();
        runtime.register_live_bridge("mock", echo_bridge("mock"));
        let bridge = runtime.bridge(None).expect("bridge");
        let orch =
            ToolOrchestrator::new(policy, workspace.path().to_path_buf()).with_mcp_live(bridge);

        let observations = orch
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "mcp-1".into(),
                    name: "mcp:mock:echo".into(),
                    arguments: json!({"text": "live"}),
                }],
                &agent,
            )
            .await
            .expect("mcp batch");

        assert_eq!(observations[0].outcome, ToolOutcomeStatus::Success);
        assert_eq!(observations[0].preview, "echo:mock");
    }
}
