//! Global concurrency cap for child / subagent runs (TODO P1 §7).
//!
//! Counter/semaphore stub for a future AgentScheduler. Does **not** spawn
//! processes, touch WorkflowEngine step concurrency, or call WorktreeManager.
//!
//! YAGNI: global cap only — per-parent limits stay out of this slice.

use std::collections::HashSet;

use thiserror::Error;

/// Default simultaneous child-run slots when no config is supplied.
pub const DEFAULT_CHILD_CONCURRENCY_CAP: usize = 4;

/// Harness-side concurrency config — typed, not prompt-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildConcurrencyConfig {
    /// Maximum simultaneous admitted child runs (must be ≥ 1).
    pub global_cap: usize,
}

impl Default for ChildConcurrencyConfig {
    fn default() -> Self {
        Self {
            global_cap: DEFAULT_CHILD_CONCURRENCY_CAP,
        }
    }
}

impl ChildConcurrencyConfig {
    /// Build config with an explicit global cap.
    pub fn new(global_cap: usize) -> Result<Self, ChildConcurrencyError> {
        if global_cap == 0 {
            return Err(ChildConcurrencyError::ZeroCap);
        }
        Ok(Self { global_cap })
    }
}

/// Failures when admitting or releasing a child-run slot.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChildConcurrencyError {
    #[error("child concurrency cap must be at least 1")]
    ZeroCap,
    #[error("child run_id must be non-empty")]
    EmptyRunId,
    #[error("child concurrency cap reached: {active} active, cap {cap}")]
    CapReached { active: usize, cap: usize },
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
    active: HashSet<String>,
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
        if config.global_cap == 0 {
            return Err(ChildConcurrencyError::ZeroCap);
        }
        Ok(Self {
            cap: config.global_cap,
            active: HashSet::new(),
        })
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Reserve a slot for `run_id` when under the global cap.
    pub fn admit(&mut self, run_id: impl Into<String>) -> Result<(), ChildConcurrencyError> {
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
        self.active.insert(run_id);
        Ok(())
    }

    /// Free the slot for `run_id` (complete or cancel).
    pub fn release(&mut self, run_id: &str) -> Result<(), ChildConcurrencyError> {
        if run_id.is_empty() {
            return Err(ChildConcurrencyError::EmptyRunId);
        }
        if !self.active.remove(run_id) {
            return Err(ChildConcurrencyError::UnknownRun(run_id.to_string()));
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
        let cfg = ChildConcurrencyConfig { global_cap: 3 };
        let gate = ChildConcurrencyGate::from_config(cfg).unwrap();
        assert_eq!(gate.cap(), 3);
    }
}
