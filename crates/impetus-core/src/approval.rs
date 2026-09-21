use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::policy::{Action, ActionFingerprint};

pub type ApprovalId = Uuid;

/// Documented schema id for the ApprovalDetail IPC UI contract.
/// Stable string clients may advertise or log alongside `schema_version`.
pub const APPROVAL_DETAIL_SCHEMA_ID: &str = "impetus.approval_detail.v1";

/// Current ApprovalDetail payload schema version (IPC UI contract).
pub const APPROVAL_DETAIL_SCHEMA_VERSION: u16 = 1;

fn default_approval_detail_schema_version() -> u16 {
    APPROVAL_DETAIL_SCHEMA_VERSION
}

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
    /// Capability version for exact approval matching.
    pub capability_version: Option<u32>,
    /// Durable sequence of the user intent that authorized this review.
    pub intent_revision: u64,
    pub reason: String,
    pub state: ApprovalState,
}

/// Extended approval detail for structured client presentation.
/// Provides diff preview, scope estimate, and attachment references.
///
/// IPC UI contract: schema id [`APPROVAL_DETAIL_SCHEMA_ID`], version field
/// [`Self::schema_version`]. Older payloads without the field deserialize as v1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalDetail {
    /// Version of this UI contract. Omitted JSON defaults to v1.
    #[serde(default = "default_approval_detail_schema_version")]
    pub schema_version: u16,
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
        Self::pending_with_version(action, reason, intent_revision, None)
    }

    pub fn pending_with_version(
        action: Action,
        reason: String,
        intent_revision: u64,
        capability_version: Option<u32>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            action_fingerprint: crate::policy::ActionFingerprint::for_action_with_version(
                &action,
                capability_version,
            ),
            action,
            capability_version,
            intent_revision,
            reason,
            state: ApprovalState::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{ActionKind, ActionOrigin};

    fn sample_detail() -> ApprovalDetail {
        let request = ApprovalRequest::pending(
            Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WriteFile,
                summary: "edit fixture".into(),
                target: Some("src/main.rs".into()),
            },
            "policy NeedsApproval".into(),
            1,
        );
        ApprovalDetail {
            schema_version: APPROVAL_DETAIL_SCHEMA_VERSION,
            request,
            diff_preview: Some("--- a\n+++ b\n@@\n-old\n+new".into()),
            affected_files: vec!["src/main.rs".into()],
            estimated_scope: Some(ScopeEstimate::Lines(2)),
            // Attachment refs are opaque UUIDs only — never secrets/tokens.
            attachment_refs: vec![Uuid::nil()],
        }
    }

    #[test]
    fn approval_detail_schema_constants_match_v1() {
        assert_eq!(APPROVAL_DETAIL_SCHEMA_ID, "impetus.approval_detail.v1");
        assert_eq!(APPROVAL_DETAIL_SCHEMA_VERSION, 1);
    }

    #[test]
    fn approval_detail_serde_round_trip() {
        let original = sample_detail();
        let json = serde_json::to_value(&original).expect("serialize");
        assert_eq!(json["schema_version"], 1);
        assert!(json.get("token").is_none());
        assert!(json.get("api_key").is_none());
        let restored: ApprovalDetail = serde_json::from_value(json).expect("deserialize");
        assert_eq!(restored, original);
        assert_eq!(restored.schema_version, APPROVAL_DETAIL_SCHEMA_VERSION);
    }

    #[test]
    fn approval_detail_missing_schema_version_defaults_to_v1() {
        let mut json = serde_json::to_value(sample_detail()).expect("serialize");
        json.as_object_mut()
            .expect("object")
            .remove("schema_version");
        let restored: ApprovalDetail = serde_json::from_value(json).expect("deserialize");
        assert_eq!(restored.schema_version, APPROVAL_DETAIL_SCHEMA_VERSION);
        assert_eq!(restored.affected_files, vec!["src/main.rs".to_string()]);
    }

    #[test]
    fn scope_estimate_serde_round_trip() {
        for estimate in [
            ScopeEstimate::Lines(10),
            ScopeEstimate::Bytes(1024),
            ScopeEstimate::Operations(3),
        ] {
            let json = serde_json::to_value(&estimate).expect("serialize");
            let restored: ScopeEstimate = serde_json::from_value(json).expect("deserialize");
            assert_eq!(restored, estimate);
        }
    }
}
