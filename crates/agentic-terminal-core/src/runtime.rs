use crate::{
    Action, ApprovalEvent, ApprovalState, Event, EventPayload, EventStore, IntentEvent,
    NoticeEvent, PolicyDecision, PolicyEngine, ProjectionError, RunEvent, reduce,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStatus {
    Idle,
    AwaitingApproval,
    Running,
    Completed,
    Failed,
    Cancelled,
    InterruptedUnknown,
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Store(#[from] crate::storage::StoreError),
    #[error(transparent)]
    Projection(#[from] ProjectionError),
    #[error("session `{0}` does not exist")]
    MissingSession(Uuid),
    #[error("approval `{0}` does not exist")]
    MissingApproval(Uuid),
    #[error("approval `{0}` is not pending")]
    ApprovalNotPending(Uuid),
    #[error("action denied by policy: {0}")]
    Denied(String),
    #[error("run `{0}` is not active")]
    InactiveRun(Uuid),
}

pub struct AgentRuntime {
    session_id: Uuid,
    store: Arc<dyn EventStore>,
    policy: PolicyEngine,
}

impl AgentRuntime {
    pub fn new(store: Arc<dyn EventStore>, policy: PolicyEngine) -> Self {
        Self::create(store, policy).expect("create durable session")
    }

    pub fn create(store: Arc<dyn EventStore>, policy: PolicyEngine) -> Result<Self, RuntimeError> {
        Ok(Self {
            session_id: store.create_session()?,
            store,
            policy,
        })
    }

    pub fn attach(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        session_id: Uuid,
    ) -> Result<Self, RuntimeError> {
        let events = store.list(session_id)?;
        if events.is_empty() {
            return Err(RuntimeError::MissingSession(session_id));
        }
        reduce(&events)?;
        Ok(Self {
            session_id,
            store,
            policy,
        })
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn submit_intent(&self, text: impl Into<String>) -> Result<(), RuntimeError> {
        self.record(EventPayload::Intent(IntentEvent { text: text.into() }))
    }

    pub fn start_run(&self) -> Result<Uuid, RuntimeError> {
        let run_id = Uuid::new_v4();
        self.record(EventPayload::Run(RunEvent::Started { run_id }))?;
        Ok(run_id)
    }

    pub fn finish_run(&self, outcome: RunEvent) -> Result<(), RuntimeError> {
        let run_id = match &outcome {
            RunEvent::Started { run_id }
            | RunEvent::Completed { run_id }
            | RunEvent::Failed { run_id, .. }
            | RunEvent::Cancelled { run_id }
            | RunEvent::InterruptedUnknown { run_id } => *run_id,
        };
        if self.projection()?.active_run_id != Some(run_id)
            || matches!(&outcome, RunEvent::Started { .. })
        {
            return Err(RuntimeError::InactiveRun(run_id));
        }
        self.record(EventPayload::Run(outcome))
    }

    pub fn record_agent_chunk(
        &self,
        run_id: Uuid,
        chunk_id: u64,
        text: impl Into<String>,
    ) -> Result<bool, RuntimeError> {
        let projection = self.projection()?;
        if projection.active_run_id != Some(run_id) {
            return Err(RuntimeError::InactiveRun(run_id));
        }
        if projection
            .agent_chunk_ids
            .get(&run_id)
            .is_some_and(|last| *last >= chunk_id)
        {
            return Ok(false);
        }
        self.record(EventPayload::Agent(crate::AgentEvent::Chunk {
            run_id,
            chunk_id,
            text: text.into(),
        }))?;
        Ok(true)
    }

    pub fn request_action(&self, action: Action) -> Result<RuntimeStatus, RuntimeError> {
        match self.policy.evaluate(&action) {
            PolicyDecision::Allow => {
                self.record(EventPayload::Notice(NoticeEvent::PolicyAllowed))?;
                Ok(RuntimeStatus::Idle)
            }
            PolicyDecision::Deny { reason } => {
                self.record(EventPayload::Notice(NoticeEvent::PolicyDenied {
                    reason: reason.clone(),
                }))?;
                Err(RuntimeError::Denied(reason))
            }
            PolicyDecision::NeedsApproval { reason } => {
                let approval = crate::ApprovalRequest::pending(action, reason);
                self.record(EventPayload::Approval(ApprovalEvent::Requested {
                    request: approval,
                }))?;
                Ok(RuntimeStatus::AwaitingApproval)
            }
        }
    }

    pub fn resolve_approval(&self, id: Uuid, accepted: bool) -> Result<(), RuntimeError> {
        let mut approval = self
            .projection()?
            .pending_approvals
            .remove(&id)
            .ok_or(RuntimeError::MissingApproval(id))?;
        if approval.state != ApprovalState::Pending {
            return Err(RuntimeError::ApprovalNotPending(id));
        }
        approval.state = if accepted {
            ApprovalState::Approved
        } else {
            ApprovalState::Rejected
        };
        self.record(EventPayload::Approval(ApprovalEvent::Resolved {
            request: approval,
        }))
    }

    pub fn events(&self) -> Result<Vec<Event>, RuntimeError> {
        Ok(self.store.list(self.session_id)?)
    }

    pub fn status(&self) -> Result<RuntimeStatus, RuntimeError> {
        let projection = self.projection()?;
        if !projection.pending_approvals.is_empty() {
            return Ok(RuntimeStatus::AwaitingApproval);
        }
        if projection.active_run_id.is_some() {
            return Ok(RuntimeStatus::Running);
        }
        Ok(match projection.outcome {
            Some(RunEvent::Completed { .. }) => RuntimeStatus::Completed,
            Some(RunEvent::Failed { .. }) => RuntimeStatus::Failed,
            Some(RunEvent::Cancelled { .. }) => RuntimeStatus::Cancelled,
            Some(RunEvent::InterruptedUnknown { .. }) => RuntimeStatus::InterruptedUnknown,
            Some(RunEvent::Started { .. }) => RuntimeStatus::Running,
            None => RuntimeStatus::Idle,
        })
    }

    pub fn cancel(&self) -> Result<RuntimeStatus, RuntimeError> {
        let projection = self.projection()?;
        let Some(run_id) = projection.active_run_id else {
            return self.status();
        };
        self.finish_run(RunEvent::Cancelled { run_id })?;
        self.status()
    }

    fn projection(&self) -> Result<crate::SessionProjection, RuntimeError> {
        reduce(&self.events()?)?.ok_or(RuntimeError::MissingSession(self.session_id))
    }

    fn record(&self, payload: EventPayload) -> Result<(), RuntimeError> {
        self.store.append_next(self.session_id, payload)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionKind, ActionOrigin, MemoryEventStore, SandboxScope, SqliteEventStore};

    #[test]
    fn write_requires_an_approval_event() {
        let runtime = AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        let status = runtime
            .request_action(Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WriteFile,
                summary: "write config".into(),
                target: Some("Cargo.toml".into()),
            })
            .expect("request approval");
        assert_eq!(status, RuntimeStatus::AwaitingApproval);
        assert!(
            runtime
                .events()
                .expect("events")
                .iter()
                .any(|event| matches!(
                    event.payload,
                    EventPayload::Approval(ApprovalEvent::Requested { .. })
                ))
        );
    }

    #[test]
    fn attach_recovers_pending_approval_and_next_sequence() {
        let test_root = std::env::temp_dir().join(format!(
            "agentic-terminal-runtime-recovery-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&test_root).expect("create isolated test directory");
        let database = test_root.join("events.sqlite3");
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let session_id;
        let approval_id;
        {
            let runtime = AgentRuntime::create(
                SqliteEventStore::open(&database).expect("open store"),
                policy.clone(),
            )
            .expect("create runtime");
            session_id = runtime.session_id();
            runtime
                .submit_intent("explain repository")
                .expect("record intent");
            runtime
                .request_action(Action {
                    origin: ActionOrigin::Agent,
                    kind: ActionKind::WriteFile,
                    summary: "edit config".into(),
                    target: Some("Cargo.toml".into()),
                })
                .expect("request approval");
            approval_id = runtime
                .projection()
                .expect("projection")
                .pending_approvals
                .keys()
                .next()
                .copied()
                .expect("pending approval");
        }
        let recovered = AgentRuntime::attach(
            SqliteEventStore::open(&database).expect("reopen store"),
            policy,
            session_id,
        )
        .expect("attach runtime");
        assert!(
            recovered
                .store
                .list_sessions()
                .expect("list sessions")
                .iter()
                .any(|session| session.id == session_id)
        );
        assert_eq!(
            recovered.status().expect("recovered status"),
            RuntimeStatus::AwaitingApproval
        );
        recovered
            .resolve_approval(approval_id, true)
            .expect("resolve recovered approval");
        let events = recovered.events().expect("recovered events");
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        std::fs::remove_dir_all(test_root).expect("remove isolated test directory");
    }

    #[test]
    fn attached_runtime_recovers_run_status() {
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let runtime = AgentRuntime::create(store.clone(), policy.clone()).expect("create runtime");
        let session_id = runtime.session_id();
        let run_id = runtime.start_run().expect("start run");
        let recovered = AgentRuntime::attach(store, policy, session_id).expect("attach runtime");
        assert_eq!(
            recovered.status().expect("running status"),
            RuntimeStatus::Running
        );
        recovered
            .finish_run(RunEvent::Completed { run_id })
            .expect("finish run");
        assert_eq!(
            recovered.status().expect("completed status"),
            RuntimeStatus::Completed
        );
    }
}
