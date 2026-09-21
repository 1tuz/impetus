use crate::RuntimeStatus;
use crate::storage::{CheckpointInfo, SessionInfo};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const IPC_VERSION: u16 = 5;
pub const IPC_CAPABILITIES: &[&str] = &[
    "session_create",
    "session_attach",
    "session_list",
    "session_fork",
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
    "artifact_upload",
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
    ForkSession {
        session_id: Uuid,
        up_to_sequence: u64,
    },
    CreateCheckpoint {
        session_id: Uuid,
        name: String,
        /// Logical sequence; when omitted, uses current session head.
        sequence: Option<u64>,
    },
    ListCheckpoints {
        session_id: Uuid,
    },
    RestoreCheckpoint {
        checkpoint_id: Uuid,
    },
    Stream {
        session_id: Uuid,
        after_sequence: u64,
    },
    Prompt {
        session_id: Uuid,
        text: String,
        /// Optional durable paste/attachment ref; raw body must not be in `text`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact: Option<crate::DurableArtifactRef>,
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
    /// Start a chunked upload into the durable artifact store.
    BeginArtifactUpload {
        session_id: Uuid,
        /// Optional declared total size; rejected early if over the hard cap.
        declared_bytes: Option<usize>,
        content_type: Option<String>,
    },
    /// Append one base64-encoded chunk. `seq` must be 0, 1, 2, …
    AppendArtifactChunk {
        upload_id: Uuid,
        seq: u64,
        data_b64: String,
    },
    /// Commit assembled bytes into `ArtifactStore` and return `ArtifactRef`.
    FinishArtifactUpload {
        upload_id: Uuid,
    },
    /// Drop a pending upload without storing.
    AbortArtifactUpload {
        upload_id: Uuid,
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
        sessions: Vec<SessionInfo>,
    },
    Checkpoint {
        checkpoint: CheckpointInfo,
    },
    Checkpoints {
        checkpoints: Vec<CheckpointInfo>,
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
    ArtifactUploadBegun {
        upload_id: Uuid,
        max_bytes: usize,
        max_chunk_bytes: usize,
    },
    ArtifactChunkAccepted {
        upload_id: Uuid,
        bytes_received: usize,
        next_seq: u64,
    },
    /// Durable content-addressed reference; body never enters this response.
    ArtifactStored {
        artifact: crate::DurableArtifactRef,
    },
    ArtifactUploadAborted {
        upload_id: Uuid,
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
    fn fork_and_checkpoint_messages_round_trip() {
        let session_id = Uuid::new_v4();
        let fork = IpcRequest::ForkSession {
            session_id,
            up_to_sequence: 3,
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&fork).unwrap()).unwrap(),
            fork
        );
        let create = IpcRequest::CreateCheckpoint {
            session_id,
            name: "stable".into(),
            sequence: Some(2),
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&create).unwrap()).unwrap(),
            create
        );
    }

    #[test]
    fn artifact_upload_messages_round_trip() {
        let session_id = Uuid::new_v4();
        let upload_id = Uuid::new_v4();
        let begin = IpcRequest::BeginArtifactUpload {
            session_id,
            declared_bytes: Some(1024),
            content_type: Some("text/plain".into()),
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&begin).unwrap()).unwrap(),
            begin
        );
        let append = IpcRequest::AppendArtifactChunk {
            upload_id,
            seq: 0,
            data_b64: "aGVsbG8=".into(),
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&append).unwrap()).unwrap(),
            append
        );
        let stored = IpcResponse::ArtifactStored {
            artifact: crate::DurableArtifactRef {
                id: "abc".into(),
                byte_count: 5,
            },
        };
        assert_eq!(
            serde_json::from_str::<IpcResponse>(&serde_json::to_string(&stored).unwrap()).unwrap(),
            stored
        );
    }
}
