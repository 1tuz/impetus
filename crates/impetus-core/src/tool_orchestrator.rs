//! Tool Orchestrator: structured tool lifecycle and effect normalization.
//!
//! The orchestrator sits between the agent loop and individual tool implementations,
//! providing:
//! - Normalized effect representation
//! - Policy admission for each effect
//! - Durable observations
//! - Tool execution coordination

use crate::{
    Action, ActionKind, ActionOrigin, AgentRuntime, PolicyDecision, PolicyEngine, RuntimeError,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("tool `{0}` not found")]
    ToolNotFound(String),
    #[error("tool `{tool}` failed: {reason}")]
    ToolFailed { tool: String, reason: String },
    #[error("tool execution denied: {0}")]
    Denied(String),
    #[error("tool execution requires approval")]
    ApprovalRequired,
}

/// Structured tool invocation request from the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRequest {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Normalized observation returned to the model after tool execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolObservation {
    pub tool_call_id: String,
    pub tool_name: String,
    pub outcome: ToolOutcomeStatus,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcomeStatus {
    Success,
    Error,
    Denied,
    ApprovalRequired,
}

/// Tool Orchestrator coordinates tool execution through the safety boundary.
#[allow(dead_code)] // Fields used in future iterations
pub struct ToolOrchestrator {
    policy: PolicyEngine,
    workspace_root: PathBuf,
}

impl ToolOrchestrator {
    pub fn new(policy: PolicyEngine, workspace_root: PathBuf) -> Self {
        Self {
            policy,
            workspace_root,
        }
    }

    /// Process a batch of tool calls from the model.
    ///
    /// For each tool:
    /// 1. Normalize into an Action
    /// 2. Request policy decision
    /// 3. If allowed, execute and capture observation
    /// 4. If denied or needs approval, record that outcome
    ///
    /// Returns observations for all tool calls (success or error).
    pub async fn process_tool_calls(
        &self,
        _run_id: Uuid,
        tool_calls: Vec<crate::ToolCall>,
        runtime: &Arc<AgentRuntime>,
    ) -> Result<Vec<String>, OrchestratorError> {
        let mut observations = Vec::new();

        for tool_call in tool_calls {
            let observation = self.process_single_tool(tool_call, runtime).await;
            observations.push(observation);
        }

        Ok(observations)
    }

    async fn process_single_tool(
        &self,
        tool_call: crate::ToolCall,
        runtime: &Arc<AgentRuntime>,
    ) -> String {
        // Step 1: Normalize tool call into Action
        let action = match self.normalize_tool_call(&tool_call) {
            Ok(action) => action,
            Err(e) => {
                return format!("Tool '{}' failed to normalize: {}", tool_call.name, e);
            }
        };

        // Step 2: Policy decision
        let decision = self.policy.evaluate(&action);

        match decision {
            PolicyDecision::Allow => {
                // Step 3: Execute tool
                match self.execute_tool(&tool_call).await {
                    Ok(outcome) => format!("Tool '{}' result: {}", tool_call.name, outcome),
                    Err(e) => format!("Tool '{}' failed: {}", tool_call.name, e),
                }
            }
            PolicyDecision::Deny { reason } => {
                // Record denial
                let _ =
                    runtime.record_event(crate::EventPayload::Tool(crate::ToolEvent::Finished {
                        name: tool_call.name.clone(),
                        summary: format!("denied: {}", reason),
                    }));
                format!("Tool '{}' denied: {}", tool_call.name, reason)
            }
            PolicyDecision::NeedsApproval { reason } => {
                // Step 4: Request approval (deferred execution)
                match runtime.request_action(action.clone()) {
                    Ok(_) => format!(
                        "Tool '{}' requires approval: {}. Execution paused.",
                        tool_call.name, reason
                    ),
                    Err(e) => format!("Tool '{}' approval request failed: {}", tool_call.name, e),
                }
            }
        }
    }

    fn normalize_tool_call(
        &self,
        tool_call: &crate::ToolCall,
    ) -> Result<Action, OrchestratorError> {
        // Map tool names to ActionKind
        let kind = match tool_call.name.as_str() {
            "read_file" => ActionKind::ReadFile,
            "write_file" | "edit_file" => ActionKind::WriteFile,
            "bash" | "shell" | "exec" => ActionKind::SpawnProcess,
            name => {
                return Err(OrchestratorError::ToolNotFound(name.to_string()));
            }
        };

        let target = tool_call
            .arguments
            .get("path")
            .or_else(|| tool_call.arguments.get("command"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(Action {
            origin: ActionOrigin::Agent,
            kind,
            summary: format!("{} via agent", tool_call.name),
            target,
        })
    }

    async fn execute_tool(&self, tool_call: &crate::ToolCall) -> Result<String, OrchestratorError> {
        // TODO: Integrate with existing ReadOnlyTools and effects system
        // For now, return placeholder
        Ok(format!(
            "Tool '{}' executed successfully (placeholder)",
            tool_call.name
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryEventStore, SandboxScope};

    #[test]
    fn normalize_read_file_tool() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let orchestrator = ToolOrchestrator::new(policy, PathBuf::from("."));

        let tool_call = crate::ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "test.txt"}),
        };

        let action = orchestrator.normalize_tool_call(&tool_call).unwrap();
        assert_eq!(action.kind, ActionKind::ReadFile);
        assert_eq!(action.target, Some("test.txt".to_string()));
    }

    #[test]
    fn normalize_unknown_tool_fails() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let orchestrator = ToolOrchestrator::new(policy, PathBuf::from("."));

        let tool_call = crate::ToolCall {
            id: "call_1".to_string(),
            name: "unknown_tool".to_string(),
            arguments: serde_json::json!({}),
        };

        let result = orchestrator.normalize_tool_call(&tool_call);
        assert!(matches!(result, Err(OrchestratorError::ToolNotFound(_))));
    }

    #[tokio::test]
    async fn process_tool_calls_returns_observations() {
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
        let orchestrator = ToolOrchestrator::new(policy, PathBuf::from("."));

        let tool_calls = vec![crate::ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "test.txt"}),
        }];

        let observations = orchestrator
            .process_tool_calls(Uuid::new_v4(), tool_calls, &runtime)
            .await
            .unwrap();

        assert_eq!(observations.len(), 1);
        assert!(observations[0].contains("read_file"));
    }
}
