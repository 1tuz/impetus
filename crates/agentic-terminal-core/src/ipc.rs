use crate::RuntimeStatus;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Compact session DAG node for TUI tree view. Derived from durable events;
/// the harnessd remains the only owner of structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DagNode {
    pub id: Uuid,
    pub parent: Option<Uuid>,
    pub label: String,
    pub status: String,
    pub is_current: bool,
}

/// One recoverable checkpoint, surfaced as an event in the transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointView {
    pub id: Uuid,
    pub sequence: u64,
    pub label: String,
}

/// Token/cost telemetry snapshot for the compact status bar and `/usage`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub sent_tokens: u64,
    pub cached_tokens: u64,
    pub model_calls: u64,
    pub estimated_cost_usd: f64,
}

/// A single risk item shown in the Risk Gate UX (scoped, never generic).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RiskItem {
    pub action: String,
    pub target: String,
    pub scope: String,
    pub reason: String,
}

/// Current risk state: what is awaiting decision and what was denied.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RiskState {
    pub pending: Vec<RiskItem>,
    pub denied: Vec<RiskItem>,
}

/// Lightweight profile descriptor; the profile text itself is never shipped
/// across the boundary, only its identity and inheritance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileInfo {
    pub name: String,
    pub source: String,
    pub inherits: Option<String>,
    pub active: bool,
}

/// Failure-learning health for `/learning`. Counts only; no rule text crosses.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LearningState {
    pub lessons: usize,
    pub candidates: usize,
    pub rejected: usize,
}

/// One agent/subagent entry for the swarm view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInfo {
    pub id: Uuid,
    pub role: String,
    pub task: String,
    pub status: String,
}

pub const IPC_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum IpcRequest {
    Hello {
        version: u16,
        capabilities: Vec<String>,
    },
    CreateSession,
    Attach {
        session_id: Uuid,
    },
    ListSessions,
    Stream {
        session_id: Uuid,
        after_sequence: u64,
    },
    Prompt {
        session_id: Uuid,
        text: String,
    },
    Cancel {
        session_id: Uuid,
    },
    Tool {
        session_id: Uuid,
        kind: crate::ReadOnlyToolKind,
        target: String,
        pattern: Option<String>,
    },
    Subscribe {
        session_id: Uuid,
        after_sequence: u64,
    },
    Fork {
        session_id: Uuid,
        label: String,
    },
    ListAgents {
        session_id: Uuid,
    },
    GetDag {
        session_id: Uuid,
    },
    GetCheckpoints {
        session_id: Uuid,
    },
    Revert {
        session_id: Uuid,
        checkpoint_id: Uuid,
    },
    GetUsage {
        session_id: Uuid,
    },
    GetRiskState {
        session_id: Uuid,
    },
    GetProfiles {
        session_id: Uuid,
    },
    SetProfile {
        session_id: Uuid,
        name: String,
    },
    GetLearningState {
        session_id: Uuid,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum IpcResponse {
    Hello {
        version: u16,
        capabilities: Vec<String>,
    },
    Session {
        session_id: Uuid,
        status: RuntimeStatus,
    },
    Sessions {
        sessions: Vec<Uuid>,
    },
    Events {
        session_id: Uuid,
        events: Vec<crate::Event>,
    },
    Status {
        session_id: Uuid,
        status: RuntimeStatus,
    },
    ToolResult {
        session_id: Uuid,
        outcome: crate::ToolOutcome,
    },
    Subscribed {
        session_id: Uuid,
    },
    Forked {
        session_id: Uuid,
        new_session_id: Uuid,
    },
    Agents {
        session_id: Uuid,
        agents: Vec<AgentInfo>,
    },
    Dag {
        session_id: Uuid,
        nodes: Vec<DagNode>,
    },
    Checkpoints {
        session_id: Uuid,
        checkpoints: Vec<CheckpointView>,
    },
    Reverted {
        session_id: Uuid,
        checkpoint_id: Uuid,
    },
    Usage {
        session_id: Uuid,
        usage: Usage,
    },
    Risk {
        session_id: Uuid,
        risk: RiskState,
    },
    Profiles {
        session_id: Uuid,
        profiles: Vec<ProfileInfo>,
    },
    ProfileSet {
        session_id: Uuid,
        name: String,
    },
    Learning {
        session_id: Uuid,
        learning: LearningState,
    },
    Incompatible {
        supported_version: u16,
    },
    Error {
        code: IpcErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IpcErrorCode {
    InvalidRequest,
    MissingSession,
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_messages_round_trip() {
        let request = IpcRequest::Hello {
            version: IPC_VERSION,
            capabilities: vec!["session_status".into()],
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(
                &serde_json::to_string(&request).expect("encode request")
            )
            .expect("decode request"),
            request
        );
    }
}
