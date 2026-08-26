use crate::RuntimeStatus;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
