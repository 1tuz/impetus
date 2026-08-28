use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::policy::{Action, ActionFingerprint};

pub type ApprovalId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ApprovalState {
    Pending,
    Approved,
    Rejected,
}

/// The only authority that can resolve a pending approval. An agent/backend
/// may request an action, but may never approve its own request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalResolver {
    User,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalResolution {
    pub id: ApprovalId,
    pub action_fingerprint: ActionFingerprint,
    pub intent_revision: u64,
    pub accepted: bool,
    pub resolver: ApprovalResolver,
}

impl ApprovalResolution {
    pub fn user(request: &ApprovalRequest, accepted: bool) -> Self {
        Self {
            id: request.id,
            action_fingerprint: request.action_fingerprint.clone(),
            intent_revision: request.intent_revision,
            accepted,
            resolver: ApprovalResolver::User,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub action: Action,
    pub action_fingerprint: ActionFingerprint,
    /// Durable sequence of the user intent that authorized this review.
    pub intent_revision: u64,
    pub reason: String,
    pub state: ApprovalState,
}

/// Extended approval detail for structured client presentation.
/// Provides diff preview, scope estimate, and attachment references.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalDetail {
    pub request: ApprovalRequest,
    /// Diff preview for write actions (unified format, max 50 lines).
    pub diff_preview: Option<String>,
    /// Affected file paths.
    pub affected_files: Vec<String>,
    /// Estimated scope: line count, byte size, or operation count.
    pub estimated_scope: Option<ScopeEstimate>,
    /// Artifact/output attachment IDs for full content retrieval.
    pub attachment_refs: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ScopeEstimate {
    Lines(u32),
    Bytes(u64),
    Operations(u32),
}

impl ApprovalRequest {
    pub fn pending(action: Action, reason: String, intent_revision: u64) -> Self {
        Self {
            id: Uuid::new_v4(),
            action_fingerprint: action.fingerprint(),
            action,
            intent_revision,
            reason,
            state: ApprovalState::Pending,
        }
    }
}
