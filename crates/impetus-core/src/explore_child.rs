//! Minimal Explore-child vertical slice (#296).
//!
//! Goal path (this module):
//! parent session → validate Explore [`ChildRunMetadata`] → admit
//! [`ChildConcurrencyGate`] → injectable [`ExploreChildExecutor`] → durable
//! [`ChildResultStore`] → [`ChildResultStore::gate_parent_resume`].
//!
//! Structural restrictions (not prompt text):
//! - role must be [`SubagentRole::Explore`]
//! - `write_roots` empty (via metadata validation)
//! - `allowed_tools` ⊆ {`list`, `read`, `search`} — write / spawn / network denied
//! - worktree optional (not forced)
//!
//! Production AgentLoop binding lives in [`crate::explore_agent_loop`].
//! This module owns harness restrictions, persistence, cancel/fail, and a
//! narrow real [`ReadOnlyTools`] path (no network).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::child_concurrency::{ChildConcurrencyError, ChildConcurrencyGate};
use crate::child_result_store::{
    ChildResult, ChildResultError, ChildResultStatus, ChildResultStore,
};
use crate::subagent_metadata::{ChildRunMetadata, ChildRunMetadataError, SubagentRole};
use crate::tools::{ReadOnlyTool, ReadOnlyToolKind, ReadOnlyTools, ToolError, ToolOutcome};
use crate::{ActionOrigin, DurableArtifactStore};

/// Explore may only claim these read-only tool labels.
pub const EXPLORE_ALLOWED_TOOLS: &[&str] = &["list", "read", "search"];

/// Parent request to spawn one Explore child (labels / paths only; no secrets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExploreChildRequest {
    pub parent_session_id: String,
    pub child_id: String,
    pub cwd: PathBuf,
    pub allowed_tools: Vec<String>,
    pub context_label: String,
    pub max_tokens: u64,
    pub max_time_ms: u64,
    pub max_depth: u32,
}

impl ExploreChildRequest {
    /// Build validated Explore [`ChildRunMetadata`] (forces role, empty write_roots).
    pub fn to_metadata(&self) -> Result<ChildRunMetadata, ExploreChildError> {
        if self.parent_session_id.trim().is_empty() {
            return Err(ExploreChildError::EmptyParentId);
        }
        if self.child_id.trim().is_empty() {
            return Err(ExploreChildError::EmptyChildId);
        }
        validate_explore_allowed_tools(&self.allowed_tools)?;
        ChildRunMetadata {
            parent_id: self.parent_session_id.clone(),
            cwd: self.cwd.clone(),
            worktree: None,
            allowed_tools: self.allowed_tools.clone(),
            write_roots: vec![],
            max_tokens: self.max_tokens,
            max_time: self.max_time_ms,
            max_depth: self.max_depth,
            role: SubagentRole::Explore,
        }
        .try_validated()
        .map_err(ExploreChildError::Metadata)
    }
}

/// Environment handed to an [`ExploreChildExecutor`].
#[derive(Debug, Clone)]
pub struct ExploreChildEnv {
    pub child_id: String,
    pub metadata: ChildRunMetadata,
    pub context_label: String,
    pub cancel: CancellationToken,
}

/// Executor output labels (never secrets / raw payloads).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExploreExecutorOutput {
    pub summary_label: String,
    pub artifact_ref_labels: Vec<String>,
}

/// Errors from an Explore executor (labels only).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExploreExecutorError {
    #[error("explore child cancelled")]
    Cancelled,
    #[error("explore child failed: {0}")]
    Failed(String),
}

/// Injectable child body — production later binds AgentLoop; tests inject fakes.
pub trait ExploreChildExecutor: Send + Sync {
    fn execute(&self, env: &ExploreChildEnv)
    -> Result<ExploreExecutorOutput, ExploreExecutorError>;
}

/// Parent-facing outcome after durable record + resume gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExploreChildOutcome {
    pub child_id: String,
    pub parent_session_id: String,
    pub status: ChildResultStatus,
    pub summary_label: String,
    pub metadata: ChildRunMetadata,
}

/// Bridge between harness and child runner.
pub trait ExploreSpawnBridge: Send + Sync {
    fn spawn_explore(
        &self,
        request: ExploreChildRequest,
        cancel: CancellationToken,
    ) -> Result<ExploreChildOutcome, ExploreChildError>;

    /// Durable child-result store used by parent-resume helpers.
    fn child_results(&self) -> &ChildResultStore;
}

/// Harness-owned bridge implementation.
pub struct HarnessExploreSpawn {
    pub gate: Arc<Mutex<ChildConcurrencyGate>>,
    pub store: Arc<ChildResultStore>,
    pub executor: Arc<dyn ExploreChildExecutor>,
}

impl ExploreSpawnBridge for HarnessExploreSpawn {
    fn spawn_explore(
        &self,
        request: ExploreChildRequest,
        cancel: CancellationToken,
    ) -> Result<ExploreChildOutcome, ExploreChildError> {
        let mut gate = self.gate.lock().unwrap();
        let mut runner = ExploreChildRunner::new(&mut gate, self.store.as_ref());
        runner.run(request, cancel, self.executor.as_ref())
    }

    fn child_results(&self) -> &ChildResultStore {
        self.store.as_ref()
    }
}

/// Failures on the Explore harness path.
#[derive(Debug, Error)]
pub enum ExploreChildError {
    #[error("parent_session_id must be non-empty")]
    EmptyParentId,
    #[error("child_id must be non-empty")]
    EmptyChildId,
    #[error(transparent)]
    Metadata(#[from] ChildRunMetadataError),
    #[error("explore allowed_tools must be non-empty")]
    EmptyAllowedTools,
    #[error("tool {0} is not allowed for Explore (read-only: list/read/search)")]
    ToolNotAllowed(String),
    #[error(transparent)]
    Concurrency(#[from] ChildConcurrencyError),
    #[error(transparent)]
    ResultStore(#[from] ChildResultError),
    #[error("explore child write_roots must stay empty")]
    WriteRootsForbidden,
    #[error("explore child role must be Explore, got {0}")]
    WrongRole(String),
    #[error("explore spawn is not configured on this harness")]
    NotConfigured,
}

/// Validate tool allowlist structurally for Explore.
pub fn validate_explore_allowed_tools(tools: &[String]) -> Result<(), ExploreChildError> {
    if tools.is_empty() {
        return Err(ExploreChildError::EmptyAllowedTools);
    }
    for tool in tools {
        let name = tool.trim();
        if name.is_empty() || !EXPLORE_ALLOWED_TOOLS.contains(&name) {
            return Err(ExploreChildError::ToolNotAllowed(tool.clone()));
        }
    }
    Ok(())
}

/// Reject write / spawn / network-style tool names before any executor runs.
pub fn is_explore_forbidden_tool(name: &str) -> bool {
    !EXPLORE_ALLOWED_TOOLS.contains(&name.trim())
}

/// Intersect requested Explore tools with parent ceiling ∩ EXPLORE allowlist.
///
/// `parent_allowed` None or empty → ceiling is full [`EXPLORE_ALLOWED_TOOLS`]
/// (never broader than Explore). Result ⊆ EXPLORE ∩ requested ∩ parent.
pub fn intersect_explore_tools(
    parent_allowed: Option<&[String]>,
    requested: &[String],
) -> Result<Vec<String>, ExploreChildError> {
    validate_explore_allowed_tools(requested)?;

    let parent_ceiling: Vec<&str> = match parent_allowed {
        None | Some([]) => EXPLORE_ALLOWED_TOOLS.to_vec(),
        Some(tools) => tools
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty() && EXPLORE_ALLOWED_TOOLS.contains(t))
            .collect(),
    };

    let mut out = Vec::with_capacity(requested.len());
    for tool in requested {
        let name = tool.trim();
        if !parent_ceiling.contains(&name) {
            return Err(ExploreChildError::ToolNotAllowed(tool.clone()));
        }
        if !out.iter().any(|existing: &String| existing == name) {
            out.push(name.to_string());
        }
    }
    if out.is_empty() {
        return Err(ExploreChildError::EmptyAllowedTools);
    }
    Ok(out)
}

/// Re-check parent resume after Explore children recorded durable results.
pub fn resume_parent_after_explore(
    store: &ChildResultStore,
    parent_id: &str,
    child_ids: &[&str],
) -> Result<Vec<ChildResult>, ExploreChildError> {
    store.gate_parent_resume(parent_id, child_ids)?;
    let mut out = Vec::with_capacity(child_ids.len());
    for child_id in child_ids {
        let result = store.load_result(child_id)?.ok_or_else(|| {
            ExploreChildError::ResultStore(ChildResultError::MissingChildResults {
                parent_id: parent_id.to_string(),
                missing: vec![(*child_id).to_string()],
            })
        })?;
        out.push(result);
    }
    Ok(out)
}

/// Orchestrates one Explore child: admit → execute → persist → resume gate.
pub struct ExploreChildRunner<'a> {
    pub gate: &'a mut ChildConcurrencyGate,
    pub store: &'a ChildResultStore,
}

impl<'a> ExploreChildRunner<'a> {
    pub fn new(gate: &'a mut ChildConcurrencyGate, store: &'a ChildResultStore) -> Self {
        Self { gate, store }
    }

    /// Run Explore child end-to-end with structural restrictions.
    pub fn run(
        &mut self,
        request: ExploreChildRequest,
        cancel: CancellationToken,
        executor: &dyn ExploreChildExecutor,
    ) -> Result<ExploreChildOutcome, ExploreChildError> {
        let metadata = request.to_metadata()?;
        enforce_explore_structural(&metadata)?;

        self.gate
            .admit_child(&request.child_id, &request.parent_session_id)?;

        let env = ExploreChildEnv {
            child_id: request.child_id.clone(),
            metadata: metadata.clone(),
            context_label: request.context_label.clone(),
            cancel: cancel.clone(),
        };

        let exec_result = if cancel.is_cancelled() {
            Err(ExploreExecutorError::Cancelled)
        } else {
            executor.execute(&env)
        };

        let (status, summary_label, artifact_ref_labels) = match exec_result {
            Ok(out) => (
                ChildResultStatus::Completed,
                out.summary_label,
                out.artifact_ref_labels,
            ),
            Err(ExploreExecutorError::Cancelled) => {
                (ChildResultStatus::Cancelled, "cancelled".into(), Vec::new())
            }
            Err(ExploreExecutorError::Failed(reason)) => {
                let label = if reason.trim().is_empty() {
                    "failed".to_string()
                } else {
                    reason
                };
                (ChildResultStatus::Failed, label, Vec::new())
            }
        };

        let mut result = ChildResult::from_metadata(
            request.child_id.clone(),
            &metadata,
            status,
            summary_label.clone(),
        );
        result.artifact_ref_labels = artifact_ref_labels;

        // Persist before release so parent resume survives process restart.
        let record_err = self.store.record_result(&result);
        let release_err = self.gate.release(&request.child_id);
        record_err?;
        release_err?;

        self.store
            .gate_parent_resume(&request.parent_session_id, &[&request.child_id])?;

        Ok(ExploreChildOutcome {
            child_id: request.child_id,
            parent_session_id: request.parent_session_id,
            status,
            summary_label,
            metadata,
        })
    }
}

fn enforce_explore_structural(metadata: &ChildRunMetadata) -> Result<(), ExploreChildError> {
    if metadata.role != SubagentRole::Explore {
        return Err(ExploreChildError::WrongRole(metadata.role.as_str().into()));
    }
    if !metadata.write_roots.is_empty() {
        return Err(ExploreChildError::WriteRootsForbidden);
    }
    validate_explore_allowed_tools(&metadata.allowed_tools)?;
    Ok(())
}

/// Narrow real path: run allowed ReadOnlyTools against `metadata.cwd` (no network).
///
/// Does not call AgentLoop. Prefers `list` on `.`, else first allowed tool.
#[derive(Debug)]
pub struct ReadOnlyExploreExecutor {
    artifact_root: PathBuf,
}

impl ReadOnlyExploreExecutor {
    pub fn new(artifact_root: impl Into<PathBuf>) -> Self {
        Self {
            artifact_root: artifact_root.into(),
        }
    }

    fn pick_tool(allowed: &[String], cwd: &Path) -> Result<ReadOnlyTool, ExploreExecutorError> {
        let names: Vec<&str> = allowed.iter().map(|s| s.as_str()).collect();
        if names.contains(&"list") {
            return Ok(ReadOnlyTool::List {
                target: cwd.to_path_buf(),
            });
        }
        if names.contains(&"read") {
            return Ok(ReadOnlyTool::Read {
                target: cwd.join("README.md"),
            });
        }
        if names.contains(&"search") {
            return Ok(ReadOnlyTool::Search {
                target: cwd.to_path_buf(),
                pattern: ".".into(),
            });
        }
        Err(ExploreExecutorError::Failed(
            "no explore read tool selected".into(),
        ))
    }
}

impl ExploreChildExecutor for ReadOnlyExploreExecutor {
    fn execute(
        &self,
        env: &ExploreChildEnv,
    ) -> Result<ExploreExecutorOutput, ExploreExecutorError> {
        if env.cancel.is_cancelled() {
            return Err(ExploreExecutorError::Cancelled);
        }
        let artifacts = DurableArtifactStore::open(&self.artifact_root)
            .map_err(|err| ExploreExecutorError::Failed(format!("artifact store: {err}")))?;
        let tools = ReadOnlyTools::new(&env.metadata.cwd);
        let tool = Self::pick_tool(&env.metadata.allowed_tools, &env.metadata.cwd)?;
        if env.cancel.is_cancelled() {
            return Err(ExploreExecutorError::Cancelled);
        }
        let outcome = tools
            .run(tool, ActionOrigin::Agent, &artifacts)
            .map_err(|err: ToolError| ExploreExecutorError::Failed(err.to_string()))?;
        match outcome {
            ToolOutcome::Allowed { result } => {
                let kind = match result.tool {
                    ReadOnlyToolKind::List => "list",
                    ReadOnlyToolKind::Read => "read",
                    ReadOnlyToolKind::Search => "search",
                };
                let mut artifact_ref_labels = Vec::new();
                if let Some(artifact) = &result.artifact {
                    artifact_ref_labels.push(artifact.id.clone());
                }
                let summary = format!("explore-{kind}:{}:{}", env.context_label, result.line_count);
                Ok(ExploreExecutorOutput {
                    summary_label: summary,
                    artifact_ref_labels,
                })
            }
            ToolOutcome::Denied { reason, .. } => Err(ExploreExecutorError::Failed(reason)),
        }
    }
}

/// Test / deferred double: fixed outcome or cancel/fail.
#[derive(Debug, Default)]
pub struct MockExploreExecutor {
    mode: Mutex<MockExploreMode>,
    last_env: Mutex<Option<ExploreChildEnv>>,
}

#[derive(Debug, Clone)]
enum MockExploreMode {
    Complete {
        summary: String,
        artifacts: Vec<String>,
    },
    Fail(String),
    Cancel,
    HonorCancelToken,
}

impl Default for MockExploreMode {
    fn default() -> Self {
        Self::Complete {
            summary: "mock-ok".into(),
            artifacts: Vec::new(),
        }
    }
}

impl MockExploreExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn completing(summary: impl Into<String>) -> Self {
        Self {
            mode: Mutex::new(MockExploreMode::Complete {
                summary: summary.into(),
                artifacts: Vec::new(),
            }),
            ..Self::default()
        }
    }

    pub fn failing(reason: impl Into<String>) -> Self {
        Self {
            mode: Mutex::new(MockExploreMode::Fail(reason.into())),
            ..Self::default()
        }
    }

    pub fn always_cancelled() -> Self {
        Self {
            mode: Mutex::new(MockExploreMode::Cancel),
            ..Self::default()
        }
    }

    /// Succeed unless cancel token already fired.
    pub fn honor_cancel_token() -> Self {
        Self {
            mode: Mutex::new(MockExploreMode::HonorCancelToken),
            last_env: Mutex::new(None),
        }
    }

    pub fn last_env(&self) -> Option<ExploreChildEnv> {
        self.last_env
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl ExploreChildExecutor for MockExploreExecutor {
    fn execute(
        &self,
        env: &ExploreChildEnv,
    ) -> Result<ExploreExecutorOutput, ExploreExecutorError> {
        *self.last_env.lock().unwrap_or_else(|p| p.into_inner()) = Some(env.clone());
        let mode = self.mode.lock().unwrap_or_else(|p| p.into_inner()).clone();
        match mode {
            MockExploreMode::Complete { summary, artifacts } => Ok(ExploreExecutorOutput {
                summary_label: summary,
                artifact_ref_labels: artifacts,
            }),
            MockExploreMode::Fail(reason) => Err(ExploreExecutorError::Failed(reason)),
            MockExploreMode::Cancel => Err(ExploreExecutorError::Cancelled),
            MockExploreMode::HonorCancelToken => {
                if env.cancel.is_cancelled() {
                    Err(ExploreExecutorError::Cancelled)
                } else {
                    Ok(ExploreExecutorOutput {
                        summary_label: "mock-ok".into(),
                        artifact_ref_labels: Vec::new(),
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChildConcurrencyConfig;

    fn temp_store() -> (tempfile::TempDir, ChildResultStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ChildResultStore::open(dir.path().join("child_results.db")).expect("open");
        (dir, store)
    }

    fn sample_request(cwd: PathBuf) -> ExploreChildRequest {
        ExploreChildRequest {
            parent_session_id: "parent-session-1".into(),
            child_id: "child-explore-1".into(),
            cwd,
            allowed_tools: vec!["list".into(), "read".into()],
            context_label: "find-auth".into(),
            max_tokens: 2_000,
            max_time_ms: 30_000,
            max_depth: 1,
        }
    }

    #[test]
    fn explore_rejects_write_and_network_tools() {
        assert!(validate_explore_allowed_tools(&["list".into()]).is_ok());
        assert!(matches!(
            validate_explore_allowed_tools(&["write".into()]),
            Err(ExploreChildError::ToolNotAllowed(_))
        ));
        assert!(matches!(
            validate_explore_allowed_tools(&["web_fetch".into()]),
            Err(ExploreChildError::ToolNotAllowed(_))
        ));
        assert!(matches!(
            validate_explore_allowed_tools(&["spawn".into()]),
            Err(ExploreChildError::ToolNotAllowed(_))
        ));
        assert!(is_explore_forbidden_tool("shell"));
        assert!(!is_explore_forbidden_tool("search"));
    }

    #[test]
    fn intersect_explore_tools_respects_parent_ceiling() {
        let full = intersect_explore_tools(None, &["list".into(), "read".into()]).expect("full");
        assert_eq!(full, vec!["list", "read"]);

        let empty_parent =
            intersect_explore_tools(Some(&[]), &["search".into()]).expect("empty parent");
        assert_eq!(empty_parent, vec!["search"]);

        let narrowed = intersect_explore_tools(
            Some(&["list".into(), "read".into()]),
            &["list".into(), "search".into()],
        );
        assert!(matches!(
            narrowed,
            Err(ExploreChildError::ToolNotAllowed(_))
        ));

        let ok = intersect_explore_tools(Some(&["list".into(), "read".into()]), &["read".into()])
            .expect("subset");
        assert_eq!(ok, vec!["read"]);
    }

    #[test]
    fn request_to_metadata_forces_explore_and_empty_write_roots() {
        let req = sample_request(PathBuf::from("/tmp/ws"));
        let meta = req.to_metadata().expect("meta");
        assert_eq!(meta.parent_id, "parent-session-1");
        assert_eq!(meta.role, SubagentRole::Explore);
        assert!(meta.write_roots.is_empty());
        assert_eq!(meta.cwd, PathBuf::from("/tmp/ws"));
        assert_eq!(meta.allowed_tools, vec!["list", "read"]);
        assert!(meta.worktree.is_none());
    }

    #[test]
    fn request_rejects_write_roots_via_tool_allowlist_and_empty_tools() {
        let mut req = sample_request(PathBuf::from("/tmp/ws"));
        req.allowed_tools = vec![];
        assert!(matches!(
            req.to_metadata(),
            Err(ExploreChildError::EmptyAllowedTools)
        ));
        req.allowed_tools = vec!["apply_patch".into()];
        assert!(matches!(
            req.to_metadata(),
            Err(ExploreChildError::ToolNotAllowed(_))
        ));
    }

    #[test]
    fn end_to_end_persist_and_parent_resume() {
        let (_dir, store) = temp_store();
        let mut gate = ChildConcurrencyGate::new();
        let req = sample_request(PathBuf::from("/tmp/ws"));
        let mock = MockExploreExecutor::completing("auth-map");
        let mut runner = ExploreChildRunner::new(&mut gate, &store);

        let out = runner
            .run(req.clone(), CancellationToken::new(), &mock)
            .expect("run");

        assert_eq!(out.child_id, "child-explore-1");
        assert_eq!(out.parent_session_id, "parent-session-1");
        assert_eq!(out.status, ChildResultStatus::Completed);
        assert_eq!(out.summary_label, "auth-map");
        assert_eq!(out.metadata.role, SubagentRole::Explore);
        assert!(!gate.is_active("child-explore-1"));

        let loaded = store
            .load_result("child-explore-1")
            .expect("load")
            .expect("present");
        assert_eq!(loaded.parent_id, "parent-session-1");
        assert_eq!(loaded.role_label, "Explore");
        assert_eq!(loaded.status, ChildResultStatus::Completed);
        assert_eq!(loaded.summary_label, "auth-map");

        store
            .gate_parent_resume("parent-session-1", &["child-explore-1"])
            .expect("resume");

        let env = mock.last_env().expect("executor saw env");
        assert_eq!(env.child_id, "child-explore-1");
        assert_eq!(env.metadata.parent_id, "parent-session-1");
        assert_eq!(env.metadata.cwd, PathBuf::from("/tmp/ws"));
        assert_eq!(env.context_label, "find-auth");
        assert!(env.metadata.write_roots.is_empty());
    }

    #[test]
    fn failure_propagates_and_persists() {
        let (_dir, store) = temp_store();
        let mut gate = ChildConcurrencyGate::new();
        let req = sample_request(PathBuf::from("/tmp/ws"));
        let mock = MockExploreExecutor::failing("parse-error");
        let mut runner = ExploreChildRunner::new(&mut gate, &store);

        let out = runner
            .run(req, CancellationToken::new(), &mock)
            .expect("failed run still returns durable outcome");

        assert_eq!(out.status, ChildResultStatus::Failed);
        assert_eq!(out.summary_label, "parse-error");
        let loaded = store
            .load_result("child-explore-1")
            .expect("load")
            .expect("present");
        assert_eq!(loaded.status, ChildResultStatus::Failed);
        assert!(!gate.is_active("child-explore-1"));
        store
            .gate_parent_resume("parent-session-1", &["child-explore-1"])
            .expect("failed child still unblocks resume once recorded");
    }

    #[test]
    fn cancellation_persists_and_releases_slot() {
        let (_dir, store) = temp_store();
        let mut gate =
            ChildConcurrencyGate::from_config(crate::ChildConcurrencyConfig::new(1).unwrap())
                .unwrap();
        let req = sample_request(PathBuf::from("/tmp/ws"));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mock = MockExploreExecutor::honor_cancel_token();
        let mut runner = ExploreChildRunner::new(&mut gate, &store);

        let out = runner.run(req, cancel, &mock).expect("cancel path");
        assert_eq!(out.status, ChildResultStatus::Cancelled);
        assert_eq!(out.summary_label, "cancelled");
        assert!(!gate.is_active("child-explore-1"));
        let loaded = store
            .load_result("child-explore-1")
            .expect("load")
            .expect("present");
        assert_eq!(loaded.status, ChildResultStatus::Cancelled);
        // Slot free for next child.
        gate.admit("child-explore-2")
            .expect("slot free after cancel");
    }

    #[test]
    fn executor_cancel_error_also_persists() {
        let (_dir, store) = temp_store();
        let mut gate = ChildConcurrencyGate::new();
        let req = sample_request(PathBuf::from("/tmp/ws"));
        let mock = MockExploreExecutor::always_cancelled();
        let mut runner = ExploreChildRunner::new(&mut gate, &store);
        let out = runner
            .run(req, CancellationToken::new(), &mock)
            .expect("cancel");
        assert_eq!(out.status, ChildResultStatus::Cancelled);
    }

    #[test]
    fn per_parent_cap_blocks_second_explore_for_same_parent() {
        let (_dir, store) = temp_store();
        let mut gate =
            ChildConcurrencyGate::from_config(ChildConcurrencyConfig::fair(4, 1).expect("config"))
                .unwrap();
        gate.admit_child("child-explore-1", "parent-session-1")
            .unwrap();
        let req = ExploreChildRequest {
            parent_session_id: "parent-session-1".into(),
            child_id: "child-explore-2".into(),
            ..sample_request(PathBuf::from("/tmp/ws"))
        };
        let mock = MockExploreExecutor::completing("x");
        let mut runner = ExploreChildRunner::new(&mut gate, &store);
        let err = runner
            .run(req, CancellationToken::new(), &mock)
            .expect_err("parent cap");
        assert!(matches!(
            err,
            ExploreChildError::Concurrency(ChildConcurrencyError::ParentCapReached { .. })
        ));
    }

    #[test]
    fn concurrency_cap_blocks_before_execute() {
        let (_dir, store) = temp_store();
        let mut gate =
            ChildConcurrencyGate::from_config(crate::ChildConcurrencyConfig::new(1).unwrap())
                .unwrap();
        gate.admit("other").unwrap();
        let req = sample_request(PathBuf::from("/tmp/ws"));
        let mock = MockExploreExecutor::completing("x");
        let mut runner = ExploreChildRunner::new(&mut gate, &store);
        let err = runner
            .run(req, CancellationToken::new(), &mock)
            .expect_err("cap");
        assert!(matches!(err, ExploreChildError::Concurrency(_)));
        assert!(store.load_result("child-explore-1").unwrap().is_none());
    }

    #[test]
    fn readonly_tools_executor_lists_temp_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("note.txt"), "hello").expect("write");
        let artifacts = dir.path().join("artifacts");
        let store = ChildResultStore::open(dir.path().join("child_results.db")).expect("store");
        let mut gate = ChildConcurrencyGate::new();
        let req = ExploreChildRequest {
            parent_session_id: "parent-ro".into(),
            child_id: "child-ro".into(),
            cwd: dir.path().to_path_buf(),
            allowed_tools: vec!["list".into()],
            context_label: "scan".into(),
            max_tokens: 500,
            max_time_ms: 10_000,
            max_depth: 1,
        };
        let executor = ReadOnlyExploreExecutor::new(artifacts);
        let mut runner = ExploreChildRunner::new(&mut gate, &store);
        let out = runner
            .run(req, CancellationToken::new(), &executor)
            .expect("readonly explore");
        assert_eq!(out.status, ChildResultStatus::Completed);
        assert!(out.summary_label.starts_with("explore-list:scan:"));
        let loaded = store.load_result("child-ro").unwrap().expect("present");
        assert_eq!(loaded.status, ChildResultStatus::Completed);
        store
            .gate_parent_resume("parent-ro", &["child-ro"])
            .expect("resume");
    }

    #[test]
    fn structural_reject_nonzero_write_roots_on_metadata() {
        // Direct enforce path: metadata with write_roots must fail even if role Explore.
        let meta = ChildRunMetadata {
            parent_id: "p".into(),
            cwd: PathBuf::from("/tmp"),
            worktree: None,
            allowed_tools: vec!["list".into()],
            write_roots: vec![PathBuf::from("/tmp/out")],
            max_tokens: 1,
            max_time: 1,
            max_depth: 1,
            role: SubagentRole::Explore,
        };
        // try_validated already rejects; enforce also rejects if bypassed.
        assert!(matches!(
            meta.clone().try_validated(),
            Err(ChildRunMetadataError::WriteRootsNotAllowed { .. })
        ));
        assert!(matches!(
            enforce_explore_structural(&meta),
            Err(ExploreChildError::WriteRootsForbidden)
        ));
    }
}
