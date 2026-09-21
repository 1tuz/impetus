//! PR-safe security/runtime lib suite (TODO P0 §5 / #174).
//!
//! Filter: `cargo test -p impetus-core --lib security_runtime_pr`
//!
//! Runs under `cargo test --lib --bins` (PR CI). No Seatbelt / process wrap.
//! Complementary full-flow harness coverage (same `--lib` target; satisfies #15
//! without a second copy under `crates/impetus-core/tests/`):
//! - `harness_api::approval_resume_returns_durable_tool_observations_to_the_model`
//! - `harness_api::rejected_approval_records_denial_and_resumes_without_execution`
//! - `harness_api::cancellation_stops_an_active_agent_run_without_a_final_answer`
//! - `runtime::attach_recovers_pending_approval_and_next_sequence`
//!
//! Evidence map: `docs/development.md` § Full request-flow coverage (#15).

use crate::module::ExecutionSemantics;
use crate::module_fallback::{OperationOutcome, UnknownOutcomePolicy};
use crate::{
    ActionOrigin, ApprovalResolution, DurableArtifactStore, EffectAdmission, EffectExecution,
    EffectSeam, NormalizedEffect, PolicyEngine, ReadOnlyToolKind, Sandbox, SandboxScope,
    ToolOutcome, ToolProvenance, ToolResult, redact_tool_outcome,
};
use std::path::PathBuf;
use uuid::Uuid;

fn temp_workspace() -> PathBuf {
    let root = std::env::temp_dir().join(format!("security-runtime-pr-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("workspace");
    std::fs::write(root.join("note.txt"), "safe").expect("fixture");
    root
}

#[test]
fn approve_then_execute_runs_capability_once() {
    let root = temp_workspace();
    let seam = EffectSeam::workspace_full(&root);
    let effect = NormalizedEffect::workspace_write(ActionOrigin::Agent, "write note", "out.txt");
    let EffectAdmission::NeedsApproval(deferred) = seam.request(effect, 7) else {
        panic!("agent write must defer");
    };
    let resolution = ApprovalResolution::user(deferred.approval(), true);
    let mut executed = 0u8;
    let outcome = seam
        .execute_after_approval(deferred, resolution, 7, || {
            executed += 1;
            Ok::<_, ()>("done")
        })
        .expect("approved path");
    assert_eq!(outcome, EffectExecution::Executed("done"));
    assert_eq!(executed, 1);
}

#[test]
fn reject_never_reaches_capability_execution() {
    let root = temp_workspace();
    let seam = EffectSeam::workspace_full(&root);
    let effect =
        NormalizedEffect::workspace_write(ActionOrigin::Agent, "write blocked", "blocked.txt");
    let EffectAdmission::NeedsApproval(deferred) = seam.request(effect, 3) else {
        panic!("agent write must defer");
    };
    let resolution = ApprovalResolution::user(deferred.approval(), false);
    let outcome = seam
        .execute_after_approval(deferred, resolution, 3, || -> Result<(), ()> {
            panic!("rejected approval must not execute")
        })
        .expect("rejection is a denial");
    assert!(matches!(outcome, EffectExecution::Denied { reason } if reason.contains("rejected")));
    assert!(!root.join("blocked.txt").exists());
}

#[test]
fn sandbox_unavailable_denies_before_execution() {
    let root = temp_workspace();
    let policy = PolicyEngine::new(SandboxScope::local_workspace(&root));
    let seam = EffectSeam::with_sandbox(policy, Sandbox::unavailable("pr-safe probe"));
    let effect = NormalizedEffect::workspace_write(ActionOrigin::Agent, "write", "x.txt");
    let outcome = seam
        .execute(&effect, || -> Result<(), ()> { panic!("must not execute") })
        .expect("denial");
    assert!(
        matches!(outcome, EffectExecution::Denied { reason } if reason.contains("unavailable"))
    );
}

#[test]
fn sandbox_out_of_scope_write_denies_before_execution() {
    let root = temp_workspace();
    let seam = EffectSeam::workspace_full(&root);
    let effect = NormalizedEffect::workspace_write(
        ActionOrigin::Agent,
        "write outside",
        "/tmp/impetus-out-of-scope.txt",
    );
    let outcome = seam
        .execute(&effect, || -> Result<(), ()> { panic!("must not execute") })
        .expect("denial");
    assert!(matches!(outcome, EffectExecution::Denied { .. }));
}

#[test]
fn sandbox_network_disabled_denies_before_execution() {
    let root = temp_workspace();
    let scope = SandboxScope::local_workspace(&root);
    let policy = PolicyEngine::new(scope.clone());
    let seam = EffectSeam::with_sandbox(policy, Sandbox::Provisioned { scope });
    let effect =
        NormalizedEffect::network_connect(ActionOrigin::Agent, "connect", "example.com:443");
    let outcome = seam
        .execute(&effect, || -> Result<(), ()> { panic!("must not execute") })
        .expect("denial");
    assert!(matches!(outcome, EffectExecution::Denied { reason } if reason.contains("network")));
}

#[test]
fn unknown_outcome_blocks_retry_for_mutating() {
    let policy = UnknownOutcomePolicy::new(ExecutionSemantics::Mutating);
    assert!(!policy.can_retry(OperationOutcome::Unknown));
    assert!(!policy.can_fallback(OperationOutcome::Unknown));
    assert!(policy.can_retry(OperationOutcome::Failure));
}

#[test]
fn redact_tool_outcome_strips_secret_preview_values() {
    let outcome = ToolOutcome::Allowed {
        result: ToolResult {
            tool: ReadOnlyToolKind::Read,
            provenance: ToolProvenance {
                workspace_root: PathBuf::from("/tmp/ws"),
                relative_path: PathBuf::from("secrets.txt"),
                in_scope: true,
            },
            preview: "API_TOKEN=raw-secret-value\nAuthorization: Bearer hidden-token\nok=1".into(),
            truncated: false,
            artifact: None,
            line_count: 3,
            byte_count: 64,
            original_tokens: 16,
            reduced_tokens: 16,
        },
    };
    let redacted = redact_tool_outcome(outcome);
    let ToolOutcome::Allowed { result } = redacted else {
        panic!("expected allowed");
    };
    assert!(!result.preview.contains("raw-secret-value"));
    assert!(!result.preview.contains("hidden-token"));
    assert!(result.preview.contains("ok=1"));
}

#[test]
fn durable_artifact_survives_store_reopen() {
    let dir = tempfile::tempdir().expect("artifact root");
    let id = {
        let store = DurableArtifactStore::open(dir.path()).expect("open");
        store.store(b"restore-me-after-reopen").expect("store").id
    };
    let reopened = DurableArtifactStore::open(dir.path()).expect("reopen");
    let bytes = reopened.read(&id).expect("read after reopen");
    assert_eq!(bytes, b"restore-me-after-reopen");
    let meta = reopened.metadata(&id).expect("meta").expect("present");
    assert_eq!(meta.byte_count, b"restore-me-after-reopen".len());
}
