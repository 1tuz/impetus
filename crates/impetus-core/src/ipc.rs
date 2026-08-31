use crate::RuntimeStatus;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const IPC_VERSION: u16 = 4;
pub const IPC_CAPABILITIES: &[&str] = &[
    "session_create",
    "session_attach",
    "session_list",
    "session_branch",
    "session_checkpoint",
    "event_stream",
    "prompt",
    "cancel",
    "tool",
    "subscribe",
    "resolve_approval",
    "get_attachment",
    "get_approval_detail",
    "context",
    "diagnostics",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum IpcRequest {
    Hello {
        version: u16,
        capabilities: Vec<String>,
    },
    CreateSession {
        workspace_root: std::path::PathBuf,
    },
    Attach {
        session_id: Uuid,
    },
    ListSessions,
    ListSessionBranches,
    ForkSession {
        session_id: Uuid,
        up_to_sequence: u64,
        branch_name: Option<String>,
    },
    CreateCheckpoint {
        session_id: Uuid,
        name: String,
        sequence: Option<u64>,
    },
    ListCheckpoints {
        session_id: Uuid,
    },
    RestoreCheckpoint {
        checkpoint_id: Uuid,
        branch_name: Option<String>,
    },
    Stream {
        session_id: Uuid,
        after_sequence: u64,
    },
    Prompt {
        session_id: Uuid,
        text: String,
    },
    Context {
        session_id: Uuid,
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
    ResolveApproval {
        session_id: Uuid,
        approval_id: Uuid,
        accepted: bool,
    },
    GetAttachment {
        session_id: Uuid,
        attachment_id: Uuid,
    },
    GetApprovalDetail {
        session_id: Uuid,
        approval_id: Uuid,
    },
    Diagnostics,
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
    SessionBranch {
        session: crate::SessionInfo,
    },
    SessionBranches {
        sessions: Vec<crate::SessionInfo>,
    },
    Checkpoint {
        checkpoint: crate::SessionCheckpoint,
    },
    Checkpoints {
        session_id: Uuid,
        checkpoints: Vec<crate::SessionCheckpoint>,
    },
    Events {
        session_id: Uuid,
        events: Vec<crate::Event>,
    },
    Status {
        session_id: Uuid,
        status: RuntimeStatus,
    },
    Context {
        session_id: Uuid,
        context: crate::ResolvedInstructions,
    },
    ToolResult {
        session_id: Uuid,
        outcome: crate::ToolOutcome,
    },
    Subscribed {
        session_id: Uuid,
    },
    ApprovalResolved {
        session_id: Uuid,
        approval_id: Uuid,
    },
    Attachment {
        session_id: Uuid,
        attachment_id: Uuid,
        content_type: String,
        content: Vec<u8>,
    },
    ApprovalDetail {
        session_id: Uuid,
        detail: Box<crate::ApprovalDetail>,
    },
    Diagnostics {
        subsystems: Box<crate::SubsystemHealth>,
    },
    Incompatible {
        supported_version: u16,
        client_version: u16,
        upgrade_recommendation: Option<String>,
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
    Unavailable,
    Conflict,
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_messages_round_trip() {
        let request = IpcRequest::Hello {
            version: IPC_VERSION,
            capabilities: vec!["session_attach".into()],
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(
                &serde_json::to_string(&request).expect("encode request")
            )
            .expect("decode request"),
            request
        );
    }

    #[test]
    fn context_messages_round_trip() {
        let request = IpcRequest::Context {
            session_id: Uuid::new_v4(),
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&request).unwrap()).unwrap(),
            request
        );
    }

    #[test]
    fn session_branch_and_checkpoint_messages_round_trip() {
        let session_id = Uuid::new_v4();
        let requests = [
            IpcRequest::ForkSession {
                session_id,
                up_to_sequence: 12,
                branch_name: Some("experiment".into()),
            },
            IpcRequest::CreateCheckpoint {
                session_id,
                name: "before refactor".into(),
                sequence: Some(12),
            },
            IpcRequest::ListCheckpoints { session_id },
            IpcRequest::RestoreCheckpoint {
                checkpoint_id: Uuid::new_v4(),
                branch_name: Some("retry".into()),
            },
            IpcRequest::ListSessionBranches,
        ];
        for request in requests {
            assert_eq!(
                serde_json::from_str::<IpcRequest>(&serde_json::to_string(&request).unwrap())
                    .unwrap(),
                request
            );
        }
    }
}
