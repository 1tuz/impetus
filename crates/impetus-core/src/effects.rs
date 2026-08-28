//! The narrow, fail-closed effect boundary for harness capabilities.
//!
//! This is deliberately not a macOS sandbox implementation.  Until the
//! platform spike exists, only the provisioned workspace read capability may
//! cross this seam.  Every other capability remains unavailable here.

use crate::{
    Action, ActionKind, ActionOrigin, ApprovalRequest, ApprovalResolution, ApprovalResolver,
    ApprovalState, PolicyDecision, PolicyEngine, SandboxScope,
};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectCapability {
    WorkspaceRead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedEffect {
    pub origin: ActionOrigin,
    pub capability: EffectCapability,
    pub action: Action,
}

impl NormalizedEffect {
    pub fn workspace_read(
        origin: ActionOrigin,
        summary: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            origin,
            capability: EffectCapability::WorkspaceRead,
            action: Action {
                origin,
                kind: ActionKind::ReadFile,
                summary: summary.into(),
                target: Some(target.into()),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectDecision {
    Allow,
    NeedsApproval { reason: String },
    Deny { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectExecution<T> {
    Executed(T),
    NeedsApproval { reason: String },
    Denied { reason: String },
}

/// A policy-approved-but-not-yet-executable effect. Its normalized action is
/// retained beside the durable approval card so a resolution cannot be reused
/// for another target or intent revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredEffect {
    effect: NormalizedEffect,
    approval: ApprovalRequest,
}

impl DeferredEffect {
    pub fn approval(&self) -> &ApprovalRequest {
        &self.approval
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectAdmission {
    Allow,
    NeedsApproval(DeferredEffect),
    Deny { reason: String },
}

/// A deliberately small sandbox gate.  `Unavailable` is a hard denial: no
/// capability code or execution closure is reached.
#[derive(Debug, Clone)]
pub enum ReadOnlySandbox {
    Provisioned { scope: SandboxScope },
    Unavailable { reason: String },
}

impl ReadOnlySandbox {
    pub fn workspace(root: impl Into<std::path::PathBuf>) -> Self {
        Self::Provisioned {
            scope: SandboxScope::local_workspace(root),
        }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }

    fn admit(&self, effect: &NormalizedEffect) -> Result<(), String> {
        let Self::Provisioned { scope } = self else {
            let Self::Unavailable { reason } = self else {
                unreachable!("all sandbox states are handled")
            };
            return Err(format!("sandbox unavailable: {reason}"));
        };

        if effect.capability != EffectCapability::WorkspaceRead
            || effect.action.kind != ActionKind::ReadFile
        {
            return Err("capability is not available in the read-only sandbox".into());
        }
        if effect.origin != effect.action.origin {
            return Err("effect origin does not match normalized action origin".into());
        }
        let Some(target) = effect.action.target.as_deref() else {
            return Err("read effect has no target".into());
        };
        if !scope.contains(Path::new(target)) {
            return Err("sandbox cannot prove target is inside workspace scope".into());
        }
        Ok(())
    }
}

/// Fixed order for the only currently executable capability:
/// normalized effect -> policy decision -> sandbox -> capability -> execution.
#[derive(Debug, Clone)]
pub struct EffectSeam {
    policy: PolicyEngine,
    sandbox: ReadOnlySandbox,
    #[cfg(test)]
    require_test_approval: bool,
}

impl EffectSeam {
    pub fn workspace_read(root: impl Into<std::path::PathBuf>) -> Self {
        let root = root.into();
        Self {
            policy: PolicyEngine::new(SandboxScope::local_workspace(root.clone())),
            sandbox: ReadOnlySandbox::workspace(root),
            #[cfg(test)]
            require_test_approval: false,
        }
    }

    pub fn with_sandbox(policy: PolicyEngine, sandbox: ReadOnlySandbox) -> Self {
        Self {
            policy,
            sandbox,
            #[cfg(test)]
            require_test_approval: false,
        }
    }

    pub fn decide(&self, effect: &NormalizedEffect) -> EffectDecision {
        match self.policy_decision(&effect.action) {
            PolicyDecision::Deny { reason } => EffectDecision::Deny { reason },
            PolicyDecision::NeedsApproval { reason } => EffectDecision::NeedsApproval { reason },
            PolicyDecision::Allow => match self.sandbox.admit(effect) {
                Ok(()) => EffectDecision::Allow,
                Err(reason) => EffectDecision::Deny { reason },
            },
        }
    }

    /// Request admission without executing the capability. `NeedsApproval`
    /// returns the exact normalized action and durable approval data that must
    /// be presented to a human before any sandbox or capability code runs.
    pub fn request(&self, effect: NormalizedEffect, intent_revision: u64) -> EffectAdmission {
        match self.policy_decision(&effect.action) {
            PolicyDecision::Deny { reason } => EffectAdmission::Deny { reason },
            PolicyDecision::Allow => match self.sandbox.admit(&effect) {
                Ok(()) => EffectAdmission::Allow,
                Err(reason) => EffectAdmission::Deny { reason },
            },
            PolicyDecision::NeedsApproval { reason } => {
                EffectAdmission::NeedsApproval(DeferredEffect {
                    approval: ApprovalRequest::pending(
                        effect.action.clone(),
                        reason,
                        intent_revision,
                    ),
                    effect,
                })
            }
        }
    }

    /// Resume a deferred effect only after a current, exact, user resolution.
    /// Policy is evaluated again, then sandbox admission occurs immediately
    /// before the capability closure. Any changed decision or unavailable
    /// sandbox fails closed without calling `execution`.
    pub fn execute_after_approval<T, E>(
        &self,
        deferred: DeferredEffect,
        resolution: ApprovalResolution,
        current_intent_revision: u64,
        execution: impl FnOnce() -> Result<T, E>,
    ) -> Result<EffectExecution<T>, E> {
        let approval = deferred.approval;
        if approval.state != ApprovalState::Pending
            || resolution.resolver != ApprovalResolver::User
            || !resolution.accepted
            || resolution.id != approval.id
            || resolution.action_fingerprint != approval.action_fingerprint
            || resolution.intent_revision != approval.intent_revision
            || current_intent_revision != approval.intent_revision
            || deferred.effect.action != approval.action
            || approval.action_fingerprint != approval.action.fingerprint()
        {
            return Ok(EffectExecution::Denied {
                reason: "approval is missing, rejected, or stale".into(),
            });
        }

        Ok(match self.policy_decision(&deferred.effect.action) {
            PolicyDecision::NeedsApproval { .. } => match self.sandbox.admit(&deferred.effect) {
                Ok(()) => EffectExecution::Executed(execution()?),
                Err(reason) => EffectExecution::Denied { reason },
            },
            PolicyDecision::Allow => EffectExecution::Denied {
                reason: "policy decision changed before approved execution".into(),
            },
            PolicyDecision::Deny { reason } => EffectExecution::Denied { reason },
        })
    }

    fn policy_decision(&self, action: &Action) -> PolicyDecision {
        #[cfg(test)]
        if self.require_test_approval && action.kind == ActionKind::ReadFile {
            return PolicyDecision::NeedsApproval {
                reason: "test-only approval gate".into(),
            };
        }
        self.policy.evaluate(action)
    }

    #[cfg(test)]
    fn workspace_read_requiring_approval(root: impl Into<std::path::PathBuf>) -> Self {
        let mut seam = Self::workspace_read(root);
        seam.require_test_approval = true;
        seam
    }

    pub fn execute<T, E>(
        &self,
        effect: &NormalizedEffect,
        execution: impl FnOnce() -> Result<T, E>,
    ) -> Result<EffectExecution<T>, E> {
        Ok(match self.decide(effect) {
            EffectDecision::Allow => EffectExecution::Executed(execution()?),
            EffectDecision::NeedsApproval { reason } => EffectExecution::NeedsApproval { reason },
            EffectDecision::Deny { reason } => EffectExecution::Denied { reason },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn workspace() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("effect-seam-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create workspace");
        std::fs::write(root.join("note.txt"), "safe evidence").expect("write fixture");
        root
    }

    #[test]
    fn read_effect_keeps_agent_origin_through_policy_and_execution() {
        let root = workspace();
        let effect = NormalizedEffect::workspace_read(ActionOrigin::Agent, "read note", "note.txt");
        let executed = EffectSeam::workspace_read(&root)
            .execute(&effect, || Ok::<_, ()>("executed"))
            .expect("effect execution");
        assert_eq!(effect.origin, ActionOrigin::Agent);
        assert_eq!(effect.action.origin, ActionOrigin::Agent);
        assert_eq!(executed, EffectExecution::Executed("executed"));
    }

    #[test]
    fn unavailable_sandbox_fails_closed_before_execution() {
        let root = workspace();
        let policy = PolicyEngine::new(SandboxScope::local_workspace(&root));
        let seam =
            EffectSeam::with_sandbox(policy, ReadOnlySandbox::unavailable("not provisioned"));
        let effect = NormalizedEffect::workspace_read(ActionOrigin::User, "read note", "note.txt");
        let outcome = seam
            .execute(&effect, || -> Result<(), ()> { panic!("must not execute") })
            .expect("denial is not an execution error");
        assert!(
            matches!(outcome, EffectExecution::Denied { reason } if reason.contains("sandbox unavailable"))
        );
    }

    #[test]
    fn read_effect_outside_workspace_is_denied_before_execution() {
        let root = workspace();
        let effect =
            NormalizedEffect::workspace_read(ActionOrigin::Agent, "read outside", "/etc/hosts");
        let outcome = EffectSeam::workspace_read(root)
            .execute(&effect, || -> Result<(), ()> { panic!("must not execute") })
            .expect("denial is not an execution error");
        assert!(matches!(outcome, EffectExecution::Denied { .. }));
    }

    #[test]
    fn exact_user_approval_resumes_deferred_read_only_capability() {
        let root = workspace();
        let seam = EffectSeam::workspace_read_requiring_approval(&root);
        let effect = NormalizedEffect::workspace_read(ActionOrigin::Agent, "read note", "note.txt");
        let EffectAdmission::NeedsApproval(deferred) = seam.request(effect, 41) else {
            panic!("test gate must defer effect");
        };
        let resolution = ApprovalResolution::user(deferred.approval(), true);
        let outcome = seam
            .execute_after_approval(deferred, resolution, 41, || Ok::<_, ()>("executed"))
            .expect("approved execution");
        assert_eq!(outcome, EffectExecution::Executed("executed"));
    }

    #[test]
    fn stale_deferred_approval_never_reaches_capability_execution() {
        let root = workspace();
        let seam = EffectSeam::workspace_read_requiring_approval(&root);
        let effect = NormalizedEffect::workspace_read(ActionOrigin::Agent, "read note", "note.txt");
        let EffectAdmission::NeedsApproval(deferred) = seam.request(effect, 41) else {
            panic!("test gate must defer effect");
        };
        let resolution = ApprovalResolution::user(deferred.approval(), true);
        let outcome = seam
            .execute_after_approval(deferred, resolution, 42, || -> Result<(), ()> {
                panic!("stale approval must not execute")
            })
            .expect("stale approval is a denial");
        assert!(matches!(outcome, EffectExecution::Denied { reason } if reason.contains("stale")));
    }
}
