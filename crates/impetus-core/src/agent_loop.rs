//! Autonomous Agent Loop: Model → Tool → Safety → Execution → Observation → Model
//!
//! The agent loop is a distinct subsystem that orchestrates the iterative cycle
//! between model inference, tool invocation, policy enforcement, execution, and
//! observation feeding back into the next model turn.

use crate::{
    AgentRuntime, ModelProvider, PolicyEngine, ProviderError, ProviderMessage, RuntimeError,
    RuntimeStatus, ToolOrchestrator,
};
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Maximum iterations per agent loop run to prevent infinite loops.
const MAX_ITERATIONS: u32 = 50;

#[derive(Debug, Error)]
pub enum AgentLoopError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Orchestrator(#[from] crate::OrchestratorError),
    #[error("agent loop exceeded maximum iterations ({MAX_ITERATIONS})")]
    MaxIterationsExceeded,
    #[error("agent loop cancelled")]
    Cancelled,
}

/// Autonomous agent loop coordinator.
///
/// Runs the iterative cycle:
/// 1. Model generates response with potential tool calls
/// 2. Tool Orchestrator normalizes and validates tool requests
/// 3. Safety/Policy/Sandbox gate each effect
/// 4. Approved effects execute and produce observations
/// 5. Observations feed back into the next model turn
/// 6. Loop continues until model produces final response or limit reached
#[allow(dead_code)] // Fields used in future iterations
pub struct AgentLoop {
    runtime: Arc<AgentRuntime>,
    policy: PolicyEngine,
    tool_orchestrator: ToolOrchestrator,
}

impl AgentLoop {
    pub fn new(
        runtime: Arc<AgentRuntime>,
        policy: PolicyEngine,
        workspace_root: std::path::PathBuf,
    ) -> Self {
        Self {
            runtime,
            policy: policy.clone(),
            tool_orchestrator: ToolOrchestrator::new(policy, workspace_root),
        }
    }

    /// Execute the autonomous agent loop for a single run.
    ///
    /// Returns when:
    /// - Model produces a final response (no pending tool calls)
    /// - Maximum iterations reached
    /// - Cancellation requested
    /// - Unrecoverable error occurs
    pub async fn execute(
        &self,
        run_id: Uuid,
        provider: Arc<dyn ModelProvider>,
        initial_messages: Vec<ProviderMessage>,
        cancellation: CancellationToken,
    ) -> Result<(), AgentLoopError> {
        let mut messages = initial_messages;
        let mut iteration = 0;

        loop {
            if cancellation.is_cancelled() {
                return Err(AgentLoopError::Cancelled);
            }

            if iteration >= MAX_ITERATIONS {
                return Err(AgentLoopError::MaxIterationsExceeded);
            }

            iteration += 1;

            // Phase 1: Model inference
            let model_response = self
                .call_model(run_id, &provider, &messages, &cancellation)
                .await?;

            // Phase 2: Extract tool calls from response
            let tool_calls = self.extract_tool_calls(&model_response);

            if tool_calls.is_empty() {
                // No more tool calls — agent loop complete
                return Ok(());
            }

            // Phase 3: Tool Orchestrator processes each tool request
            let observations = self
                .tool_orchestrator
                .process_tool_calls(run_id, tool_calls, &self.runtime)
                .await?;

            // Phase 4: Add observations to message history for next turn
            // Note: Using 'user' role for observations as assistant/tool_result
            // roles are not yet implemented in ProviderMessage
            messages.push(ProviderMessage::user(format!(
                "[Agent response]: {}",
                model_response
            )));
            for observation in observations {
                messages.push(ProviderMessage::user(format!(
                    "[Tool result]: {}",
                    observation
                )));
            }
        }
    }

    async fn call_model(
        &self,
        run_id: Uuid,
        provider: &Arc<dyn ModelProvider>,
        messages: &[ProviderMessage],
        cancellation: &CancellationToken,
    ) -> Result<String, AgentLoopError> {
        // Check runtime status before calling model
        if !matches!(self.runtime.status(), Ok(RuntimeStatus::Running)) {
            return Err(AgentLoopError::Runtime(RuntimeError::InactiveRun(run_id)));
        }

        let accumulated = Arc::new(std::sync::Mutex::new(String::new()));
        let runtime = self.runtime.clone();
        let chunk_id = Arc::new(std::sync::Mutex::new(1u64));
        let accumulated_clone = accumulated.clone();

        provider
            .stream_messages(
                messages,
                None, // credential resolution handled at provider level
                cancellation.clone(),
                Box::new(move |chunk| {
                    let id = {
                        let mut counter = chunk_id.lock().unwrap();
                        let id = *counter;
                        *counter += 1;
                        id
                    };
                    runtime
                        .record_agent_chunk(run_id, id, chunk.clone())
                        .map(|_| ())
                        .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;
                    accumulated_clone.lock().unwrap().push_str(&chunk);
                    Ok(())
                }),
            )
            .await?;

        let result = accumulated.lock().unwrap().clone();
        Ok(result)
    }

    fn extract_tool_calls(&self, response: &str) -> Vec<ToolCall> {
        // Simple XML-style tool call parser for Anthropic/Claude format:
        // <tool_use>
        // <tool_name>name</tool_name>
        // <parameters>{...}</parameters>
        // </tool_use>
        //
        // This is a minimal parser, not a full XML implementation.
        let mut calls = Vec::new();
        let mut pos = 0;

        while let Some(start) = response[pos..].find("<tool_use>") {
            let abs_start = pos + start;
            let search_from = abs_start + "<tool_use>".len();

            if let Some(end_offset) = response[search_from..].find("</tool_use>") {
                let abs_end = search_from + end_offset;
                let block = &response[search_from..abs_end];

                // Extract tool_name
                let tool_name = if let Some(name_start) = block.find("<tool_name>") {
                    let name_content_start = name_start + "<tool_name>".len();
                    if let Some(name_end) = block[name_content_start..].find("</tool_name>") {
                        block[name_content_start..name_content_start + name_end].trim().to_string()
                    } else {
                        pos = abs_end + "</tool_use>".len();
                        continue;
                    }
                } else {
                    pos = abs_end + "</tool_use>".len();
                    continue;
                };

                // Extract parameters
                let arguments = if let Some(params_start) = block.find("<parameters>") {
                    let params_content_start = params_start + "<parameters>".len();
                    if let Some(params_end) = block[params_content_start..].find("</parameters>") {
                        let params_str = block[params_content_start..params_content_start + params_end]
                            .trim();
                        // Try to parse as JSON, fallback to empty object
                        serde_json::from_str(params_str).unwrap_or(serde_json::json!({}))
                    } else {
                        serde_json::json!({})
                    }
                } else {
                    serde_json::json!({})
                };

                calls.push(ToolCall {
                    id: format!("tool_{}", calls.len() + 1),
                    name: tool_name,
                    arguments,
                });

                pos = abs_end + "</tool_use>".len();
            } else {
                break;
            }
        }

        calls
    }
}

/// A tool invocation request extracted from model response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_iterations_constant_is_reasonable() {
        // MAX_ITERATIONS is set to 50, which is reasonable for agent loops
        const { assert!(MAX_ITERATIONS >= 10) };
        const { assert!(MAX_ITERATIONS <= 100) };
    }

    #[test]
    fn extract_tool_calls_single_call() {
        let response = r#"
Some reasoning text here.
<tool_use>
<tool_name>bash</tool_name>
<parameters>{"command": "ls -la"}</parameters>
</tool_use>
More text after.
"#;

        let store = Arc::new(crate::MemoryEventStore::default());
        let workspace = std::env::temp_dir();
        let scope = crate::SandboxScope {
            workspace_root: workspace.clone(),
            allow_network: false,
            allowed_hosts: vec![],
        };
        let policy = PolicyEngine::new(scope);
        let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
        let agent_loop = AgentLoop::new(runtime, policy, workspace);

        let calls = agent_loop.extract_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments["command"], "ls -la");
    }

    #[test]
    fn extract_tool_calls_multiple_calls() {
        let response = r#"
<tool_use>
<tool_name>read_file</tool_name>
<parameters>{"path": "src/main.rs"}</parameters>
</tool_use>

<tool_use>
<tool_name>bash</tool_name>
<parameters>{"command": "cargo test"}</parameters>
</tool_use>
"#;

        let store = Arc::new(crate::MemoryEventStore::default());
        let workspace = std::env::temp_dir();
        let scope = crate::SandboxScope {
            workspace_root: workspace.clone(),
            allow_network: false,
            allowed_hosts: vec![],
        };
        let policy = PolicyEngine::new(scope);
        let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
        let agent_loop = AgentLoop::new(runtime, policy, workspace);

        let calls = agent_loop.extract_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[1].name, "bash");
    }

    #[test]
    fn extract_tool_calls_no_calls() {
        let response = "Just a plain text response with no tool calls.";

        let store = Arc::new(crate::MemoryEventStore::default());
        let workspace = std::env::temp_dir();
        let scope = crate::SandboxScope {
            workspace_root: workspace.clone(),
            allow_network: false,
            allowed_hosts: vec![],
        };
        let policy = PolicyEngine::new(scope);
        let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
        let agent_loop = AgentLoop::new(runtime, policy, workspace);

        let calls = agent_loop.extract_tool_calls(response);
        assert_eq!(calls.len(), 0);
    }

    #[test]
    fn extract_tool_calls_malformed_skips() {
        let response = r#"
<tool_use>
<tool_name>valid_tool</tool_name>
<parameters>{"a": 1}</parameters>
</tool_use>

<tool_use>
<tool_name>broken_tool
</tool_use>
"#;

        let store = Arc::new(crate::MemoryEventStore::default());
        let workspace = std::env::temp_dir();
        let scope = crate::SandboxScope {
            workspace_root: workspace.clone(),
            allow_network: false,
            allowed_hosts: vec![],
        };
        let policy = PolicyEngine::new(scope);
        let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
        let agent_loop = AgentLoop::new(runtime, policy, workspace);

        let calls = agent_loop.extract_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "valid_tool");
    }

    #[test]
    fn extract_tool_calls_empty_parameters() {
        let response = r#"
<tool_use>
<tool_name>no_params_tool</tool_name>
</tool_use>
"#;

        let store = Arc::new(crate::MemoryEventStore::default());
        let workspace = std::env::temp_dir();
        let scope = crate::SandboxScope {
            workspace_root: workspace.clone(),
            allow_network: false,
            allowed_hosts: vec![],
        };
        let policy = PolicyEngine::new(scope);
        let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
        let agent_loop = AgentLoop::new(runtime, policy, workspace);

        let calls = agent_loop.extract_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "no_params_tool");
        assert_eq!(calls[0].arguments, serde_json::json!({}));
    }
}
