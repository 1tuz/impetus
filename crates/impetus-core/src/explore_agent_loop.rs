//! Production Explore executor: restricted [`AgentLoop`] (#306).
//!
//! Binds [`ExploreChildExecutor`] to AgentLoop + allowlisted tools.
//! Parent policy overrides inherit; network forced off. Child context is
//! one user message from `context_label` (not full parent transcript).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::agent_loop::AgentLoop;
use crate::budget::BudgetConfig;
use crate::explore_child::{
    ExploreChildEnv, ExploreChildExecutor, ExploreExecutorError, ExploreExecutorOutput,
};
use crate::policy::PolicyEngine;
use crate::provider::ProviderMessage;
use crate::provider_trait::ModelProvider;
use crate::runtime::AgentRuntime;
use crate::storage::EventStore;
use crate::tool_orchestrator::ToolOrchestrator;
use crate::tool_schema::{BuiltinToolSchema, builtin_tool_schemas, canonical_tool_name};

/// Map Explore allowlist labels to provider/schema tool names.
///
/// `list`/`read`/`search` → `list_files`/`read_file`/`search`.
pub fn explore_provider_tool_names(allowed: &[String]) -> Vec<&'static str> {
    let mut out = Vec::new();
    for tool in allowed {
        let mapped = match tool.trim() {
            "list" | "list_files" => Some("list_files"),
            "read" | "read_file" => Some("read_file"),
            "search" => Some("search"),
            other => canonical_tool_name(other)
                .filter(|name| matches!(*name, "list_files" | "read_file" | "search")),
        };
        if let Some(name) = mapped
            && !out.contains(&name)
        {
            out.push(name);
        }
    }
    out
}

/// Builtin schemas filtered to Explore-mapped provider names.
pub fn explore_provider_tool_schemas(allowed: &[String]) -> Vec<&'static BuiltinToolSchema> {
    let names = explore_provider_tool_names(allowed);
    builtin_tool_schemas()
        .iter()
        .filter(|schema| names.contains(&schema.name))
        .collect()
}

/// Factory for per-child event stores (Explore isolation).
pub type ExploreStoreFactory = Box<dyn Fn(&str) -> Arc<dyn EventStore> + Send + Sync>;

/// Production executor that drives a restricted [`AgentLoop`] for Explore.
pub struct AgentLoopExploreExecutor {
    pub provider: Arc<dyn ModelProvider>,
    pub store_factory: ExploreStoreFactory,
    pub policy_template: PolicyEngine,
    pub artifact_root: PathBuf,
}

impl AgentLoopExploreExecutor {
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        store_factory: ExploreStoreFactory,
        policy_template: PolicyEngine,
        artifact_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            provider,
            store_factory,
            policy_template,
            artifact_root: artifact_root.into(),
        }
    }
}

impl ExploreChildExecutor for AgentLoopExploreExecutor {
    fn execute(
        &self,
        env: &ExploreChildEnv,
    ) -> Result<ExploreExecutorOutput, ExploreExecutorError> {
        let mut scope = self.policy_template.scope().clone();
        scope.workspace_root = env.metadata.cwd.clone();
        scope.allow_network = false;
        scope.allow_web_outbound = false;
        scope.allow_private_network = false;
        // Child permissions ⊆ parent: inherit overrides, force network off above.
        let policy = PolicyEngine::with_config(scope, self.policy_template.config());

        let store = (self.store_factory)(&env.child_id);

        let mut runtime = AgentRuntime::new(store, policy.clone());
        runtime
            .set_budget(BudgetConfig {
                max_tokens: Some(env.metadata.max_tokens),
                max_wall_time: Some(Duration::from_millis(env.metadata.max_time)),
                ..Default::default()
            })
            .map_err(|e| ExploreExecutorError::Failed(e.to_string()))?;
        runtime
            .submit_intent(format!("Explore: {}", env.context_label))
            .map_err(|e| ExploreExecutorError::Failed(e.to_string()))?;
        let run_id = runtime
            .start_run()
            .map_err(|e| ExploreExecutorError::Failed(e.to_string()))?;
        let runtime = Arc::new(runtime);

        let orchestrator = ToolOrchestrator::with_artifact_root(
            policy,
            env.metadata.cwd.clone(),
            self.artifact_root.clone(),
        )
        .with_allowed_tools(env.metadata.allowed_tools.clone());

        let agent_loop = AgentLoop::with_tool_orchestrator(runtime.clone(), orchestrator);
        let messages = vec![ProviderMessage::user(env.context_label.clone())];
        let result = block_on_explore(agent_loop.execute(
            run_id,
            self.provider.clone(),
            messages,
            env.cancel.clone(),
            None,
            crate::StreamOptions::default(),
        ));

        match result {
            Ok(_) => {
                let summary = runtime
                    .events()
                    .ok()
                    .and_then(|events| {
                        events.iter().rev().find_map(|event| match &event.payload {
                            crate::EventPayload::Agent(crate::AgentEvent::Final {
                                text, ..
                            }) => Some(text.clone()),
                            _ => None,
                        })
                    })
                    .unwrap_or_else(|| "explore completed".to_string());

                let artifact_ref_labels = runtime
                    .events()
                    .ok()
                    .map(|events| {
                        events
                            .iter()
                            .filter_map(|event| match &event.payload {
                                crate::EventPayload::Tool(crate::ToolEvent::Observed {
                                    artifact: Some(artifact),
                                    ..
                                }) => Some(artifact.id.clone()),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                Ok(ExploreExecutorOutput {
                    summary_label: summary,
                    artifact_ref_labels,
                })
            }
            Err(crate::agent_loop::AgentLoopError::Cancelled) => {
                Err(ExploreExecutorError::Cancelled)
            }
            Err(e) => Err(ExploreExecutorError::Failed(e.to_string())),
        }
    }
}

/// Run Explore AgentLoop from the sync [`ExploreChildExecutor`] trait.
///
/// ponytail: sync trait forces block_on. Ceiling — nested runtime / worker
/// thread; upgrade path: async executor or `spawn_blocking` at call site.
fn block_on_explore<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("explore tokio runtime")
            .block_on(fut),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::child_concurrency::ChildConcurrencyGate;
    use crate::child_result_store::{ChildResultStatus, ChildResultStore};
    use crate::explore_child::{
        ExploreChildRequest, ExploreChildRunner, ExploreSpawnBridge, HarnessExploreSpawn,
        resume_parent_after_explore,
    };
    use crate::mock_provider::{MockProvider, MockStreamItem};
    use crate::policy_config::{POLICY_CONFIG_VERSION, PolicyConfig, PolicyConfigDecision};
    use crate::storage::MemoryEventStore;
    use crate::{Action, ActionKind, ActionOrigin, PolicyDecision, SandboxScope};
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    fn temp_child_store() -> (tempfile::TempDir, ChildResultStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ChildResultStore::open(dir.path().join("child_results.db")).expect("open");
        (dir, store)
    }

    fn sample_request(cwd: PathBuf) -> ExploreChildRequest {
        ExploreChildRequest {
            parent_session_id: "parent-explore-e2e".into(),
            child_id: "child-explore-e2e".into(),
            cwd,
            allowed_tools: vec!["list".into(), "read".into(), "search".into()],
            context_label: "map auth module".into(),
            max_tokens: 4_000,
            max_time_ms: 30_000,
            max_depth: 1,
        }
    }

    #[test]
    fn explore_provider_names_map_labels() {
        assert_eq!(
            explore_provider_tool_names(&["list".into(), "read".into()]),
            vec!["list_files", "read_file"]
        );
        assert_eq!(
            explore_provider_tool_names(&["list_files".into(), "search".into()]),
            vec!["list_files", "search"]
        );
        let schemas = explore_provider_tool_schemas(&["read".into()]);
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "read_file");
    }

    #[test]
    fn agent_loop_explore_e2e_persists_and_gates_parent() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("note.txt"), "hello explore").expect("write fixture");
        let artifacts = tempfile::tempdir().expect("artifacts");
        let (_child_dir, child_store) = temp_child_store();

        let provider = Arc::new(MockProvider::scripted(
            "explore-mock",
            "test",
            [
                vec![MockStreamItem::ToolCall {
                    id: "call_1".into(),
                    tool: "list_files".into(),
                    arguments: r#"{"path":"."}"#.into(),
                }],
                vec![MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "auth-map-summary".into(),
                }],
            ],
        ));

        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let executor = AgentLoopExploreExecutor::new(
            provider,
            Box::new(|_child_id| Arc::new(MemoryEventStore::default())),
            policy,
            artifacts.path(),
        );

        let mut gate = ChildConcurrencyGate::new();
        let mut runner = ExploreChildRunner::new(&mut gate, &child_store);
        let out = runner
            .run(
                sample_request(workspace.path().to_path_buf()),
                CancellationToken::new(),
                &executor,
            )
            .expect("explore e2e");

        assert_eq!(out.status, ChildResultStatus::Completed);
        assert_eq!(out.summary_label, "auth-map-summary");
        let resumed =
            resume_parent_after_explore(&child_store, "parent-explore-e2e", &["child-explore-e2e"])
                .expect("parent resume");
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].summary_label, "auth-map-summary");
    }

    #[test]
    fn explore_child_denies_write_and_bash() {
        let workspace = tempfile::tempdir().expect("workspace");
        let artifacts = tempfile::tempdir().expect("artifacts");
        let (_child_dir, child_store) = temp_child_store();

        let provider = Arc::new(MockProvider::scripted(
            "explore-deny",
            "test",
            [
                vec![MockStreamItem::ToolCall {
                    id: "w1".into(),
                    tool: "write_file".into(),
                    arguments: r#"{"path":"x.txt","content":"nope"}"#.into(),
                }],
                vec![MockStreamItem::ToolCall {
                    id: "b1".into(),
                    tool: "bash".into(),
                    arguments: r#"{"command":"echo hi"}"#.into(),
                }],
                vec![MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "denied-then-done".into(),
                }],
            ],
        ));

        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let executor = AgentLoopExploreExecutor::new(
            provider.clone(),
            Box::new(|_id| Arc::new(MemoryEventStore::default())),
            policy,
            artifacts.path(),
        );

        let bridge = HarnessExploreSpawn {
            gate: Arc::new(Mutex::new(ChildConcurrencyGate::new())),
            store: Arc::new(child_store),
            executor: Arc::new(executor),
            parent_events: None,
        };
        let out = bridge
            .spawn_explore(
                sample_request(workspace.path().to_path_buf()),
                CancellationToken::new(),
            )
            .expect("spawn");
        assert_eq!(out.status, ChildResultStatus::Completed);
        assert_eq!(out.summary_label, "denied-then-done");

        // Second/third model turns received tool observations (denials).
        let messages = provider.received_messages();
        assert!(messages.len() >= 2);
        let denied = messages
            .iter()
            .flatten()
            .any(|msg| msg.role() == "tool" && msg.content().contains("not in tool allowlist"));
        assert!(denied, "expected allowlist denial in tool observations");
    }

    #[test]
    fn explore_child_inherits_parent_deny_override() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut overrides = BTreeMap::new();
        overrides.insert(ActionKind::ReadFile, PolicyConfigDecision::Deny);
        let parent = PolicyEngine::with_config(
            SandboxScope::local_workspace(workspace.path()),
            PolicyConfig {
                version: POLICY_CONFIG_VERSION,
                overrides,
            },
        );

        let mut scope = parent.scope().clone();
        scope.allow_network = false;
        let child = PolicyEngine::with_config(scope, parent.config());

        let action = Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::ReadFile,
            summary: "read note".into(),
            target: Some(workspace.path().join("note.txt").display().to_string()),
        };
        assert!(matches!(
            child.evaluate(&action),
            PolicyDecision::Deny { .. }
        ));
    }
}
