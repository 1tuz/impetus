use crate::{
    Action, ApprovalRequest, ApprovalState, Event, EventKind, EventStore, PolicyDecision,
    PolicyEngine,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatus {
    Idle,
    AwaitingApproval,
    Running,
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Store(#[from] crate::storage::StoreError),
    #[error("approval `{0}` does not exist")]
    MissingApproval(Uuid),
    #[error("approval `{0}` is not pending")]
    ApprovalNotPending(Uuid),
    #[error("action denied by policy: {0}")]
    Denied(String),
    #[error("runtime state lock poisoned")]
    Poisoned,
}

pub struct AgentRuntime {
    session_id: Uuid,
    store: Arc<dyn EventStore>,
    policy: PolicyEngine,
    next_sequence: Mutex<u64>,
    approvals: Mutex<BTreeMap<Uuid, ApprovalRequest>>,
}

impl AgentRuntime {
    pub fn new(store: Arc<dyn EventStore>, policy: PolicyEngine) -> Self {
        Self {
            session_id: Uuid::new_v4(),
            store,
            policy,
            next_sequence: Mutex::new(1),
            approvals: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn submit_intent(&self, text: impl Into<String>) -> Result<(), RuntimeError> {
        self.record(
            EventKind::UserIntent,
            serde_json::json!({ "text": text.into() }),
        )
    }

    pub fn request_action(&self, action: Action) -> Result<RuntimeStatus, RuntimeError> {
        match self.policy.evaluate(&action) {
            PolicyDecision::Allow => {
                self.record(
                    EventKind::PolicyEvaluated,
                    serde_json::json!({ "decision": "allow", "action": action }),
                )?;
                Ok(RuntimeStatus::Idle)
            }
            PolicyDecision::Deny { reason } => {
                self.record(
                    EventKind::PolicyEvaluated,
                    serde_json::json!({ "decision": "deny", "reason": reason }),
                )?;
                Err(RuntimeError::Denied(reason))
            }
            PolicyDecision::NeedsApproval { reason } => {
                let approval = ApprovalRequest::pending(action, reason);
                self.record(
                    EventKind::ApprovalRequested,
                    serde_json::to_value(&approval).expect("serializable approval"),
                )?;
                self.approvals
                    .lock()
                    .map_err(|_| RuntimeError::Poisoned)?
                    .insert(approval.id, approval);
                Ok(RuntimeStatus::AwaitingApproval)
            }
        }
    }

    pub fn resolve_approval(&self, id: Uuid, accepted: bool) -> Result<(), RuntimeError> {
        let mut approvals = self.approvals.lock().map_err(|_| RuntimeError::Poisoned)?;
        let approval = approvals
            .get_mut(&id)
            .ok_or(RuntimeError::MissingApproval(id))?;
        if approval.state != ApprovalState::Pending {
            return Err(RuntimeError::ApprovalNotPending(id));
        }
        approval.state = if accepted {
            ApprovalState::Approved
        } else {
            ApprovalState::Rejected
        };
        self.record(
            EventKind::ApprovalResolved,
            serde_json::to_value(approval).expect("serializable approval"),
        )
    }

    pub fn events(&self) -> Result<Vec<Event>, RuntimeError> {
        Ok(self.store.list(self.session_id)?)
    }

    fn record(&self, kind: EventKind, body: serde_json::Value) -> Result<(), RuntimeError> {
        let mut sequence = self
            .next_sequence
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?;
        self.store
            .append(&Event::new(self.session_id, *sequence, kind, body))?;
        *sequence += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionKind, ActionOrigin, MemoryEventStore, SandboxScope};

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
            .unwrap();
        assert_eq!(status, RuntimeStatus::AwaitingApproval);
        assert!(
            runtime
                .events()
                .unwrap()
                .iter()
                .any(|event| event.kind == EventKind::ApprovalRequested)
        );
    }
}
