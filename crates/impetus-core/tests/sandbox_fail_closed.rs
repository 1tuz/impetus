//! Evidence that sandbox enforcement is fail-closed: unsafe capabilities
//! are denied before execution when sandbox is unavailable or scope check fails.

use impetus_core::{
    ActionOrigin, EffectExecution, EffectSeam, NormalizedEffect, PolicyEngine, Sandbox,
    SandboxScope,
};
use std::path::PathBuf;
use uuid::Uuid;

fn temp_workspace() -> PathBuf {
    let root = std::env::temp_dir().join(format!("sandbox-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create workspace");
    root
}

#[test]
fn write_capability_denied_when_sandbox_unavailable() {
    let root = temp_workspace();
    let policy = PolicyEngine::new(SandboxScope::local_workspace(&root));
    let seam = EffectSeam::with_sandbox(policy, Sandbox::unavailable("test unavailable"));

    let effect = NormalizedEffect::workspace_write(ActionOrigin::Agent, "write file", "test.txt");

    let outcome = seam
        .execute(&effect, || -> Result<(), ()> {
            panic!("unavailable sandbox must not execute")
        })
        .expect("denial");

    assert!(
        matches!(outcome, EffectExecution::Denied { reason } if reason.contains("unavailable"))
    );
}

#[test]
fn process_spawn_denied_when_sandbox_unavailable() {
    let root = temp_workspace();
    let policy = PolicyEngine::new(SandboxScope::local_workspace(&root));
    let seam = EffectSeam::with_sandbox(policy, Sandbox::unavailable("test unavailable"));

    let effect = NormalizedEffect::process_spawn(ActionOrigin::Agent, "spawn process", "echo test");

    let outcome = seam
        .execute(&effect, || -> Result<(), ()> {
            panic!("unavailable sandbox must not execute")
        })
        .expect("denial");

    assert!(
        matches!(outcome, EffectExecution::Denied { reason } if reason.contains("unavailable"))
    );
}

#[test]
fn process_working_directory_outside_workspace_is_denied() {
    let root = temp_workspace();
    let outside = root
        .parent()
        .expect("workspace parent")
        .join(Uuid::new_v4().to_string());
    std::fs::create_dir(&outside).expect("outside directory");
    let seam = EffectSeam::workspace_full(&root);
    let effect = NormalizedEffect::process_spawn(
        ActionOrigin::Agent,
        "spawn outside workspace",
        outside.display().to_string(),
    );

    let outcome = seam
        .execute(&effect, || -> Result<(), ()> {
            panic!("out-of-scope process must not execute")
        })
        .expect("denial");

    assert!(matches!(outcome, EffectExecution::Denied { .. }));
    std::fs::remove_dir_all(outside).expect("remove outside directory");
}

#[test]
fn write_outside_workspace_denied_by_sandbox() {
    let root = temp_workspace();
    let seam = EffectSeam::workspace_full(&root);

    let effect =
        NormalizedEffect::workspace_write(ActionOrigin::Agent, "write outside", "/tmp/outside.txt");

    let outcome = seam
        .execute(&effect, || -> Result<(), ()> {
            panic!("out-of-scope write must not execute")
        })
        .expect("denial");

    assert!(
        matches!(outcome, EffectExecution::Denied { reason } if reason.contains("outside") || reason.contains("scope"))
    );
}

#[test]
fn read_outside_workspace_denied_by_sandbox() {
    let root = temp_workspace();
    let seam = EffectSeam::workspace_full(&root);

    let effect =
        NormalizedEffect::workspace_read(ActionOrigin::Agent, "read outside", "/etc/passwd");

    let outcome = seam
        .execute(&effect, || -> Result<(), ()> {
            panic!("out-of-scope read must not execute")
        })
        .expect("denial");

    assert!(matches!(outcome, EffectExecution::Denied { .. }));
}

#[test]
fn network_denied_when_scope_disallows_network() {
    let root = temp_workspace();
    let scope = SandboxScope::local_workspace(&root); // no network by default
    let policy = PolicyEngine::new(scope.clone());
    let seam = EffectSeam::with_sandbox(policy, Sandbox::Provisioned { scope });

    let effect =
        NormalizedEffect::network_connect(ActionOrigin::Agent, "connect", "example.com:443");

    let outcome = seam
        .execute(&effect, || -> Result<(), ()> {
            panic!("network disabled must not execute")
        })
        .expect("denial");

    assert!(matches!(outcome, EffectExecution::Denied { reason } if reason.contains("network")));
}

#[test]
fn mismatched_capability_and_action_kind_denied() {
    let root = temp_workspace();
    let seam = EffectSeam::workspace_full(&root);

    // Create a normalized effect with mismatched capability and action kind
    let mut effect = NormalizedEffect::workspace_read(ActionOrigin::Agent, "read file", "test.txt");
    effect.capability = impetus_core::EffectCapability::WorkspaceWrite; // mismatch

    let outcome = seam
        .execute(&effect, || -> Result<(), ()> {
            panic!("mismatched capability must not execute")
        })
        .expect("denial");

    assert!(matches!(outcome, EffectExecution::Denied { .. }));
}
