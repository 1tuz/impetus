//! Live MCP tools bridge for [`crate::ToolOrchestrator`].
//!
//! Discovers tools via `tools/list`, registers them into a session catalog, and
//! invokes `tools/call` after the usual policy → sandbox admission path.
//! Mutating tools default-safe (no `readOnlyHint` → mutating / needs approval).
//! Unknown transport outcomes never auto-retry when semantics are mutating.

use crate::mcp_adapter::{McpAdapter, McpTool};
use crate::module::ExecutionSemantics;
use crate::module_fallback::{OperationOutcome, UnknownOutcomePolicy};
use crate::tool_schema::{ToolArgError, validate_arguments_with_schema};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Catalog name prefix: `mcp:{server}:{tool}`.
pub fn catalog_name(server: &str, tool: &str) -> String {
    format!("mcp:{server}:{tool}")
}

/// One discovered MCP tool ready for orchestrator dispatch.
#[derive(Debug, Clone)]
pub struct McpLiveToolEntry {
    pub catalog_name: String,
    pub server: String,
    pub tool: String,
    pub description: String,
    pub input_schema: Value,
    pub semantics: ExecutionSemantics,
}

impl McpLiveToolEntry {
    pub fn from_mcp_tool(server: &str, tool: &McpTool) -> Self {
        let schema = if tool.input_schema.is_null() {
            serde_json::json!({"type": "object"})
        } else {
            tool.input_schema.clone()
        };
        Self {
            catalog_name: catalog_name(server, &tool.name),
            server: server.to_string(),
            tool: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: schema,
            semantics: semantics_from_annotations(tool.annotations.as_ref()),
        }
    }
}

fn semantics_from_annotations(annotations: Option<&Value>) -> ExecutionSemantics {
    let read_only = annotations
        .and_then(|a| a.get("readOnlyHint"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if read_only {
        ExecutionSemantics::ReadOnly
    } else {
        // ponytail: unknown MCP tools default mutating (approval + no Unknown retry)
        ExecutionSemantics::Mutating
    }
}

/// Result of a live `tools/call` round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpLiveCallResult {
    Ok {
        preview: String,
    },
    Failed {
        message: String,
    },
    /// Request may have reached the server; outcome unknown — do not retry if mutating.
    Unknown {
        message: String,
    },
}

impl McpLiveCallResult {
    pub fn operation_outcome(&self) -> OperationOutcome {
        match self {
            Self::Ok { .. } => OperationOutcome::Success,
            Self::Failed { .. } => OperationOutcome::Failure,
            Self::Unknown { .. } => OperationOutcome::Unknown,
        }
    }
}

/// Transport used by [`McpLiveBridge`] (real adapter or test double).
#[async_trait]
pub trait McpLiveCaller: Send + Sync {
    async fn call_tool(&self, tool: &str, arguments: Value) -> McpLiveCallResult;
}

/// Mutex-wrapped [`McpAdapter`] for concurrent orchestrator dispatch.
pub struct McpAdapterCaller {
    inner: Mutex<McpAdapter>,
}

impl McpAdapterCaller {
    pub fn new(adapter: McpAdapter) -> Self {
        Self {
            inner: Mutex::new(adapter),
        }
    }
}

#[async_trait]
impl McpLiveCaller for McpAdapterCaller {
    async fn call_tool(&self, tool: &str, arguments: Value) -> McpLiveCallResult {
        let mut guard = self.inner.lock().await;
        match guard.call_tool(tool, arguments).await {
            Ok(value) => preview_from_call_result(value),
            Err(error) => classify_transport_error(&error.to_string()),
        }
    }
}

fn preview_from_call_result(value: Value) -> McpLiveCallResult {
    let preview = extract_text_preview(&value);
    if value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        McpLiveCallResult::Failed { message: preview }
    } else {
        McpLiveCallResult::Ok { preview }
    }
}

fn extract_text_preview(value: &Value) -> String {
    if let Some(items) = value.get("content").and_then(Value::as_array) {
        let joined: Vec<&str> = items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect();
        if !joined.is_empty() {
            return crate::tools::redact_text(&joined.join("\n"))
                .chars()
                .take(1024)
                .collect();
        }
    }
    crate::tools::redact_text(&value.to_string())
        .chars()
        .take(1024)
        .collect()
}

fn classify_transport_error(message: &str) -> McpLiveCallResult {
    let lower = message.to_ascii_lowercase();
    if lower.contains("timed out")
        || lower.contains("closed the connection")
        || lower.contains("connection reset")
    {
        McpLiveCallResult::Unknown {
            message: message.to_string(),
        }
    } else {
        McpLiveCallResult::Failed {
            message: message.to_string(),
        }
    }
}

/// Session catalog of discovered MCP tools + call backend.
pub struct McpLiveBridge {
    tools: HashMap<String, McpLiveToolEntry>,
    caller: Arc<dyn McpLiveCaller>,
}

impl McpLiveBridge {
    /// Build a bridge from already-listed tools and a caller.
    pub fn from_tools(server: &str, tools: Vec<McpTool>, caller: Arc<dyn McpLiveCaller>) -> Self {
        let tools = tools
            .into_iter()
            .map(|tool| {
                let entry = McpLiveToolEntry::from_mcp_tool(server, &tool);
                (entry.catalog_name.clone(), entry)
            })
            .collect();
        Self { tools, caller }
    }

    /// Discover via `tools/list` and wrap the adapter as the caller.
    pub async fn discover(server: &str, mut adapter: McpAdapter) -> anyhow::Result<Self> {
        let listed = adapter.list_tools().await?;
        let caller = Arc::new(McpAdapterCaller::new(adapter));
        Ok(Self::from_tools(server, listed, caller))
    }

    pub fn list(&self) -> Vec<&McpLiveToolEntry> {
        let mut entries: Vec<_> = self.tools.values().collect();
        entries.sort_by(|a, b| a.catalog_name.cmp(&b.catalog_name));
        entries
    }

    pub fn get(&self, catalog_name: &str) -> Option<&McpLiveToolEntry> {
        self.tools.get(catalog_name)
    }

    pub fn contains(&self, catalog_name: &str) -> bool {
        self.tools.contains_key(catalog_name)
    }

    pub fn validate_arguments(
        &self,
        catalog_name: &str,
        arguments: &Value,
    ) -> Result<&McpLiveToolEntry, ToolArgError> {
        let entry = self.tools.get(catalog_name).ok_or_else(|| ToolArgError {
            tool: catalog_name.to_string(),
            reason: "unknown MCP tool".into(),
        })?;
        validate_arguments_with_schema(catalog_name, arguments, &entry.input_schema)?;
        Ok(entry)
    }

    pub async fn call(&self, catalog_name: &str, arguments: Value) -> McpLiveCallResult {
        let Some(entry) = self.tools.get(catalog_name) else {
            return McpLiveCallResult::Failed {
                message: format!("unknown MCP tool `{catalog_name}`"),
            };
        };
        let result = self.caller.call_tool(&entry.tool, arguments).await;
        if matches!(result, McpLiveCallResult::Unknown { .. }) {
            let policy = UnknownOutcomePolicy::new(entry.semantics);
            if !policy.can_retry(OperationOutcome::Unknown) {
                return McpLiveCallResult::Unknown {
                    message: format!(
                        "unknown MCP outcome for `{}`; not retrying (semantics={:?})",
                        entry.catalog_name, entry.semantics
                    ),
                };
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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

    #[test]
    fn read_only_hint_maps_to_read_only_semantics() {
        let tool = McpTool {
            name: "echo".into(),
            description: "Echo".into(),
            input_schema: json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
            annotations: Some(json!({"readOnlyHint": true})),
        };
        let entry = McpLiveToolEntry::from_mcp_tool("mock", &tool);
        assert_eq!(entry.catalog_name, "mcp:mock:echo");
        assert_eq!(entry.semantics, ExecutionSemantics::ReadOnly);
    }

    #[test]
    fn missing_hint_defaults_to_mutating() {
        let tool = McpTool {
            name: "sum".into(),
            description: "Sum".into(),
            input_schema: json!({"type": "object"}),
            annotations: None,
        };
        let entry = McpLiveToolEntry::from_mcp_tool("mock", &tool);
        assert_eq!(entry.semantics, ExecutionSemantics::Mutating);
    }

    #[tokio::test]
    async fn unknown_outcome_does_not_retry_mutating() {
        let tool = McpTool {
            name: "sum".into(),
            description: "Sum".into(),
            input_schema: json!({"type": "object"}),
            annotations: None,
        };
        let caller = Arc::new(CountingCaller {
            calls: AtomicUsize::new(0),
            result: McpLiveCallResult::Unknown {
                message: "timed out waiting for tools/call response".into(),
            },
        });
        let bridge = McpLiveBridge::from_tools("mock", vec![tool], caller.clone());
        let result = bridge.call("mcp:mock:sum", json!({"a": 1, "b": 2})).await;
        assert!(matches!(result, McpLiveCallResult::Unknown { .. }));
        assert_eq!(caller.calls.load(Ordering::SeqCst), 1);
        let policy = UnknownOutcomePolicy::new(ExecutionSemantics::Mutating);
        assert!(!policy.can_retry(result.operation_outcome()));
    }

    #[tokio::test]
    async fn discover_and_call_echo_via_mock_server() {
        let (adapter, server_task) = crate::mcp_adapter::tests::duplex_adapter().await;
        let bridge = McpLiveBridge::discover("mock", adapter)
            .await
            .expect("discover");
        let names: Vec<_> = bridge
            .list()
            .iter()
            .map(|t| t.catalog_name.as_str())
            .collect();
        assert!(names.contains(&"mcp:mock:echo"));
        assert!(names.contains(&"mcp:mock:sum"));

        let echo = bridge.get("mcp:mock:echo").expect("echo entry");
        assert_eq!(echo.semantics, ExecutionSemantics::ReadOnly);
        bridge
            .validate_arguments("mcp:mock:echo", &json!({"text": "hi"}))
            .expect("schema ok");

        let result = bridge.call("mcp:mock:echo", json!({"text": "hi"})).await;
        assert_eq!(
            result,
            McpLiveCallResult::Ok {
                preview: "echo:hi".into()
            }
        );
        server_task.abort();
    }
}
