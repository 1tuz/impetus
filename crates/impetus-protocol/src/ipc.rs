use crate::Event;
use crate::types::{
    ApprovalDetail, CheckpointInfo, ChildResult, DurableArtifactMeta, DurableArtifactRef,
    ExecutionMode, GitBranchInfo, GitChangedFile, GitCurrentBranch, GitDiffPayload,
    GitRepositoryState, GitStatusSnapshot, HoverInfo, McpServerStatus, MergeReadyReport,
    ModelProviderStatus, PolicyConfig, PolicyStore, PtySessionState, ReadOnlyToolKind,
    ResolvedInstructions, RuntimeStatus, SessionInfo, SessionModelSelection, SourceLocation,
    SubsystemHealth, ToolOutcome, UserPromptIntent, WorkspaceDirListing, WorkspaceFileContent,
    WorkspaceFileMetadata, WorkspaceSearchResult, WorktreeInfo,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Hard newline-delimited IPC frame cap (daemon + clients).
pub const MAX_IPC_LINE_BYTES: usize = 64 * 1024;

/// Soft budget for Stream/Subscribe `Events` JSON lines (headroom under hard cap).
pub const IPC_EVENTS_FRAME_BUDGET: usize = 60 * 1024;

/// Trim an event batch so serialized [`IpcResponse::Events`] stays ≤
/// [`IPC_EVENTS_FRAME_BUDGET`]. Always keeps at least one event when non-empty
/// (a single max Chunk fits under [`MAX_IPC_LINE_BYTES`]). Cursor resume:
/// callers re-request with `after_sequence = last.sequence`.
pub fn trim_events_to_ipc_frame(session_id: Uuid, mut events: Vec<Event>) -> Vec<Event> {
    if events.len() <= 1 {
        return events;
    }
    if events_frame_len(session_id, &events) <= IPC_EVENTS_FRAME_BUDGET {
        return events;
    }
    let mut lo = 1usize;
    let mut hi = events.len();
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if events_frame_len(session_id, &events[..mid]) <= IPC_EVENTS_FRAME_BUDGET {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    events.truncate(lo.max(1));
    events
}

fn events_frame_len(session_id: Uuid, events: &[Event]) -> usize {
    let response = IpcResponse::Events {
        session_id,
        events: events.to_vec(),
    };
    serde_json::to_vec(&response)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

pub const IPC_VERSION: u16 = 13;
/// Inclusive lower bound for Hello negotiation (clients on 12..13 accepted).
pub const IPC_MIN_SUPPORTED: u16 = 12;

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
    // ApprovalDetail UI contract: schema id impetus.approval_detail.v1
    // (see APPROVAL_DETAIL_SCHEMA_* / docs note in ARCHITECTURE.md).
    "get_approval_detail",
    "context",
    "diagnostics",
    "artifact_upload",
    // Durable artifact read / metadata (MIME persisted on upload).
    "artifact_read",
    // Coding-tools IPC: definition (paths/ranges only; no secrets).
    "coding_definition",
    "execution_mode",
    "reload_policy_config",
    "reload_policy_store",
    "list_child_runs",
    "coding_hover",
    "workflow_control",
    "approval_scope_file_edits",
    "approval_scope_full_auto",
    // Workspace Files IPC (structured list/stat/read/search; path-safe).
    "workspace_list_dir",
    "workspace_stat_file",
    "workspace_read_file",
    "workspace_search_files",
    // Daemon-owned Git (WorktreeManager + system git CLI).
    "git",
    // GetDiff/GetFileDiff include DiffObservation hunks (IPC v11).
    "structured_diff",
    // Daemon-owned PTY (portable-pty). No terminal emulator in harness.
    "pty",
    // Daemon SoT MCP / model catalog (labels + status only; no secrets).
    "list_mcp",
    "list_models",
    "list_providers",
    "session_model",
    // Managed git worktrees lifecycle (IPC v13).
    "worktrees",
    // MCP SoT mutate/reload under $IMPETUS_DATA_DIR/mcp (IPC v13).
    "mcp_manage",
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
        artifact: Option<DurableArtifactRef>,
        /// Prompt | Steer | FollowUp. Absent / default → Prompt (compat).
        #[serde(default)]
        intent: UserPromptIntent,
    },
    Context {
        session_id: Uuid,
    },
    Cancel {
        session_id: Uuid,
    },
    Tool {
        session_id: Uuid,
        kind: ReadOnlyToolKind,
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
    /// Read durable artifact bytes (wire-capped; use range for large bodies).
    ReadArtifact {
        artifact_id: String,
        /// Optional read cap; hard-capped by `MAX_ARTIFACT_UPLOAD_CHUNK_BYTES`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_bytes: Option<usize>,
    },
    /// Durable artifact metadata (id, size, sha256, optional content_type).
    GetArtifactMetadata {
        artifact_id: String,
    },
    /// Read a byte range from a durable artifact (wire-capped per response).
    ReadArtifactRange {
        artifact_id: String,
        start: usize,
        len: usize,
    },
    Diagnostics,
    /// Resolve go-to-definition via optional coding-tools provider.
    /// Paths/ranges only — never secrets or raw credentials.
    GotoDefinition {
        path: std::path::PathBuf,
        line: u32,
        character: u32,
    },
    SetExecutionMode {
        session_id: Uuid,
        mode: ExecutionMode,
    },
    GetExecutionMode {
        session_id: Uuid,
    },
    /// Replace live PolicyConfig overrides without daemon restart.
    /// Supply `path` or `config_json`, not both.
    ReloadPolicyConfig {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<std::path::PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config_json: Option<String>,
    },
    /// Load / replace governed-instruction PolicyStore from path or JSON.
    ReloadPolicyStore {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<std::path::PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store_json: Option<String>,
    },
    /// Export current PolicyStore (empty catalog when unset).
    GetPolicyStore,
    /// List durable child-run results for a parent session.
    ListChildRuns {
        session_id: Uuid,
    },
    /// Load one child-run result by child_id.
    GetChildRun {
        child_id: String,
    },
    /// Resolve hover via optional coding-tools provider.
    Hover {
        path: std::path::PathBuf,
        line: u32,
        character: u32,
    },
    /// Start a workflow recipe for a session (bug|feature|refactor).
    StartWorkflow {
        session_id: Uuid,
        recipe: String,
    },
    /// Cancel session workflow + child admissions.
    CancelWorkflow {
        session_id: Uuid,
    },
    /// Advance one ready workflow step (live child spawn).
    AdvanceWorkflow {
        session_id: Uuid,
    },
    /// List directory entries under a workspace-relative path.
    ListWorkspaceDir {
        session_id: Uuid,
        /// Workspace-relative; empty / `.` = session workspace root.
        #[serde(default)]
        path: std::path::PathBuf,
    },
    /// Stat a workspace-relative path (symlink escape refused).
    StatWorkspaceFile {
        session_id: Uuid,
        path: std::path::PathBuf,
    },
    /// Read a text file under the session workspace (size + binary limits).
    ReadWorkspaceFile {
        session_id: Uuid,
        path: std::path::PathBuf,
        /// Optional read cap; still hard-capped by `MAX_WORKSPACE_FILE_BYTES`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_bytes: Option<usize>,
    },
    /// Case-insensitive text search under a workspace-relative path.
    SearchWorkspaceFiles {
        session_id: Uuid,
        #[serde(default)]
        path: std::path::PathBuf,
        pattern: String,
    },
    /// Git repository state for the session workspace / active worktree.
    GetRepositoryState {
        session_id: Uuid,
    },
    ListBranches {
        session_id: Uuid,
    },
    GetCurrentBranch {
        session_id: Uuid,
    },
    CreateBranch {
        session_id: Uuid,
        name: String,
        /// When true, refuses dirty/conflict worktrees (same safeguards as switch).
        #[serde(default)]
        checkout: bool,
    },
    SwitchBranch {
        session_id: Uuid,
        name: String,
    },
    GitStatus {
        session_id: Uuid,
    },
    ListChangedFiles {
        session_id: Uuid,
    },
    /// Working-tree or staged-vs-`base_ref` unified diff + structured observation.
    GetDiff {
        session_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_ref: Option<String>,
    },
    GetFileDiff {
        session_id: Uuid,
        path: std::path::PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_ref: Option<String>,
    },
    /// Spawn a daemon-owned PTY (policy origin=user). Emits Pty lifecycle on owner session.
    PtyStart {
        session_id: Uuid,
        command: String,
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<std::path::PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cols: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<u16>,
    },
    /// Re-attach a previously detached live PTY owned by `session_id`.
    PtyAttach {
        session_id: Uuid,
        pty_id: u64,
    },
    /// Write bytes to PTY stdin (base64). Caller must own the PTY.
    PtyInput {
        session_id: Uuid,
        pty_id: u64,
        data_b64: String,
    },
    /// Drain bounded output from the PTY ring buffer.
    PtyOutput {
        session_id: Uuid,
        pty_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_bytes: Option<usize>,
    },
    PtyResize {
        session_id: Uuid,
        pty_id: u64,
        cols: u16,
        rows: u16,
    },
    PtyDetach {
        session_id: Uuid,
        pty_id: u64,
    },
    PtyTerminate {
        session_id: Uuid,
        pty_id: u64,
    },
    PtyStatus {
        session_id: Uuid,
        pty_id: u64,
    },
    /// List daemon MCP catalog (labels + connected flag; no env/args/secrets).
    ListMcpServers,
    /// List registered model providers (ids + health; no credentials).
    ListModels,
    /// List provider catalog (same SoT as ListModels; explicit cap for clients).
    ListProviders,
    /// Current session model selection (daemon SoT).
    GetSessionModel {
        session_id: Uuid,
    },
    /// Override session model / reasoning without rebinding ProviderProfile.
    SetSessionModel {
        session_id: Uuid,
        provider_id: String,
        model_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
    },
    /// Create a managed git worktree bound to the session (Build role when `for_build`).
    CreateWorktree {
        session_id: Uuid,
        #[serde(default)]
        for_build: bool,
    },
    ListWorktrees {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<Uuid>,
    },
    GetWorktree {
        worktree_id: String,
    },
    CloseWorktree {
        worktree_id: String,
    },
    ResumeWorktree {
        session_id: Uuid,
    },
    StopWorktree {
        session_id: Uuid,
    },
    CheckWorktreeMergeReady {
        session_id: Uuid,
        base_ref: String,
    },
    MergeWorktree {
        session_id: Uuid,
        base_ref: String,
    },
    /// Reload MCP catalog from daemon SoT (`$IMPETUS_DATA_DIR/mcp/*.json`).
    ReloadMcpServers,
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
        events: Vec<Event>,
    },
    Status {
        session_id: Uuid,
        status: RuntimeStatus,
    },
    Context {
        session_id: Uuid,
        context: ResolvedInstructions,
    },
    ToolResult {
        session_id: Uuid,
        outcome: ToolOutcome,
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
        detail: Box<ApprovalDetail>,
    },
    Diagnostics {
        subsystems: Box<SubsystemHealth>,
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
        artifact: DurableArtifactRef,
    },
    ArtifactUploadAborted {
        upload_id: Uuid,
    },
    /// Durable artifact body (base64); may be truncated to wire chunk cap.
    ArtifactContent {
        artifact_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
        /// Full artifact size on disk (not necessarily returned bytes).
        byte_count: usize,
        /// Bytes returned in this response.
        returned_bytes: usize,
        data_b64: String,
        truncated: bool,
    },
    ArtifactMetadata {
        meta: DurableArtifactMeta,
    },
    /// Byte-range slice of a durable artifact (base64).
    ArtifactRange {
        artifact_id: String,
        start: usize,
        /// Bytes returned in this response.
        returned_bytes: usize,
        data_b64: String,
        /// True when more bytes remain on disk after this response.
        truncated: bool,
    },
    /// Definition locations (workspace paths + ranges only).
    Definition {
        locations: Vec<SourceLocation>,
    },
    ExecutionMode {
        session_id: Uuid,
        mode: ExecutionMode,
    },
    PolicyConfig {
        config: PolicyConfig,
    },
    PolicyStore {
        store: PolicyStore,
    },
    ChildRuns {
        session_id: Uuid,
        runs: Vec<ChildResult>,
    },
    ChildRun {
        run: ChildResult,
    },
    Hover {
        info: Option<HoverInfo>,
    },
    WorkflowStatus {
        session_id: Uuid,
        status: String,
        last_summary: Option<String>,
    },
    WorkspaceDirListing {
        session_id: Uuid,
        listing: WorkspaceDirListing,
    },
    WorkspaceFileStat {
        session_id: Uuid,
        metadata: WorkspaceFileMetadata,
    },
    WorkspaceFileContent {
        session_id: Uuid,
        content: WorkspaceFileContent,
    },
    WorkspaceSearchResult {
        session_id: Uuid,
        result: WorkspaceSearchResult,
    },
    RepositoryState {
        session_id: Uuid,
        state: GitRepositoryState,
    },
    Branches {
        session_id: Uuid,
        branches: Vec<GitBranchInfo>,
    },
    CurrentBranch {
        session_id: Uuid,
        branch: GitCurrentBranch,
    },
    GitStatus {
        session_id: Uuid,
        status: GitStatusSnapshot,
    },
    ChangedFiles {
        session_id: Uuid,
        files: Vec<GitChangedFile>,
    },
    Diff {
        session_id: Uuid,
        diff: GitDiffPayload,
    },
    PtySession {
        pty_id: u64,
        owner_session_id: Uuid,
        state: PtySessionState,
        command: String,
        cols: u16,
        rows: u16,
    },
    PtyOutput {
        pty_id: u64,
        data_b64: String,
        dropped_total: u64,
        eof: bool,
        /// Overflow bytes spilled out of the bounded ring (when store wired).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spill_artifact: Option<DurableArtifactRef>,
    },
    PtyOk {
        pty_id: u64,
    },
    /// Daemon MCP catalog snapshot (empty when no ToolProviderRuntime wired).
    McpServers {
        servers: Vec<McpServerStatus>,
    },
    /// Registered model providers (ids + health labels only).
    Models {
        providers: Vec<ModelProviderStatus>,
    },
    /// Session model selection (daemon SoT).
    SessionModel {
        session_id: Uuid,
        selection: SessionModelSelection,
    },
    Worktree {
        worktree: WorktreeInfo,
    },
    Worktrees {
        worktrees: Vec<WorktreeInfo>,
    },
    WorktreeMergeReady {
        report: MergeReadyReport,
    },
    WorktreeMerged {
        worktree: WorktreeInfo,
    },
    McpReloaded {
        servers: Vec<McpServerStatus>,
    },
    Incompatible {
        supported_version: u16,
        /// Inclusive lower bound the server still accepts.
        #[serde(default = "default_min_supported")]
        min_supported: u16,
        client_version: u16,
        upgrade_recommendation: Option<String>,
    },
    Error {
        code: IpcErrorCode,
        message: String,
    },
}

fn default_min_supported() -> u16 {
    IPC_MIN_SUPPORTED
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
mod sentinel_protocol {
    //! PR-safe IPC protocol lib suite (TODO P2 CI / #315).
    //!
    //! Filter: `cargo test -p impetus-protocol --lib sentinel_protocol`
    //! All named sentinels: `cargo test -p impetus-protocol --lib -- sentinel`
    //!                     + `cargo test -p impetus-core --lib -- sentinel`

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
    fn trim_events_keeps_stream_frame_under_budget() {
        use crate::events::{AgentEvent, EVENT_SCHEMA_VERSION, EventPayload};

        let session_id = Uuid::nil();
        // Match max inline Chunk (16 KiB); 8 of them exceed hard line without trim.
        let body = "y".repeat(16 * 1024);
        let events: Vec<Event> = (1u64..=8)
            .map(|i| Event {
                schema_version: EVENT_SCHEMA_VERSION,
                id: Uuid::from_u128(u128::from(i)),
                session_id,
                sequence: i,
                at_unix_ms: 0,
                payload: EventPayload::Agent(AgentEvent::Chunk {
                    run_id: Uuid::nil(),
                    chunk_id: i,
                    text: body.clone(),
                    artifact: None,
                }),
            })
            .collect();

        let unbounded = serde_json::to_vec(&IpcResponse::Events {
            session_id,
            events: events.clone(),
        })
        .unwrap()
        .len();
        assert!(
            unbounded > MAX_IPC_LINE_BYTES,
            "fixture must exceed hard line cap before trim ({unbounded})"
        );

        let trimmed = trim_events_to_ipc_frame(session_id, events);
        assert!(!trimmed.is_empty() && trimmed.len() < 8);
        let frame = serde_json::to_vec(&IpcResponse::Events {
            session_id,
            events: trimmed,
        })
        .unwrap();
        assert!(
            frame.len() <= IPC_EVENTS_FRAME_BUDGET,
            "trimmed frame {} exceeds budget {IPC_EVENTS_FRAME_BUDGET}",
            frame.len()
        );
        assert!(frame.len() <= MAX_IPC_LINE_BYTES);
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
    fn goto_definition_messages_round_trip() {
        let request = IpcRequest::GotoDefinition {
            path: std::path::PathBuf::from("src/lib.rs"),
            line: 42,
            character: 5,
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&request).unwrap()).unwrap(),
            request
        );
        let response = IpcResponse::Definition {
            locations: vec![SourceLocation::new(
                "src/lib.rs",
                crate::types::SourceRange::new(10, 0, 10, 3),
            )],
        };
        assert_eq!(
            serde_json::from_str::<IpcResponse>(&serde_json::to_string(&response).unwrap())
                .unwrap(),
            response
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
            artifact: DurableArtifactRef {
                id: "abc".into(),
                byte_count: 5,
            },
        };
        assert_eq!(
            serde_json::from_str::<IpcResponse>(&serde_json::to_string(&stored).unwrap()).unwrap(),
            stored
        );

        let read = IpcRequest::ReadArtifact {
            artifact_id: "abc".into(),
            max_bytes: Some(64),
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&read).unwrap()).unwrap(),
            read
        );
        let meta_req = IpcRequest::GetArtifactMetadata {
            artifact_id: "abc".into(),
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&meta_req).unwrap()).unwrap(),
            meta_req
        );
        let range = IpcRequest::ReadArtifactRange {
            artifact_id: "abc".into(),
            start: 0,
            len: 4,
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&range).unwrap()).unwrap(),
            range
        );
        let content = IpcResponse::ArtifactContent {
            artifact_id: "abc".into(),
            content_type: Some("text/plain".into()),
            byte_count: 5,
            returned_bytes: 5,
            data_b64: "aGVsbG8=".into(),
            truncated: false,
        };
        assert_eq!(
            serde_json::from_str::<IpcResponse>(&serde_json::to_string(&content).unwrap()).unwrap(),
            content
        );
    }

    #[test]
    fn execution_mode_messages_round_trip() {
        let session_id = Uuid::new_v4();
        for mode in [
            ExecutionMode::Ask,
            ExecutionMode::Plan,
            ExecutionMode::AcceptEdits,
            ExecutionMode::Auto,
            ExecutionMode::Bypass,
        ] {
            let set = IpcRequest::SetExecutionMode { session_id, mode };
            assert_eq!(
                serde_json::from_str::<IpcRequest>(&serde_json::to_string(&set).unwrap()).unwrap(),
                set
            );
            let get = IpcRequest::GetExecutionMode { session_id };
            assert_eq!(
                serde_json::from_str::<IpcRequest>(&serde_json::to_string(&get).unwrap()).unwrap(),
                get
            );
            let response = IpcResponse::ExecutionMode { session_id, mode };
            assert_eq!(
                serde_json::from_str::<IpcResponse>(&serde_json::to_string(&response).unwrap())
                    .unwrap(),
                response
            );
        }
    }

    #[test]
    fn reload_policy_config_messages_round_trip() {
        let request = IpcRequest::ReloadPolicyConfig {
            path: Some(std::path::PathBuf::from("/tmp/policy.json")),
            config_json: None,
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&request).unwrap()).unwrap(),
            request
        );
        let inline = IpcRequest::ReloadPolicyConfig {
            path: None,
            config_json: Some(r#"{"version":1}"#.into()),
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&inline).unwrap()).unwrap(),
            inline
        );
        let response = IpcResponse::PolicyConfig {
            config: PolicyConfig::parse(r#"{"version":1}"#).expect("config"),
        };
        assert_eq!(
            serde_json::from_str::<IpcResponse>(&serde_json::to_string(&response).unwrap())
                .unwrap(),
            response
        );
    }

    #[test]
    fn hello_includes_execution_mode_capabilities() {
        let request = IpcRequest::Hello {
            version: IPC_VERSION,
            capabilities: vec![
                "execution_mode".into(),
                "approval_scope_file_edits".into(),
                "approval_scope_full_auto".into(),
            ],
        };
        let encoded = serde_json::to_string(&request).expect("encode");
        assert!(encoded.contains("execution_mode"));
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&encoded).expect("decode"),
            request
        );
    }

    #[test]
    fn workspace_files_messages_round_trip() {
        let session_id = Uuid::new_v4();
        let list = IpcRequest::ListWorkspaceDir {
            session_id,
            path: std::path::PathBuf::from("src"),
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&list).unwrap()).unwrap(),
            list
        );
        let read = IpcRequest::ReadWorkspaceFile {
            session_id,
            path: std::path::PathBuf::from("README.md"),
            max_bytes: Some(1024),
        };
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&read).unwrap()).unwrap(),
            read
        );
        let response = IpcResponse::WorkspaceFileContent {
            session_id,
            content: WorkspaceFileContent {
                path: "README.md".into(),
                content: "hi".into(),
                byte_count: 2,
            },
        };
        assert_eq!(
            serde_json::from_str::<IpcResponse>(&serde_json::to_string(&response).unwrap())
                .unwrap(),
            response
        );
        assert!(IPC_CAPABILITIES.contains(&"workspace_list_dir"));
        assert!(IPC_CAPABILITIES.contains(&"workspace_read_file"));
        assert!(IPC_CAPABILITIES.contains(&"workspace_stat_file"));
        assert!(IPC_CAPABILITIES.contains(&"workspace_search_files"));
    }

    #[test]
    fn list_mcp_and_models_messages_round_trip() {
        let list_mcp = IpcRequest::ListMcpServers;
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&list_mcp).unwrap()).unwrap(),
            list_mcp
        );
        let list_models = IpcRequest::ListModels;
        assert_eq!(
            serde_json::from_str::<IpcRequest>(&serde_json::to_string(&list_models).unwrap())
                .unwrap(),
            list_models
        );

        let mcp_response = IpcResponse::McpServers {
            servers: vec![McpServerStatus {
                id: "demo".into(),
                name: "Demo MCP".into(),
                transport: Some(crate::types::McpTransport::Stdio),
                connected: false,
                capabilities: crate::types::McpCapabilities {
                    tools: true,
                    ..crate::types::McpCapabilities::default()
                },
            }],
        };
        assert_eq!(
            serde_json::from_str::<IpcResponse>(&serde_json::to_string(&mcp_response).unwrap())
                .unwrap(),
            mcp_response
        );

        let models_response = IpcResponse::Models {
            providers: vec![ModelProviderStatus::basic(
                "mock",
                "mock-model",
                crate::types::ModelProviderHealthLabel::Healthy,
                true,
            )],
        };
        assert_eq!(
            serde_json::from_str::<IpcResponse>(&serde_json::to_string(&models_response).unwrap())
                .unwrap(),
            models_response
        );
        assert!(IPC_CAPABILITIES.contains(&"list_mcp"));
        assert!(IPC_CAPABILITIES.contains(&"list_models"));
        assert!(IPC_CAPABILITIES.contains(&"structured_diff"));
        assert!(IPC_CAPABILITIES.contains(&"worktrees"));
        assert!(IPC_CAPABILITIES.contains(&"session_model"));
        assert_eq!(IPC_VERSION, 13);
        assert_eq!(IPC_MIN_SUPPORTED, 12);
    }

    #[test]
    fn prompt_intent_discriminant_round_trips_and_defaults() {
        let session_id = Uuid::new_v4();
        for intent in [
            UserPromptIntent::Prompt,
            UserPromptIntent::Steer,
            UserPromptIntent::FollowUp,
        ] {
            let request = IpcRequest::Prompt {
                session_id,
                text: "nudge".into(),
                artifact: None,
                intent,
            };
            let encoded = serde_json::to_string(&request).expect("encode");
            let decoded: IpcRequest = serde_json::from_str(&encoded).expect("decode");
            assert_eq!(decoded, request);
            let needle = match intent {
                UserPromptIntent::Prompt => "\"prompt\"",
                UserPromptIntent::Steer => "\"steer\"",
                UserPromptIntent::FollowUp => "\"follow_up\"",
            };
            assert!(
                encoded.contains(needle),
                "encoded intent discriminant missing in {encoded}"
            );
        }

        // Legacy clients omit intent → Prompt.
        let legacy = format!(
            r#"{{"method":"prompt","params":{{"session_id":"{session_id}","text":"hi"}}}}"#
        );
        let decoded: IpcRequest = serde_json::from_str(&legacy).expect("legacy decode");
        match decoded {
            IpcRequest::Prompt { intent, text, .. } => {
                assert_eq!(intent, UserPromptIntent::Prompt);
                assert_eq!(text, "hi");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
