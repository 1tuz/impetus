use anyhow::Result;
use async_trait::async_trait;
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
    async fn resume_session(&self, session_id: Uuid) -> Result<String>;
    async fn send_message(&self, session_id: Uuid, text: String) -> Result<String>;
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
    async fn subscribe(
        &self,
        session_id: Uuid,
        after_sequence: u64,
    ) -> Result<Box<dyn UiEventStream>>;
}
