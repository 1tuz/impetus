//! Live WorkflowEngine runtime: schedule → role/explore child spawn → complete.
//!
//! Closes #311 orchestration gaps: live spawn from recipes, cancel/replace
//! wired to session-run terminal, fair per-parent concurrency on the live path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent_scheduler::InMemoryAgentScheduler;
use crate::child_concurrency::{ChildConcurrencyConfig, ChildConcurrencyGate};
use crate::child_result_store::ChildResultStore;
use crate::explore_child::{
    EXPLORE_ALLOWED_TOOLS, ExploreChildExecutor, ExploreChildRequest, ExploreChildRunner,
};
use crate::role_child::{RoleChildExecutor, RoleChildRequest, RoleChildRunner, allowed_tools_for};
use crate::storage::EventStore;
use crate::subagent_metadata::SubagentRole;
use crate::user_intent::{QueuedFollowUp, UserIntentRouter};
use crate::workflow_engine::{WorkflowEngine, WorkflowError, WorkflowRecipe, WorkflowStatus};

#[derive(Debug, Error)]
pub enum WorkflowRuntimeError {
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    #[error("no ready step")]
    NoReadyStep,
    #[error("workflow runtime not configured")]
    NotConfigured,
    #[error("role child: {0}")]
    RoleChild(String),
    #[error("explore child: {0}")]
    ExploreChild(String),
    #[error("unknown session binding: {0}")]
    UnknownSession(Uuid),
}

/// Per-session workflow binding (engine + scheduler + cancel tokens).
struct SessionWorkflow {
    engine: WorkflowEngine,
    scheduler: InMemoryAgentScheduler,
    step_cancels: HashMap<String, CancellationToken>,
    cwd: PathBuf,
}

/// Shared live orchestration surface for harness / daemon.
pub struct WorkflowRuntime {
    gate: Arc<Mutex<ChildConcurrencyGate>>,
    store: Arc<ChildResultStore>,
    role_executor: Arc<dyn RoleChildExecutor>,
    explore_executor: Arc<dyn ExploreChildExecutor>,
    parent_events: Option<Arc<dyn EventStore>>,
    worktrees: Option<Arc<crate::WorktreeManager>>,
    sessions: Mutex<HashMap<Uuid, SessionWorkflow>>,
}

impl WorkflowRuntime {
    pub fn new(
        store: Arc<ChildResultStore>,
        role_executor: Arc<dyn RoleChildExecutor>,
        explore_executor: Arc<dyn ExploreChildExecutor>,
    ) -> Result<Self, WorkflowRuntimeError> {
        Self::with_parent_events(store, role_executor, explore_executor, None)
    }

    pub fn with_parent_events(
        store: Arc<ChildResultStore>,
        role_executor: Arc<dyn RoleChildExecutor>,
        explore_executor: Arc<dyn ExploreChildExecutor>,
        parent_events: Option<Arc<dyn EventStore>>,
    ) -> Result<Self, WorkflowRuntimeError> {
        let gate = ChildConcurrencyGate::from_config(
            ChildConcurrencyConfig::fair(4, 2).expect("fair caps"),
        )
        .expect("gate");
        Ok(Self {
            gate: Arc::new(Mutex::new(gate)),
            store,
            role_executor,
            explore_executor,
            parent_events,
            worktrees: None,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// Attach daemon WorktreeManager so Build steps create real worktrees.
    pub fn with_worktree_manager(mut self, manager: Arc<crate::WorktreeManager>) -> Self {
        self.worktrees = Some(manager);
        self
    }

    pub fn child_results(&self) -> &ChildResultStore {
        self.store.as_ref()
    }

    pub fn gate(&self) -> Arc<Mutex<ChildConcurrencyGate>> {
        self.gate.clone()
    }

    /// Bind a recipe to a session (replaces any prior binding after cancel).
    pub fn start(
        &self,
        session_id: Uuid,
        recipe: WorkflowRecipe,
        cwd: PathBuf,
    ) -> Result<(), WorkflowRuntimeError> {
        self.cancel_session(session_id);
        let mut engine = WorkflowEngine::new(recipe, Default::default())?;
        let _ = engine.start();
        let mut sessions = self.sessions.lock().expect("workflow sessions");
        sessions.insert(
            session_id,
            SessionWorkflow {
                engine,
                scheduler: InMemoryAgentScheduler::new(),
                step_cancels: HashMap::new(),
                cwd,
            },
        );
        Ok(())
    }

    /// Replace recipe: cancel current admissions, then start fresh.
    pub fn replace(
        &self,
        session_id: Uuid,
        recipe: WorkflowRecipe,
        cwd: PathBuf,
    ) -> Result<(), WorkflowRuntimeError> {
        self.cancel_session(session_id);
        self.start(session_id, recipe, cwd)
    }

    pub fn status(&self, session_id: Uuid) -> Option<WorkflowStatus> {
        self.sessions
            .lock()
            .expect("workflow sessions")
            .get(&session_id)
            .map(|s| s.engine.status())
    }

    /// Cancel workflow + child tokens; scheduler admissions drained.
    pub fn cancel_session(&self, session_id: Uuid) {
        let mut sessions = self.sessions.lock().expect("workflow sessions");
        if let Some(mut sw) = sessions.remove(&session_id) {
            for token in sw.step_cancels.values() {
                token.cancel();
            }
            sw.engine.cancel_with_scheduler(&mut sw.scheduler);
        }
    }

    /// Session-run Cancelled/Completed hook: cancel workflow, optionally drain
    /// one FollowUp (at-most-once vs cancel race via UserIntentRouter).
    pub fn on_session_run_terminal(
        &self,
        session_id: Uuid,
        finished_run_id: Uuid,
        intents: &mut UserIntentRouter,
    ) -> Result<Option<QueuedFollowUp>, WorkflowRuntimeError> {
        self.cancel_session(session_id);
        intents
            .take_follow_up_on_run_terminal(session_id, finished_run_id)
            .map_err(|_| WorkflowRuntimeError::UnknownSession(session_id))
    }

    /// Spawn the next ready step as a live child; complete scheduler slot.
    pub fn run_next_ready_step(&self, session_id: Uuid) -> Result<String, WorkflowRuntimeError> {
        let mut sessions = self.sessions.lock().expect("workflow sessions");
        let sw = sessions
            .get_mut(&session_id)
            .ok_or(WorkflowRuntimeError::UnknownSession(session_id))?;

        if matches!(
            sw.engine.status(),
            WorkflowStatus::Completed
                | WorkflowStatus::Cancelled
                | WorkflowStatus::Failed
                | WorkflowStatus::BudgetExhausted
        ) {
            return Err(WorkflowRuntimeError::Workflow(
                WorkflowError::AlreadyFinished(sw.engine.status()),
            ));
        }

        let step_id = sw
            .engine
            .next_ready_step()
            .map(|s| s.id.clone())
            .ok_or(WorkflowRuntimeError::NoReadyStep)?;

        let admission = sw
            .engine
            .begin_step_with_scheduler(&step_id, &mut sw.scheduler)?;
        let _ = admission;

        let role = sw.engine.step_role(&step_id)?;
        let cancel = CancellationToken::new();
        sw.step_cancels.insert(step_id.clone(), cancel.clone());
        let cwd = sw.cwd.clone();
        drop(sessions); // release lock during spawn

        let summary = match role {
            Some(SubagentRole::Explore) => {
                self.spawn_explore(session_id, &step_id, &cwd, cancel)?
            }
            Some(role) => self.spawn_role(session_id, &step_id, role, &cwd, cancel)?,
            None => {
                // Approval / non-agent step: complete immediately with label.
                "approval-skip".into()
            }
        };

        let mut sessions = self.sessions.lock().expect("workflow sessions");
        let sw = sessions
            .get_mut(&session_id)
            .ok_or(WorkflowRuntimeError::UnknownSession(session_id))?;
        sw.step_cancels.remove(&step_id);
        sw.engine.complete_step_with_scheduler(
            &step_id,
            summary.clone(),
            1,
            1,
            &mut sw.scheduler,
        )?;
        Ok(summary)
    }

    /// Advance until blocked, failed, or completed.
    pub fn advance_until_blocked(
        &self,
        session_id: Uuid,
    ) -> Result<WorkflowStatus, WorkflowRuntimeError> {
        loop {
            match self.run_next_ready_step(session_id) {
                Ok(_) => continue,
                Err(WorkflowRuntimeError::NoReadyStep) => {
                    let status = self.status(session_id).unwrap_or(WorkflowStatus::Completed);
                    return Ok(status);
                }
                Err(WorkflowRuntimeError::Workflow(WorkflowError::AlreadyFinished(status))) => {
                    return Ok(status);
                }
                Err(WorkflowRuntimeError::Workflow(WorkflowError::BlockedByDependency(_, _))) => {
                    return Ok(self.status(session_id).unwrap_or(WorkflowStatus::Running));
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn spawn_role(
        &self,
        session_id: Uuid,
        step_id: &str,
        role: SubagentRole,
        cwd: &Path,
        cancel: CancellationToken,
    ) -> Result<String, WorkflowRuntimeError> {
        let tools: Vec<String> = allowed_tools_for(role)
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let (write_roots, worktree) = if role == SubagentRole::Build {
            if let Some(mgr) = self.worktrees.as_ref() {
                let binding = match mgr.get_by_session(session_id) {
                    Ok(Some(existing)) => {
                        // Resume stopped bindings; Active is reused as-is.
                        match existing.state {
                            crate::WorktreeLifecycleState::Active => existing,
                            crate::WorktreeLifecycleState::Stopped
                            | crate::WorktreeLifecycleState::Stale => {
                                mgr.resume(session_id).map_err(|err| {
                                    WorkflowRuntimeError::RoleChild(format!(
                                        "worktree resume: {err}"
                                    ))
                                })?
                            }
                            crate::WorktreeLifecycleState::Closed => mgr
                                .create_for_role(session_id, cwd, crate::AgentWorkRole::Build)
                                .map_err(|err| {
                                    WorkflowRuntimeError::RoleChild(format!(
                                        "worktree create_for_role: {err}"
                                    ))
                                })?,
                        }
                    }
                    Ok(None) => mgr
                        .create_for_role(session_id, cwd, crate::AgentWorkRole::Build)
                        .map_err(|err| {
                            WorkflowRuntimeError::RoleChild(format!(
                                "worktree create_for_role: {err}"
                            ))
                        })?,
                    Err(err) => {
                        return Err(WorkflowRuntimeError::RoleChild(format!(
                            "worktree lookup: {err}"
                        )));
                    }
                };
                (vec![binding.path.clone()], Some(binding.worktree_id))
            } else {
                return Err(WorkflowRuntimeError::RoleChild(
                    "Build role requires WorktreeManager; synthetic worktree ids removed".into(),
                ));
            }
        } else {
            (vec![], None)
        };
        let request = RoleChildRequest {
            parent_session_id: session_id.to_string(),
            child_id: format!("{session_id}-{step_id}"),
            role,
            cwd: cwd.to_path_buf(),
            allowed_tools: tools,
            write_roots,
            worktree,
            context_label: step_id.to_string(),
            max_tokens: 1_000,
            max_time_ms: 30_000,
            max_depth: 2,
            // Production: explicit program required — ProcessRoleChildExecutor
            // no longer defaults to /bin/echo success.
            program: Some(PathBuf::from("git")),
            args: vec![
                "-C".into(),
                cwd.display().to_string(),
                "status".into(),
                "--short".into(),
            ],
        };
        let mut gate = self.gate.lock().expect("workflow gate");
        let mut runner = RoleChildRunner::new(&mut gate, self.store.as_ref());
        if let Some(events) = self.parent_events.as_ref() {
            runner = runner.with_parent_events(events.as_ref());
        }
        let out = runner
            .run(request, cancel, self.role_executor.as_ref())
            .map_err(|e| WorkflowRuntimeError::RoleChild(e.to_string()))?;
        Ok(out.summary_label)
    }

    fn spawn_explore(
        &self,
        session_id: Uuid,
        step_id: &str,
        cwd: &Path,
        cancel: CancellationToken,
    ) -> Result<String, WorkflowRuntimeError> {
        let request = ExploreChildRequest {
            parent_session_id: session_id.to_string(),
            child_id: format!("{session_id}-{step_id}"),
            cwd: cwd.to_path_buf(),
            allowed_tools: EXPLORE_ALLOWED_TOOLS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            context_label: step_id.to_string(),
            max_tokens: 1_000,
            max_time_ms: 30_000,
            max_depth: 2,
        };
        let mut gate = self.gate.lock().expect("workflow gate");
        let mut runner = ExploreChildRunner::new(&mut gate, self.store.as_ref());
        if let Some(events) = self.parent_events.as_ref() {
            runner = runner.with_parent_events(events.as_ref());
        }
        let out = runner
            .run(request, cancel, self.explore_executor.as_ref())
            .map_err(|e| WorkflowRuntimeError::ExploreChild(e.to_string()))?;
        Ok(out.summary_label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActionOrigin;
    use crate::explore_child::MockExploreExecutor;
    use crate::role_child::MockRoleExecutor;
    use crate::user_intent::{UserIntentSubmission, UserPromptIntent};
    use tempfile::tempdir;

    fn runtime(dir: &std::path::Path) -> WorkflowRuntime {
        let store = Arc::new(ChildResultStore::open(dir.join("c.db")).unwrap());
        // Minimal git repo so Build can create_for_role via WorktreeManager.
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let _ = std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .status()
            .unwrap();
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&repo)
            .status();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&repo)
            .status();
        std::fs::write(repo.join("README"), b"x").unwrap();
        let _ = std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .status();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&repo)
            .status();
        let wt = Arc::new(
            crate::WorktreeManager::open(dir.join("wt.db"), dir.join("worktrees")).unwrap(),
        );
        WorkflowRuntime::new(
            store,
            Arc::new(MockRoleExecutor::completing("role-ok")),
            Arc::new(MockExploreExecutor::completing("explore-ok")),
        )
        .unwrap()
        .with_worktree_manager(wt)
    }

    #[test]
    fn bug_skeleton_advances_with_live_spawn() {
        let dir = tempdir().unwrap();
        let rt = runtime(dir.path());
        let sid = Uuid::new_v4();
        let cwd = dir.path().join("repo");
        rt.start(sid, WorkflowEngine::bug_skeleton_recipe(), cwd)
            .unwrap();
        let status = rt.advance_until_blocked(sid).unwrap();
        assert_eq!(status, WorkflowStatus::Completed);
        let kids = rt.child_results().list_by_parent(&sid.to_string()).unwrap();
        assert!(!kids.is_empty());
    }

    #[test]
    fn cancel_mid_run_drains_scheduler() {
        let dir = tempdir().unwrap();
        let rt = runtime(dir.path());
        let sid = Uuid::new_v4();
        rt.start(
            sid,
            WorkflowEngine::feature_skeleton_recipe(),
            dir.path().join("repo"),
        )
        .unwrap();
        rt.run_next_ready_step(sid).unwrap();
        rt.cancel_session(sid);
        assert!(rt.status(sid).is_none());
    }

    #[test]
    fn explore_failure_persists_child_and_releases_gate() {
        let dir = tempdir().unwrap();
        let store = Arc::new(ChildResultStore::open(dir.path().join("c.db")).unwrap());
        let rt = WorkflowRuntime::new(
            store.clone(),
            Arc::new(MockRoleExecutor::completing("role-ok")),
            Arc::new(MockExploreExecutor::failing("explore-boom")),
        )
        .unwrap();
        let sid = Uuid::new_v4();
        rt.start(
            sid,
            WorkflowEngine::bug_skeleton_recipe(),
            dir.path().join("repo"),
        )
        .unwrap();
        let summary = rt.run_next_ready_step(sid).unwrap();
        assert_eq!(summary, "explore-boom");
        let kids = store.list_by_parent(&sid.to_string()).unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].status, crate::ChildResultStatus::Failed);
        assert_eq!(kids[0].summary_label, "explore-boom");
        // Gate slot released — another explore can admit.
        assert!(!rt.gate().lock().unwrap().is_active(&kids[0].child_id));
    }

    #[test]
    fn explore_cancel_persists_and_session_cleanup() {
        let dir = tempdir().unwrap();
        let store = Arc::new(ChildResultStore::open(dir.path().join("c.db")).unwrap());
        let rt = WorkflowRuntime::new(
            store.clone(),
            Arc::new(MockRoleExecutor::completing("role-ok")),
            Arc::new(MockExploreExecutor::always_cancelled()),
        )
        .unwrap();
        let sid = Uuid::new_v4();
        rt.start(
            sid,
            WorkflowEngine::bug_skeleton_recipe(),
            dir.path().join("repo"),
        )
        .unwrap();
        let summary = rt.run_next_ready_step(sid).unwrap();
        assert_eq!(summary, "cancelled");
        let kids = store.list_by_parent(&sid.to_string()).unwrap();
        assert_eq!(kids[0].status, crate::ChildResultStatus::Cancelled);
        rt.cancel_session(sid);
        assert!(rt.status(sid).is_none());
        assert!(!rt.gate().lock().unwrap().is_active(&kids[0].child_id));
    }

    #[test]
    fn explore_artifacts_recorded_on_child_result() {
        let dir = tempdir().unwrap();
        let store = Arc::new(ChildResultStore::open(dir.path().join("c.db")).unwrap());
        let rt = WorkflowRuntime::new(
            store.clone(),
            Arc::new(MockRoleExecutor::completing("role-ok")),
            Arc::new(MockExploreExecutor::completing_with_artifacts(
                "scan-ok",
                ["art-1", "art-2"],
            )),
        )
        .unwrap();
        let sid = Uuid::new_v4();
        rt.start(
            sid,
            WorkflowEngine::bug_skeleton_recipe(),
            dir.path().join("repo"),
        )
        .unwrap();
        assert_eq!(rt.run_next_ready_step(sid).unwrap(), "scan-ok");
        let kids = store.list_by_parent(&sid.to_string()).unwrap();
        assert_eq!(
            kids[0].artifact_ref_labels,
            vec!["art-1".to_string(), "art-2".to_string()]
        );
    }

    #[test]
    fn replace_cancels_then_restarts() {
        let dir = tempdir().unwrap();
        let rt = runtime(dir.path());
        let sid = Uuid::new_v4();
        rt.start(
            sid,
            WorkflowEngine::bug_skeleton_recipe(),
            dir.path().join("repo"),
        )
        .unwrap();
        rt.run_next_ready_step(sid).unwrap();
        rt.replace(
            sid,
            WorkflowEngine::feature_skeleton_recipe(),
            dir.path().join("repo"),
        )
        .unwrap();
        assert_eq!(rt.status(sid), Some(WorkflowStatus::Running));
    }

    #[test]
    fn terminal_hook_drains_follow_up_at_most_once() {
        let dir = tempdir().unwrap();
        let rt = runtime(dir.path());
        let sid = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let mut intents = UserIntentRouter::new();
        intents.open_session(sid);
        intents.set_active_run(sid, Some(run_id)).unwrap();
        intents
            .submit(UserIntentSubmission {
                session_id: sid,
                intent: UserPromptIntent::FollowUp,
                text: "next".into(),
                origin: ActionOrigin::User,
            })
            .unwrap();
        rt.start(
            sid,
            WorkflowEngine::bug_skeleton_recipe(),
            dir.path().join("repo"),
        )
        .unwrap();
        let drained = rt
            .on_session_run_terminal(sid, run_id, &mut intents)
            .unwrap();
        assert_eq!(drained.unwrap().text, "next");
        let again = intents.take_follow_up_on_run_terminal(sid, run_id).unwrap();
        assert!(again.is_none());
    }
}
