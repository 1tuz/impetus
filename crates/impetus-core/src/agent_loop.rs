//! Autonomous Agent Loop: Model → Tool → Safety → Execution → Observation → Model
//!
//! The agent loop is a distinct subsystem that orchestrates the iterative cycle
//! between model inference, tool invocation, policy enforcement, execution, and
//! observation feeding back into the next model turn.

use crate::{
    AgentRuntime, BudgetError, EventPayload, FinishReason, ModelProvider, PolicyEngine,
    ProviderError, ProviderMessage, RetryEvent, RuntimeError, RuntimeStatus, StreamEvent,
    ToolOrchestrator,
};
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Maximum iterations per agent loop run to prevent infinite loops.
const MAX_ITERATIONS: u32 = 50;

/// Maximum retry attempts for transient errors
const MAX_RETRY_ATTEMPTS: u32 = 3;

/// Initial backoff in milliseconds
const INITIAL_BACKOFF_MS: u64 = 1000;

/// Backoff multiplier for exponential backoff
const BACKOFF_MULTIPLIER: u64 = 2;

#[derive(Debug, Error)]
pub enum AgentLoopError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Orchestrator(#[from] crate::OrchestratorError),
    #[error(transparent)]
    Budget(#[from] BudgetError),
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
    pub fn new(runtime: Arc<AgentRuntime>) -> Self {
        let policy = runtime.policy();
        let workspace_root = runtime
            .workspace_root()
            .expect("runtime always has a workspace root");
        let mut web_research = crate::web_research::WebResearchEngine::production(
            crate::web_research::EgressPolicy::default(),
        );
        if let Ok(artifacts) = crate::DurableArtifactStore::open(crate::default_artifact_root()) {
            web_research = web_research.with_artifact_store(
                Arc::new(artifacts),
                crate::web_research::ArtifactPolicy::default(),
            );
        }
        Self {
            runtime,
            policy: policy.clone(),
            tool_orchestrator: ToolOrchestrator::new(policy, workspace_root)
                .with_web_research(Arc::new(web_research)),
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

            // Phase 1: Model inference with retry logic
            let turn_result = self
                .call_model_with_retry(run_id, &provider, &messages, &cancellation)
                .await?;

            // Phase 2: Tool calls are already extracted from StreamEvents
            let tool_calls = turn_result.tool_calls;

            if tool_calls.is_empty() {
                // No more tool calls — agent loop complete
                self.runtime.record_agent_final(run_id, turn_result.text)?;
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
            messages.push(ProviderMessage::assistant(turn_result.text));
            for observation in observations {
                messages.push(ProviderMessage::tool(
                    serde_json::to_string(&observation)
                        .map_err(|error| ProviderError::RequestFailed(error.to_string()))?,
                ));
            }
        }
    }

    async fn call_model_with_retry(
        &self,
        run_id: Uuid,
        provider: &Arc<dyn ModelProvider>,
        messages: &[ProviderMessage],
        cancellation: &CancellationToken,
    ) -> Result<ModelTurnResult, AgentLoopError> {
        let mut attempt = 0;

        loop {
            attempt += 1;

            match self
                .call_model(run_id, provider, messages, cancellation)
                .await
            {
                Ok(response) => {
                    // Success — emit retry success event if we retried
                    if attempt > 1 {
                        self.runtime
                            .record_event(EventPayload::Retry(RetryEvent::Succeeded { attempt }))?;
                    }
                    return Ok(response);
                }
                Err(AgentLoopError::Provider(provider_error)) => {
                    // Check if error is transient and we haven't exhausted retries
                    if provider_error.is_transient() && attempt < MAX_RETRY_ATTEMPTS {
                        let backoff_ms = INITIAL_BACKOFF_MS * BACKOFF_MULTIPLIER.pow(attempt - 1);

                        // Emit retry attempt event
                        self.runtime
                            .record_event(EventPayload::Retry(RetryEvent::Attempting {
                                attempt,
                                max_attempts: MAX_RETRY_ATTEMPTS,
                                reason: provider_error.to_string(),
                                backoff_ms,
                            }))?;

                        // Wait with cancellation check
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {},
                            _ = cancellation.cancelled() => {
                                return Err(AgentLoopError::Cancelled);
                            }
                        }

                        continue;
                    } else {
                        // Permanent error or retries exhausted
                        if attempt > 1 {
                            self.runtime.record_event(EventPayload::Retry(
                                RetryEvent::Exhausted {
                                    attempts: attempt,
                                    last_error: provider_error.to_string(),
                                },
                            ))?;
                        }
                        return Err(AgentLoopError::Provider(provider_error));
                    }
                }
                Err(other_error) => {
                    // Non-provider errors (Budget, Runtime, Cancelled) — fail immediately
                    return Err(other_error);
                }
            }
        }
    }

    async fn call_model(
        &self,
        run_id: Uuid,
        provider: &Arc<dyn ModelProvider>,
        messages: &[ProviderMessage],
        cancellation: &CancellationToken,
    ) -> Result<ModelTurnResult, AgentLoopError> {
        // Check runtime status before calling model
        if !matches!(self.runtime.status(), Ok(RuntimeStatus::Running)) {
            return Err(AgentLoopError::Runtime(RuntimeError::InactiveRun(run_id)));
        }

        // Budget enforcement: check before model call
        let estimated_tokens = self.estimate_request_tokens(messages);
        if let Err(budget_error) = self.runtime.check_budget(estimated_tokens) {
            self.emit_budget_limit_event(&budget_error)?;
            return Err(AgentLoopError::Budget(budget_error));
        }

        let accumulator = Arc::new(std::sync::Mutex::new(TurnAccumulator::default()));
        let runtime = self.runtime.clone();
        let chunk_id = Arc::new(std::sync::Mutex::new(
            self.runtime
                .events()?
                .iter()
                .rev()
                .find_map(|event| match &event.payload {
                    crate::EventPayload::Agent(crate::AgentEvent::Chunk {
                        run_id: event_run,
                        chunk_id,
                        ..
                    }) if *event_run == run_id => Some(*chunk_id + 1),
                    _ => None,
                })
                .unwrap_or(1),
        ));
        let accumulator_clone = accumulator.clone();

        provider
            .stream_messages(
                messages,
                None, // credential resolution handled at provider level
                Some(self.runtime.clone()),
                cancellation.clone(),
                Box::new(move |event| {
                    match event {
                        StreamEvent::TextDelta { delta } => {
                            let id = {
                                let mut counter = chunk_id.lock().unwrap();
                                let id = *counter;
                                *counter += 1;
                                id
                            };
                            runtime
                                .record_agent_chunk(run_id, id, delta.clone())
                                .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;
                            // Lock only for mutation
                            accumulator_clone.lock().unwrap().text.push_str(&delta);
                        }
                        StreamEvent::ToolCall {
                            id,
                            name,
                            arguments,
                        } => {
                            accumulator_clone.lock().unwrap().tool_calls.push(ToolCall {
                                id,
                                name,
                                arguments,
                            });
                        }
                        StreamEvent::Usage {
                            prompt_tokens,
                            completion_tokens,
                            measured,
                        } => {
                            accumulator_clone.lock().unwrap().usage =
                                Some((prompt_tokens, completion_tokens, measured));
                        }
                        StreamEvent::Finish { reason } => {
                            accumulator_clone.lock().unwrap().finish_reason = Some(reason);
                        }
                        StreamEvent::Reasoning { .. } => {
                            // Future: record reasoning traces
                        }
                    }
                    Ok(())
                }),
            )
            .await?;

        let acc = accumulator.lock().unwrap();
        let text = acc.text.clone();
        let tool_calls = acc.tool_calls.clone();
        let measured_usage = acc
            .usage
            .filter(|(_, _, measured)| *measured)
            .map(|(p, c, _)| (p, c));

        // Record turn completion with token usage
        let tokens_used = if let Some((prompt, completion)) = measured_usage {
            prompt + completion
        } else {
            // Fallback to estimation if provider didn't send usage
            estimated_tokens + self.estimate_response_tokens(&text)
        };
        self.runtime.record_turn(tokens_used)?;

        // Emit budget update event
        if let Some(state) = self.runtime.budget_state() {
            let context_percent = self
                .runtime
                .budget()
                .as_ref()
                .and_then(|b| {
                    let guard = b.lock().unwrap();
                    guard.config().context_limit
                })
                .map(|limit| state.context_used_percent(limit))
                .unwrap_or(0);

            self.runtime
                .record_event(EventPayload::Budget(crate::BudgetEvent::Updated {
                    turns_used: state.turns_used,
                    tokens_used: state.tokens_used,
                    compaction_count: state.compaction_count,
                    context_used_percent: context_percent,
                }))?;

            // Emit approaching warnings
            if let Some(checker) = self.runtime.budget() {
                let guard = checker.lock().unwrap();
                self.emit_approaching_warnings(&guard, &state)?;
            }
        }

        Ok(ModelTurnResult {
            text,
            tool_calls,
            measured_usage,
        })
    }

    // extract_tool_calls removed: tool calls now come directly from StreamEvent::ToolCall

    /// Estimate request tokens (rough heuristic: 4 chars per token)
    fn estimate_request_tokens(&self, messages: &[ProviderMessage]) -> u64 {
        let total_chars: usize = messages.iter().map(|m| m.content().len()).sum();
        (total_chars / 4).max(100) as u64
    }

    /// Estimate response tokens
    fn estimate_response_tokens(&self, response: &str) -> u64 {
        (response.len() / 4).max(50) as u64
    }

    /// Emit budget limit event when budget check fails
    fn emit_budget_limit_event(&self, error: &BudgetError) -> Result<(), RuntimeError> {
        match error {
            BudgetError::TurnLimitExceeded { limit, used } => {
                self.runtime.record_event(EventPayload::Budget(
                    crate::BudgetEvent::TurnLimitApproaching {
                        limit: *limit,
                        used: *used,
                    },
                ))?;
            }
            BudgetError::TokenLimitExceeded { limit, used, .. } => {
                self.runtime.record_event(EventPayload::Budget(
                    crate::BudgetEvent::TokenLimitApproaching {
                        limit: *limit,
                        used: *used,
                    },
                ))?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Emit approaching warnings at 80% and 95% thresholds
    fn emit_approaching_warnings(
        &self,
        checker: &crate::BudgetChecker,
        state: &crate::BudgetState,
    ) -> Result<(), RuntimeError> {
        if let Some(max_turns) = checker.config().max_turns {
            let percent = (state.turns_used as f64 / max_turns as f64 * 100.0) as u8;
            if (80..100).contains(&percent) {
                self.runtime.record_event(EventPayload::Budget(
                    crate::BudgetEvent::TurnLimitApproaching {
                        limit: max_turns,
                        used: state.turns_used,
                    },
                ))?;
            }
        }

        if let Some(max_tokens) = checker.config().max_tokens {
            let percent = (state.tokens_used as f64 / max_tokens as f64 * 100.0) as u8;
            if (80..100).contains(&percent) {
                self.runtime.record_event(EventPayload::Budget(
                    crate::BudgetEvent::TokenLimitApproaching {
                        limit: max_tokens,
                        used: state.tokens_used,
                    },
                ))?;
            }
        }

        Ok(())
    }
}

/// A tool invocation request extracted from model response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Accumulates typed stream events into a complete model turn.
#[derive(Debug, Default)]
struct TurnAccumulator {
    text: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<(u64, u64, bool)>, // (prompt, completion, measured)
    finish_reason: Option<FinishReason>,
}

/// Result of a model turn with structured data.
#[derive(Debug)]
struct ModelTurnResult {
    text: String,
    tool_calls: Vec<ToolCall>,
    #[allow(dead_code)] // Used for future usage tracking
    measured_usage: Option<(u64, u64)>, // (prompt_tokens, completion_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_provider::MockStreamItem;
    use crate::{MemoryEventStore, MockProvider, SandboxScope};

    #[test]
    fn max_iterations_constant_is_reasonable() {
        // MAX_ITERATIONS is set to 50, which is reasonable for agent loops
        const { assert!(MAX_ITERATIONS >= 10) };
        const { assert!(MAX_ITERATIONS <= 100) };
    }

    // extract_tool_calls tests removed: tool calls now come from StreamEvent::ToolCall

    #[tokio::test]
    async fn read_observation_is_returned_to_the_next_model_turn() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        std::fs::write(workspace.path().join("evidence.txt"), "verified evidence")
            .expect("write fixture");
        let runtime = Arc::new(
            AgentRuntime::create_with_workspace(
                Arc::new(MemoryEventStore::default()),
                PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
                workspace.path().to_path_buf(),
            )
            .expect("runtime"),
        );
        let run_id = runtime
            .submit_intent_and_start_run("read evidence")
            .expect("start run");
        let provider = Arc::new(MockProvider::scripted(
            "scripted",
            "test",
            [
                vec![MockStreamItem::ToolCall {
                    id: "call_1".to_string(),
                    tool: "read_file".to_string(),
                    arguments: r#"{"path":"evidence.txt"}"#.to_string(),
                }],
                vec![MockStreamItem::Chunk {
                    chunk_id: 2,
                    text: "final answer".into(),
                }],
            ],
        ));
        AgentLoop::new(runtime.clone())
            .execute(
                run_id,
                provider.clone(),
                vec![ProviderMessage::user("read evidence")],
                CancellationToken::new(),
            )
            .await
            .expect("agent loop");
        let messages = provider.received_messages();
        assert_eq!(messages.len(), 2);
        let second_turn = serde_json::to_value(&messages[1]).expect("serialize messages");
        assert!(second_turn.as_array().unwrap().iter().any(|message| {
            message["role"] == "tool"
                && message["content"]
                    .as_str()
                    .unwrap()
                    .contains("verified evidence")
        }));
        assert!(runtime.events().unwrap().iter().any(|event| matches!(
            &event.payload,
            crate::EventPayload::Agent(crate::AgentEvent::Final { text, .. }) if text == "final answer"
        )));
    }
}
