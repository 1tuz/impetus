//! Live child spawn for Research / Build / Review roles (#311).
//!
//! Mirrors [`crate::explore_child`] structural path: validate metadata → admit
//! [`ChildConcurrencyGate`] → injectable executor (AgentLoop production or
//! process/mock for tests) → [`ChildResultStore`]. Explore stays in its
//! dedicated module.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::ChildEvent;
use crate::child_concurrency::{ChildConcurrencyError, ChildConcurrencyGate};
use crate::child_result_store::{ChildResultError, ChildResultStatus, ChildResultStore};
use crate::runtime::AgentRuntime;
use crate::storage::EventStore;
use crate::subagent_metadata::{ChildRunMetadata, ChildRunMetadataError, SubagentRole};

/// Research may claim read-only + web-label tools.
pub const RESEARCH_ALLOWED_TOOLS: &[&str] = &["list", "read", "search", "web"];
/// Build may claim write under an isolated worktree.
pub const BUILD_ALLOWED_TOOLS: &[&str] = &["list", "read", "search", "write"];
/// Review is read-only over diffs/tests.
pub const REVIEW_ALLOWED_TOOLS: &[&str] = &["list", "read", "search"];

/// Parent request to spawn one role-tagged child (labels / paths only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleChildRequest {
    pub parent_session_id: String,
    pub child_id: String,
    pub role: SubagentRole,
    pub cwd: PathBuf,
    pub allowed_tools: Vec<String>,
    pub write_roots: Vec<PathBuf>,
    pub worktree: Option<String>,
    pub context_label: String,
    pub max_tokens: u64,
    pub max_time_ms: u64,
    pub max_depth: u32,
    /// Optional program + argv for [`ProcessRoleChildExecutor`] (no secrets).
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
}

impl RoleChildRequest {
    pub fn to_metadata(&self) -> Result<ChildRunMetadata, RoleChildError> {
        if self.parent_session_id.trim().is_empty() {
            return Err(RoleChildError::EmptyParentId);
        }
        if self.child_id.trim().is_empty() {
            return Err(RoleChildError::EmptyChildId);
        }
        if self.role == SubagentRole::Explore {
            return Err(RoleChildError::UseExploreModule);
        }
        validate_role_allowed_tools(self.role, &self.allowed_tools)?;
        ChildRunMetadata {
            parent_id: self.parent_session_id.clone(),
            cwd: self.cwd.clone(),
            worktree: self.worktree.clone(),
            allowed_tools: self.allowed_tools.clone(),
            write_roots: self.write_roots.clone(),
            max_tokens: self.max_tokens,
            max_time: self.max_time_ms,
            max_depth: self.max_depth,
            role: self.role,
        }
        .try_validated()
        .map_err(RoleChildError::Metadata)
    }
}

#[derive(Debug, Clone)]
pub struct RoleChildEnv {
    pub child_id: String,
    pub metadata: ChildRunMetadata,
    pub context_label: String,
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleExecutorOutput {
    pub summary_label: String,
    pub artifact_ref_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RoleExecutorError {
    #[error("role child cancelled")]
    Cancelled,
    #[error("role child failed: {0}")]
    Failed(String),
}

pub trait RoleChildExecutor: Send + Sync {
    fn execute(&self, env: &RoleChildEnv) -> Result<RoleExecutorOutput, RoleExecutorError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleChildOutcome {
    pub child_id: String,
    pub parent_session_id: String,
    pub status: ChildResultStatus,
    pub summary_label: String,
    pub metadata: ChildRunMetadata,
}

#[derive(Debug, Error)]
pub enum RoleChildError {
    #[error("parent_session_id must be non-empty")]
    EmptyParentId,
    #[error("child_id must be non-empty")]
    EmptyChildId,
    #[error("use ExploreChildRunner for Explore role")]
    UseExploreModule,
    #[error("role child not configured on harness")]
    NotConfigured,
    #[error("disallowed tool `{0}` for role")]
    DisallowedTool(String),
    #[error(transparent)]
    Metadata(#[from] ChildRunMetadataError),
    #[error(transparent)]
    Concurrency(#[from] ChildConcurrencyError),
    #[error(transparent)]
    Store(#[from] ChildResultError),
}

pub trait RoleSpawnBridge: Send + Sync {
    fn spawn_role(
        &self,
        request: RoleChildRequest,
        cancel: CancellationToken,
    ) -> Result<RoleChildOutcome, RoleChildError>;

    fn child_results(&self) -> &ChildResultStore;
}

/// Production bridge: fair concurrency gate + durable store + shared executor.
pub struct HarnessRoleSpawn {
    pub gate: Arc<Mutex<ChildConcurrencyGate>>,
    pub store: Arc<ChildResultStore>,
    pub executor: Arc<dyn RoleChildExecutor>,
    pub parent_events: Option<Arc<dyn EventStore>>,
}

impl RoleSpawnBridge for HarnessRoleSpawn {
    fn spawn_role(
        &self,
        request: RoleChildRequest,
        cancel: CancellationToken,
    ) -> Result<RoleChildOutcome, RoleChildError> {
        let mut gate = self.gate.lock().expect("role child gate");
        let mut runner = RoleChildRunner::new(&mut gate, self.store.as_ref());
        if let Some(events) = self.parent_events.as_ref() {
            runner = runner.with_parent_events(events.as_ref());
        }
        runner.run(request, cancel, self.executor.as_ref())
    }

    fn child_results(&self) -> &ChildResultStore {
        self.store.as_ref()
    }
}

pub struct RoleChildRunner<'a> {
    pub gate: &'a mut ChildConcurrencyGate,
    pub store: &'a ChildResultStore,
    parent_events: Option<&'a dyn EventStore>,
}

impl<'a> RoleChildRunner<'a> {
    pub fn new(gate: &'a mut ChildConcurrencyGate, store: &'a ChildResultStore) -> Self {
        Self {
            gate,
            store,
            parent_events: None,
        }
    }

    pub fn with_parent_events(mut self, parent_events: &'a dyn EventStore) -> Self {
        self.parent_events = Some(parent_events);
        self
    }

    pub fn run(
        &mut self,
        request: RoleChildRequest,
        cancel: CancellationToken,
        executor: &dyn RoleChildExecutor,
    ) -> Result<RoleChildOutcome, RoleChildError> {
        let metadata = request.to_metadata()?;
        self.gate
            .admit_child(&request.child_id, &request.parent_session_id)?;

        self.emit_parent_child(
            &request.parent_session_id,
            ChildEvent::Started {
                child_id: request.child_id.clone(),
                parent_id: request.parent_session_id.clone(),
                role: metadata.role.as_str().to_string(),
            },
        );
        self.emit_parent_child(
            &request.parent_session_id,
            ChildEvent::StatusChanged {
                child_id: request.child_id.clone(),
                status: "running".into(),
                current_action: Some(request.context_label.clone()),
            },
        );

        let env = RoleChildEnv {
            child_id: request.child_id.clone(),
            metadata: metadata.clone(),
            context_label: request.context_label.clone(),
            program: request.program.clone(),
            args: request.args.clone(),
            cancel: cancel.clone(),
        };

        let exec_result = if cancel.is_cancelled() {
            Err(RoleExecutorError::Cancelled)
        } else {
            executor.execute(&env)
        };

        let (status, summary_label, artifact_ref_labels) = match exec_result {
            Ok(out) => (
                ChildResultStatus::Completed,
                out.summary_label,
                out.artifact_ref_labels,
            ),
            Err(RoleExecutorError::Cancelled) => {
                (ChildResultStatus::Cancelled, "cancelled".into(), Vec::new())
            }
            Err(RoleExecutorError::Failed(reason)) => {
                let label = if reason.trim().is_empty() {
                    "failed".to_string()
                } else {
                    reason
                };
                (ChildResultStatus::Failed, label, Vec::new())
            }
        };

        let mut result = crate::child_result_store::child_result_from_metadata(
            request.child_id.clone(),
            &metadata,
            status,
            summary_label.clone(),
        );
        result.artifact_ref_labels = artifact_ref_labels;

        let record_err = self.store.record_result(&result);
        let release_err = self.gate.release(&request.child_id);
        record_err?;
        release_err?;

        self.store
            .gate_parent_resume(&request.parent_session_id, &[&request.child_id])?;

        let (summary, error) = match status {
            ChildResultStatus::Completed => (Some(summary_label.clone()), None),
            ChildResultStatus::Failed => (None, Some(summary_label.clone())),
            ChildResultStatus::Cancelled => (Some(summary_label.clone()), None),
        };
        self.emit_parent_child(
            &request.parent_session_id,
            ChildEvent::Finished {
                child_id: request.child_id.clone(),
                status: status.as_str().to_string(),
                summary,
                error,
            },
        );

        Ok(RoleChildOutcome {
            child_id: request.child_id,
            parent_session_id: request.parent_session_id,
            status,
            summary_label,
            metadata,
        })
    }

    fn emit_parent_child(&self, parent_session_id: &str, event: ChildEvent) {
        if let Some(store) = self.parent_events {
            let _ = AgentRuntime::emit_parent_child_event(store, parent_session_id, event);
        }
    }
}

/// OS process executor for unit tests that pass explicit program/args.
///
/// Production WorkflowRuntime injects [`crate::AgentLoopRoleExecutor`] instead.
#[derive(Debug, Default)]
pub struct ProcessRoleChildExecutor;

impl ProcessRoleChildExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl RoleChildExecutor for ProcessRoleChildExecutor {
    fn execute(&self, env: &RoleChildEnv) -> Result<RoleExecutorOutput, RoleExecutorError> {
        if env.cancel.is_cancelled() {
            return Err(RoleExecutorError::Cancelled);
        }
        let program = env.program.clone().ok_or_else(|| {
            RoleExecutorError::Failed(
                "role child requires explicit program (no /bin/echo default in production)".into(),
            )
        })?;
        let args = env.args.clone();
        if args.is_empty() {
            return Err(RoleExecutorError::Failed(
                "role child requires non-empty args when program is set".into(),
            ));
        }

        let mut child = Command::new(&program)
            .args(&args)
            .current_dir(&env.metadata.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RoleExecutorError::Failed(format!("spawn: {e}")))?;

        // Poll cancel while waiting; kill on cancel.
        loop {
            if env.cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RoleExecutorError::Cancelled);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        return Ok(RoleExecutorOutput {
                            summary_label: format!(
                                "{}-process:{}",
                                env.metadata.role.as_str().to_lowercase(),
                                env.context_label
                            ),
                            artifact_ref_labels: Vec::new(),
                        });
                    }
                    return Err(RoleExecutorError::Failed(format!(
                        "exit {}",
                        status.code().unwrap_or(-1)
                    )));
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(e) => return Err(RoleExecutorError::Failed(format!("wait: {e}"))),
            }
        }
    }
}

/// Fixed-outcome test double.
#[derive(Debug, Default)]
pub struct MockRoleExecutor {
    summary: String,
    fail: Option<String>,
    honor_cancel: bool,
}

impl MockRoleExecutor {
    pub fn completing(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            fail: None,
            honor_cancel: false,
        }
    }

    pub fn failing(reason: impl Into<String>) -> Self {
        Self {
            summary: String::new(),
            fail: Some(reason.into()),
            honor_cancel: false,
        }
    }

    pub fn honor_cancel() -> Self {
        Self {
            summary: String::new(),
            fail: None,
            honor_cancel: true,
        }
    }
}

impl RoleChildExecutor for MockRoleExecutor {
    fn execute(&self, env: &RoleChildEnv) -> Result<RoleExecutorOutput, RoleExecutorError> {
        if self.honor_cancel && env.cancel.is_cancelled() {
            return Err(RoleExecutorError::Cancelled);
        }
        if let Some(reason) = &self.fail {
            return Err(RoleExecutorError::Failed(reason.clone()));
        }
        Ok(RoleExecutorOutput {
            summary_label: self.summary.clone(),
            artifact_ref_labels: Vec::new(),
        })
    }
}

pub fn allowed_tools_for(role: SubagentRole) -> &'static [&'static str] {
    match role {
        SubagentRole::Explore => crate::explore_child::EXPLORE_ALLOWED_TOOLS,
        SubagentRole::Research => RESEARCH_ALLOWED_TOOLS,
        SubagentRole::Build => BUILD_ALLOWED_TOOLS,
        SubagentRole::Review => REVIEW_ALLOWED_TOOLS,
    }
}

fn validate_role_allowed_tools(role: SubagentRole, tools: &[String]) -> Result<(), RoleChildError> {
    let allow = allowed_tools_for(role);
    for tool in tools {
        if !allow.contains(&tool.as_str()) {
            return Err(RoleChildError::DisallowedTool(tool.clone()));
        }
    }
    Ok(())
}

/// Default program path helper for tests (exists on macOS/Linux).
pub fn default_echo_program() -> PathBuf {
    PathBuf::from("/bin/echo")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChildConcurrencyConfig;
    use std::path::Path;
    use tempfile::tempdir;

    fn research_req(parent: &str, child: &str, cwd: &Path) -> RoleChildRequest {
        RoleChildRequest {
            parent_session_id: parent.into(),
            child_id: child.into(),
            role: SubagentRole::Research,
            cwd: cwd.to_path_buf(),
            allowed_tools: vec!["list".into(), "read".into()],
            write_roots: vec![],
            worktree: None,
            context_label: "research-ctx".into(),
            max_tokens: 100,
            max_time_ms: 5_000,
            max_depth: 1,
            program: Some(default_echo_program()),
            args: vec!["research-ok".into()],
        }
    }

    #[test]
    fn research_process_spawn_records_result() {
        let dir = tempdir().unwrap();
        let store = ChildResultStore::open(dir.path().join("c.db")).unwrap();
        let mut gate =
            ChildConcurrencyGate::from_config(ChildConcurrencyConfig::fair(4, 2).unwrap()).unwrap();
        let mut runner = RoleChildRunner::new(&mut gate, &store);
        let out = runner
            .run(
                research_req("p1", "c1", dir.path()),
                CancellationToken::new(),
                &ProcessRoleChildExecutor::new(),
            )
            .expect("spawn");
        assert_eq!(out.status, ChildResultStatus::Completed);
        assert!(out.summary_label.contains("research"));
        let loaded = store.load_result("c1").unwrap().expect("stored");
        assert_eq!(loaded.role_label, "Research");
    }

    #[test]
    fn build_requires_worktree() {
        let dir = tempdir().unwrap();
        let req = RoleChildRequest {
            parent_session_id: "p".into(),
            child_id: "b1".into(),
            role: SubagentRole::Build,
            cwd: dir.path().to_path_buf(),
            allowed_tools: vec!["write".into()],
            write_roots: vec![dir.path().to_path_buf()],
            worktree: None,
            context_label: "build".into(),
            max_tokens: 10,
            max_time_ms: 1000,
            max_depth: 1,
            program: None,
            args: vec![],
        };
        assert!(matches!(
            req.to_metadata(),
            Err(RoleChildError::Metadata(
                ChildRunMetadataError::BuildMissingWorktree
            ))
        ));
    }

    #[test]
    fn per_parent_cap_blocks_second_child() {
        let dir = tempdir().unwrap();
        let store = ChildResultStore::open(dir.path().join("c.db")).unwrap();
        let mut gate =
            ChildConcurrencyGate::from_config(ChildConcurrencyConfig::fair(4, 1).unwrap()).unwrap();
        gate.admit_child("held", "p1").unwrap();
        let mut runner = RoleChildRunner::new(&mut gate, &store);
        let err = runner
            .run(
                research_req("p1", "c2", dir.path()),
                CancellationToken::new(),
                &MockRoleExecutor::completing("x"),
            )
            .expect_err("parent cap");
        assert!(matches!(
            err,
            RoleChildError::Concurrency(ChildConcurrencyError::ParentCapReached { .. })
        ));
    }

    #[test]
    fn cancel_kills_process_path() {
        let dir = tempdir().unwrap();
        let store = ChildResultStore::open(dir.path().join("c.db")).unwrap();
        let mut gate = ChildConcurrencyGate::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut runner = RoleChildRunner::new(&mut gate, &store);
        let out = runner
            .run(
                research_req("p1", "c3", dir.path()),
                cancel,
                &MockRoleExecutor::honor_cancel(),
            )
            .unwrap();
        assert_eq!(out.status, ChildResultStatus::Cancelled);
    }

    #[test]
    fn explore_role_rejected() {
        let dir = tempdir().unwrap();
        let req = RoleChildRequest {
            parent_session_id: "p".into(),
            child_id: "e".into(),
            role: SubagentRole::Explore,
            cwd: dir.path().to_path_buf(),
            allowed_tools: vec!["list".into()],
            write_roots: vec![],
            worktree: None,
            context_label: "x".into(),
            max_tokens: 10,
            max_time_ms: 1000,
            max_depth: 1,
            program: None,
            args: vec![],
        };
        assert!(matches!(
            req.to_metadata(),
            Err(RoleChildError::UseExploreModule)
        ));
    }
}
