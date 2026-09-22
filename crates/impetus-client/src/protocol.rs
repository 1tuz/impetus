//! Versioned protocol DTOs exposed through the client boundary.
//!
//! Presentation crates should import these types from `impetus-client`, not
//! depend on `impetus-core` directly. The daemon remains the sole authority;
//! these are transport/event data shapes only.
//!
//! Types are owned by the runtime-free `impetus-protocol` crate (no rusqlite /
//! reqwest). Hello remains **exact-match** on [`IPC_VERSION`] until an RFC for
//! `min_supported`. Bump the version when adding Git / Files / PTY / MCP-model
//! catalog / activity capabilities. See ARCHITECTURE.md § IPC compatibility.

pub use impetus_protocol::{
    AgentEvent, ApprovalEvent, ApprovalState, BackendEvent, BudgetEvent, CheckpointInfo,
    ChildEvent, CommandEvent, DiffHunk, DiffObservation, DiffSource, DurableArtifactMeta,
    DurableArtifactRef, Event, EventPayload, ExecutionMode, GitBranchInfo, GitChangeKind,
    GitChangedFile, GitCurrentBranch, GitDiffPayload, GitRepositoryState, GitStatusSnapshot,
    IPC_CAPABILITIES, IPC_EVENTS_FRAME_BUDGET, IPC_VERSION, IpcErrorCode, IpcRequest, IpcResponse,
    MAX_ARTIFACT_UPLOAD_BYTES, MAX_ARTIFACT_UPLOAD_CHUNK_BYTES, MAX_IPC_LINE_BYTES,
    MAX_WORKSPACE_FILE_BYTES, McpCapabilities, McpServerStatus, McpTransport,
    ModelProviderHealthLabel, ModelProviderStatus, NoticeEvent, PolicyConfig, PtyEvent,
    PtySessionState, ReadOnlyToolKind, ResolvedInstructions, RetryEvent, RunEvent, RuntimeStatus,
    SandboxEvent, SandboxPrepareState, SessionEvent, SessionInfo, ToolEvent, ToolOutcome,
    UserPromptIntent, WorkspaceDirEntry, WorkspaceDirListing, WorkspaceFileContent,
    WorkspaceFileMetadata, WorkspaceSearchHit, WorkspaceSearchResult, trim_events_to_ipc_frame,
};
