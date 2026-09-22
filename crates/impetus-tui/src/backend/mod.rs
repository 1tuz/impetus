use anyhow::Result;
use async_trait::async_trait;
use impetus_client::protocol::{
    CheckpointInfo, DurableArtifactMeta, DurableArtifactRef, GitBranchInfo, GitChangedFile,
    GitCurrentBranch, GitDiffPayload, GitStatusSnapshot, WorkspaceDirListing, WorkspaceFileContent,
    WorkspaceSearchResult,
};
use std::path::PathBuf;
use uuid::Uuid;

use crate::model::{ApprovalDetailView, ConnectionInfo, SessionSummary, UiEvent};

pub mod impetus;
pub mod mock;

#[async_trait]
pub trait UiEventStream: Send {
    async fn next_batch(&mut self) -> Result<Vec<UiEvent>>;
}

#[async_trait]
pub trait UiBackend: Send + Sync {
    async fn connection_info(&self) -> Result<ConnectionInfo>;
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>>;
    async fn create_session(&self, workspace_root: PathBuf) -> Result<Uuid>;
    /// Shared-prefix fork at inclusive logical sequence (`session_fork`).
    async fn fork_session(&self, session_id: Uuid, up_to_sequence: u64) -> Result<Uuid>;
    /// Named durable checkpoint (`session_checkpoint`); `sequence` None → tip.
    async fn create_checkpoint(
        &self,
        session_id: Uuid,
        name: String,
        sequence: Option<u64>,
    ) -> Result<CheckpointInfo>;
    async fn list_checkpoints(&self, session_id: Uuid) -> Result<Vec<CheckpointInfo>>;
    /// Restore checkpoint as a new shared-prefix session branch.
    async fn restore_checkpoint(&self, checkpoint_id: Uuid) -> Result<Uuid>;
    async fn resume_session(&self, session_id: Uuid) -> Result<String>;
    async fn send_message(
        &self,
        session_id: Uuid,
        text: String,
        intent: impetus_client::protocol::UserPromptIntent,
        artifact: Option<DurableArtifactRef>,
    ) -> Result<String>;
    /// Chunked upload then prompt with `ArtifactRef` (large paste path).
    async fn send_large_paste(
        &self,
        session_id: Uuid,
        label: String,
        body: Vec<u8>,
        intent: impetus_client::protocol::UserPromptIntent,
    ) -> Result<String>;
    /// Chunked Begin/Append/Finish upload; returns ArtifactRef only (no prompt).
    async fn upload_artifact(
        &self,
        session_id: Uuid,
        bytes: Vec<u8>,
        content_type: Option<String>,
    ) -> Result<DurableArtifactRef>;
    /// Optional metadata (size / MIME) after upload.
    async fn get_artifact_metadata(&self, artifact_id: String) -> Result<DurableArtifactMeta>;
    async fn cancel(&self, session_id: Uuid) -> Result<String>;
    async fn resolve_approval(
        &self,
        session_id: Uuid,
        approval_id: Uuid,
        accepted: bool,
    ) -> Result<()>;
    async fn approval_detail(
        &self,
        session_id: Uuid,
        approval_id: Uuid,
    ) -> Result<ApprovalDetailView>;
    async fn diagnostics(&self) -> Result<String>;
    async fn list_child_runs(&self, session_id: Uuid) -> Result<String>;
    async fn get_execution_mode(
        &self,
        session_id: Uuid,
    ) -> Result<impetus_client::protocol::ExecutionMode>;
    async fn set_execution_mode(
        &self,
        session_id: Uuid,
        mode: impetus_client::protocol::ExecutionMode,
    ) -> Result<impetus_client::protocol::ExecutionMode>;
    /// List workspace directory via harness IPC (no local fs).
    async fn list_workspace_dir(
        &self,
        session_id: Uuid,
        path: PathBuf,
    ) -> Result<WorkspaceDirListing>;
    /// Read workspace text file via harness IPC (no local fs).
    async fn read_workspace_file(
        &self,
        session_id: Uuid,
        path: PathBuf,
        max_bytes: Option<usize>,
    ) -> Result<WorkspaceFileContent>;
    /// Case-insensitive workspace text search via harness IPC.
    async fn search_workspace_files(
        &self,
        session_id: Uuid,
        path: PathBuf,
        pattern: String,
    ) -> Result<WorkspaceSearchResult>;
    /// List git branches via harness IPC (no local git).
    async fn list_branches(&self, session_id: Uuid) -> Result<Vec<GitBranchInfo>>;
    /// Current branch via harness IPC.
    async fn get_current_branch(&self, session_id: Uuid) -> Result<GitCurrentBranch>;
    /// Create branch via harness IPC; `checkout` switches when true.
    async fn create_branch(
        &self,
        session_id: Uuid,
        name: String,
        checkout: bool,
    ) -> Result<GitCurrentBranch>;
    /// Switch branch via harness IPC.
    async fn switch_branch(&self, session_id: Uuid, name: String) -> Result<GitCurrentBranch>;
    /// Working-tree status via harness Git IPC.
    async fn git_status(&self, session_id: Uuid) -> Result<GitStatusSnapshot>;
    /// Changed files via harness Git IPC.
    async fn list_changed_files(&self, session_id: Uuid) -> Result<Vec<GitChangedFile>>;
    /// Whole-tree (or base_ref) unified patch via harness Git IPC.
    async fn get_diff(&self, session_id: Uuid, base_ref: Option<String>) -> Result<GitDiffPayload>;
    /// Per-file unified patch via harness Git IPC.
    async fn get_file_diff(
        &self,
        session_id: Uuid,
        path: PathBuf,
        base_ref: Option<String>,
    ) -> Result<GitDiffPayload>;
    /// Spawn daemon-owned PTY (capability `pty`).
    async fn pty_start(
        &self,
        session_id: Uuid,
        command: String,
        args: Vec<String>,
        working_dir: Option<PathBuf>,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Result<impetus_client::PtySessionView>;
    async fn pty_input(&self, session_id: Uuid, pty_id: u64, data: &[u8]) -> Result<()>;
    async fn pty_output(
        &self,
        session_id: Uuid,
        pty_id: u64,
        max_bytes: Option<usize>,
    ) -> Result<impetus_client::PtyOutputView>;
    async fn pty_resize(&self, session_id: Uuid, pty_id: u64, cols: u16, rows: u16) -> Result<()>;
    async fn pty_detach(&self, session_id: Uuid, pty_id: u64) -> Result<()>;
    async fn subscribe(
        &self,
        session_id: Uuid,
        after_sequence: u64,
    ) -> Result<Box<dyn UiEventStream>>;
}
