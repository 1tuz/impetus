//! Real daemon-boundary workflow E2E (Start/Advance/Cancel over Unix socket).
//!
//! Covers the #322 hole: unit tests exercise AgentLoop roles, but
//! `daemon_unix_e2e` never called workflow IPC. No transport mocks.

mod common;

use common::{DaemonFixture, workspace_root};
use impetus_client::{HarnessClient, UnixSocketTransport, WorkflowIpcStatus};

fn is_terminal(status: &str) -> bool {
    matches!(
        status,
        "Completed" | "Failed" | "Cancelled" | "BudgetExhausted"
    )
}

fn assert_agent_loop_summary(summary: &str) {
    assert!(
        summary.contains("Mock response"),
        "expected AgentLoop mock output, not git stub / empty: {summary:?}"
    );
    assert!(
        !summary.contains("git status"),
        "git stub must not appear in workflow summary: {summary:?}"
    );
}

async fn session_with_mock(client: &UnixSocketTransport) -> uuid::Uuid {
    let _ = client.hello().await.expect("hello");
    let session_id = client
        .create_session(workspace_root())
        .await
        .expect("create session");
    let _ = client
        .set_session_model(
            session_id,
            "mock".into(),
            "mock-model".into(),
            Some("high".into()),
            None,
            serde_json::Value::Null,
        )
        .await
        .expect("set mock session model");
    session_id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_workflow_feature_advance_to_completed() {
    let daemon = DaemonFixture::spawn();
    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect + Hello");
    let session_id = session_with_mock(&client).await;

    let started = client
        .start_workflow(session_id, "feature")
        .await
        .expect("StartWorkflow feature");
    assert_eq!(started.session_id, session_id);
    assert!(
        matches!(started.status.as_str(), "Idle" | "Running"),
        "start status: {}",
        started.status
    );

    let mut saw_agent_loop = false;
    let mut last = started;
    for step in 0..16 {
        let advanced = client
            .advance_workflow(session_id)
            .await
            .unwrap_or_else(|e| panic!("AdvanceWorkflow step {step}: {e}"));
        assert_eq!(advanced.session_id, session_id);
        if let Some(summary) = advanced.last_summary.as_deref() {
            if summary.contains("Mock response") {
                saw_agent_loop = true;
                assert_agent_loop_summary(summary);
            }
            // Approval skip is honest label, not fake agent success.
            if summary == "approval-skip" {
                assert_ne!(
                    advanced.status, "Failed",
                    "approval-skip must not mark Failed"
                );
            }
        }
        last = advanced;
        if is_terminal(&last.status) {
            break;
        }
    }

    assert!(
        saw_agent_loop,
        "at least one step must show Mock response (AgentLoopRoleExecutor / Explore)"
    );
    assert!(
        matches!(last.status.as_str(), "Completed" | "Failed"),
        "terminal must be Completed or honest Failed, got {}",
        last.status
    );
    // Fake success on failure forbidden: if Failed, summary must still be real evidence.
    if last.status == "Failed" {
        let summary = last.last_summary.as_deref().unwrap_or("").to_string();
        assert!(
            !summary.is_empty() && !summary.contains("git status"),
            "Failed without honest evidence: {summary:?}"
        );
    } else {
        assert_eq!(last.status, "Completed");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_workflow_cancel_after_first_advance() {
    let daemon = DaemonFixture::spawn();
    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect + Hello");
    let session_id = session_with_mock(&client).await;

    client
        .start_workflow(session_id, "feature")
        .await
        .expect("StartWorkflow");

    let first: WorkflowIpcStatus = client
        .advance_workflow(session_id)
        .await
        .expect("AdvanceWorkflow research");
    let summary = first
        .last_summary
        .as_deref()
        .expect("research step summary");
    assert_agent_loop_summary(summary);
    assert!(
        !is_terminal(&first.status),
        "first research step should leave workflow non-terminal, got {}",
        first.status
    );

    let cancelled = client
        .cancel_workflow(session_id)
        .await
        .expect("CancelWorkflow");
    assert_eq!(cancelled.session_id, session_id);
    assert_eq!(cancelled.status, "Cancelled");
}
