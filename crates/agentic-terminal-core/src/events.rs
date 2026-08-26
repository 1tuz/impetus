use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub id: Uuid,
    pub session_id: Uuid,
    pub sequence: u64,
    pub at_unix_ms: u64,
    pub kind: EventKind,
    pub body: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    UserIntent,
    PlanCreated,
    PolicyEvaluated,
    ApprovalRequested,
    ApprovalResolved,
    ToolStarted,
    ToolFinished,
    CapabilityLoaded,
    CompactionCreated,
    ForkCreated,
    RuntimeNotice,
}

impl Event {
    pub fn new(session_id: Uuid, sequence: u64, kind: EventKind, body: serde_json::Value) -> Self {
        let at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_millis() as u64;
        Self {
            id: Uuid::new_v4(),
            session_id,
            sequence,
            at_unix_ms,
            kind,
            body,
        }
    }
}
