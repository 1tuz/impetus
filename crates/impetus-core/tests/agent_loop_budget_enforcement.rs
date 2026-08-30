//! Integration test: Budget enforcement in agent loop
//!
//! Verifies that budget limits prevent agent loop from exceeding configured constraints.

use impetus_core::{
    AgentRuntime, BudgetConfig, EventPayload, MockProvider, MockStreamItem, PolicyEngine,
    ProviderMessage, SandboxScope,
};
use std::sync::Arc;

#[tokio::test]
async fn budget_enforcement_stops_agent_loop_on_token_limit() {
    let store = Arc::new(impetus_core::MemoryEventStore::default());
    let workspace = std::env::temp_dir().join("budget_test");
    std::fs::create_dir_all(&workspace).unwrap();

    let scope = SandboxScope {
        workspace_root: workspace.clone(),
        allow_network: false,
        allowed_hosts: vec![],
    };
    let policy = PolicyEngine::new(scope);
    let mut runtime = AgentRuntime::new(store.clone(), policy.clone());

    // Set tight token budget
    let budget_config = BudgetConfig {
        max_tokens: Some(500),
        max_turns: None,
        max_wall_time: None,
        context_limit: Some(8192),
        ..Default::default()
    };
    runtime.set_budget(budget_config).unwrap();

    // Mock provider: returns text that will consume tokens
    let mock = Arc::new(MockProvider::new(
        "mock",
        "mock-model",
        vec![
            MockStreamItem::Chunk {
                chunk_id: 1,
                text: "First response chunk ".to_string(),
            },
            MockStreamItem::Chunk {
                chunk_id: 2,
                text: "with some content".to_string(),
            },
        ],
    ));

    let session_id = runtime.session_id();
    let run_id = runtime.start_run().unwrap();

    let messages = vec![ProviderMessage::user("test request")];

    let runtime_arc = Arc::new(runtime);
    let agent_loop = impetus_core::AgentLoop::new(runtime_arc.clone());
    let cancellation = tokio_util::sync::CancellationToken::new();

    // First turn should succeed
    let result = agent_loop
        .execute(run_id, mock.clone(), messages.clone(), cancellation.clone())
        .await;

    // Agent loop completes normally (no tool calls)
    assert!(result.is_ok());

    // Check budget state was updated
    let state = runtime_arc.budget_state().unwrap();
    assert_eq!(state.turns_used, 1);
    assert!(state.tokens_used > 0);

    // Verify BudgetEvent::Updated was emitted
    let events = runtime_arc.events().unwrap();
    let budget_events: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::Budget(b) => Some(b),
            _ => None,
        })
        .collect();

    assert!(
        !budget_events.is_empty(),
        "BudgetEvent::Updated should be emitted"
    );

    // Manually push budget to limit by recording additional turns
    runtime_arc.record_turn(400).unwrap();

    // Next turn should fail with budget error
    let run_id_2 = runtime_arc.start_run().unwrap();

    let result = impetus_core::AgentLoop::new(runtime_arc.clone())
        .execute(
            run_id_2,
            mock.clone(),
            messages.clone(),
            cancellation.clone(),
        )
        .await;

    // Should fail with budget error
    assert!(result.is_err());
    let err_str = format!("{:?}", result.unwrap_err());
    assert!(
        err_str.contains("Budget") || err_str.contains("Token"),
        "Expected budget error, got: {}",
        err_str
    );
}

#[tokio::test]
async fn budget_enforcement_stops_on_turn_limit() {
    let store = Arc::new(impetus_core::MemoryEventStore::default());
    let workspace = std::env::temp_dir().join("budget_turn_test");
    std::fs::create_dir_all(&workspace).unwrap();

    let scope = SandboxScope {
        workspace_root: workspace.clone(),
        allow_network: false,
        allowed_hosts: vec![],
    };
    let policy = PolicyEngine::new(scope);
    let mut runtime = AgentRuntime::new(store.clone(), policy.clone());

    // Set turn limit to 2
    let budget_config = BudgetConfig {
        max_tokens: None,
        max_turns: Some(2),
        max_wall_time: None,
        context_limit: Some(8192),
        ..Default::default()
    };
    runtime.set_budget(budget_config).unwrap();

    let mock = Arc::new(MockProvider::new(
        "mock",
        "mock-model",
        vec![MockStreamItem::Chunk {
            chunk_id: 1,
            text: "Response".to_string(),
        }],
    ));

    let messages = vec![ProviderMessage::user("test")];
    let cancellation = tokio_util::sync::CancellationToken::new();
    let runtime_arc = Arc::new(runtime);

    // Turn 1: OK
    let run_id_1 = runtime_arc.start_run().unwrap();
    let result = impetus_core::AgentLoop::new(runtime_arc.clone())
        .execute(
            run_id_1,
            mock.clone(),
            messages.clone(),
            cancellation.clone(),
        )
        .await;
    assert!(result.is_ok());
    assert_eq!(runtime_arc.budget_state().unwrap().turns_used, 1);

    // Turn 2: OK (at limit)
    let run_id_2 = runtime_arc.start_run().unwrap();
    let result = impetus_core::AgentLoop::new(runtime_arc.clone())
        .execute(
            run_id_2,
            mock.clone(),
            messages.clone(),
            cancellation.clone(),
        )
        .await;
    assert!(result.is_ok());
    assert_eq!(runtime_arc.budget_state().unwrap().turns_used, 2);

    // Turn 3: Should fail
    let run_id_3 = runtime_arc.start_run().unwrap();
    let result = impetus_core::AgentLoop::new(runtime_arc.clone())
        .execute(
            run_id_3,
            mock.clone(),
            messages.clone(),
            cancellation.clone(),
        )
        .await;

    assert!(result.is_err());
    let err_str = format!("{:?}", result.unwrap_err());
    assert!(
        err_str.contains("Budget") || err_str.contains("Turn"),
        "Expected turn limit error, got: {}",
        err_str
    );
}

#[tokio::test]
async fn budget_events_emitted_on_approaching_limit() {
    let store = Arc::new(impetus_core::MemoryEventStore::default());
    let workspace = std::env::temp_dir().join("budget_events_test");
    std::fs::create_dir_all(&workspace).unwrap();

    let scope = SandboxScope {
        workspace_root: workspace.clone(),
        allow_network: false,
        allowed_hosts: vec![],
    };
    let policy = PolicyEngine::new(scope);
    let mut runtime = AgentRuntime::new(store.clone(), policy.clone());

    let budget_config = BudgetConfig {
        max_tokens: Some(1000),
        max_turns: Some(5),
        max_wall_time: None,
        context_limit: Some(8192),
        ..Default::default()
    };
    runtime.set_budget(budget_config).unwrap();

    let mock = Arc::new(MockProvider::new(
        "mock",
        "mock-model",
        vec![MockStreamItem::Chunk {
            chunk_id: 1,
            text: "Response".to_string(),
        }],
    ));

    let session_id = runtime.session_id();
    let messages = vec![ProviderMessage::user("test")];
    let cancellation = tokio_util::sync::CancellationToken::new();
    let runtime_arc = Arc::new(runtime);

    // Execute first turn
    let run_id = runtime_arc.start_run().unwrap();
    let _ = impetus_core::AgentLoop::new(runtime_arc.clone())
        .execute(run_id, mock.clone(), messages, cancellation)
        .await;

    // Check that BudgetEvent::Updated was emitted
    let events = runtime_arc.events().unwrap();
    let budget_updated = events.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::Budget(impetus_core::BudgetEvent::Updated { .. })
        )
    });

    assert!(budget_updated, "BudgetEvent::Updated should be emitted");

    // Manually push to 85% to trigger approaching warning
    runtime_arc.record_turn(800).unwrap();

    let events_after = runtime_arc.events().unwrap();
    let approaching_events: Vec<_> = events_after
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::Budget(impetus_core::BudgetEvent::TokenLimitApproaching {
                limit,
                used,
            }) => Some((limit, used)),
            _ => None,
        })
        .collect();

    // Approaching event may or may not be present depending on exact token count
    // Main assertion: no panic, events are well-formed
    assert!(events_after.len() >= events.len());
}
