//! Typed subagent roles + structured child-run metadata (TODO P1 §7).
//!
//! Vertical slice: harness-enforceable fields for a future AgentScheduler.
//! Does **not** spawn children, touch WorkflowEngine, or call WorktreeManager.

use std::path::PathBuf;
use thiserror::Error;

/// Explicit subagent role — same four names as WorkflowEngine step hints /
/// [`crate::worktree_manager::AgentWorkRole`]. Not a fifth agent "type".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentRole {
    /// Read-only workspace inspection; no write_roots.
    Explore,
    /// Read-only plus approved web egress; no isolated worktree required.
    Research,
    /// Writes only inside an attached isolated worktree.
    Build,
    /// Read-only review of diffs / tests; no write_roots.
    Review,
}

/// Documented capability intent for a [`SubagentRole`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentCapabilityIntent {
    /// Filesystem / tool use is read-only.
    ReadOnly,
    /// Read-only plus approved web outbound.
    ReadPlusApprovedWeb,
    /// May write only under an isolated worktree's write roots.
    IsolatedWorktreeWrites,
    /// Read-only inspection of diffs and test output.
    ReadOnlyDiffAndTests,
}

impl SubagentRole {
    /// Stable role label (matches WorkflowEngine string hints).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explore => "Explore",
            Self::Research => "Research",
            Self::Build => "Build",
            Self::Review => "Review",
        }
    }

    /// Parse a WorkflowEngine / stored role label. Unknown labels → `None`.
    pub fn parse_label(label: &str) -> Option<Self> {
        match label {
            "Explore" => Some(Self::Explore),
            "Research" => Some(Self::Research),
            "Build" => Some(Self::Build),
            "Review" => Some(Self::Review),
            _ => None,
        }
    }

    /// Capability intent the harness should enforce for this role.
    pub fn capability_intent(self) -> SubagentCapabilityIntent {
        match self {
            Self::Explore => SubagentCapabilityIntent::ReadOnly,
            Self::Research => SubagentCapabilityIntent::ReadPlusApprovedWeb,
            Self::Build => SubagentCapabilityIntent::IsolatedWorktreeWrites,
            Self::Review => SubagentCapabilityIntent::ReadOnlyDiffAndTests,
        }
    }

    /// Whether this role may declare non-empty `write_roots`.
    pub fn may_claim_write_roots(self) -> bool {
        !matches!(self, Self::Explore | Self::Review)
    }

    /// Whether this role requires a worktree id on child metadata.
    pub fn requires_worktree(self) -> bool {
        matches!(self, Self::Build)
    }
}

/// Structured metadata for a child subagent run — not prompt-only.
///
/// Limits are positive integers. `max_time` is wall-clock milliseconds.
/// `worktree` is an optional managed worktree id (label), never a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRunMetadata {
    pub parent_id: String,
    pub cwd: PathBuf,
    pub worktree: Option<String>,
    pub allowed_tools: Vec<String>,
    pub write_roots: Vec<PathBuf>,
    pub max_tokens: u64,
    /// Wall-clock budget in milliseconds.
    pub max_time: u64,
    pub max_depth: u32,
    pub role: SubagentRole,
}

/// Validation failures for [`ChildRunMetadata`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChildRunMetadataError {
    #[error("parent_id must be non-empty")]
    EmptyParentId,
    #[error("max_tokens must be positive")]
    NonPositiveMaxTokens,
    #[error("max_time must be positive")]
    NonPositiveMaxTime,
    #[error("max_depth must be positive")]
    NonPositiveMaxDepth,
    #[error("Build role requires worktree id")]
    BuildMissingWorktree,
    #[error("{role:?} role must not claim write_roots")]
    WriteRootsNotAllowed { role: SubagentRole },
}

impl ChildRunMetadata {
    /// Validate then return `self` (construct fields, then call).
    pub fn try_validated(self) -> Result<Self, ChildRunMetadataError> {
        self.validate()?;
        Ok(self)
    }

    /// Validate fields the harness can enforce without spawning.
    pub fn validate(&self) -> Result<(), ChildRunMetadataError> {
        if self.parent_id.trim().is_empty() {
            return Err(ChildRunMetadataError::EmptyParentId);
        }
        if self.max_tokens == 0 {
            return Err(ChildRunMetadataError::NonPositiveMaxTokens);
        }
        if self.max_time == 0 {
            return Err(ChildRunMetadataError::NonPositiveMaxTime);
        }
        if self.max_depth == 0 {
            return Err(ChildRunMetadataError::NonPositiveMaxDepth);
        }
        if self.role.requires_worktree() {
            let missing = self
                .worktree
                .as_ref()
                .map(|id| id.trim().is_empty())
                .unwrap_or(true);
            if missing {
                return Err(ChildRunMetadataError::BuildMissingWorktree);
            }
        }
        if !self.role.may_claim_write_roots() && !self.write_roots.is_empty() {
            return Err(ChildRunMetadataError::WriteRootsNotAllowed { role: self.role });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_explore() -> ChildRunMetadata {
        ChildRunMetadata {
            parent_id: "parent-1".into(),
            cwd: PathBuf::from("/tmp/ws"),
            worktree: None,
            allowed_tools: vec!["read".into()],
            write_roots: vec![],
            max_tokens: 1_000,
            max_time: 60_000,
            max_depth: 1,
            role: SubagentRole::Explore,
        }
    }

    #[test]
    fn role_capability_intents() {
        assert_eq!(
            SubagentRole::Explore.capability_intent(),
            SubagentCapabilityIntent::ReadOnly
        );
        assert_eq!(
            SubagentRole::Research.capability_intent(),
            SubagentCapabilityIntent::ReadPlusApprovedWeb
        );
        assert_eq!(
            SubagentRole::Build.capability_intent(),
            SubagentCapabilityIntent::IsolatedWorktreeWrites
        );
        assert_eq!(
            SubagentRole::Review.capability_intent(),
            SubagentCapabilityIntent::ReadOnlyDiffAndTests
        );
        assert_eq!(SubagentRole::Build.as_str(), "Build");
    }

    #[test]
    fn try_validated_accepts_valid_explore() {
        let meta = valid_explore().try_validated().unwrap();
        assert_eq!(meta.role, SubagentRole::Explore);
        assert!(meta.worktree.is_none());
    }

    #[test]
    fn build_requires_worktree() {
        let err = ChildRunMetadata {
            role: SubagentRole::Build,
            write_roots: vec![PathBuf::from("/tmp/ws/.worktrees/x")],
            ..valid_explore()
        }
        .try_validated()
        .unwrap_err();
        assert_eq!(err, ChildRunMetadataError::BuildMissingWorktree);

        let ok = ChildRunMetadata {
            role: SubagentRole::Build,
            worktree: Some("wt-abc".into()),
            write_roots: vec![PathBuf::from("/tmp/ws/.worktrees/x")],
            ..valid_explore()
        }
        .try_validated()
        .unwrap();
        assert_eq!(ok.worktree.as_deref(), Some("wt-abc"));
    }

    #[test]
    fn build_rejects_empty_worktree_id() {
        let err = ChildRunMetadata {
            role: SubagentRole::Build,
            worktree: Some("  ".into()),
            ..valid_explore()
        }
        .try_validated()
        .unwrap_err();
        assert_eq!(err, ChildRunMetadataError::BuildMissingWorktree);
    }

    #[test]
    fn explore_and_review_reject_write_roots() {
        let mut explore = valid_explore();
        explore.write_roots = vec![PathBuf::from("/tmp/ws/src")];
        assert_eq!(
            explore.validate(),
            Err(ChildRunMetadataError::WriteRootsNotAllowed {
                role: SubagentRole::Explore
            })
        );

        let mut review = valid_explore();
        review.role = SubagentRole::Review;
        review.write_roots = vec![PathBuf::from("/tmp/ws/src")];
        assert_eq!(
            review.validate(),
            Err(ChildRunMetadataError::WriteRootsNotAllowed {
                role: SubagentRole::Review
            })
        );

        // Empty write_roots OK for Explore / Review.
        assert!(valid_explore().validate().is_ok());
        let mut review_ok = valid_explore();
        review_ok.role = SubagentRole::Review;
        assert!(review_ok.validate().is_ok());
    }

    #[test]
    fn rejects_empty_parent_and_non_positive_limits() {
        let mut meta = valid_explore();
        meta.parent_id = "  ".into();
        assert_eq!(meta.validate(), Err(ChildRunMetadataError::EmptyParentId));

        meta = valid_explore();
        meta.max_tokens = 0;
        assert_eq!(
            meta.validate(),
            Err(ChildRunMetadataError::NonPositiveMaxTokens)
        );

        meta = valid_explore();
        meta.max_time = 0;
        assert_eq!(
            meta.validate(),
            Err(ChildRunMetadataError::NonPositiveMaxTime)
        );

        meta = valid_explore();
        meta.max_depth = 0;
        assert_eq!(
            meta.validate(),
            Err(ChildRunMetadataError::NonPositiveMaxDepth)
        );
    }

    #[test]
    fn research_allows_empty_write_roots_without_worktree() {
        let meta = ChildRunMetadata {
            role: SubagentRole::Research,
            allowed_tools: vec!["web_fetch".into()],
            max_tokens: 50,
            max_time: 30_000,
            max_depth: 2,
            ..valid_explore()
        }
        .try_validated()
        .unwrap();
        assert_eq!(
            meta.role.capability_intent(),
            SubagentCapabilityIntent::ReadPlusApprovedWeb
        );
    }
}
