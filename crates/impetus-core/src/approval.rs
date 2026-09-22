use crate::schema::{SCHEMA_APPROVAL_DETAIL, require_version};

pub use impetus_protocol::{
    APPROVAL_DETAIL_SCHEMA_ID, APPROVAL_DETAIL_SCHEMA_VERSION, ApprovalDetail, ApprovalId,
    ApprovalRequest, ApprovalResolution, ApprovalResolver, ApprovalState, ScopeEstimate,
};

/// Bridge ApprovalDetail validation to the core schema registry.
pub trait ApprovalDetailSchemaExt {
    fn validate_against_registry(&self) -> Result<(), crate::schema::SchemaValidationError>;
}

impl ApprovalDetailSchemaExt for ApprovalDetail {
    fn validate_against_registry(&self) -> Result<(), crate::schema::SchemaValidationError> {
        require_version(&SCHEMA_APPROVAL_DETAIL, self.schema_version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Action, ActionKind, ActionOrigin};
    use uuid::Uuid;

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
            diff_observation: None,
            affected_files: vec!["src/main.rs".into()],
            estimated_scope: Some(ScopeEstimate::Lines(2)),
            attachment_refs: vec![Uuid::nil()],
        }
    }

    #[test]
    fn approval_detail_schema_constants_match_v1() {
        assert_eq!(APPROVAL_DETAIL_SCHEMA_ID, "impetus.approval_detail.v1");
        assert_eq!(APPROVAL_DETAIL_SCHEMA_VERSION, 1);
        assert_eq!(APPROVAL_DETAIL_SCHEMA_ID, SCHEMA_APPROVAL_DETAIL.id);
        assert_eq!(
            APPROVAL_DETAIL_SCHEMA_VERSION,
            SCHEMA_APPROVAL_DETAIL.version
        );
        sample_detail()
            .validate_against_registry()
            .expect("v1 detail ok");
        sample_detail()
            .validate_schema_version()
            .expect("protocol validate ok");
    }
}
