//! Production macOS Seatbelt execution evidence.

#![cfg(target_os = "macos")]

use impetus_core::{
    ActionOrigin, AgentRuntime, EffectAdmission, EffectSeam, MemoryEventStore, PolicyEngine,
    ProcessExecutionError, ProcessExecutionRequest, SandboxDecisionState, SandboxScope,
    ToolEventOutcome, ToolOrchestrator, UnavailableSandboxProvider, production_sandbox_provider,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn admitted(
    request: &ProcessExecutionRequest,
    workspace: &Path,
) -> impetus_core::AdmittedOperation {
    match request
        .request(&EffectSeam::workspace_full(workspace))
        .expect("request admission")
    {
        EffectAdmission::Allow(admission) => admission,
        other => panic!("expected admitted user process, got {other:?}"),
    }
}

fn request(command: &str, args: Vec<String>, workspace: &Path) -> ProcessExecutionRequest {
    ProcessExecutionRequest::new(command, args, ActionOrigin::User, 1)
        .with_working_dir(workspace.to_path_buf())
        .with_timeout(Duration::from_secs(5))
}

fn process_exists(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

fn recorded_child_pid(workspace: &Path) -> u32 {
    let pid_path = workspace.join("child.pid");
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Ok(pid) = fs::read_to_string(&pid_path) {
            return pid.trim().parse().expect("child pid");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("sandboxed shell did not record its child pid");
}

#[tokio::test]
async fn production_sandbox_allows_workspace_write() {
    let workspace = TempDir::new().expect("workspace");
    let request = request(
        "/usr/bin/touch",
        vec![workspace.path().join("allowed").display().to_string()],
        workspace.path(),
    );
    let admission = admitted(&request, workspace.path());

    let output = request
        .execute(&admission)
        .await
        .expect("sandboxed command");

    assert_eq!(output.exit_code, Some(0));
    assert!(workspace.path().join("allowed").is_file());
}

#[tokio::test]
async fn approved_agent_shell_uses_the_production_sandbox() {
    let root = TempDir::new().expect("root");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace");
    let outside = root.path().join("agent-escape");
    let runtime = Arc::new(
        AgentRuntime::create_with_workspace(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(&workspace)),
            workspace.clone(),
        )
        .expect("runtime"),
    );
    runtime
        .submit_intent("run approved command")
        .expect("intent");
    let orchestrator = ToolOrchestrator::new(runtime.policy(), workspace);
    let observations = orchestrator
        .process_tool_calls(
            Uuid::new_v4(),
            vec![impetus_core::ToolCall {
                id: "sandbox-agent-shell".into(),
                name: "bash".into(),
                arguments: serde_json::json!({
                    "command": format!("/usr/bin/touch '{}'", outside.display())
                }),
            }],
            &runtime,
        )
        .await
        .expect("request shell");
    assert_eq!(observations[0].outcome, ToolEventOutcome::ApprovalRequired);
    let request = runtime
        .events()
        .expect("events")
        .into_iter()
        .find_map(|event| match event.payload {
            impetus_core::EventPayload::Approval(impetus_core::ApprovalEvent::Requested {
                request,
            }) => Some(request),
            _ => None,
        })
        .expect("approval request");
    let deferred = runtime
        .deferred_tool(request.id)
        .expect("deferred lookup")
        .expect("deferred shell");
    let resolution = impetus_core::ApprovalResolution::user(&request, true);
    runtime
        .resolve_approval(resolution.clone())
        .expect("approval");

    let observation =
        ToolOrchestrator::execute_approved_bash(&runtime, request, resolution, deferred)
            .expect("sandboxed approved shell");

    assert!(observation.preview.contains("exit_code=Some("));
    assert!(!outside.exists());
    assert!(runtime.events().expect("events").iter().any(|event| {
        matches!(
            &event.payload,
            impetus_core::EventPayload::Notice(impetus_core::NoticeEvent::SandboxDecision {
                decision
            }) if decision.state == SandboxDecisionState::Prepared
                && decision.backend == "macos_seatbelt"
                && !decision.network_allowed
                && decision.reason_code.is_none()
        )
    }));
}

#[tokio::test]
async fn production_sandbox_denies_write_outside_workspace() {
    let root = TempDir::new().expect("root");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace");
    let outside = root.path().join("outside");
    let request = request(
        "/usr/bin/touch",
        vec![outside.display().to_string()],
        &workspace,
    );
    let admission = admitted(&request, &workspace);

    let output = request
        .execute(&admission)
        .await
        .expect("sandboxed command");

    assert_ne!(output.exit_code, Some(0));
    assert!(!outside.exists());
}

#[tokio::test]
async fn production_sandbox_denies_sensitive_home_read() {
    let workspace = TempDir::new().expect("workspace");
    let home = std::env::var_os("HOME").map(PathBuf::from).expect("HOME");
    let candidates = [
        home.join(".ssh/config"),
        home.join("Library/Keychains/login.keychain-db"),
        home.join("Library/Safari/Bookmarks.plist"),
    ];
    let sensitive = candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .expect("macOS sandbox test needs an existing sensitive fixture");
    let request = request(
        "/usr/bin/cat",
        vec![sensitive.display().to_string()],
        workspace.path(),
    );
    let admission = admitted(&request, workspace.path());

    let output = request
        .execute(&admission)
        .await
        .expect("sandboxed command");

    assert_ne!(output.exit_code, Some(0));
    assert!(output.stdout.is_empty());
}

#[tokio::test]
async fn production_sandbox_denies_network_by_default() {
    let workspace = TempDir::new().expect("workspace");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().expect("listener address").port();
    let request = request(
        "/usr/bin/nc",
        vec!["-z".into(), "127.0.0.1".into(), port.to_string()],
        workspace.path(),
    );
    let admission = admitted(&request, workspace.path());

    let output = request
        .execute(&admission)
        .await
        .expect("sandboxed command");

    assert_ne!(output.exit_code, Some(0));
    assert!(listener.accept().is_err(), "sandboxed process connected");
}

#[tokio::test]
async fn child_process_inherits_workspace_confinement() {
    let root = TempDir::new().expect("root");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace");
    let outside = root.path().join("child-escape");
    let request = request(
        "/bin/sh",
        vec![
            "-c".into(),
            "/bin/sh -c '/usr/bin/touch \"$1\"' child \"$1\"".into(),
            "parent".into(),
            outside.display().to_string(),
        ],
        &workspace,
    );
    let admission = admitted(&request, &workspace);

    let output = request
        .execute(&admission)
        .await
        .expect("sandboxed command");

    assert_ne!(output.exit_code, Some(0));
    assert!(!outside.exists());
}

#[tokio::test]
async fn symlink_cannot_redirect_workspace_write_outside_scope() {
    let root = TempDir::new().expect("root");
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside");
    fs::create_dir(&workspace).expect("workspace");
    fs::create_dir(&outside).expect("outside");
    std::os::unix::fs::symlink(&outside, workspace.join("escape")).expect("symlink");
    let escaped_file = workspace.join("escape/blocked");
    let request = request(
        "/usr/bin/touch",
        vec![escaped_file.display().to_string()],
        &workspace,
    );
    let admission = admitted(&request, &workspace);

    let output = request
        .execute(&admission)
        .await
        .expect("sandboxed command");

    assert_ne!(output.exit_code, Some(0));
    assert!(!outside.join("blocked").exists());
}

#[tokio::test]
async fn unavailable_sandbox_fails_closed_before_spawn() {
    let workspace = TempDir::new().expect("workspace");
    let marker = workspace.path().join("must-not-exist");
    let request = request(
        "/usr/bin/touch",
        vec![marker.display().to_string()],
        workspace.path(),
    );
    let admission = admitted(&request, workspace.path());
    let provider = UnavailableSandboxProvider::new("test backend unavailable");

    let result = request.execute_with_provider(&admission, &provider).await;

    assert!(matches!(
        result,
        Err(ProcessExecutionError::SandboxUnavailable)
    ));
    assert!(!marker.exists());
}

#[tokio::test]
async fn timeout_terminates_the_process_group() {
    let workspace = TempDir::new().expect("workspace");
    let request = request(
        "/bin/sh",
        vec!["-c".into(), "sleep 30 & echo $! > child.pid; wait".into()],
        workspace.path(),
    )
    .with_timeout(Duration::from_millis(250));
    let admission = admitted(&request, workspace.path());

    let result = request.execute(&admission).await;
    let child_pid = recorded_child_pid(workspace.path());

    assert!(matches!(result, Err(ProcessExecutionError::Timeout(_))));
    assert!(!process_exists(child_pid), "timed-out child survived");
}

#[tokio::test]
async fn cancellation_terminates_the_process_group() {
    let workspace = TempDir::new().expect("workspace");
    let request = request(
        "/bin/sh",
        vec!["-c".into(), "sleep 30 & echo $! > child.pid; wait".into()],
        workspace.path(),
    );
    let admission = admitted(&request, workspace.path());
    let cancellation = CancellationToken::new();
    let canceller = cancellation.clone();
    let workspace_path = workspace.path().to_path_buf();
    let cancel_task = std::thread::spawn(move || {
        let _ = recorded_child_pid(&workspace_path);
        canceller.cancel();
    });
    let provider = production_sandbox_provider();

    let result = request
        .execute_with_provider_and_cancellation(&admission, provider.as_ref(), cancellation, |_| {
            Ok(())
        })
        .await;
    cancel_task.join().expect("cancel task");
    let child_pid = recorded_child_pid(workspace.path());

    assert!(matches!(result, Err(ProcessExecutionError::Cancelled)));
    assert!(!process_exists(child_pid), "cancelled child survived");
}

#[test]
fn production_provider_reports_available_on_supported_macos() {
    production_sandbox_provider()
        .probe()
        .expect("production Seatbelt backend");
}
