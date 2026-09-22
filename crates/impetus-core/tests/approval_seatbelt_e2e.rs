//! E2E sentinel: Policy → Approval → Seatbelt → Execution → Result (macOS).
//!
//! Covers the production admit → approve → sandboxed spawn path used by
//! `ToolOrchestrator::execute_approved_bash` / harness `ResolveApproval`,
//! including durable `SandboxDecision` (backend=`macos_seatbelt`) event.

#![cfg(target_os = "macos")]

use impetus_core::{
    ActionOrigin, AgentRuntime, ApprovalResolution, ApprovalResolver, EffectExecution,
    EventPayload, MemoryEventStore, NormalizedEffect, PolicyEngine, RuntimeError,
    SandboxCommandRequest, SandboxDecisionState, SandboxEvent, SandboxPrepareState, SandboxScope,
    ToolEventOutcome, ToolOrchestrator, production_sandbox_provider,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

fn unique_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "impetus-approval-seatbelt-e2e-{nonce}-{}",
        std::process::id()
    ))
}

fn setup_workspace(root: &Path) -> PathBuf {
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).expect("create workspace");
    workspace.canonicalize().expect("canonical workspace")
}

async fn request_bash_approval(
    runtime: &Arc<AgentRuntime>,
    orchestrator: &ToolOrchestrator,
    command: &str,
    tool_call_id: &str,
) -> (
    impetus_core::ApprovalRequest,
    (String, String, serde_json::Value),
) {
    let observations = orchestrator
        .process_tool_calls(
            Uuid::new_v4(),
            vec![impetus_core::ToolCall {
                id: tool_call_id.into(),
                name: "bash".into(),
                arguments: serde_json::json!({ "command": command }),
            }],
            runtime,
        )
        .await
        .expect("bash admission");
    assert_eq!(
        observations[0].outcome,
        ToolEventOutcome::ApprovalRequired,
        "RiskGate must defer bash to human approval"
    );

    let request = runtime
        .events()
        .expect("events")
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            impetus_core::EventPayload::Approval(impetus_core::ApprovalEvent::Requested {
                request,
            }) => Some(request.clone()),
            _ => None,
        })
        .expect("pending approval request");
    let deferred = runtime
        .deferred_tool(request.id)
        .expect("deferred lookup")
        .expect("deferred bash tool");
    (request, deferred)
}

#[tokio::test]
async fn forged_resolve_approval_is_noop_then_user_approve_runs_under_seatbelt() {
    let provider = production_sandbox_provider();
    provider.probe().expect("sandbox-exec must be available");

    let root = unique_root();
    let workspace = setup_workspace(&root);
    let allowed = workspace.join("allowed.txt");
    let blocked = root.join("blocked.txt");

    // In-process SandboxDecision evidence from prepare().
    let probe_args = vec![allowed.display().to_string()];
    let prepared = provider
        .prepare(&SandboxCommandRequest {
            executable: "/usr/bin/touch",
            args: &probe_args,
            workspace_root: &workspace,
            working_dir: &workspace,
            explicit_env: &[],
            allow_network: false,
        })
        .expect("prepare seatbelt decision");
    let decision = prepared.decision().clone();
    assert_eq!(decision.backend, "macos_seatbelt");
    assert_eq!(decision.state, SandboxDecisionState::Prepared);
    assert!(!decision.network_allowed);
    drop(prepared);

    let store = Arc::new(MemoryEventStore::default());
    let policy = PolicyEngine::new(SandboxScope::local_workspace(&workspace));
    let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
    runtime
        .submit_intent("seatbelt e2e sentinel")
        .expect("user intent");
    let orchestrator = ToolOrchestrator::new(policy, workspace.clone());

    let allow_cmd = format!("/usr/bin/touch {}", allowed.display());
    let (request, deferred) =
        request_bash_approval(&runtime, &orchestrator, &allow_cmd, "bash-allow").await;

    // Forged agent ResolveApproval must be a no-op: error + pending stays + no write.
    let mut forged = ApprovalResolution::user(&request, true);
    forged.resolver = ApprovalResolver::Agent;
    assert!(
        matches!(
            runtime.resolve_approval(forged.clone()),
            Err(RuntimeError::ApprovalResolverNotUser(id)) if id == request.id
        ),
        "agent must not resolve its own approval"
    );
    assert!(
        runtime
            .pending_approval(request.id)
            .expect("pending check")
            .is_some(),
        "forged resolve must leave approval pending"
    );
    assert!(
        !allowed.exists(),
        "forged resolve must not execute the write"
    );

    // EffectSeam also fail-closes if forged resolution is passed to execute.
    let effect = NormalizedEffect::process_spawn(
        ActionOrigin::Agent,
        "bash via agent",
        workspace.display().to_string(),
    );
    let seam = runtime.effect_seam().expect("effect seam");
    let forged_exec = seam
        .execute_after_approval(
            impetus_core::DeferredEffect::from_durable(effect, request.clone()),
            forged,
            request.intent_revision,
            || Ok::<_, ()>(()),
        )
        .expect("seam returns EffectExecution");
    assert!(
        matches!(forged_exec, EffectExecution::Denied { .. }),
        "forged resolution must not admit execution at EffectSeam"
    );

    // Legitimate user approve → Seatbelt spawn → workspace write succeeds.
    let resolution = ApprovalResolution::user(&request, true);
    runtime
        .resolve_approval(resolution.clone())
        .expect("user resolve");
    let observation =
        ToolOrchestrator::execute_approved_bash(&runtime, request, resolution, deferred)
            .expect("approved seatbelt bash");
    assert_eq!(observation.outcome, ToolEventOutcome::Success);
    assert!(
        allowed.is_file(),
        "workspace write must succeed under Seatbelt after approval"
    );

    // Durable SandboxDecision on parent session log after approve+exec.
    let events = runtime.events().expect("events");
    let sandbox_events: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::Sandbox(SandboxEvent::Decision {
                backend,
                prepare_state,
                network_allowed,
                ..
            }) => Some((backend.clone(), *prepare_state, *network_allowed)),
            _ => None,
        })
        .collect();
    assert!(
        sandbox_events.iter().any(|(backend, state, network)| {
            backend == "macos_seatbelt" && *state == SandboxPrepareState::Prepared && !*network
        }),
        "durable SandboxDecision (macos_seatbelt/prepared) must be recorded: {sandbox_events:?}"
    );

    // Sibling write after a fresh admit→approve must fail under Seatbelt.
    let deny_cmd = format!("/usr/bin/touch {}", blocked.display());
    let (deny_request, deny_deferred) =
        request_bash_approval(&runtime, &orchestrator, &deny_cmd, "bash-deny").await;
    let deny_resolution = ApprovalResolution::user(&deny_request, true);
    runtime
        .resolve_approval(deny_resolution.clone())
        .expect("user resolve sibling attempt");
    let deny_observation = ToolOrchestrator::execute_approved_bash(
        &runtime,
        deny_request,
        deny_resolution,
        deny_deferred,
    )
    .expect("approved sibling bash returns observation");
    // Seatbelt denies the write; shell exits non-zero. Orchestrator still
    // records Executed as Success with exit_code in the preview.
    assert!(
        !blocked.exists(),
        "sibling write must be rejected by Seatbelt after approval"
    );
    assert!(
        deny_observation.preview.contains("exit_code=Some(")
            && !deny_observation.preview.contains("exit_code=Some(0)"),
        "sibling Seatbelt denial must surface non-zero exit: {}",
        deny_observation.preview
    );

    fs::remove_dir_all(root).expect("cleanup e2e root");
}
