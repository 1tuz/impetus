//! In-memory AgentScheduler for WorkflowEngine role steps (TODO P1 §6).
//!
//! Admits role-tagged tasks and records schedule id / result-slot labels.
//! Does **not** spawn agents, open network, or touch secrets.
//!
//! Distinct from the async [`crate::service_contract::AgentScheduler`] trait
//! (replaceable service contract for later live orchestration).

use std::collections::HashMap;

use thiserror::Error;

use crate::subagent_metadata::SubagentRole;

/// Role-tagged task sourced from a WorkflowEngine step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleScheduleTask {
    pub workflow_id: String,
    pub step_id: String,
    /// Parsed step role hint; `None` for role-less steps (e.g. Approval).
    pub role: Option<SubagentRole>,
}

/// Deterministic handles returned when the scheduler admits a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleAdmission {
    pub schedule_id: String,
    pub result_slot: String,
}

/// Lifecycle of one admitted schedule record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleStatus {
    Admitted,
    Completed,
    Cancelled,
}

/// Stored schedule entry (labels only — no secrets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleRecord {
    pub task: RoleScheduleTask,
    pub schedule_id: String,
    pub result_slot: String,
    pub status: ScheduleStatus,
    pub result: Option<String>,
}

/// Failures from schedule / complete / cancel.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AgentSchedulerError {
    #[error("scheduler rejected task for step {step_id}")]
    Rejected { step_id: String },
    #[error("unknown schedule id: {0}")]
    UnknownSchedule(String),
    #[error("schedule {0} is not admitted (status {1:?})")]
    NotAdmitted(String, ScheduleStatus),
    #[error("unknown role label: {0}")]
    UnknownRole(String),
}

/// In-memory / mock AgentScheduler.
///
/// `admit` gate lets tests reject without inventing new agent types.
/// Schedule ids are sequential (`sched-0001`, …) — deterministic in one process.
#[derive(Debug, Clone)]
pub struct InMemoryAgentScheduler {
    next_seq: u64,
    admit: bool,
    records: HashMap<String, ScheduleRecord>,
}

impl Default for InMemoryAgentScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryAgentScheduler {
    /// Empty scheduler that admits every valid task.
    pub fn new() -> Self {
        Self {
            next_seq: 0,
            admit: true,
            records: HashMap::new(),
        }
    }

    /// When `false`, [`Self::schedule`] rejects (test hook).
    pub fn set_admit(&mut self, admit: bool) {
        self.admit = admit;
    }

    pub fn admit_enabled(&self) -> bool {
        self.admit
    }

    pub fn active_count(&self) -> usize {
        self.records
            .values()
            .filter(|r| r.status == ScheduleStatus::Admitted)
            .count()
    }

    pub fn record(&self, schedule_id: &str) -> Option<&ScheduleRecord> {
        self.records.get(schedule_id)
    }

    /// Admit a role-tagged task; returns deterministic schedule id + result slot.
    pub fn schedule(
        &mut self,
        task: RoleScheduleTask,
    ) -> Result<ScheduleAdmission, AgentSchedulerError> {
        if !self.admit {
            return Err(AgentSchedulerError::Rejected {
                step_id: task.step_id,
            });
        }
        self.next_seq = self.next_seq.saturating_add(1);
        let schedule_id = format!("sched-{:04}", self.next_seq);
        let result_slot = format!("slot-{}-{}", task.workflow_id, task.step_id);
        let admission = ScheduleAdmission {
            schedule_id: schedule_id.clone(),
            result_slot: result_slot.clone(),
        };
        self.records.insert(
            schedule_id.clone(),
            ScheduleRecord {
                task,
                schedule_id,
                result_slot,
                status: ScheduleStatus::Admitted,
                result: None,
            },
        );
        Ok(admission)
    }

    /// Record an opaque result into the schedule's result slot.
    pub fn complete(
        &mut self,
        schedule_id: &str,
        result: impl Into<String>,
    ) -> Result<(), AgentSchedulerError> {
        let rec = self
            .records
            .get_mut(schedule_id)
            .ok_or_else(|| AgentSchedulerError::UnknownSchedule(schedule_id.to_string()))?;
        if rec.status != ScheduleStatus::Admitted {
            return Err(AgentSchedulerError::NotAdmitted(
                schedule_id.to_string(),
                rec.status,
            ));
        }
        rec.status = ScheduleStatus::Completed;
        rec.result = Some(result.into());
        Ok(())
    }

    /// Cancel an admitted schedule (no-op complete/cancel after this).
    pub fn cancel(&mut self, schedule_id: &str) -> Result<(), AgentSchedulerError> {
        let rec = self
            .records
            .get_mut(schedule_id)
            .ok_or_else(|| AgentSchedulerError::UnknownSchedule(schedule_id.to_string()))?;
        if rec.status != ScheduleStatus::Admitted {
            return Err(AgentSchedulerError::NotAdmitted(
                schedule_id.to_string(),
                rec.status,
            ));
        }
        rec.status = ScheduleStatus::Cancelled;
        Ok(())
    }
}

/// Parse a WorkflowEngine role hint into [`SubagentRole`].
pub fn parse_step_role(label: &str) -> Result<SubagentRole, AgentSchedulerError> {
    SubagentRole::parse_label(label)
        .ok_or_else(|| AgentSchedulerError::UnknownRole(label.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(step: &str, role: Option<SubagentRole>) -> RoleScheduleTask {
        RoleScheduleTask {
            workflow_id: "bug".into(),
            step_id: step.into(),
            role,
        }
    }

    #[test]
    fn schedule_ids_are_deterministic() {
        let mut sched = InMemoryAgentScheduler::new();
        let a = sched
            .schedule(task("reproduce", Some(SubagentRole::Explore)))
            .unwrap();
        let b = sched
            .schedule(task("fix", Some(SubagentRole::Build)))
            .unwrap();
        assert_eq!(a.schedule_id, "sched-0001");
        assert_eq!(b.schedule_id, "sched-0002");
        assert_eq!(a.result_slot, "slot-bug-reproduce");
        assert_eq!(b.result_slot, "slot-bug-fix");
        assert_eq!(sched.active_count(), 2);
    }

    #[test]
    fn reject_when_admit_disabled() {
        let mut sched = InMemoryAgentScheduler::new();
        sched.set_admit(false);
        let err = sched
            .schedule(task("reproduce", Some(SubagentRole::Explore)))
            .unwrap_err();
        assert!(matches!(
            err,
            AgentSchedulerError::Rejected { step_id } if step_id == "reproduce"
        ));
    }

    #[test]
    fn complete_and_cancel() {
        let mut sched = InMemoryAgentScheduler::new();
        let adm = sched
            .schedule(task("review", Some(SubagentRole::Review)))
            .unwrap();
        sched.complete(&adm.schedule_id, "ok:review").unwrap();
        assert_eq!(
            sched.record(&adm.schedule_id).unwrap().status,
            ScheduleStatus::Completed
        );
        assert_eq!(
            sched.record(&adm.schedule_id).unwrap().result.as_deref(),
            Some("ok:review")
        );
        assert_eq!(sched.active_count(), 0);

        let adm2 = sched.schedule(task("approval", None)).unwrap();
        sched.cancel(&adm2.schedule_id).unwrap();
        assert_eq!(
            sched.record(&adm2.schedule_id).unwrap().status,
            ScheduleStatus::Cancelled
        );
    }

    #[test]
    fn parse_known_roles_only() {
        assert_eq!(parse_step_role("Explore").unwrap(), SubagentRole::Explore);
        assert!(matches!(
            parse_step_role("Swarm"),
            Err(AgentSchedulerError::UnknownRole(_))
        ));
    }
}
