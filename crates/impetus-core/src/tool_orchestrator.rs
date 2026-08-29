//! Tool Orchestrator: structured tool lifecycle and effect normalization.
//!
//! The orchestrator sits between the agent loop and individual tool implementations,
//! providing:
//! - Normalized effect representation
//! - Policy admission for each effect
//! - Durable observations
//! - Tool execution coordination

use crate::{
    Action, ActionKind, ActionOrigin, AgentRuntime, DurableArtifactStore, EffectSeam, PolicyEngine,
    ReadOnlyTool, ReadOnlyTools, RuntimeError, Sandbox, ToolEvent, ToolEventOutcome, ToolOutcome,
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
    pub arguments_summary: String,
    pub outcome: ToolOutcomeStatus,
    pub preview: String,
    pub artifact: Option<crate::DurableArtifactRef>,
    pub error: Option<String>,
}

pub type ToolOutcomeStatus = ToolEventOutcome;

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
    ) -> Result<Vec<ToolObservation>, OrchestratorError> {
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
    ) -> ToolObservation {
        let arguments_summary = summarize_arguments(&tool_call.arguments);
        // Step 1: Normalize tool call into Action
        let action = match self.normalize_tool_call(&tool_call) {
            Ok(action) => action,
            Err(e) => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Error,
                    String::new(),
                    None,
                    Some(e.to_string()),
                );
            }
        };

        if matches!(
            action.kind,
            ActionKind::WriteFile | ActionKind::SpawnProcess
        ) {
            if contains_sensitive_value(&tool_call.arguments) {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Denied,
                    String::new(),
                    None,
                    Some("mutating tool arguments contain sensitive material".into()),
                );
            }
            let outcome = match runtime.request_action_with_capability_version(action, Some(1)) {
                Ok(_) => {
                    if let Some(approval_id) = runtime.events().ok().and_then(|events| {
                        events.iter().rev().find_map(|event| match &event.payload {
                            crate::EventPayload::Approval(crate::ApprovalEvent::Requested {
                                request,
                            }) => Some(request.id),
                            _ => None,
                        })
                    }) {
                        let _ = runtime.record_deferred_tool(
                            approval_id,
                            tool_call.id.clone(),
                            tool_call.name.clone(),
                            tool_call.arguments.clone(),
                        );
                    }
                    ToolOutcomeStatus::ApprovalRequired
                }
                Err(RuntimeError::Denied(_)) => ToolOutcomeStatus::Denied,
                Err(_) => ToolOutcomeStatus::Error,
            };
            return Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                outcome,
                String::new(),
                None,
                Some("awaiting user approval".into()),
            );
        }

        match self.execute_read_only(&tool_call) {
            Ok(ToolOutcome::Allowed { result }) => Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                ToolOutcomeStatus::Success,
                result.preview,
                result.artifact,
                None,
            ),
            Ok(ToolOutcome::Denied { reason, .. }) => Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                ToolOutcomeStatus::Denied,
                String::new(),
                None,
                Some(reason),
            ),
            Err(error) => Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                ToolOutcomeStatus::Error,
                String::new(),
                None,
                Some(error.to_string()),
            ),
        }
    }

    fn normalize_tool_call(
        &self,
        tool_call: &crate::ToolCall,
    ) -> Result<Action, OrchestratorError> {
        // Map tool names to ActionKind
        let kind = match tool_call.name.as_str() {
            "list_files" | "read_file" | "search" => ActionKind::ReadFile,
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
        let target = if kind == ActionKind::SpawnProcess {
            Some(self.workspace_root.display().to_string())
        } else {
            target
        };

        Ok(Action {
            origin: ActionOrigin::Agent,
            kind,
            summary: format!("{} via agent", tool_call.name),
            target,
        })
    }

    fn execute_read_only(
        &self,
        tool_call: &crate::ToolCall,
    ) -> Result<ToolOutcome, OrchestratorError> {
        let target = tool_call
            .arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(".");
        let tool = match tool_call.name.as_str() {
            "list_files" => ReadOnlyTool::List {
                target: target.into(),
            },
            "read_file" => ReadOnlyTool::Read {
                target: target.into(),
            },
            "search" => ReadOnlyTool::Search {
                target: target.into(),
                pattern: tool_call
                    .arguments
                    .get("pattern")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .into(),
            },
            name => return Err(OrchestratorError::ToolNotFound(name.into())),
        };
        let artifacts =
            DurableArtifactStore::open(std::env::temp_dir().join("impetus-agent-artifacts"))
                .map_err(|error| OrchestratorError::ToolFailed {
                    tool: tool_call.name.clone(),
                    reason: error.to_string(),
                })?;
        let seam = EffectSeam::with_sandbox(
            self.policy.clone(),
            Sandbox::workspace(&self.workspace_root),
        );
        ReadOnlyTools::new(&self.workspace_root)
            .run_with_seam(tool, ActionOrigin::Agent, &artifacts, &seam)
            .map_err(|error| OrchestratorError::ToolFailed {
                tool: tool_call.name.clone(),
                reason: error.to_string(),
            })
    }

    fn record_observation(
        runtime: &Arc<AgentRuntime>,
        tool_call: crate::ToolCall,
        arguments_summary: String,
        outcome: ToolOutcomeStatus,
        preview: String,
        artifact: Option<crate::DurableArtifactRef>,
        error: Option<String>,
    ) -> ToolObservation {
        let _ = runtime.record_event(crate::EventPayload::Tool(ToolEvent::Observed {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            arguments_summary: arguments_summary.clone(),
            outcome: outcome.clone(),
            preview: preview.clone(),
            artifact: artifact.clone(),
            error: error.clone(),
        }));
        ToolObservation {
            tool_call_id: tool_call.id,
            tool_name: tool_call.name,
            arguments_summary,
            outcome,
            preview,
            artifact,
            error,
        }
    }

    pub fn record_approval_rejection(
        runtime: &Arc<AgentRuntime>,
        deferred: (String, String, serde_json::Value),
    ) -> ToolObservation {
        let (tool_call_id, tool_name, arguments) = deferred;
        Self::record_observation(
            runtime,
            crate::ToolCall {
                id: tool_call_id,
                name: tool_name,
                arguments: arguments.clone(),
            },
            summarize_arguments(&arguments),
            ToolOutcomeStatus::Denied,
            String::new(),
            None,
            Some("user rejected approval".into()),
        )
    }

    pub fn execute_approved_write(
        runtime: &Arc<AgentRuntime>,
        request: crate::ApprovalRequest,
        resolution: crate::ApprovalResolution,
        deferred: (String, String, serde_json::Value),
    ) -> Result<ToolObservation, OrchestratorError> {
        let (tool_call_id, tool_name, arguments) = deferred;
        if !matches!(tool_name.as_str(), "write_file" | "edit_file") {
            return Err(OrchestratorError::ToolNotFound(tool_name));
        }
        let path = arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| OrchestratorError::ToolFailed {
                tool: tool_name.clone(),
                reason: "write tool requires path".into(),
            })?;
        let content = arguments
            .get("content")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| OrchestratorError::ToolFailed {
                tool: tool_name.clone(),
                reason: "write tool requires content".into(),
            })?;
        let effect = crate::NormalizedEffect::workspace_write(
            ActionOrigin::Agent,
            format!("{tool_name} via agent"),
            path,
        );
        if effect.action != request.action {
            return Err(OrchestratorError::Denied(
                "deferred action no longer matches approval".into(),
            ));
        }
        let workspace = runtime.workspace_root()?;
        let seam = EffectSeam::with_sandbox(runtime.policy(), Sandbox::workspace(&workspace));
        let execution = seam
            .execute_after_approval(
                crate::DeferredEffect::from_durable(effect, request.clone()),
                resolution,
                request.intent_revision,
                || {
                    crate::tools::write_file_in_scope(
                        &workspace,
                        PathBuf::from(path).as_path(),
                        content,
                    )
                },
            )
            .map_err(|error| OrchestratorError::ToolFailed {
                tool: tool_name.clone(),
                reason: error.to_string(),
            })?;
        let (outcome, preview, error) = match execution {
            crate::EffectExecution::Executed(()) => {
                (ToolOutcomeStatus::Success, "file written".into(), None)
            }
            crate::EffectExecution::Denied { reason } => {
                (ToolOutcomeStatus::Denied, String::new(), Some(reason))
            }
            crate::EffectExecution::NeedsApproval { reason } => (
                ToolOutcomeStatus::ApprovalRequired,
                String::new(),
                Some(reason),
            ),
        };
        Ok(Self::record_observation(
            runtime,
            crate::ToolCall {
                id: tool_call_id,
                name: tool_name,
                arguments: arguments.clone(),
            },
            summarize_arguments(&arguments),
            outcome,
            preview,
            None,
            error,
        ))
    }

    pub fn execute_approved_bash(
        runtime: &Arc<AgentRuntime>,
        request: crate::ApprovalRequest,
        resolution: crate::ApprovalResolution,
        deferred: (String, String, serde_json::Value),
    ) -> Result<ToolObservation, OrchestratorError> {
        let (tool_call_id, tool_name, arguments) = deferred;
        if !matches!(tool_name.as_str(), "bash" | "shell" | "exec") {
            return Err(OrchestratorError::ToolNotFound(tool_name));
        }
        let command = arguments
            .get("command")
            .and_then(serde_json::Value::as_str)
            .filter(|command| !command.trim().is_empty())
            .ok_or_else(|| OrchestratorError::ToolFailed {
                tool: tool_name.clone(),
                reason: "shell tool requires command".into(),
            })?;
        let workspace = runtime.workspace_root()?;
        let effect = crate::NormalizedEffect::process_spawn(
            ActionOrigin::Agent,
            format!("{tool_name} via agent"),
            workspace.display().to_string(),
        );
        if effect.action != request.action {
            return Err(OrchestratorError::Denied(
                "deferred action no longer matches approval".into(),
            ));
        }
        let process = crate::ProcessExecutionRequest::new(
            "/bin/sh",
            vec!["-lc".into(), command.into()],
            ActionOrigin::Agent,
            request.intent_revision,
        )
        .with_working_dir(workspace.clone());
        let seam = EffectSeam::with_sandbox(runtime.policy(), Sandbox::workspace(&workspace));
        let execution = seam
            .execute_after_approval_with_admission(
                crate::DeferredEffect::from_durable(effect, request.clone()),
                resolution,
                request.intent_revision,
                |admission| {
                    std::thread::scope(|scope| {
                        scope
                            .spawn(|| {
                                tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                    .map_err(crate::ProcessExecutionError::Io)?
                                    .block_on(process.execute(admission))
                            })
                            .join()
                            .map_err(|_| {
                                crate::ProcessExecutionError::ExecutionFailed(
                                    "process worker panicked".into(),
                                )
                            })
                    })?
                },
            )
            .map_err(|error| OrchestratorError::ToolFailed {
                tool: tool_name.clone(),
                reason: error.to_string(),
            })?;
        let (outcome, preview, error) = match execution {
            crate::EffectExecution::Executed(output) => (
                ToolOutcomeStatus::Success,
                crate::tools::redact_text(&format!(
                    "exit_code={:?}\nstdout:\n{}\nstderr:\n{}",
                    output.exit_code, output.stdout, output.stderr
                )),
                None,
            ),
            crate::EffectExecution::Denied { reason } => {
                (ToolOutcomeStatus::Denied, String::new(), Some(reason))
            }
            crate::EffectExecution::NeedsApproval { reason } => (
                ToolOutcomeStatus::ApprovalRequired,
                String::new(),
                Some(reason),
            ),
        };
        Ok(Self::record_observation(
            runtime,
            crate::ToolCall {
                id: tool_call_id,
                name: tool_name,
                arguments: arguments.clone(),
            },
            summarize_arguments(&arguments),
            outcome,
            preview,
            None,
            error,
        ))
    }
}

fn summarize_arguments(arguments: &serde_json::Value) -> String {
    crate::tools::redact_text(&serde_json::to_string(arguments).unwrap_or_default())
        .chars()
        .take(1024)
        .collect()
}

fn contains_sensitive_value(arguments: &serde_json::Value) -> bool {
    let raw = serde_json::to_string(arguments).unwrap_or_default();
    crate::tools::redact_text(&raw) != raw
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
        let workspace = tempfile::tempdir().expect("temp workspace");
        std::fs::write(
            workspace.path().join("evidence.txt"),
            "secret=hidden\nproof",
        )
        .expect("write fixture");
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
        let orchestrator = ToolOrchestrator::new(policy, workspace.path().to_path_buf());

        let tool_calls = vec![crate::ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "evidence.txt"}),
        }];

        let observations = orchestrator
            .process_tool_calls(Uuid::new_v4(), tool_calls, &runtime)
            .await
            .unwrap();

        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].outcome, ToolOutcomeStatus::Success);
        assert!(observations[0].preview.contains("[REDACTED]"));
        assert!(runtime.events().unwrap().iter().any(|event| {
            matches!(
                &event.payload,
                crate::EventPayload::Tool(crate::ToolEvent::Observed {
                    tool_call_id,
                    outcome: ToolOutcomeStatus::Success,
                    ..
                }) if tool_call_id == "call_1"
            )
        }));
    }

    #[tokio::test]
    async fn write_is_durable_deferred_until_approval() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
        runtime.submit_intent("write a note").expect("intent");
        let orchestrator = ToolOrchestrator::new(policy, workspace.path().to_path_buf());
        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "write-1".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path": "note.txt", "content": "approved"}),
                }],
                &runtime,
            )
            .await
            .expect("tool call");
        assert_eq!(observations[0].outcome, ToolOutcomeStatus::ApprovalRequired);
        assert!(!workspace.path().join("note.txt").exists());
        assert!(runtime.events().unwrap().iter().any(|event| {
            matches!(
                &event.payload,
                crate::EventPayload::Tool(crate::ToolEvent::Deferred {
                    tool_call_id,
                    tool_name,
                    ..
                }) if tool_call_id == "write-1" && tool_name == "write_file"
            )
        }));
        let request = runtime
            .events()
            .unwrap()
            .iter()
            .find_map(|event| match &event.payload {
                crate::EventPayload::Approval(crate::ApprovalEvent::Requested { request }) => {
                    Some(request.clone())
                }
                _ => None,
            })
            .expect("approval request");
        let deferred = runtime
            .deferred_tool(request.id)
            .expect("deferred lookup")
            .expect("deferred tool");
        let resolution = crate::ApprovalResolution::user(&request, true);
        runtime
            .resolve_approval(resolution.clone())
            .expect("resolve approval");
        let observation =
            ToolOrchestrator::execute_approved_write(&runtime, request, resolution, deferred)
                .expect("execute approved write");
        assert_eq!(observation.outcome, ToolOutcomeStatus::Success);
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("note.txt")).expect("written file"),
            "approved"
        );
    }

    #[tokio::test]
    async fn bash_executes_only_after_exact_user_approval() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        ));
        runtime.submit_intent("inspect workspace").expect("intent");
        let orchestrator = ToolOrchestrator::new(runtime.policy(), workspace.path().to_path_buf());
        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "bash-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "printf verified"}),
                }],
                &runtime,
            )
            .await
            .expect("request bash");
        assert_eq!(observations[0].outcome, ToolOutcomeStatus::ApprovalRequired);
        let request = runtime
            .events()
            .unwrap()
            .iter()
            .find_map(|event| match &event.payload {
                crate::EventPayload::Approval(crate::ApprovalEvent::Requested { request }) => {
                    Some(request.clone())
                }
                _ => None,
            })
            .expect("approval request");
        let deferred = runtime
            .deferred_tool(request.id)
            .expect("deferred lookup")
            .expect("deferred bash");
        let resolution = crate::ApprovalResolution::user(&request, true);
        runtime
            .resolve_approval(resolution.clone())
            .expect("approval");
        let observation =
            ToolOrchestrator::execute_approved_bash(&runtime, request, resolution, deferred)
                .expect("approved shell execution");
        assert_eq!(observation.outcome, ToolOutcomeStatus::Success);
        assert!(observation.preview.contains("verified"));
    }
}
