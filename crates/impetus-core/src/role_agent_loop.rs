//! Production Research / Build / Review executor: restricted [`AgentLoop`] (#322).
//!
//! Mirrors [`crate::explore_agent_loop::AgentLoopExploreExecutor`]: role allowlist
//! → provider tool schemas, child context from `context_label`, policy inherit
//! with role-specific network / workspace binding. Explore stays dedicated.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::agent_loop::AgentLoop;
use crate::budget::BudgetConfig;
use crate::policy::PolicyEngine;
use crate::provider::ProviderMessage;
use crate::provider_trait::ModelProvider;
use crate::role_child::{RoleChildEnv, RoleChildExecutor, RoleExecutorError, RoleExecutorOutput};
use crate::runtime::AgentRuntime;
use crate::storage::EventStore;
use crate::subagent_metadata::SubagentRole;
use crate::tool_orchestrator::ToolOrchestrator;
use crate::tool_schema::{BuiltinToolSchema, builtin_tool_schemas, canonical_tool_name};

/// Map role allowlist labels to provider/schema tool names.
///
/// `list`/`read`/`search`/`write`/`web` → schema names. `web` expands to
/// read-oriented web tools (`web_search`, `web_fetch`).
pub fn role_provider_tool_names(allowed: &[String]) -> Vec<&'static str> {
    let mut out = Vec::new();
    for tool in allowed {
        match tool.trim() {
            "list" | "list_files" => push_unique(&mut out, "list_files"),
            "read" | "read_file" => push_unique(&mut out, "read_file"),
            "search" => push_unique(&mut out, "search"),
            "write" | "write_file" | "edit_file" => push_unique(&mut out, "write_file"),
            "web" => {
                push_unique(&mut out, "web_search");
                push_unique(&mut out, "web_fetch");
            }
            "web_search" => push_unique(&mut out, "web_search"),
            "web_fetch" => push_unique(&mut out, "web_fetch"),
            other => {
                if let Some(name) = canonical_tool_name(other).filter(|name| {
                    matches!(
                        *name,
                        "list_files"
                            | "read_file"
                            | "search"
                            | "write_file"
                            | "web_search"
                            | "web_fetch"
                    )
                }) {
                    push_unique(&mut out, name);
                }
            }
        }
    }
    out
}

/// Builtin schemas filtered to role-mapped provider names.
pub fn role_provider_tool_schemas(allowed: &[String]) -> Vec<&'static BuiltinToolSchema> {
    let names = role_provider_tool_names(allowed);
    builtin_tool_schemas()
        .iter()
        .filter(|schema| names.contains(&schema.name))
        .collect()
}

fn push_unique(out: &mut Vec<&'static str>, name: &'static str) {
    if !out.contains(&name) {
        out.push(name);
    }
}

fn allowlist_has_web(allowed: &[String]) -> bool {
    allowed.iter().any(|tool| {
        matches!(
            tool.trim(),
            "web" | "web_search" | "web_fetch" | "web_download" | "web_browser"
        )
    })
}

/// Factory for per-child event stores (role isolation).
pub type RoleStoreFactory = Box<dyn Fn(&str) -> Arc<dyn EventStore> + Send + Sync>;

/// Production executor that drives a restricted [`AgentLoop`] for Research/Build/Review.
pub struct AgentLoopRoleExecutor {
    pub provider: Arc<dyn ModelProvider>,
    pub store_factory: RoleStoreFactory,
    pub policy_template: PolicyEngine,
    pub artifact_root: PathBuf,
}

impl AgentLoopRoleExecutor {
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        store_factory: RoleStoreFactory,
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

impl RoleChildExecutor for AgentLoopRoleExecutor {
    fn execute(&self, env: &RoleChildEnv) -> Result<RoleExecutorOutput, RoleExecutorError> {
        if env.metadata.role == SubagentRole::Explore {
            return Err(RoleExecutorError::Failed(
                "use AgentLoopExploreExecutor for Explore role".into(),
            ));
        }

        let workspace_root = role_workspace_root(env);
        let has_web = allowlist_has_web(&env.metadata.allowed_tools)
            && env.metadata.role == SubagentRole::Research;

        let mut scope = self.policy_template.scope().clone();
        scope.workspace_root = workspace_root.clone();
        // Research may enable read-only web when allowlist includes web tools.
        // Build/Review keep network off. Outbound mutating web stays closed.
        scope.allow_network = has_web;
        scope.allow_web_outbound = false;
        scope.allow_private_network = false;
        let policy = PolicyEngine::with_config(scope, self.policy_template.config());

        let store = (self.store_factory)(&env.child_id);

        let role_tag = env.metadata.role.as_str();
        let mut runtime = AgentRuntime::new(store, policy.clone());
        runtime
            .set_budget(BudgetConfig {
                max_tokens: Some(env.metadata.max_tokens),
                max_wall_time: Some(Duration::from_millis(env.metadata.max_time)),
                ..Default::default()
            })
            .map_err(|e| RoleExecutorError::Failed(e.to_string()))?;
        runtime
            .submit_intent(format!("{role_tag}: {}", env.context_label))
            .map_err(|e| RoleExecutorError::Failed(e.to_string()))?;
        let run_id = runtime
            .start_run()
            .map_err(|e| RoleExecutorError::Failed(e.to_string()))?;
        let runtime = Arc::new(runtime);

        let mut orchestrator = ToolOrchestrator::with_artifact_root(
            policy.clone(),
            workspace_root,
            self.artifact_root.clone(),
        )
        .with_allowed_tools(env.metadata.allowed_tools.clone());

        if has_web {
            let mut web_research =
                crate::web_research::WebResearchEngine::production(policy.egress_policy());
            if let Ok(artifacts) = crate::DurableArtifactStore::open(&self.artifact_root) {
                web_research = web_research.with_artifact_store(
                    Arc::new(artifacts),
                    crate::web_research::ArtifactPolicy::default(),
                );
            }
            orchestrator = orchestrator.with_web_research(Arc::new(web_research));
        }

        let agent_loop = AgentLoop::with_tool_orchestrator(runtime.clone(), orchestrator);
        let messages = vec![ProviderMessage::user(env.context_label.clone())];
        let result = block_on_role(agent_loop.execute(
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
                    .unwrap_or_else(|| format!("{} completed", role_tag.to_lowercase()));

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

                Ok(RoleExecutorOutput {
                    summary_label: summary,
                    artifact_ref_labels,
                })
            }
            Err(crate::agent_loop::AgentLoopError::Cancelled) => Err(RoleExecutorError::Cancelled),
            Err(e) => Err(RoleExecutorError::Failed(e.to_string())),
        }
    }
}

/// Build binds workspace to worktree write root; other roles use metadata cwd.
fn role_workspace_root(env: &RoleChildEnv) -> PathBuf {
    if env.metadata.role == SubagentRole::Build {
        env.metadata
            .write_roots
            .first()
            .cloned()
            .unwrap_or_else(|| env.metadata.cwd.clone())
    } else {
        env.metadata.cwd.clone()
    }
}

/// Run role AgentLoop from the sync [`RoleChildExecutor`] trait.
///
/// ponytail: sync trait forces block_on. Ceiling — nested runtime / worker
/// thread; upgrade path: async executor or `spawn_blocking` at call site.
fn block_on_role<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("role tokio runtime")
            .block_on(fut),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::child_concurrency::ChildConcurrencyGate;
    use crate::child_result_store::{ChildResultStatus, ChildResultStore};
    use crate::mock_provider::{MockProvider, MockStreamItem};
    use crate::role_child::{
        BUILD_ALLOWED_TOOLS, RESEARCH_ALLOWED_TOOLS, REVIEW_ALLOWED_TOOLS, RoleChildRequest,
        RoleChildRunner,
    };
    use crate::storage::MemoryEventStore;
    use crate::{PolicyEngine, SandboxScope};
    use std::path::PathBuf;
    use tokio_util::sync::CancellationToken;

    fn temp_child_store() -> (tempfile::TempDir, ChildResultStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ChildResultStore::open(dir.path().join("child_results.db")).expect("open");
        (dir, store)
    }

    fn research_request(cwd: PathBuf) -> RoleChildRequest {
        RoleChildRequest {
            parent_session_id: "parent-research-e2e".into(),
            child_id: "child-research-e2e".into(),
            role: SubagentRole::Research,
            cwd,
            allowed_tools: RESEARCH_ALLOWED_TOOLS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            write_roots: vec![],
            worktree: None,
            context_label: "survey auth docs".into(),
            max_tokens: 4_000,
            max_time_ms: 30_000,
            max_depth: 1,
            program: None,
            args: vec![],
        }
    }

    fn review_request(cwd: PathBuf) -> RoleChildRequest {
        RoleChildRequest {
            parent_session_id: "parent-review-e2e".into(),
            child_id: "child-review-e2e".into(),
            role: SubagentRole::Review,
            cwd,
            allowed_tools: REVIEW_ALLOWED_TOOLS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            write_roots: vec![],
            worktree: None,
            context_label: "review diff".into(),
            max_tokens: 4_000,
            max_time_ms: 30_000,
            max_depth: 1,
            program: None,
            args: vec![],
        }
    }

    fn build_request(cwd: PathBuf, write_root: PathBuf) -> RoleChildRequest {
        RoleChildRequest {
            parent_session_id: "parent-build-e2e".into(),
            child_id: "child-build-e2e".into(),
            role: SubagentRole::Build,
            cwd: cwd.clone(),
            allowed_tools: BUILD_ALLOWED_TOOLS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            write_roots: vec![write_root],
            worktree: Some("wt-build-e2e".into()),
            context_label: "implement fix".into(),
            max_tokens: 4_000,
            max_time_ms: 30_000,
            max_depth: 1,
            program: None,
            args: vec![],
        }
    }

    #[test]
    fn role_provider_names_map_labels() {
        assert_eq!(
            role_provider_tool_names(&["list".into(), "read".into(), "write".into()]),
            vec!["list_files", "read_file", "write_file"]
        );
        assert_eq!(
            role_provider_tool_names(&["web".into()]),
            vec!["web_search", "web_fetch"]
        );
        let schemas = role_provider_tool_schemas(&["search".into(), "write".into()]);
        assert_eq!(schemas.len(), 2);
    }

    #[test]
    fn agent_loop_research_e2e_invokes_mock_provider() {
        let workspace = tempfile::tempdir().expect("workspace");
        let artifacts = tempfile::tempdir().expect("artifacts");
        let (_child_dir, child_store) = temp_child_store();

        let provider = Arc::new(MockProvider::scripted(
            "research-mock",
            "test",
            [vec![MockStreamItem::Chunk {
                chunk_id: 1,
                text: "research-summary".into(),
            }]],
        ));

        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let executor = AgentLoopRoleExecutor::new(
            provider.clone(),
            Box::new(|_id| Arc::new(MemoryEventStore::default())),
            policy,
            artifacts.path(),
        );

        let mut gate = ChildConcurrencyGate::new();
        let mut runner = RoleChildRunner::new(&mut gate, &child_store);
        let out = runner
            .run(
                research_request(workspace.path().to_path_buf()),
                CancellationToken::new(),
                &executor,
            )
            .expect("research e2e");

        assert_eq!(out.status, ChildResultStatus::Completed);
        assert_eq!(out.summary_label, "research-summary");
        assert!(
            !provider.received_messages().is_empty(),
            "AgentLoop must call provider (not git stub)"
        );
    }

    #[test]
    fn agent_loop_review_e2e_invokes_mock_provider() {
        let workspace = tempfile::tempdir().expect("workspace");
        let artifacts = tempfile::tempdir().expect("artifacts");
        let (_child_dir, child_store) = temp_child_store();

        let provider = Arc::new(MockProvider::scripted(
            "review-mock",
            "test",
            [vec![MockStreamItem::Chunk {
                chunk_id: 1,
                text: "review-summary".into(),
            }]],
        ));

        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let executor = AgentLoopRoleExecutor::new(
            provider.clone(),
            Box::new(|_id| Arc::new(MemoryEventStore::default())),
            policy,
            artifacts.path(),
        );

        let mut gate = ChildConcurrencyGate::new();
        let mut runner = RoleChildRunner::new(&mut gate, &child_store);
        let out = runner
            .run(
                review_request(workspace.path().to_path_buf()),
                CancellationToken::new(),
                &executor,
            )
            .expect("review e2e");

        assert_eq!(out.status, ChildResultStatus::Completed);
        assert_eq!(out.summary_label, "review-summary");
        assert!(!provider.received_messages().is_empty());
    }

    #[test]
    fn agent_loop_build_e2e_binds_worktree_and_invokes_provider() {
        let workspace = tempfile::tempdir().expect("workspace");
        let worktree = tempfile::tempdir().expect("worktree");
        std::fs::write(worktree.path().join("src.rs"), "fn main() {}").expect("fixture");
        let artifacts = tempfile::tempdir().expect("artifacts");
        let (_child_dir, child_store) = temp_child_store();

        let provider = Arc::new(MockProvider::scripted(
            "build-mock",
            "test",
            [
                vec![MockStreamItem::ToolCall {
                    id: "call_1".into(),
                    tool: "list_files".into(),
                    arguments: r#"{"path":"."}"#.into(),
                }],
                vec![MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "build-summary".into(),
                }],
            ],
        ));

        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let executor = AgentLoopRoleExecutor::new(
            provider.clone(),
            Box::new(|_id| Arc::new(MemoryEventStore::default())),
            policy,
            artifacts.path(),
        );

        let mut gate = ChildConcurrencyGate::new();
        let mut runner = RoleChildRunner::new(&mut gate, &child_store);
        let out = runner
            .run(
                build_request(
                    workspace.path().to_path_buf(),
                    worktree.path().to_path_buf(),
                ),
                CancellationToken::new(),
                &executor,
            )
            .expect("build e2e");

        assert_eq!(out.status, ChildResultStatus::Completed);
        assert_eq!(out.summary_label, "build-summary");
        assert!(!provider.received_messages().is_empty());
    }

    #[test]
    fn build_child_denies_bash_not_in_allowlist() {
        let workspace = tempfile::tempdir().expect("workspace");
        let worktree = tempfile::tempdir().expect("worktree");
        let artifacts = tempfile::tempdir().expect("artifacts");
        let (_child_dir, child_store) = temp_child_store();

        let provider = Arc::new(MockProvider::scripted(
            "build-deny",
            "test",
            [
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
        let executor = AgentLoopRoleExecutor::new(
            provider.clone(),
            Box::new(|_id| Arc::new(MemoryEventStore::default())),
            policy,
            artifacts.path(),
        );

        let mut gate = ChildConcurrencyGate::new();
        let mut runner = RoleChildRunner::new(&mut gate, &child_store);
        let out = runner
            .run(
                build_request(
                    workspace.path().to_path_buf(),
                    worktree.path().to_path_buf(),
                ),
                CancellationToken::new(),
                &executor,
            )
            .expect("spawn");
        assert_eq!(out.status, ChildResultStatus::Completed);
        assert_eq!(out.summary_label, "denied-then-done");

        let messages = provider.received_messages();
        let denied = messages
            .iter()
            .flatten()
            .any(|msg| msg.role() == "tool" && msg.content().contains("not in tool allowlist"));
        assert!(denied, "expected allowlist denial in tool observations");
    }

    #[test]
    fn review_child_denies_write() {
        let workspace = tempfile::tempdir().expect("workspace");
        let artifacts = tempfile::tempdir().expect("artifacts");
        let (_child_dir, child_store) = temp_child_store();

        let provider = Arc::new(MockProvider::scripted(
            "review-deny-write",
            "test",
            [
                vec![MockStreamItem::ToolCall {
                    id: "w1".into(),
                    tool: "write_file".into(),
                    arguments: r#"{"path":"x.txt","content":"nope"}"#.into(),
                }],
                vec![MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "review-denied-write".into(),
                }],
            ],
        ));

        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let executor = AgentLoopRoleExecutor::new(
            provider.clone(),
            Box::new(|_id| Arc::new(MemoryEventStore::default())),
            policy,
            artifacts.path(),
        );

        let mut gate = ChildConcurrencyGate::new();
        let mut runner = RoleChildRunner::new(&mut gate, &child_store);
        let out = runner
            .run(
                review_request(workspace.path().to_path_buf()),
                CancellationToken::new(),
                &executor,
            )
            .expect("spawn");
        assert_eq!(out.summary_label, "review-denied-write");
        let denied = provider
            .received_messages()
            .iter()
            .flatten()
            .any(|msg| msg.role() == "tool" && msg.content().contains("not in tool allowlist"));
        assert!(denied);
    }
}
