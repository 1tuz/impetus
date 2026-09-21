//! Global and per-parent concurrency caps for child / subagent runs (TODO P1 §7).
//!
//! Counter/semaphore stub for a future AgentScheduler. Does **not** spawn
//! processes, touch WorkflowEngine step concurrency, or call WorktreeManager.

use std::collections::{HashMap, HashSet};

use thiserror::Error;

/// Default simultaneous child-run slots when no config is supplied.
pub const DEFAULT_CHILD_CONCURRENCY_CAP: usize = 4;

/// Default per-parent child slots when a per-parent cap is enabled.
pub const DEFAULT_PER_PARENT_CHILD_CAP: usize = 2;

/// Harness-side concurrency config — typed, not prompt-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildConcurrencyConfig {
    /// Maximum simultaneous admitted child runs (must be ≥ 1).
    pub global_cap: usize,
    /// Optional fair cap per parent session id (must be ≥ 1 when set).
    pub per_parent_cap: Option<usize>,
}

impl Default for ChildConcurrencyConfig {
    fn default() -> Self {
        Self {
            global_cap: DEFAULT_CHILD_CONCURRENCY_CAP,
            per_parent_cap: None,
        }
    }
}

impl ChildConcurrencyConfig {
    /// Build config with an explicit global cap only.
    pub fn new(global_cap: usize) -> Result<Self, ChildConcurrencyError> {
        Self::with_caps(global_cap, None)
    }

    /// Build config with global and optional per-parent caps.
    pub fn with_caps(
        global_cap: usize,
        per_parent_cap: Option<usize>,
    ) -> Result<Self, ChildConcurrencyError> {
        if global_cap == 0 {
            return Err(ChildConcurrencyError::ZeroCap);
        }
        if per_parent_cap == Some(0) {
            return Err(ChildConcurrencyError::ZeroCap);
        }
        Ok(Self {
            global_cap,
            per_parent_cap,
        })
    }

    /// Fair scheduling preset: global cap plus per-parent ceiling.
    pub fn fair(global_cap: usize, per_parent_cap: usize) -> Result<Self, ChildConcurrencyError> {
        Self::with_caps(global_cap, Some(per_parent_cap))
    }
}

/// Failures when admitting or releasing a child-run slot.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChildConcurrencyError {
    #[error("child concurrency cap must be at least 1")]
    ZeroCap,
    #[error("child run_id must be non-empty")]
    EmptyRunId,
    #[error("parent_id must be non-empty")]
    EmptyParentId,
    #[error("child concurrency cap reached: {active} active, cap {cap}")]
    CapReached { active: usize, cap: usize },
    #[error("parent {parent_id} child cap reached: {active} active, cap {cap}")]
    ParentCapReached {
        parent_id: String,
        active: usize,
        cap: usize,
    },
    #[error("child run {0} is already admitted")]
    AlreadyAdmitted(String),
    #[error("unknown child run {0}")]
    UnknownRun(String),
}

/// In-memory global semaphore for child runs.
///
/// `admit` reserves a slot; `release` frees it on complete or cancel.
/// Live process spawn is out of scope — this only tracks opaque run labels.
#[derive(Debug, Clone)]
pub struct ChildConcurrencyGate {
    cap: usize,
    per_parent_cap: Option<usize>,
    active: HashSet<String>,
    parent_of: HashMap<String, String>,
    active_by_parent: HashMap<String, usize>,
}

impl Default for ChildConcurrencyGate {
    fn default() -> Self {
        Self::from_config(ChildConcurrencyConfig::default())
            .expect("default child concurrency cap is non-zero")
    }
}

impl ChildConcurrencyGate {
    /// Gate with the default global cap ([`DEFAULT_CHILD_CONCURRENCY_CAP`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Gate from a typed config.
    pub fn from_config(config: ChildConcurrencyConfig) -> Result<Self, ChildConcurrencyError> {
        if config.global_cap == 0 || config.per_parent_cap == Some(0) {
            return Err(ChildConcurrencyError::ZeroCap);
        }
        Ok(Self {
            cap: config.global_cap,
            per_parent_cap: config.per_parent_cap,
            active: HashSet::new(),
            parent_of: HashMap::new(),
            active_by_parent: HashMap::new(),
        })
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn per_parent_cap(&self) -> Option<usize> {
        self.per_parent_cap
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    pub fn active_count_for_parent(&self, parent_id: &str) -> usize {
        self.active_by_parent.get(parent_id).copied().unwrap_or(0)
    }

    /// Reserve a slot for `run_id` when under the global cap (no parent scope).
    pub fn admit(&mut self, run_id: impl Into<String>) -> Result<(), ChildConcurrencyError> {
        self.admit_for_parent(run_id, None)
    }

    /// Reserve a slot scoped to `parent_id` when under global and per-parent caps.
    pub fn admit_for_parent(
        &mut self,
        run_id: impl Into<String>,
        parent_id: Option<&str>,
    ) -> Result<(), ChildConcurrencyError> {
        let run_id = run_id.into();
        if run_id.is_empty() {
            return Err(ChildConcurrencyError::EmptyRunId);
        }
        if self.active.contains(&run_id) {
            return Err(ChildConcurrencyError::AlreadyAdmitted(run_id));
        }
        if self.active.len() >= self.cap {
            return Err(ChildConcurrencyError::CapReached {
                active: self.active.len(),
                cap: self.cap,
            });
        }

        let parent_key = match parent_id {
            None => None,
            Some(id) => {
                let trimmed = id.trim();
                if trimmed.is_empty() {
                    return Err(ChildConcurrencyError::EmptyParentId);
                }
                Some(trimmed.to_string())
            }
        };

        if let (Some(parent_id), Some(parent_cap)) = (&parent_key, self.per_parent_cap) {
            let active = self.active_count_for_parent(parent_id);
            if active >= parent_cap {
                return Err(ChildConcurrencyError::ParentCapReached {
                    parent_id: parent_id.clone(),
                    active,
                    cap: parent_cap,
                });
            }
        }

        if let Some(parent_id) = parent_key {
            *self.active_by_parent.entry(parent_id.clone()).or_insert(0) += 1;
            self.parent_of.insert(run_id.clone(), parent_id);
        }

        self.active.insert(run_id);
        Ok(())
    }

    /// Convenience for Explore and other role spawn helpers.
    pub fn admit_child(
        &mut self,
        child_run_id: impl Into<String>,
        parent_id: &str,
    ) -> Result<(), ChildConcurrencyError> {
        self.admit_for_parent(child_run_id, Some(parent_id))
    }

    /// Free the slot for `run_id` (complete or cancel).
    pub fn release(&mut self, run_id: &str) -> Result<(), ChildConcurrencyError> {
        if run_id.is_empty() {
            return Err(ChildConcurrencyError::EmptyRunId);
        }
        if !self.active.remove(run_id) {
            return Err(ChildConcurrencyError::UnknownRun(run_id.to_string()));
        }
        if let Some(parent_id) = self.parent_of.remove(run_id)
            && let Some(count) = self.active_by_parent.get_mut(&parent_id)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.active_by_parent.remove(&parent_id);
            }
        }
        Ok(())
    }

    pub fn is_active(&self, run_id: &str) -> bool {
        self.active.contains(run_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cap_is_small() {
        let gate = ChildConcurrencyGate::new();
        assert_eq!(gate.cap(), DEFAULT_CHILD_CONCURRENCY_CAP);
        assert!(gate.cap() >= 2 && gate.cap() <= 4);
        assert_eq!(gate.active_count(), 0);
        assert_eq!(gate.per_parent_cap(), None);
    }

    #[test]
    fn admit_under_cap() {
        let mut gate =
            ChildConcurrencyGate::from_config(ChildConcurrencyConfig::new(2).unwrap()).unwrap();
        assert!(gate.admit("c1").is_ok());
        assert!(gate.admit("c2").is_ok());
        assert_eq!(gate.active_count(), 2);
        assert!(gate.is_active("c1"));
        assert!(gate.is_active("c2"));
    }

    #[test]
    fn reject_at_cap() {
        let mut gate =
            ChildConcurrencyGate::from_config(ChildConcurrencyConfig::new(2).unwrap()).unwrap();
        gate.admit("c1").unwrap();
        gate.admit("c2").unwrap();
        let err = gate.admit("c3").unwrap_err();
        assert_eq!(err, ChildConcurrencyError::CapReached { active: 2, cap: 2 });
        assert!(!gate.is_active("c3"));
        assert_eq!(gate.active_count(), 2);
    }

    #[test]
    fn free_on_finish_allows_re_admit() {
        let mut gate =
            ChildConcurrencyGate::from_config(ChildConcurrencyConfig::new(1).unwrap()).unwrap();
        gate.admit("c1").unwrap();
        assert_eq!(
            gate.admit("c2").unwrap_err(),
            ChildConcurrencyError::CapReached { active: 1, cap: 1 }
        );
        gate.release("c1").unwrap();
        assert_eq!(gate.active_count(), 0);
        assert!(!gate.is_active("c1"));
        gate.admit("c2").unwrap();
        assert!(gate.is_active("c2"));
    }

    #[test]
    fn release_on_cancel_same_as_complete() {
        let mut gate =
            ChildConcurrencyGate::from_config(ChildConcurrencyConfig::new(1).unwrap()).unwrap();
        gate.admit("child-cancel").unwrap();
        // Cancel path: same release API as complete.
        gate.release("child-cancel").unwrap();
        gate.admit("next").unwrap();
    }

    #[test]
    fn reject_zero_cap_and_empty_id() {
        assert_eq!(
            ChildConcurrencyConfig::new(0).unwrap_err(),
            ChildConcurrencyError::ZeroCap
        );
        let mut gate = ChildConcurrencyGate::new();
        assert_eq!(
            gate.admit("").unwrap_err(),
            ChildConcurrencyError::EmptyRunId
        );
        assert_eq!(
            gate.release("").unwrap_err(),
            ChildConcurrencyError::EmptyRunId
        );
    }

    #[test]
    fn reject_double_admit_and_unknown_release() {
        let mut gate = ChildConcurrencyGate::new();
        gate.admit("c1").unwrap();
        assert_eq!(
            gate.admit("c1").unwrap_err(),
            ChildConcurrencyError::AlreadyAdmitted("c1".into())
        );
        assert_eq!(
            gate.release("missing").unwrap_err(),
            ChildConcurrencyError::UnknownRun("missing".into())
        );
    }

    #[test]
    fn config_is_typed_not_prompt_only() {
        let cfg = ChildConcurrencyConfig {
            global_cap: 3,
            per_parent_cap: None,
        };
        let gate = ChildConcurrencyGate::from_config(cfg).unwrap();
        assert_eq!(gate.cap(), 3);
    }

    #[test]
    fn per_parent_cap_blocks_second_child_for_same_parent() {
        let mut gate =
            ChildConcurrencyGate::from_config(ChildConcurrencyConfig::fair(4, 1).expect("config"))
                .unwrap();
        gate.admit_child("child-a1", "parent-a").unwrap();
        let err = gate.admit_child("child-a2", "parent-a").unwrap_err();
        assert_eq!(
            err,
            ChildConcurrencyError::ParentCapReached {
                parent_id: "parent-a".into(),
                active: 1,
                cap: 1,
            }
        );
        assert!(gate.is_active("child-a1"));
        assert!(!gate.is_active("child-a2"));
    }

    #[test]
    fn per_parent_cap_allows_other_parents_under_global_cap() {
        let mut gate =
            ChildConcurrencyGate::from_config(ChildConcurrencyConfig::fair(4, 1).expect("config"))
                .unwrap();
        gate.admit_child("child-a1", "parent-a").unwrap();
        gate.admit_child("child-b1", "parent-b").unwrap();
        assert_eq!(gate.active_count(), 2);
        assert_eq!(gate.active_count_for_parent("parent-a"), 1);
        assert_eq!(gate.active_count_for_parent("parent-b"), 1);
    }

    #[test]
    fn global_cap_still_applies_with_per_parent_cap() {
        let mut gate =
            ChildConcurrencyGate::from_config(ChildConcurrencyConfig::fair(2, 2).expect("config"))
                .unwrap();
        gate.admit_child("c1", "parent-a").unwrap();
        gate.admit_child("c2", "parent-b").unwrap();
        let err = gate.admit_child("c3", "parent-c").unwrap_err();
        assert_eq!(err, ChildConcurrencyError::CapReached { active: 2, cap: 2 });
    }

    #[test]
    fn release_frees_parent_counter() {
        let mut gate =
            ChildConcurrencyGate::from_config(ChildConcurrencyConfig::fair(4, 1).expect("config"))
                .unwrap();
        gate.admit_child("child-a1", "parent-a").unwrap();
        gate.release("child-a1").unwrap();
        assert_eq!(gate.active_count_for_parent("parent-a"), 0);
        gate.admit_child("child-a2", "parent-a").unwrap();
    }
}
