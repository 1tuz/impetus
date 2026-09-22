//! Autonomous Agent Loop: Model → Tool → Safety → Execution → Observation → Model
//!
//! The agent loop is a distinct subsystem that orchestrates the iterative cycle
//! between model inference, tool invocation, policy enforcement, execution, and
//! observation feeding back into the next model turn.

use crate::{
    AgentRuntime, BudgetError, EventPayload, FinishReason, ModelProvider, PolicyEngine,
    ProviderError, ProviderMessage, RetryEvent, RuntimeError, RuntimeStatus, StreamEvent,
    StreamOptions, ToolOrchestrator,
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
        let mut web_research =
            crate::web_research::WebResearchEngine::production(policy.egress_policy());
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

    /// Build a loop with a pre-configured orchestrator (e.g. for restricted subagents).
    pub fn with_tool_orchestrator(
        runtime: Arc<AgentRuntime>,
        orchestrator: ToolOrchestrator,
    ) -> Self {
        Self {
            policy: runtime.policy(),
            runtime,
            tool_orchestrator: orchestrator,
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
        steer_pending: Option<&crate::SteerPendingQueue>,
        stream_options: StreamOptions,
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

            if let Some(queue) = steer_pending {
                for fragment in queue.drain(self.runtime.session_id()) {
                    messages.push(ProviderMessage::user(fragment));
                }
            }

            // Durable compaction when context budget threshold is hit.
            // Prompt messages fold; event log stays append-only with typed state.
            if self.runtime.compaction_needed().is_some() {
                messages = self.runtime.run_durable_compaction(messages)?;
            }

            // Phase 1: Model inference with retry logic
            let turn_result = self
                .call_model_with_retry(run_id, &provider, &messages, &cancellation, &stream_options)
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
            let artifact_store =
                crate::DurableArtifactStore::open(crate::default_artifact_root()).ok();
            let artifact_budget = crate::TokenBudget { max_tokens: 2_000 };
            for mut observation in observations {
                if let Some(artifact) = observation.artifact.as_ref()
                    && let Some(store) = artifact_store.as_ref()
                {
                    match crate::ContextBuilder::new(store, artifact_budget).materialize(artifact) {
                        Ok(materialized) => {
                            observation.preview = materialized.content;
                        }
                        Err(error) => {
                            observation.error = Some(match observation.error.take() {
                                Some(existing) => format!("{existing}; {error}"),
                                None => error.to_string(),
                            });
                        }
                    }
                }
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
        stream_options: &StreamOptions,
    ) -> Result<ModelTurnResult, AgentLoopError> {
        let mut attempt = 0;

        loop {
            attempt += 1;

            match self
                .call_model(run_id, provider, messages, cancellation, stream_options)
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
        stream_options: &StreamOptions,
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
        let coalesce_buf = Arc::new(std::sync::Mutex::new(String::new()));
        let accumulator_clone = accumulator.clone();
        let coalesce_for_cb = coalesce_buf.clone();
        let chunk_id_for_cb = chunk_id.clone();
        let runtime_for_cb = runtime.clone();

        provider
            .stream_messages(
                messages,
                None, // credential resolution handled at provider level
                Some(self.runtime.clone()),
                cancellation.clone(),
                stream_options.clone(),
                Box::new(move |event| {
                    match event {
                        StreamEvent::TextDelta { delta } => {
                            {
                                let mut buf = coalesce_for_cb.lock().unwrap();
                                buf.push_str(&delta);
                            }
                            flush_coalesced_chunk(
                                &coalesce_for_cb,
                                &chunk_id_for_cb,
                                &runtime_for_cb,
                                run_id,
                                false,
                            )?;
                            // Lock only for mutation
                            accumulator_clone.lock().unwrap().text.push_str(&delta);
                        }
                        StreamEvent::ToolCall {
                            id,
                            name,
                            arguments,
                        } => {
                            flush_coalesced_chunk(
                                &coalesce_for_cb,
                                &chunk_id_for_cb,
                                &runtime_for_cb,
                                run_id,
                                true,
                            )?;
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
                            flush_coalesced_chunk(
                                &coalesce_for_cb,
                                &chunk_id_for_cb,
                                &runtime_for_cb,
                                run_id,
                                true,
                            )?;
                            accumulator_clone.lock().unwrap().finish_reason = Some(reason);
                        }
                        StreamEvent::Reasoning { content } => {
                            flush_coalesced_chunk(
                                &coalesce_for_cb,
                                &chunk_id_for_cb,
                                &runtime_for_cb,
                                run_id,
                                true,
                            )?;
                            // Summary only — runtime bounds + skips empty; never CoT dump.
                            runtime_for_cb
                                .record_agent_reasoning_summary(run_id, content)
                                .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;
                        }
                    }
                    Ok(())
                }),
            )
            .await?;

        // Flush any trailing coalesced deltas that never hit the size threshold.
        flush_coalesced_chunk(&coalesce_buf, &chunk_id, &runtime, run_id, true)
            .map_err(AgentLoopError::Provider)?;

        let acc = accumulator.lock().unwrap();
        let text = acc.text.clone();
        let tool_calls = acc.tool_calls.clone();
        let measured_usage = acc
            .usage
            .filter(|(_, _, measured)| *measured)
            .map(|(p, c, _)| (p, c));

        // Record turn completion with token usage (measured when provider sent Usage).
        let (tokens_used, measured) = if let Some((prompt, completion)) = measured_usage {
            (prompt + completion, true)
        } else {
            (
                estimated_tokens + self.estimate_response_tokens(&text),
                false,
            )
        };
        self.runtime.record_turn_with_usage(tokens_used, measured)?;

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
                    measured: state.measured_tokens > 0,
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

/// Flush coalesced stream deltas into one durable chunk when forced or when the
/// buffer reaches [`crate::AGENT_CHUNK_COALESCE_BYTES`].
fn flush_coalesced_chunk(
    buf: &std::sync::Mutex<String>,
    chunk_id: &std::sync::Mutex<u64>,
    runtime: &AgentRuntime,
    run_id: Uuid,
    force: bool,
) -> Result<(), ProviderError> {
    let text = {
        let mut buf = buf.lock().unwrap();
        if buf.is_empty() {
            return Ok(());
        }
        if !force && buf.len() < crate::AGENT_CHUNK_COALESCE_BYTES {
            return Ok(());
        }
        std::mem::take(&mut *buf)
    };
    let id = {
        let mut counter = chunk_id.lock().unwrap();
        let id = *counter;
        *counter += 1;
        id
    };
    runtime
        .record_agent_chunk(run_id, id, text)
        .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;
    Ok(())
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

    #[test]
    fn mid_run_policy_clone_ignores_later_reload() {
        // AgentLoop::new clones PolicyEngine into ToolOrchestrator at construction.
        // Harness ReloadPolicyConfig mutates only the live harness Mutex policy;
        // an in-flight loop keeps the pre-reload clone until the next Prompt.
        use crate::{Action, ActionKind, ActionOrigin, PolicyConfig, PolicyDecision};

        let workspace = tempfile::tempdir().expect("temp workspace");
        let mut live = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let loop_snapshot = live.clone();

        live.reload_config(
            PolicyConfig::parse(r#"{"version":1,"overrides":{"write_file":"allow"}}"#)
                .expect("config"),
        );

        let write = Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WriteFile,
            summary: "create file".into(),
            target: Some("new.txt".into()),
        };
        assert!(
            matches!(
                loop_snapshot.evaluate(&write),
                PolicyDecision::NeedsApproval { .. }
            ),
            "in-flight AgentLoop clone must keep pre-reload decision"
        );
        assert_eq!(live.evaluate(&write), PolicyDecision::Allow);
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
                None,
                crate::StreamOptions::default(),
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

    #[tokio::test]
    async fn reasoning_summary_is_recorded_from_stream() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let runtime = Arc::new(
            AgentRuntime::create_with_workspace(
                Arc::new(MemoryEventStore::default()),
                PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
                workspace.path().to_path_buf(),
            )
            .expect("runtime"),
        );
        let run_id = runtime
            .submit_intent_and_start_run("think then answer")
            .expect("start run");
        let provider = Arc::new(MockProvider::new(
            "scripted",
            "test",
            [
                MockStreamItem::Reasoning {
                    content: "  check workspace first  ".into(),
                },
                MockStreamItem::Reasoning {
                    content: String::new(), // skipped
                },
                MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "done".into(),
                },
            ],
        ));
        AgentLoop::new(runtime.clone())
            .execute(
                run_id,
                provider,
                vec![ProviderMessage::user("think then answer")],
                CancellationToken::new(),
                None,
                crate::StreamOptions::default(),
            )
            .await
            .expect("agent loop");
        let events = runtime.events().unwrap();
        let summaries: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.payload {
                crate::EventPayload::Agent(crate::AgentEvent::ReasoningSummary {
                    text, ..
                }) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(summaries, ["check workspace first"]);
        // Reasoning must not pollute agent_output projection.
        assert!(
            !runtime
                .projection()
                .unwrap()
                .agent_output
                .contains("check workspace first")
        );
    }

    #[tokio::test]
    async fn tiny_deltas_coalesce_into_fewer_chunk_events() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let runtime = Arc::new(
            AgentRuntime::create_with_workspace(
                Arc::new(MemoryEventStore::default()),
                PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
                workspace.path().to_path_buf(),
            )
            .expect("runtime"),
        );
        let run_id = runtime
            .submit_intent_and_start_run("coalesce")
            .expect("start run");
        // 20 one-byte deltas → under force-flush at end should be 1 durable chunk
        // (total 20 < COALESCE) or few flushes if we grew past threshold.
        let items: Vec<_> = (0..20)
            .map(|i| MockStreamItem::Chunk {
                chunk_id: i + 1,
                text: "a".into(),
            })
            .collect();
        let provider = Arc::new(MockProvider::new("scripted", "test", items));
        AgentLoop::new(runtime.clone())
            .execute(
                run_id,
                provider,
                vec![ProviderMessage::user("coalesce")],
                CancellationToken::new(),
                None,
                crate::StreamOptions::default(),
            )
            .await
            .expect("agent loop");
        let chunk_count = runtime
            .events()
            .unwrap()
            .iter()
            .filter(|event| {
                matches!(
                    &event.payload,
                    crate::EventPayload::Agent(crate::AgentEvent::Chunk { .. })
                )
            })
            .count();
        assert!(
            chunk_count < 20,
            "expected coalesce to reduce 20 deltas; got {chunk_count} chunks"
        );
        assert_eq!(chunk_count, 1);
    }

    #[tokio::test]
    async fn large_stream_chunk_events_do_not_store_megabyte_strings() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let data_dir = tempfile::tempdir().expect("data dir");
        // Isolate DurableArtifactStore root for this process (spill path).
        // SAFETY: test-only env; serial within this test body.
        unsafe {
            std::env::set_var("IMPETUS_DATA_DIR", data_dir.path());
        }

        let runtime = Arc::new(
            AgentRuntime::create_with_workspace(
                Arc::new(MemoryEventStore::default()),
                PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
                workspace.path().to_path_buf(),
            )
            .expect("runtime"),
        );
        let run_id = runtime
            .submit_intent_and_start_run("large")
            .expect("start run");
        let megabyte = "z".repeat(crate::MAX_AGENT_CHUNK_EVENT_BYTES + 128 * 1024);
        let provider = Arc::new(MockProvider::new(
            "scripted",
            "test",
            [MockStreamItem::Chunk {
                chunk_id: 1,
                text: megabyte.clone(),
            }],
        ));
        AgentLoop::new(runtime.clone())
            .execute(
                run_id,
                provider,
                vec![ProviderMessage::user("large")],
                CancellationToken::new(),
                None,
                crate::StreamOptions::default(),
            )
            .await
            .expect("agent loop");

        let events = runtime.events().unwrap();
        let chunks: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.payload {
                crate::EventPayload::Agent(crate::AgentEvent::Chunk {
                    text,
                    artifact,
                    chunk_id,
                    ..
                }) => Some((text.len(), artifact.is_some(), *chunk_id)),
                _ => None,
            })
            .collect();
        assert!(!chunks.is_empty());
        for (text_len, spilled, chunk_id) in &chunks {
            assert!(
                *text_len <= crate::MAX_AGENT_CHUNK_EVENT_BYTES,
                "chunk {chunk_id} inline text {text_len} exceeds bound"
            );
            assert!(
                *text_len < megabyte.len(),
                "chunk {chunk_id} must not embed the megabyte body"
            );
            assert!(
                *spilled,
                "chunk {chunk_id} should spill large body to artifact"
            );
        }
        let ids: Vec<_> = chunks.iter().map(|c| c.2).collect();
        assert_eq!(ids, (1..=ids.len() as u64).collect::<Vec<_>>());
    }
}
