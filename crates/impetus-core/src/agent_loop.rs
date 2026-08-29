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

    fn extract_tool_calls(&self, _response: &str) -> Vec<ToolCall> {
        // TODO: Parse tool calls from model response
        // This requires provider-specific parsing (OpenAI function calling format,
        // Anthropic tool use blocks, etc.)
        // For now, return empty to signal no tool calls
        vec![]
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
}
