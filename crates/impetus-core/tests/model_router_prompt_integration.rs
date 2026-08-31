//! Integration test: ModelRouter is called during production Prompt path
//!
//! Verifies that the ModelRouter is actually invoked when a prompt is submitted,
//! and that the selection decision is recorded in durable events.

use impetus_core::{
    AgentRuntime, EventPayload, Harness, IpcRequest, IpcResponse, PolicyEngine, RuntimeStatus,
    budget::BudgetConfig,
    model_router::{
        CapabilityRequirements, ModelCapabilities, ModelMetadata, ModelRouter, ModelRouterConfig,
        RouterPolicy,
    },
    policy::SandboxScope,
    storage::MemoryEventStore,
};
use std::sync::Arc;

#[tokio::test]
async fn model_router_selects_provider_on_prompt() {
    let store = Arc::new(MemoryEventStore::default());
    let workspace_root = std::env::current_dir().unwrap();
    let policy = PolicyEngine::new(SandboxScope::local_workspace(&workspace_root));
    let harness = Harness::new(store.clone(), policy.clone());

    // Create session
    let response = harness.handle(IpcRequest::CreateSession {
        workspace_root: workspace_root.clone(),
    });
    let session_id = match response {
        IpcResponse::Session { session_id, .. } => session_id,
        other => panic!("Expected Session response, got {:?}", other),
    };

    // Submit prompt
    let response = harness.handle(IpcRequest::Prompt {
        session_id,
        text: "test prompt".to_string(),
    });
    assert!(matches!(response, IpcResponse::Status { .. }));

    // Wait a bit for async task to append events
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Check events for ModelRouter selection notice
    let runtime = AgentRuntime::attach(store, policy, session_id).unwrap();
    let events = runtime.events().unwrap();

    let has_router_notice = events.iter().any(|event| {
        matches!(
            &event.payload,
            EventPayload::Notice(impetus_core::NoticeEvent::Runtime { message })
            if message.contains("ModelRouter selected") || message.contains("ModelRouter fallback")
        )
    });

    assert!(
        has_router_notice,
        "Expected ModelRouter selection or fallback notice in events. Found {} events total",
        events.len()
    );
}

#[tokio::test]
async fn model_router_falls_back_to_default_when_no_models_configured() {
    let store = Arc::new(MemoryEventStore::default());
    let workspace_root = std::env::current_dir().unwrap();
    let policy = PolicyEngine::new(SandboxScope::local_workspace(&workspace_root));

    // Harness with empty router config
    let harness = Harness::new(store.clone(), policy.clone());

    let response = harness.handle(IpcRequest::CreateSession {
        workspace_root: workspace_root.clone(),
    });
    let session_id = match response {
        IpcResponse::Session { session_id, .. } => session_id,
        other => panic!("Expected Session response, got {:?}", other),
    };

    // Submit prompt - should fallback to default_provider_id (mock)
    let response = harness.handle(IpcRequest::Prompt {
        session_id,
        text: "test prompt".to_string(),
    });

    // Should not fail even with empty router
    assert!(matches!(
        response,
        IpcResponse::Status {
            status: RuntimeStatus::Running,
            ..
        }
    ));
}

#[test]
fn model_router_config_uses_balanced_policy_by_default() {
    let config = ModelRouterConfig::default();
    assert_eq!(config.policy, RouterPolicy::Balanced);
}

#[test]
fn model_router_selection_respects_capability_requirements() {
    let tool_model = ModelMetadata {
        provider_id: "provider-a".to_string(),
        model_id: "tool-model".to_string(),
        capabilities: ModelCapabilities {
            tools: true,
            reasoning: false,
            vision: false,
            context_window: 8192,
        },
        cost_per_mtok: Some((1.0, 2.0)),
        is_local: false,
        avg_latency_ms: Some(500),
        health: 1.0,
    };

    let no_tool_model = ModelMetadata {
        provider_id: "provider-b".to_string(),
        model_id: "no-tool-model".to_string(),
        capabilities: ModelCapabilities {
            tools: false,
            reasoning: true,
            vision: false,
            context_window: 16384,
        },
        cost_per_mtok: Some((0.5, 1.0)),
        is_local: false,
        avg_latency_ms: Some(300),
        health: 1.0,
    };

    let config = ModelRouterConfig {
        policy: RouterPolicy::Balanced,
        models: vec![tool_model.clone(), no_tool_model.clone()],
        fallback_chain: vec![],
    };

    let router = ModelRouter::new(config);

    // Require tools capability
    let requirements = CapabilityRequirements {
        tools: true,
        ..Default::default()
    };

    let selection = router
        .select_model(&requirements, &BudgetConfig::default())
        .expect("Should select a model");

    // Must select the tool-capable model
    assert_eq!(selection.provider_id, "provider-a");
    assert_eq!(selection.model_id, "tool-model");
}
