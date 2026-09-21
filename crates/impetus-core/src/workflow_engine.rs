//! In-memory WorkflowEngine skeleton (TODO P1 §6).
//!
//! Owns step order, dependency gating, per-workflow budget stubs, cancellation,
//! minimal per-step retry, and per-step checkpoint / result slots. Does **not**
//! spawn subagents or call LLMs — role strings on steps are hints for a future
//! [`crate::service_contract::AgentScheduler`].

use std::collections::HashMap;
use thiserror::Error;

/// Declared step inside a [`WorkflowRecipe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowStep {
    pub id: String,
    pub name: String,
    /// Optional role hint for AgentScheduler (`"Explore"`, `"Research"`, …).
    /// Not a new agent type — string only in this slice.
    pub role: Option<String>,
    /// Explicit step-id dependencies that must complete before this step runs.
    pub depends_on: Vec<String>,
    /// Per-step retry override. `None` → use [`WorkflowBudget::max_retries`].
    pub max_retries: Option<u32>,
}

/// Declarative ordered recipe. Step list order is the default advance order;
/// `depends_on` still gates readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRecipe {
    pub id: String,
    pub name: String,
    pub steps: Vec<WorkflowStep>,
}

/// Soft budget hooks for one workflow run (stubs; no wall clock in tests).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkflowBudget {
    pub max_tokens: Option<u64>,
    /// Cumulative step-reported duration budget (milliseconds).
    pub max_wall_ms: Option<u64>,
    /// Default max failure retries per step (`0` = fail once → failed checkpoint).
    pub max_retries: u32,
}

/// Lifecycle of a single step checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Running,
    Completed,
    /// Retries exhausted; step will not run again.
    Failed,
    Cancelled,
}

/// Per-step durable slot: status + opaque result string (no secrets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepCheckpoint {
    pub step_id: String,
    pub status: StepStatus,
    pub result: Option<String>,
    /// Count of recorded failures for this step (drives retry budget).
    pub attempts: u32,
}

/// Overall run status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowStatus {
    Idle,
    Running,
    Completed,
    Cancelled,
    BudgetExhausted,
    /// A step exhausted its retry budget.
    Failed,
}

/// Errors from advancing or completing steps.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowError {
    #[error("workflow already finished with status {0:?}")]
    AlreadyFinished(WorkflowStatus),
    #[error("unknown step id: {0}")]
    UnknownStep(String),
    #[error("step {0} blocked by unfinished dependencies: {1:?}")]
    BlockedByDependency(String, Vec<String>),
    #[error("step {0} is not runnable (status {1:?})")]
    NotRunnable(String, StepStatus),
    #[error("workflow budget exhausted ({kind})")]
    BudgetExhausted { kind: &'static str },
    #[error("step {0} retries exhausted")]
    RetriesExhausted(String),
    #[error("workflow cancelled")]
    Cancelled,
    #[error("invalid recipe: {0}")]
    InvalidRecipe(String),
}

/// Boring in-memory state machine over a recipe.
///
/// ponytail: single-workflow sequential advance only; concurrency /
/// cross-workflow caps stay for a later slice. Retry is fail→Pending under
/// `max_retries`, then [`StepStatus::Failed`] — no backoff / jitter.
#[derive(Debug, Clone)]
pub struct WorkflowEngine {
    recipe: WorkflowRecipe,
    budget: WorkflowBudget,
    checkpoints: HashMap<String, StepCheckpoint>,
    tokens_used: u64,
    wall_ms_used: u64,
    status: WorkflowStatus,
}

impl WorkflowEngine {
    /// Validate recipe and build an idle engine.
    pub fn new(recipe: WorkflowRecipe, budget: WorkflowBudget) -> Result<Self, WorkflowError> {
        validate_recipe(&recipe)?;
        let checkpoints = recipe
            .steps
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    StepCheckpoint {
                        step_id: s.id.clone(),
                        status: StepStatus::Pending,
                        result: None,
                        attempts: 0,
                    },
                )
            })
            .collect();
        Ok(Self {
            recipe,
            budget,
            checkpoints,
            tokens_used: 0,
            wall_ms_used: 0,
            status: WorkflowStatus::Idle,
        })
    }

    /// Feature skeleton: Research → Plan → Tests → Implement → Review → Approval.
    pub fn feature_skeleton_recipe() -> WorkflowRecipe {
        WorkflowRecipe {
            id: "feature".into(),
            name: "Feature".into(),
            steps: vec![
                step("research", "Research", Some("Research"), &[]),
                step("plan", "Plan", Some("Explore"), &["research"]),
                step("tests", "Tests", Some("Build"), &["plan"]),
                step("implement", "Implement", Some("Build"), &["tests"]),
                step("review", "Review", Some("Review"), &["implement"]),
                step("approval", "Approval", None, &["review"]),
            ],
        }
    }

    /// Bug skeleton: Reproduce → Failing regression → Fix → Review.
    pub fn bug_skeleton_recipe() -> WorkflowRecipe {
        WorkflowRecipe {
            id: "bug".into(),
            name: "Bug".into(),
            steps: vec![
                step("reproduce", "Reproduce", Some("Explore"), &[]),
                step(
                    "failing_regression",
                    "Failing regression",
                    Some("Build"),
                    &["reproduce"],
                ),
                step("fix", "Fix", Some("Build"), &["failing_regression"]),
                step("review", "Review", Some("Review"), &["fix"]),
            ],
        }
    }

    /// Refactor skeleton: Baseline tests → Characterization if needed → Refactor
    /// → Validation → Review. "If needed" is recipe text only — no branch logic.
    pub fn refactor_skeleton_recipe() -> WorkflowRecipe {
        WorkflowRecipe {
            id: "refactor".into(),
            name: "Refactor".into(),
            steps: vec![
                step("baseline_tests", "Baseline tests", Some("Build"), &[]),
                step(
                    "characterization",
                    "Characterization if needed",
                    Some("Explore"),
                    &["baseline_tests"],
                ),
                step("refactor", "Refactor", Some("Build"), &["characterization"]),
                step("validation", "Validation", Some("Build"), &["refactor"]),
                step("review", "Review", Some("Review"), &["validation"]),
            ],
        }
    }

    pub fn recipe(&self) -> &WorkflowRecipe {
        &self.recipe
    }

    pub fn status(&self) -> WorkflowStatus {
        self.status
    }

    pub fn tokens_used(&self) -> u64 {
        self.tokens_used
    }

    pub fn wall_ms_used(&self) -> u64 {
        self.wall_ms_used
    }

    pub fn checkpoint(&self, step_id: &str) -> Option<&StepCheckpoint> {
        self.checkpoints.get(step_id)
    }

    pub fn checkpoints(&self) -> impl Iterator<Item = &StepCheckpoint> {
        self.recipe
            .steps
            .iter()
            .filter_map(|s| self.checkpoints.get(&s.id))
    }

    /// Effective retry budget for `step_id` (step override or workflow default).
    pub fn effective_max_retries(&self, step_id: &str) -> Option<u32> {
        self.recipe
            .steps
            .iter()
            .find(|s| s.id == step_id)
            .map(|s| s.max_retries.unwrap_or(self.budget.max_retries))
    }

    /// Mark run as Running (no-op if already running).
    pub fn start(&mut self) -> Result<(), WorkflowError> {
        match self.status {
            WorkflowStatus::Idle => {
                self.status = WorkflowStatus::Running;
                Ok(())
            }
            WorkflowStatus::Running => Ok(()),
            other => Err(WorkflowError::AlreadyFinished(other)),
        }
    }

    /// Next step that is Pending and whose dependencies are Completed,
    /// preferring recipe order.
    pub fn next_ready_step(&self) -> Option<&WorkflowStep> {
        if !matches!(self.status, WorkflowStatus::Running | WorkflowStatus::Idle) {
            return None;
        }
        self.recipe.steps.iter().find(|s| self.is_ready(&s.id))
    }

    /// Whether `step_id` is Pending and all `depends_on` are Completed.
    pub fn is_ready(&self, step_id: &str) -> bool {
        let Some(step) = self.recipe.steps.iter().find(|s| s.id == step_id) else {
            return false;
        };
        let Some(cp) = self.checkpoints.get(step_id) else {
            return false;
        };
        if cp.status != StepStatus::Pending {
            return false;
        }
        step.depends_on.iter().all(|dep| {
            self.checkpoints
                .get(dep)
                .is_some_and(|d| d.status == StepStatus::Completed)
        })
    }

    /// Unfinished dependency ids blocking `step_id` (empty if ready / unknown).
    pub fn blocking_deps(&self, step_id: &str) -> Vec<String> {
        let Some(step) = self.recipe.steps.iter().find(|s| s.id == step_id) else {
            return Vec::new();
        };
        step.depends_on
            .iter()
            .filter(|dep| {
                !self
                    .checkpoints
                    .get(*dep)
                    .is_some_and(|d| d.status == StepStatus::Completed)
            })
            .cloned()
            .collect()
    }

    /// Begin a ready step (sets Running). Checks cancel + budget before start.
    pub fn begin_step(&mut self, step_id: &str) -> Result<(), WorkflowError> {
        self.ensure_runnable()?;
        if let Some(kind) = self.budget_exceeded() {
            self.status = WorkflowStatus::BudgetExhausted;
            return Err(WorkflowError::BudgetExhausted { kind });
        }
        let blocking = self.blocking_deps(step_id);
        if !blocking.is_empty() {
            return Err(WorkflowError::BlockedByDependency(
                step_id.to_string(),
                blocking,
            ));
        }
        let cp = self
            .checkpoints
            .get_mut(step_id)
            .ok_or_else(|| WorkflowError::UnknownStep(step_id.to_string()))?;
        if cp.status != StepStatus::Pending {
            return Err(WorkflowError::NotRunnable(step_id.to_string(), cp.status));
        }
        cp.status = StepStatus::Running;
        Ok(())
    }

    /// Complete the running step, store result, account budget stubs.
    pub fn complete_step(
        &mut self,
        step_id: &str,
        result: impl Into<String>,
        tokens: u64,
        wall_ms: u64,
    ) -> Result<(), WorkflowError> {
        self.ensure_runnable()?;
        {
            let cp = self
                .checkpoints
                .get_mut(step_id)
                .ok_or_else(|| WorkflowError::UnknownStep(step_id.to_string()))?;
            if cp.status != StepStatus::Running {
                return Err(WorkflowError::NotRunnable(step_id.to_string(), cp.status));
            }
            cp.status = StepStatus::Completed;
            cp.result = Some(result.into());
        }
        self.tokens_used = self.tokens_used.saturating_add(tokens);
        self.wall_ms_used = self.wall_ms_used.saturating_add(wall_ms);

        if let Some(kind) = self.budget_exceeded() {
            // Step result is kept; further advances refuse.
            self.status = WorkflowStatus::BudgetExhausted;
            return Err(WorkflowError::BudgetExhausted { kind });
        }

        if self.all_completed() {
            self.status = WorkflowStatus::Completed;
        }
        Ok(())
    }

    /// Record a step failure. Under retry budget → Pending again; else Failed
    /// checkpoint and workflow [`WorkflowStatus::Failed`].
    pub fn fail_step(
        &mut self,
        step_id: &str,
        reason: impl Into<String>,
        tokens: u64,
        wall_ms: u64,
    ) -> Result<(), WorkflowError> {
        self.ensure_runnable()?;
        let max_retries = self
            .effective_max_retries(step_id)
            .ok_or_else(|| WorkflowError::UnknownStep(step_id.to_string()))?;
        let reason = reason.into();
        {
            let cp = self
                .checkpoints
                .get_mut(step_id)
                .ok_or_else(|| WorkflowError::UnknownStep(step_id.to_string()))?;
            if cp.status != StepStatus::Running {
                return Err(WorkflowError::NotRunnable(step_id.to_string(), cp.status));
            }
            cp.attempts = cp.attempts.saturating_add(1);
            cp.result = Some(reason);
            if cp.attempts <= max_retries {
                cp.status = StepStatus::Pending;
            } else {
                cp.status = StepStatus::Failed;
            }
        }
        self.tokens_used = self.tokens_used.saturating_add(tokens);
        self.wall_ms_used = self.wall_ms_used.saturating_add(wall_ms);

        if let Some(kind) = self.budget_exceeded() {
            self.status = WorkflowStatus::BudgetExhausted;
            return Err(WorkflowError::BudgetExhausted { kind });
        }

        let cp = self
            .checkpoints
            .get(step_id)
            .ok_or_else(|| WorkflowError::UnknownStep(step_id.to_string()))?;
        if cp.status == StepStatus::Failed {
            self.status = WorkflowStatus::Failed;
            return Err(WorkflowError::RetriesExhausted(step_id.to_string()));
        }
        Ok(())
    }

    /// Cancel the workflow: running/pending steps become Cancelled.
    pub fn cancel(&mut self) {
        if matches!(
            self.status,
            WorkflowStatus::Completed
                | WorkflowStatus::Cancelled
                | WorkflowStatus::BudgetExhausted
                | WorkflowStatus::Failed
        ) {
            return;
        }
        for cp in self.checkpoints.values_mut() {
            if matches!(cp.status, StepStatus::Pending | StepStatus::Running) {
                cp.status = StepStatus::Cancelled;
            }
        }
        self.status = WorkflowStatus::Cancelled;
    }

    fn ensure_runnable(&self) -> Result<(), WorkflowError> {
        match self.status {
            WorkflowStatus::Idle | WorkflowStatus::Running => Ok(()),
            WorkflowStatus::Cancelled => Err(WorkflowError::Cancelled),
            WorkflowStatus::BudgetExhausted => {
                Err(WorkflowError::BudgetExhausted { kind: "exhausted" })
            }
            WorkflowStatus::Failed => Err(WorkflowError::AlreadyFinished(WorkflowStatus::Failed)),
            other => Err(WorkflowError::AlreadyFinished(other)),
        }
    }

    fn budget_exceeded(&self) -> Option<&'static str> {
        if let Some(max) = self.budget.max_tokens
            && self.tokens_used >= max
        {
            return Some("tokens");
        }
        if let Some(max) = self.budget.max_wall_ms
            && self.wall_ms_used >= max
        {
            return Some("wall_ms");
        }
        None
    }

    fn all_completed(&self) -> bool {
        self.checkpoints
            .values()
            .all(|c| c.status == StepStatus::Completed)
    }
}

fn step(id: &str, name: &str, role: Option<&str>, deps: &[&str]) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        name: name.into(),
        role: role.map(str::to_string),
        depends_on: deps.iter().map(|d| (*d).to_string()).collect(),
        max_retries: None,
    }
}

fn validate_recipe(recipe: &WorkflowRecipe) -> Result<(), WorkflowError> {
    if recipe.steps.is_empty() {
        return Err(WorkflowError::InvalidRecipe("empty steps".into()));
    }
    let mut seen = HashMap::new();
    for (i, s) in recipe.steps.iter().enumerate() {
        if s.id.is_empty() {
            return Err(WorkflowError::InvalidRecipe(format!("empty id at {i}")));
        }
        if seen.insert(s.id.clone(), i).is_some() {
            return Err(WorkflowError::InvalidRecipe(format!(
                "duplicate step id {}",
                s.id
            )));
        }
    }
    for s in &recipe.steps {
        for dep in &s.depends_on {
            if !seen.contains_key(dep) {
                return Err(WorkflowError::InvalidRecipe(format!(
                    "step {} depends on unknown {}",
                    s.id, dep
                )));
            }
            if dep == &s.id {
                return Err(WorkflowError::InvalidRecipe(format!(
                    "step {} depends on itself",
                    s.id
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_step(engine: &mut WorkflowEngine, id: &str, tokens: u64, wall_ms: u64) {
        engine.start().unwrap();
        engine.begin_step(id).unwrap();
        engine
            .complete_step(id, format!("ok:{id}"), tokens, wall_ms)
            .unwrap();
    }

    #[test]
    fn feature_skeleton_happy_path() {
        let recipe = WorkflowEngine::feature_skeleton_recipe();
        assert_eq!(recipe.steps.len(), 6);
        let mut engine = WorkflowEngine::new(recipe, WorkflowBudget::default()).unwrap();
        engine.start().unwrap();

        let order: Vec<String> = engine.recipe().steps.iter().map(|s| s.id.clone()).collect();
        for id in &order {
            assert_eq!(engine.next_ready_step().unwrap().id, id.as_str());
            run_step(&mut engine, id, 10, 5);
            let expected = format!("ok:{id}");
            assert_eq!(
                engine.checkpoint(id).unwrap().result.as_deref(),
                Some(expected.as_str())
            );
        }
        assert_eq!(engine.status(), WorkflowStatus::Completed);
        assert_eq!(engine.tokens_used(), 60);
        assert_eq!(engine.wall_ms_used(), 30);
    }

    #[test]
    fn bug_skeleton_fixture_loads() {
        let recipe = WorkflowEngine::bug_skeleton_recipe();
        assert_eq!(recipe.id, "bug");
        assert_eq!(recipe.steps.len(), 4);
        assert_eq!(recipe.steps[2].depends_on, vec!["failing_regression"]);
        let engine = WorkflowEngine::new(recipe, WorkflowBudget::default()).unwrap();
        assert_eq!(engine.status(), WorkflowStatus::Idle);
        assert!(engine.is_ready("reproduce"));
        assert!(!engine.is_ready("fix"));
    }

    #[test]
    fn refactor_skeleton_happy_path() {
        let recipe = WorkflowEngine::refactor_skeleton_recipe();
        assert_eq!(recipe.id, "refactor");
        assert_eq!(recipe.steps.len(), 5);
        assert_eq!(
            recipe
                .steps
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "baseline_tests",
                "characterization",
                "refactor",
                "validation",
                "review",
            ]
        );
        assert_eq!(recipe.steps[1].name, "Characterization if needed");
        assert_eq!(recipe.steps[2].depends_on, vec!["characterization"]);

        let mut engine = WorkflowEngine::new(recipe, WorkflowBudget::default()).unwrap();
        engine.start().unwrap();
        let order: Vec<String> = engine.recipe().steps.iter().map(|s| s.id.clone()).collect();
        for id in &order {
            assert_eq!(engine.next_ready_step().unwrap().id, id.as_str());
            run_step(&mut engine, id, 1, 1);
        }
        assert_eq!(engine.status(), WorkflowStatus::Completed);
        assert_eq!(engine.tokens_used(), 5);
    }

    #[test]
    fn retry_then_success() {
        let budget = WorkflowBudget {
            max_retries: 1,
            ..WorkflowBudget::default()
        };
        let mut engine =
            WorkflowEngine::new(WorkflowEngine::bug_skeleton_recipe(), budget).unwrap();
        engine.start().unwrap();
        engine.begin_step("reproduce").unwrap();
        engine.fail_step("reproduce", "transient", 2, 1).unwrap();
        let cp = engine.checkpoint("reproduce").unwrap();
        assert_eq!(cp.status, StepStatus::Pending);
        assert_eq!(cp.attempts, 1);
        assert_eq!(cp.result.as_deref(), Some("transient"));
        assert!(engine.is_ready("reproduce"));

        engine.begin_step("reproduce").unwrap();
        engine
            .complete_step("reproduce", "ok:reproduce", 1, 1)
            .unwrap();
        assert_eq!(
            engine.checkpoint("reproduce").unwrap().status,
            StepStatus::Completed
        );
        assert_eq!(engine.status(), WorkflowStatus::Running);
        assert!(engine.is_ready("failing_regression"));
    }

    #[test]
    fn retry_exhausted_marks_failed_checkpoint() {
        let mut recipe = WorkflowEngine::refactor_skeleton_recipe();
        // Per-step override: zero retries → first failure fails permanently.
        recipe.steps[0].max_retries = Some(0);
        let budget = WorkflowBudget {
            max_retries: 5, // workflow default ignored for overridden step
            ..WorkflowBudget::default()
        };
        let mut engine = WorkflowEngine::new(recipe, budget).unwrap();
        engine.start().unwrap();
        assert_eq!(engine.effective_max_retries("baseline_tests"), Some(0));

        engine.begin_step("baseline_tests").unwrap();
        let err = engine
            .fail_step("baseline_tests", "hard fail", 1, 1)
            .unwrap_err();
        assert!(matches!(
            err,
            WorkflowError::RetriesExhausted(ref id) if id == "baseline_tests"
        ));
        let cp = engine.checkpoint("baseline_tests").unwrap();
        assert_eq!(cp.status, StepStatus::Failed);
        assert_eq!(cp.attempts, 1);
        assert_eq!(cp.result.as_deref(), Some("hard fail"));
        assert_eq!(engine.status(), WorkflowStatus::Failed);
        assert!(engine.next_ready_step().is_none());
        assert!(matches!(
            engine.begin_step("characterization"),
            Err(WorkflowError::AlreadyFinished(WorkflowStatus::Failed))
        ));
    }

    #[test]
    fn blocked_by_dependency() {
        let mut engine = WorkflowEngine::new(
            WorkflowEngine::bug_skeleton_recipe(),
            WorkflowBudget::default(),
        )
        .unwrap();
        engine.start().unwrap();
        let err = engine.begin_step("fix").unwrap_err();
        match err {
            WorkflowError::BlockedByDependency(id, deps) => {
                assert_eq!(id, "fix");
                assert!(deps.contains(&"failing_regression".to_string()));
            }
            other => panic!("expected BlockedByDependency, got {other:?}"),
        }
        // Still can run the root step.
        run_step(&mut engine, "reproduce", 1, 1);
        assert!(engine.is_ready("failing_regression"));
    }

    #[test]
    fn cancel_mid_run() {
        let mut engine = WorkflowEngine::new(
            WorkflowEngine::feature_skeleton_recipe(),
            WorkflowBudget::default(),
        )
        .unwrap();
        run_step(&mut engine, "research", 1, 1);
        engine.begin_step("plan").unwrap();
        engine.cancel();

        assert_eq!(engine.status(), WorkflowStatus::Cancelled);
        assert_eq!(
            engine.checkpoint("research").unwrap().status,
            StepStatus::Completed
        );
        assert_eq!(
            engine.checkpoint("plan").unwrap().status,
            StepStatus::Cancelled
        );
        assert_eq!(
            engine.checkpoint("implement").unwrap().status,
            StepStatus::Cancelled
        );
        assert!(matches!(
            engine.begin_step("tests"),
            Err(WorkflowError::Cancelled)
        ));
    }

    #[test]
    fn budget_exhausted_on_tokens() {
        let budget = WorkflowBudget {
            max_tokens: Some(15),
            max_wall_ms: None,
            max_retries: 0,
        };
        let mut engine =
            WorkflowEngine::new(WorkflowEngine::bug_skeleton_recipe(), budget).unwrap();
        run_step(&mut engine, "reproduce", 10, 0);
        engine.begin_step("failing_regression").unwrap();
        let err = engine
            .complete_step("failing_regression", "ok", 10, 0)
            .unwrap_err();
        assert!(matches!(
            err,
            WorkflowError::BudgetExhausted { kind: "tokens" }
        ));
        assert_eq!(engine.status(), WorkflowStatus::BudgetExhausted);
        // Result still recorded for the step that tipped the budget.
        assert_eq!(
            engine.checkpoint("failing_regression").unwrap().status,
            StepStatus::Completed
        );
        assert!(matches!(
            engine.begin_step("fix"),
            Err(WorkflowError::BudgetExhausted { .. })
        ));
    }

    #[test]
    fn budget_exhausted_before_begin() {
        let budget = WorkflowBudget {
            max_tokens: Some(5),
            max_wall_ms: None,
            max_retries: 0,
        };
        let mut engine =
            WorkflowEngine::new(WorkflowEngine::bug_skeleton_recipe(), budget).unwrap();
        // Complete first step over budget.
        engine.start().unwrap();
        engine.begin_step("reproduce").unwrap();
        let err = engine.complete_step("reproduce", "ok", 10, 0).unwrap_err();
        assert!(matches!(
            err,
            WorkflowError::BudgetExhausted { kind: "tokens" }
        ));
        assert!(engine.next_ready_step().is_none());
    }
}
