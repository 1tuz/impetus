//! Integration test for v0.5 gate: exact approval, sandbox fail-closed, policy replay.
//!
//! Gate criteria:
//! - Mutating effect requires exact approval or explicit Allow
//! - Sandbox denial blocks unsafe capability
//! - Policy replay gives identical decision for historical event

use impetus_core::{
    ActionOrigin, ApprovalResolution, EffectAdmission, EffectExecution, EffectSeam,
    NormalizedEffect, PolicyEngine, PolicySnapshot, SandboxScope,
};
use std::path::PathBuf;
use uuid::Uuid;

fn temp_workspace() -> PathBuf {
    let root = std::env::temp_dir().join(format!("v05-gate-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create workspace");
    root
}

#[test]
fn mutating_effect_requires_exact_approval() {
    let root = temp_workspace();
    let seam = EffectSeam::workspace_full(&root);

    // Agent write requires approval
    let effect =
        NormalizedEffect::workspace_write(ActionOrigin::Agent, "create config", "config.toml");

    let EffectAdmission::NeedsApproval(deferred) = seam.request(effect, 1) else {
        panic!("mutating effect must need approval");
    };

    // Exact approval allows execution
    let resolution = ApprovalResolution::user(deferred.approval(), true);
    let outcome = seam
        .execute_after_approval(deferred, resolution, 1, || Ok::<_, ()>("executed"))
        .expect("approved execution");

    assert_eq!(outcome, EffectExecution::Executed("executed"));
}

#[test]
fn sandbox_denial_blocks_unsafe_capability() {
    let root = temp_workspace();
    let seam = EffectSeam::workspace_full(&root);

    // Write outside workspace is denied by sandbox before approval
    let effect = NormalizedEffect::workspace_write(
        ActionOrigin::Agent,
        "write outside",
        "/tmp/forbidden.txt",
    );

    let admission = seam.request(effect, 1);
    assert!(
        matches!(admission, EffectAdmission::Deny { reason } if reason.contains("outside") || reason.contains("scope"))
    );
}

#[test]
fn policy_replay_gives_identical_decision_for_historical_event() {
    let root = temp_workspace();
    let policy = PolicyEngine::new(SandboxScope::local_workspace(&root));
    let snapshot = PolicySnapshot::capture(&policy);

    let action = impetus_core::Action {
        origin: ActionOrigin::Agent,
        kind: impetus_core::ActionKind::WriteFile,
        summary: "write file".into(),
        target: Some("test.txt".into()),
    };

    let current_decision = policy.evaluate(&action);
    let replayed_decision = policy.replay(&snapshot, &action);

    assert_eq!(current_decision, replayed_decision);
}

#[test]
fn exact_approval_with_version_prevents_reuse() {
    let root = temp_workspace();
    let seam = EffectSeam::workspace_full(&root);

    let effect1 =
        NormalizedEffect::workspace_write(ActionOrigin::Agent, "write file1", "file1.txt");

    let EffectAdmission::NeedsApproval(deferred1) = seam.request(effect1, 1) else {
        panic!("effect must need approval");
    };

    // Different effect with same action kind
    let effect2 =
        NormalizedEffect::workspace_write(ActionOrigin::Agent, "write file2", "file2.txt");

    let EffectAdmission::NeedsApproval(deferred2) = seam.request(effect2, 1) else {
        panic!("effect must need approval");
    };

    // Try to use approval1 for effect2 (should fail)
    let resolution1 = ApprovalResolution::user(deferred1.approval(), true);
    let outcome = seam
        .execute_after_approval(deferred2, resolution1, 1, || -> Result<(), ()> {
            panic!("mismatched approval must not execute")
        })
        .expect("mismatched approval is denial");

    assert!(matches!(outcome, EffectExecution::Denied { .. }));
}

#[test]
fn full_v05_gate_pass() {
    let root = temp_workspace();
    std::fs::write(root.join("existing.txt"), "content").expect("write fixture");
    let seam = EffectSeam::workspace_full(&root);
    let policy = PolicyEngine::new(SandboxScope::local_workspace(&root));
    let snapshot = PolicySnapshot::capture(&policy);

    // 1. Mutating effect requires exact approval
    let write_effect =
        NormalizedEffect::workspace_write(ActionOrigin::Agent, "update file", "existing.txt");

    let EffectAdmission::NeedsApproval(deferred) = seam.request(write_effect.clone(), 1) else {
        panic!("write must need approval");
    };

    let resolution = ApprovalResolution::user(deferred.approval(), true);
    let write_outcome = seam
        .execute_after_approval(deferred, resolution, 1, || Ok::<_, ()>("written"))
        .expect("approved write");
    assert_eq!(write_outcome, EffectExecution::Executed("written"));

    // 2. Sandbox denial blocks unsafe capability
    let unsafe_effect =
        NormalizedEffect::workspace_write(ActionOrigin::Agent, "write outside", "/etc/forbidden");
    let unsafe_admission = seam.request(unsafe_effect, 1);
    assert!(matches!(unsafe_admission, EffectAdmission::Deny { .. }));

    // 3. Policy replay gives identical decision
    let replayed = policy.replay(&snapshot, &write_effect.action);
    let current = policy.evaluate(&write_effect.action);
    assert_eq!(replayed, current);
}
