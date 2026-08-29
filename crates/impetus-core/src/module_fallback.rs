use crate::module::{ExecutionSemantics, ModuleKind};
use serde::{Deserialize, Serialize};

/// Fallback policy for a module kind
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackPolicy {
    pub module_kind: ModuleKind,
    pub strategy: FallbackStrategy,
    pub max_retries: u32,
    pub allow_degraded: bool,
}

/// Fallback strategy when module fails
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackStrategy {
    /// Fail immediately, no fallback
    FailFast,
    /// Retry same module
    Retry,
    /// Switch to alternate module of same kind
    Alternate,
    /// Continue with degraded functionality
    Degrade,
    /// Use safe default implementation
    SafeDefault,
}

/// Outcome tracking for retry/fallback decisions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    Success,
    Failure,
    /// Outcome unknown - MUST NOT retry mutating/non-replayable operations
    Unknown,
}

/// Policy for handling UnknownOutcome
#[derive(Debug, Clone)]
pub struct UnknownOutcomePolicy {
    semantics: ExecutionSemantics,
}

impl UnknownOutcomePolicy {
    pub fn new(semantics: ExecutionSemantics) -> Self {
        Self { semantics }
    }

    /// Check if retry is safe given the outcome
    pub fn can_retry(&self, outcome: OperationOutcome) -> bool {
        match outcome {
            OperationOutcome::Success => false, // no need to retry
            OperationOutcome::Failure => true,  // safe to retry
            OperationOutcome::Unknown => {
                // Only retry if operation is idempotent or read-only
                matches!(
                    self.semantics,
                    ExecutionSemantics::ReadOnly | ExecutionSemantics::Idempotent
                )
            }
        }
    }

    /// Check if fallback to alternate backend is safe
    pub fn can_fallback(&self, outcome: OperationOutcome) -> bool {
        match outcome {
            OperationOutcome::Success => false,
            OperationOutcome::Failure => true,
            OperationOutcome::Unknown => {
                // Never fallback for mutating/non-replayable with unknown outcome
                matches!(
                    self.semantics,
                    ExecutionSemantics::ReadOnly | ExecutionSemantics::Idempotent
                )
            }
        }
    }
}

/// Default fallback policies by module kind
impl FallbackPolicy {
    pub fn default_for_kind(kind: ModuleKind) -> Self {
        match kind {
            ModuleKind::AgentLoop => Self {
                module_kind: kind,
                strategy: FallbackStrategy::Retry,
                max_retries: 2,
                allow_degraded: false,
            },
            ModuleKind::Scheduler => Self {
                module_kind: kind,
                strategy: FallbackStrategy::Alternate,
                max_retries: 1,
                allow_degraded: true,
            },
            ModuleKind::ToolProvider => Self {
                module_kind: kind,
                strategy: FallbackStrategy::Degrade,
                max_retries: 1,
                allow_degraded: true,
            },
            ModuleKind::SearchBackend | ModuleKind::BrowserProvider => Self {
                module_kind: kind,
                strategy: FallbackStrategy::Alternate,
                max_retries: 2,
                allow_degraded: false,
            },
            ModuleKind::CredentialResolver => Self {
                module_kind: kind,
                strategy: FallbackStrategy::FailFast,
                max_retries: 0,
                allow_degraded: false,
            },
            ModuleKind::PolicyExtension => Self {
                module_kind: kind,
                strategy: FallbackStrategy::SafeDefault,
                max_retries: 0,
                allow_degraded: false,
            },
            ModuleKind::Custom => Self {
                module_kind: kind,
                strategy: FallbackStrategy::Retry,
                max_retries: 1,
                allow_degraded: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_outcome_prevents_mutating_retry() {
        let policy = UnknownOutcomePolicy::new(ExecutionSemantics::Mutating);

        assert!(policy.can_retry(OperationOutcome::Failure));
        assert!(!policy.can_retry(OperationOutcome::Unknown));
        assert!(!policy.can_fallback(OperationOutcome::Unknown));
    }

    #[test]
    fn unknown_outcome_allows_idempotent_retry() {
        let policy = UnknownOutcomePolicy::new(ExecutionSemantics::Idempotent);

        assert!(policy.can_retry(OperationOutcome::Failure));
        assert!(policy.can_retry(OperationOutcome::Unknown));
        assert!(policy.can_fallback(OperationOutcome::Unknown));
    }

    #[test]
    fn unknown_outcome_allows_readonly_retry() {
        let policy = UnknownOutcomePolicy::new(ExecutionSemantics::ReadOnly);

        assert!(policy.can_retry(OperationOutcome::Unknown));
        assert!(policy.can_fallback(OperationOutcome::Unknown));
    }

    #[test]
    fn non_replayable_blocks_unknown_retry() {
        let policy = UnknownOutcomePolicy::new(ExecutionSemantics::NonReplayable);

        assert!(!policy.can_retry(OperationOutcome::Unknown));
        assert!(!policy.can_fallback(OperationOutcome::Unknown));
    }

    #[test]
    fn credential_resolver_fail_fast() {
        let policy = FallbackPolicy::default_for_kind(ModuleKind::CredentialResolver);
        assert_eq!(policy.strategy, FallbackStrategy::FailFast);
        assert_eq!(policy.max_retries, 0);
    }

    #[test]
    fn search_backend_allows_alternate() {
        let policy = FallbackPolicy::default_for_kind(ModuleKind::SearchBackend);
        assert_eq!(policy.strategy, FallbackStrategy::Alternate);
        assert!(policy.max_retries > 0);
    }
}
