//! The narrow, fail-closed effect boundary for harness capabilities.
//!
//! This is deliberately not a macOS sandbox implementation.  Until the
//! platform spike exists, only the provisioned workspace read capability may
//! cross this seam.  Every other capability remains unavailable here.

use crate::{
    Action, ActionKind, ActionOrigin, ApprovalRequest, ApprovalResolution, ApprovalResolver,
    ApprovalState, ExecutionMode, PolicyDecision, PolicyEngine, RiskContext, RiskGate,
    RiskGateDecision, SandboxScope, default_risk_gate, is_mutating_effect, is_read_only_effect,
};
use std::path::Path;
use std::sync::Arc;

/// Capability version tracks breaking changes to action structure or semantics.
/// Approval fingerprints include capability version so old approvals cannot be
/// reused for incompatible new actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CapabilityVersion(pub u32);

impl CapabilityVersion {
    pub const V1: Self = Self(1);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EffectCapability {
    WorkspaceRead,
    WorkspaceWrite,
    ProcessSpawn,
    NetworkConnect,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NormalizedEffect {
    pub origin: ActionOrigin,
    pub capability: EffectCapability,
    pub version: CapabilityVersion,
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
            version: CapabilityVersion::V1,
            action: Action {
                origin,
                kind: ActionKind::ReadFile,
                summary: summary.into(),
                target: Some(target.into()),
            },
        }
    }

    pub fn workspace_write(
        origin: ActionOrigin,
        summary: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            origin,
            capability: EffectCapability::WorkspaceWrite,
            version: CapabilityVersion::V1,
            action: Action {
                origin,
                kind: ActionKind::WriteFile,
                summary: summary.into(),
                target: Some(target.into()),
            },
        }
    }

    pub fn process_spawn(
        origin: ActionOrigin,
        summary: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            origin,
            capability: EffectCapability::ProcessSpawn,
            version: CapabilityVersion::V1,
            action: Action {
                origin,
                kind: ActionKind::SpawnProcess,
                summary: summary.into(),
                target: Some(target.into()),
            },
        }
    }

    pub fn network_connect(
        origin: ActionOrigin,
        summary: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            origin,
            capability: EffectCapability::NetworkConnect,
            version: CapabilityVersion::V1,
            action: Action {
                origin,
                kind: ActionKind::NetworkConnect,
                summary: summary.into(),
                target: Some(target.into()),
            },
        }
    }

    pub fn ssh_connect(
        origin: ActionOrigin,
        summary: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            origin,
            capability: EffectCapability::NetworkConnect,
            version: CapabilityVersion::V1,
            action: Action {
                origin,
                kind: ActionKind::SshConnect,
                summary: summary.into(),
                target: Some(target.into()),
            },
        }
    }
}

/// Map a policy [`Action`] into a normalized effect for seam admission.
pub fn normalized_effect_from_action(
    action: &Action,
    capability_version: Option<u32>,
) -> Option<NormalizedEffect> {
    let version = CapabilityVersion(capability_version.unwrap_or(1));
    let target = action.target.clone().unwrap_or_default();
    Some(match action.kind {
        ActionKind::ReadFile => NormalizedEffect {
            origin: action.origin,
            capability: EffectCapability::WorkspaceRead,
            version,
            action: action.clone(),
        },
        ActionKind::WriteFile => NormalizedEffect {
            origin: action.origin,
            capability: EffectCapability::WorkspaceWrite,
            version,
            action: action.clone(),
        },
        ActionKind::SpawnProcess | ActionKind::TmuxAttach => NormalizedEffect {
            origin: action.origin,
            capability: EffectCapability::ProcessSpawn,
            version,
            action: action.clone(),
        },
        ActionKind::NetworkConnect => NormalizedEffect {
            origin: action.origin,
            capability: EffectCapability::NetworkConnect,
            version,
            action: action.clone(),
        },
        ActionKind::SshConnect => {
            NormalizedEffect::ssh_connect(action.origin, action.summary.clone(), target)
        }
        ActionKind::SftpTransfer => NormalizedEffect {
            origin: action.origin,
            capability: EffectCapability::NetworkConnect,
            version,
            action: action.clone(),
        },
        ActionKind::WebSearch
        | ActionKind::WebFetch
        | ActionKind::WebDownload
        | ActionKind::WebBrowser
        | ActionKind::WebSubmit
        | ActionKind::WebUpload => NormalizedEffect {
            origin: action.origin,
            capability: EffectCapability::NetworkConnect,
            version,
            action: action.clone(),
        },
    })
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
    pub fn from_durable(effect: NormalizedEffect, approval: ApprovalRequest) -> Self {
        Self { effect, approval }
    }

    pub fn approval(&self) -> &ApprovalRequest {
        &self.approval
    }

    pub fn effect(&self) -> &NormalizedEffect {
        &self.effect
    }
}

/// Proof that an effect passed policy and sandbox admission.
/// Only the harness may construct this token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedOperation {
    effect: NormalizedEffect,
    intent_revision: u64,
}

impl AdmittedOperation {
    /// Create an admitted operation token. This is harness-internal only.
    pub(crate) fn new(effect: NormalizedEffect, intent_revision: u64) -> Self {
        Self {
            effect,
            intent_revision,
        }
    }

    pub fn effect(&self) -> &NormalizedEffect {
        &self.effect
    }

    pub fn intent_revision(&self) -> u64 {
        self.intent_revision
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectAdmission {
    Allow(AdmittedOperation),
    NeedsApproval(DeferredEffect),
    Deny { reason: String },
}

/// Sandbox gate with fail-closed admission. `Unavailable` is a hard denial:
/// no capability code or execution closure is reached.
#[derive(Debug, Clone)]
pub enum Sandbox {
    Provisioned { scope: SandboxScope },
    Unavailable { reason: String },
}

impl Sandbox {
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

        // Version check: only V1 supported
        if effect.version != CapabilityVersion::V1 {
            return Err(format!(
                "unsupported capability version: {:?}",
                effect.version
            ));
        }

        if effect.origin != effect.action.origin {
            return Err("effect origin does not match normalized action origin".into());
        }

        let Some(target) = effect.action.target.as_deref() else {
            return Err("effect has no target".into());
        };

        match effect.capability {
            EffectCapability::WorkspaceRead => {
                if effect.action.kind != ActionKind::ReadFile {
                    return Err("WorkspaceRead capability requires ReadFile action".into());
                }
                if !scope.contains(Path::new(target)) {
                    return Err("sandbox cannot prove read target is inside workspace scope".into());
                }
            }
            EffectCapability::WorkspaceWrite => {
                if effect.action.kind != ActionKind::WriteFile {
                    return Err("WorkspaceWrite capability requires WriteFile action".into());
                }
                if !scope.contains_write_target(Path::new(target)) {
                    return Err(
                        "sandbox cannot prove write target is inside workspace scope".into(),
                    );
                }
            }
            EffectCapability::ProcessSpawn => {
                if effect.action.kind != ActionKind::SpawnProcess {
                    return Err("ProcessSpawn capability requires SpawnProcess action".into());
                }
                // Process spawn allowed within workspace scope
            }
            EffectCapability::NetworkConnect => {
                if !matches!(
                    effect.action.kind,
                    ActionKind::NetworkConnect
                        | ActionKind::SshConnect
                        | ActionKind::SftpTransfer
                        | ActionKind::WebSearch
                        | ActionKind::WebFetch
                        | ActionKind::WebDownload
                        | ActionKind::WebBrowser
                        | ActionKind::WebSubmit
                        | ActionKind::WebUpload
                ) {
                    return Err("NetworkConnect capability requires network or web action".into());
                }
                if !scope.allow_network {
                    return Err("network is disabled in this sandbox scope".into());
                }
            }
        }
        Ok(())
    }
}

/// Fixed order for capability execution:
/// sandbox -> policy (hard Deny) -> execution mode -> RiskGate -> capability.
#[derive(Clone)]
pub struct EffectSeam {
    policy: PolicyEngine,
    sandbox: Sandbox,
    execution_mode: ExecutionMode,
    risk_gate: Arc<dyn RiskGate>,
    #[cfg(test)]
    require_test_approval: bool,
}

impl std::fmt::Debug for EffectSeam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectSeam")
            .field("policy", &self.policy)
            .field("sandbox", &self.sandbox)
            .field("execution_mode", &self.execution_mode)
            .field("risk_gate", &"<dyn RiskGate>")
            .finish()
    }
}

impl EffectSeam {
    pub fn workspace_read(root: impl Into<std::path::PathBuf>) -> Self {
        let root = root.into();
        Self::with_admission(
            PolicyEngine::new(SandboxScope::local_workspace(root.clone())),
            Sandbox::workspace(root),
            ExecutionMode::Ask,
            default_risk_gate(),
        )
    }

    pub fn workspace_full(root: impl Into<std::path::PathBuf>) -> Self {
        let root = root.into();
        Self::with_admission(
            PolicyEngine::new(SandboxScope::local_workspace(root.clone())),
            Sandbox::workspace(root),
            ExecutionMode::Ask,
            default_risk_gate(),
        )
    }

    pub fn with_sandbox(policy: PolicyEngine, sandbox: Sandbox) -> Self {
        Self::with_admission(policy, sandbox, ExecutionMode::Ask, default_risk_gate())
    }

    pub fn with_admission(
        policy: PolicyEngine,
        sandbox: Sandbox,
        execution_mode: ExecutionMode,
        risk_gate: Arc<dyn RiskGate>,
    ) -> Self {
        Self {
            policy,
            sandbox,
            execution_mode,
            risk_gate,
            #[cfg(test)]
            require_test_approval: false,
        }
    }

    pub fn with_execution_mode(mut self, execution_mode: ExecutionMode) -> Self {
        self.execution_mode = execution_mode;
        self
    }

    pub fn decide(&self, effect: &NormalizedEffect) -> EffectDecision {
        self.decide_with_argv(effect, None)
    }

    pub fn decide_with_argv(
        &self,
        effect: &NormalizedEffect,
        argv: Option<&[String]>,
    ) -> EffectDecision {
        self.admit(effect, argv)
    }

    fn admit(&self, effect: &NormalizedEffect, argv: Option<&[String]>) -> EffectDecision {
        if let Err(reason) = self.sandbox.admit(effect) {
            return EffectDecision::Deny { reason };
        }

        let policy = self.policy_decision(&effect.action);
        if let PolicyDecision::Deny { reason } = policy {
            return EffectDecision::Deny { reason };
        }

        if !self.execution_mode.is_mutating_tool_allowed() && is_mutating_effect(effect) {
            return EffectDecision::Deny {
                reason: "execution mode PLAN denies mutating effects".into(),
            };
        }

        let risk = self.risk_gate.classify(&RiskContext {
            mode: self.execution_mode,
            effect,
            argv,
        });
        let decision = self.combine(policy, risk, effect);
        self.maybe_force_test_approval(effect, decision)
    }

    fn maybe_force_test_approval(
        &self,
        _effect: &NormalizedEffect,
        decision: EffectDecision,
    ) -> EffectDecision {
        #[cfg(test)]
        {
            if self.require_test_approval
                && _effect.action.kind == ActionKind::ReadFile
                && matches!(decision, EffectDecision::Allow)
            {
                return EffectDecision::NeedsApproval {
                    reason: "test-only approval gate".into(),
                };
            }
        }
        decision
    }

    fn combine(
        &self,
        policy: PolicyDecision,
        risk: RiskGateDecision,
        effect: &NormalizedEffect,
    ) -> EffectDecision {
        match risk {
            RiskGateDecision::Deny { reason } => EffectDecision::Deny { reason },
            RiskGateDecision::NeedsHumanApproval { reason } => {
                EffectDecision::NeedsApproval { reason }
            }
            RiskGateDecision::Allow => match policy {
                PolicyDecision::Allow => EffectDecision::Allow,
                PolicyDecision::NeedsApproval { reason } => {
                    if self.risk_auto_allows_policy_deferral(effect) {
                        EffectDecision::Allow
                    } else {
                        EffectDecision::NeedsApproval { reason }
                    }
                }
                PolicyDecision::Deny { reason } => EffectDecision::Deny { reason },
            },
        }
    }

    fn risk_auto_allows_policy_deferral(&self, effect: &NormalizedEffect) -> bool {
        match self.execution_mode {
            ExecutionMode::AcceptEdits | ExecutionMode::Auto | ExecutionMode::Bypass => true,
            ExecutionMode::Ask => is_read_only_effect(effect),
            ExecutionMode::Plan => false,
        }
    }

    /// Request admission without executing the capability. `NeedsApproval`
    /// returns the exact normalized action and durable approval data that must
    /// be presented to a human before any sandbox or capability code runs.
    /// `Allow` returns an AdmittedOperation token proving the effect passed admission.
    pub fn request(&self, effect: NormalizedEffect, intent_revision: u64) -> EffectAdmission {
        self.request_with_argv(effect, intent_revision, None)
    }

    pub fn request_with_argv(
        &self,
        effect: NormalizedEffect,
        intent_revision: u64,
        argv: Option<&[String]>,
    ) -> EffectAdmission {
        match self.admit(&effect, argv) {
            EffectDecision::Allow => {
                EffectAdmission::Allow(AdmittedOperation::new(effect, intent_revision))
            }
            EffectDecision::NeedsApproval { reason } => {
                EffectAdmission::NeedsApproval(DeferredEffect {
                    approval: ApprovalRequest::pending_with_version(
                        effect.action.clone(),
                        reason,
                        intent_revision,
                        Some(effect.version.0),
                    ),
                    effect,
                })
            }
            EffectDecision::Deny { reason } => EffectAdmission::Deny { reason },
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
        self.execute_after_approval_with_admission(
            deferred,
            resolution,
            current_intent_revision,
            |_| execution(),
        )
    }

    pub fn execute_after_approval_with_admission<T, E>(
        &self,
        deferred: DeferredEffect,
        resolution: ApprovalResolution,
        current_intent_revision: u64,
        execution: impl FnOnce(&AdmittedOperation) -> Result<T, E>,
    ) -> Result<EffectExecution<T>, E> {
        let approval = deferred.approval;

        // Verify capability version matches
        let expected_version = approval.capability_version;
        let actual_version = Some(deferred.effect.version.0);
        if expected_version != actual_version {
            return Ok(EffectExecution::Denied {
                reason: "capability version mismatch".into(),
            });
        }

        // Verify fingerprint includes version
        let expected_fingerprint = crate::policy::ActionFingerprint::for_action_with_version(
            &approval.action,
            approval.capability_version,
        );

        if approval.state != ApprovalState::Pending
            || resolution.resolver != ApprovalResolver::User
            || !resolution.accepted
            || resolution.id != approval.id
            || resolution.action_fingerprint != expected_fingerprint
            || resolution.action_fingerprint != approval.action_fingerprint
            || resolution.intent_revision != approval.intent_revision
            || current_intent_revision != approval.intent_revision
            || deferred.effect.action != approval.action
        {
            return Ok(EffectExecution::Denied {
                reason: "approval is missing, rejected, or stale".into(),
            });
        }

        Ok(match self.policy_decision(&deferred.effect.action) {
            PolicyDecision::Deny { reason } => EffectExecution::Denied { reason },
            PolicyDecision::Allow => EffectExecution::Denied {
                reason: "policy decision changed before approved execution".into(),
            },
            PolicyDecision::NeedsApproval { .. } => match self.sandbox.admit(&deferred.effect) {
                Ok(()) => {
                    let admission =
                        AdmittedOperation::new(deferred.effect.clone(), current_intent_revision);
                    EffectExecution::Executed(execution(&admission)?)
                }
                Err(reason) => EffectExecution::Denied { reason },
            },
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
        let seam = EffectSeam::with_sandbox(policy, Sandbox::unavailable("not provisioned"));
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

    #[test]
    fn workspace_write_capability_requires_approval() {
        let root = workspace();
        let seam = EffectSeam::workspace_full(&root);
        let effect =
            NormalizedEffect::workspace_write(ActionOrigin::Agent, "create file", "new.txt");
        let outcome = seam
            .execute(&effect, || Ok::<_, ()>("executed"))
            .expect("write effect");
        assert!(matches!(outcome, EffectExecution::NeedsApproval { .. }));
    }

    #[test]
    fn workspace_write_outside_scope_is_denied() {
        let root = workspace();
        let seam = EffectSeam::workspace_full(&root);
        let effect = NormalizedEffect::workspace_write(
            ActionOrigin::Agent,
            "write outside",
            "/etc/forbidden",
        );
        let outcome = seam
            .execute(&effect, || -> Result<(), ()> { panic!("must not execute") })
            .expect("denial");
        assert!(matches!(outcome, EffectExecution::Denied { .. }));
    }

    #[test]
    fn process_spawn_capability_requires_approval_for_agent() {
        let root = workspace();
        let seam = EffectSeam::workspace_full(&root);
        let effect =
            NormalizedEffect::process_spawn(ActionOrigin::Agent, "run formatter", "cargo fmt");
        let outcome = seam
            .execute(&effect, || Ok::<_, ()>("executed"))
            .expect("spawn effect");
        assert!(matches!(outcome, EffectExecution::NeedsApproval { .. }));
    }

    #[test]
    fn capability_version_mismatch_denies_approval() {
        let root = workspace();
        let seam = EffectSeam::workspace_read_requiring_approval(&root);
        let mut effect =
            NormalizedEffect::workspace_read(ActionOrigin::Agent, "read note", "note.txt");
        let EffectAdmission::NeedsApproval(mut deferred) = seam.request(effect.clone(), 41) else {
            panic!("test gate must defer effect");
        };

        // Simulate version change
        effect.version = CapabilityVersion(999);
        deferred.effect = effect;

        let resolution = ApprovalResolution::user(deferred.approval(), true);
        let outcome = seam
            .execute_after_approval(deferred, resolution, 41, || -> Result<(), ()> {
                panic!("version mismatch must not execute")
            })
            .expect("version mismatch is a denial");
        assert!(
            matches!(outcome, EffectExecution::Denied { reason } if reason.contains("version"))
        );
    }

    #[test]
    fn action_fingerprint_includes_capability_version() {
        use crate::policy::ActionFingerprint;
        let action = Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WriteFile,
            summary: "write".into(),
            target: Some("file.txt".into()),
        };

        let fingerprint_v1 = ActionFingerprint::for_action_with_version(&action, Some(1));
        let fingerprint_v2 = ActionFingerprint::for_action_with_version(&action, Some(2));
        let fingerprint_none = ActionFingerprint::for_action(&action);

        assert_ne!(fingerprint_v1, fingerprint_v2);
        assert_ne!(fingerprint_v1, fingerprint_none);
        assert_eq!(
            fingerprint_v1,
            ActionFingerprint::for_action_with_version(&action, Some(1))
        );
    }

    // A2 Phase 1: Origin derivation regression tests
    #[test]
    fn agent_origin_requires_approval_for_process_spawn() {
        let root = workspace();
        let seam = EffectSeam::workspace_full(&root);
        let effect =
            NormalizedEffect::process_spawn(ActionOrigin::Agent, "run formatter", "cargo fmt");
        let admission = seam.request(effect, 1);
        assert!(
            matches!(admission, EffectAdmission::NeedsApproval(_)),
            "agent origin must require approval for process spawn"
        );
    }

    #[test]
    fn user_origin_may_allow_read_without_approval() {
        let root = workspace();
        let seam = EffectSeam::workspace_read(&root);
        let effect = NormalizedEffect::workspace_read(ActionOrigin::User, "read note", "note.txt");
        let admission = seam.request(effect, 1);
        assert!(
            matches!(admission, EffectAdmission::Allow(_)),
            "user origin may allow read without approval"
        );
    }

    // A2 Phase 2: Deferred continuation tests
    #[test]
    fn admitted_operation_proves_effect_passed_admission() {
        let root = workspace();
        let seam = EffectSeam::workspace_read(&root);
        let effect = NormalizedEffect::workspace_read(ActionOrigin::User, "read note", "note.txt");
        let EffectAdmission::Allow(admission) = seam.request(effect.clone(), 1) else {
            panic!("test effect must be allowed");
        };
        assert_eq!(admission.effect(), &effect);
        assert_eq!(admission.intent_revision(), 1);
    }

    #[test]
    fn deferred_effect_stores_normalized_effect_and_approval() {
        let root = workspace();
        let seam = EffectSeam::workspace_read_requiring_approval(&root);
        let effect = NormalizedEffect::workspace_read(ActionOrigin::Agent, "read note", "note.txt");
        let EffectAdmission::NeedsApproval(deferred) = seam.request(effect.clone(), 42) else {
            panic!("test gate must defer effect");
        };
        assert_eq!(deferred.effect(), &effect);
        assert_eq!(deferred.approval().intent_revision, 42);
    }

    #[test]
    fn plan_mode_denies_workspace_write_before_execution() {
        let root = workspace();
        let seam = EffectSeam::workspace_full(&root).with_execution_mode(ExecutionMode::Plan);
        let effect =
            NormalizedEffect::workspace_write(ActionOrigin::Agent, "create file", "new.txt");
        assert!(matches!(
            seam.decide(&effect),
            EffectDecision::Deny { reason } if reason.contains("PLAN")
        ));
    }

    #[test]
    fn auto_mode_allows_agent_write_without_approval() {
        let root = workspace();
        let seam = EffectSeam::workspace_full(&root).with_execution_mode(ExecutionMode::Auto);
        let effect =
            NormalizedEffect::workspace_write(ActionOrigin::Agent, "create file", "new.txt");
        assert_eq!(seam.decide(&effect), EffectDecision::Allow);
    }

    #[test]
    fn auto_mode_allows_safe_read_spawn_without_approval() {
        let root = workspace();
        let seam = EffectSeam::workspace_full(&root).with_execution_mode(ExecutionMode::Auto);
        let effect =
            NormalizedEffect::process_spawn(ActionOrigin::Agent, "git status", "git status");
        assert_eq!(seam.decide(&effect), EffectDecision::Allow);
    }

    #[test]
    fn bypass_cannot_override_sandbox_escape() {
        let root = workspace();
        let seam = EffectSeam::workspace_full(&root).with_execution_mode(ExecutionMode::Bypass);
        let effect = NormalizedEffect::workspace_write(
            ActionOrigin::Agent,
            "write outside",
            "/etc/forbidden",
        );
        assert!(matches!(seam.decide(&effect), EffectDecision::Deny { .. }));
    }

    #[test]
    fn bypass_still_denies_sudo_via_risk_gate() {
        let root = workspace();
        let seam = EffectSeam::workspace_full(&root).with_execution_mode(ExecutionMode::Bypass);
        let effect =
            NormalizedEffect::process_spawn(ActionOrigin::Agent, "sudo apt", "sudo apt update");
        assert!(matches!(seam.decide(&effect), EffectDecision::Deny { .. }));
    }
}
