//! Integration test: Error recovery and retry logic in agent loop

use impetus_core::{
    AgentRuntime, EventPayload, MockProvider, MockProviderItem, PolicyEngine, ProviderMessage,
    RetryEvent, SandboxScope,
};
use std::sync::Arc;

#[tokio::test]
async fn transient_error_triggers_retry_with_backoff() {
    let store = Arc::new(impetus_core::MemoryEventStore::default());
    let workspace = std::env::temp_dir().join("retry_test");
    std::fs::create_dir_all(&workspace).unwrap();

    let scope = SandboxScope {
        workspace_root: workspace.clone(),
        allow_network: false,
        allowed_hosts: vec![],
    };
    let policy = PolicyEngine::new(scope);
    let runtime = AgentRuntime::new(store.clone(), policy.clone());

    // Mock provider: first call fails with transient error, second succeeds
    let mock = Arc::new(MockProvider::scripted(
        "mock",
        "mock-model",
        vec![
            vec![MockProviderItem::TransientError {
                message: "Rate limit exceeded".to_string(),
            }],
            vec![MockProviderItem::Chunk {
                chunk_id: 1,
                text: "Success after retry".to_string(),
            }],
        ],
    ));

    let messages = vec![ProviderMessage::user("test request")];
    let runtime_arc = Arc::new(runtime);
    let agent_loop = impetus_core::AgentLoop::new(runtime_arc.clone());
    let cancellation = tokio_util::sync::CancellationToken::new();

    let run_id = runtime_arc.start_run().unwrap();

    let result = agent_loop
        .execute(run_id, mock.clone(), messages, cancellation)
        .await;

    // Should succeed after retry
    assert!(result.is_ok(), "Expected success after retry");

    runtime_arc
        .finish_run(impetus_core::RunEvent::Completed { run_id })
        .unwrap();

    // Verify retry events were emitted
    let events = runtime_arc.events().unwrap();
    let retry_events: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::Retry(r) => Some(r.clone()),
            _ => None,
        })
        .collect();

    // Should have: Attempting, Succeeded
    assert!(
        retry_events.len() >= 2,
        "Expected at least 2 retry events, got {}",
        retry_events.len()
    );

    // Check for RetryEvent::Attempting
    let has_attempting = retry_events.iter().any(|e| {
        matches!(
            e,
            RetryEvent::Attempting {
                attempt: 1,
                max_attempts: 3,
                ..
            }
        )
    });
    assert!(has_attempting, "Expected RetryEvent::Attempting");

    // Check for RetryEvent::Succeeded
    let has_succeeded = retry_events
        .iter()
        .any(|e| matches!(e, RetryEvent::Succeeded { attempt: 2 }));
    assert!(has_succeeded, "Expected RetryEvent::Succeeded");
}

#[tokio::test]
async fn permanent_error_fails_immediately() {
    let store = Arc::new(impetus_core::MemoryEventStore::default());
    let workspace = std::env::temp_dir().join("permanent_error_test");
    std::fs::create_dir_all(&workspace).unwrap();

    let scope = SandboxScope {
        workspace_root: workspace.clone(),
        allow_network: false,
        allowed_hosts: vec![],
    };
    let policy = PolicyEngine::new(scope);
    let runtime = AgentRuntime::new(store.clone(), policy.clone());

    // Mock provider: permanent error
    let mock = Arc::new(MockProvider::new(
        "mock",
        "mock-model",
        vec![MockProviderItem::PermanentError {
            message: "Invalid credentials".to_string(),
        }],
    ));

    let messages = vec![ProviderMessage::user("test request")];
    let runtime_arc = Arc::new(runtime);
    let agent_loop = impetus_core::AgentLoop::new(runtime_arc.clone());
    let cancellation = tokio_util::sync::CancellationToken::new();

    let run_id = runtime_arc.start_run().unwrap();

    let result = agent_loop
        .execute(run_id, mock.clone(), messages, cancellation)
        .await;

    // Should fail immediately
    assert!(result.is_err(), "Expected immediate failure");

    // Verify NO retry events were emitted (permanent error)
    let events = runtime_arc.events().unwrap();
    let retry_events: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::Retry(r) => Some(r),
            _ => None,
        })
        .collect();

    assert_eq!(
        retry_events.len(),
        0,
        "Expected no retry events for permanent error"
    );
}

#[tokio::test]
async fn retry_exhaustion_emits_exhausted_event() {
    let store = Arc::new(impetus_core::MemoryEventStore::default());
    let workspace = std::env::temp_dir().join("retry_exhaustion_test");
    std::fs::create_dir_all(&workspace).unwrap();

    let scope = SandboxScope {
        workspace_root: workspace.clone(),
        allow_network: false,
        allowed_hosts: vec![],
    };
    let policy = PolicyEngine::new(scope);
    let runtime = AgentRuntime::new(store.clone(), policy.clone());

    // Mock provider: always fails with transient error (3 attempts)
    let mock = Arc::new(MockProvider::scripted(
        "mock",
        "mock-model",
        vec![
            vec![MockProviderItem::TransientError {
                message: "Timeout".to_string(),
            }],
            vec![MockProviderItem::TransientError {
                message: "Timeout".to_string(),
            }],
            vec![MockProviderItem::TransientError {
                message: "Timeout".to_string(),
            }],
        ],
    ));

    let messages = vec![ProviderMessage::user("test request")];
    let runtime_arc = Arc::new(runtime);
    let agent_loop = impetus_core::AgentLoop::new(runtime_arc.clone());
    let cancellation = tokio_util::sync::CancellationToken::new();

    let run_id = runtime_arc.start_run().unwrap();

    let result = agent_loop
        .execute(run_id, mock.clone(), messages, cancellation)
        .await;

    // Should fail after exhausting retries
    assert!(result.is_err(), "Expected failure after retry exhaustion");

    // Verify RetryEvent::Exhausted was emitted
    let events = runtime_arc.events().unwrap();
    let has_exhausted = events.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::Retry(RetryEvent::Exhausted { attempts: 3, .. })
        )
    });

    assert!(has_exhausted, "Expected RetryEvent::Exhausted");
}
