//! Integration test: Budget tracking and model routing vertical slice
//!
//! Demonstrates:
//! - Budget configuration and enforcement
//! - Durable budget state persistence
//! - Model router selection based on capabilities and policies
//! - Cost estimation and warnings

use impetus_core::{
    AgentRuntime, PolicyEngine,
    budget::{BudgetChecker, BudgetConfig, BudgetError},
    cost_estimation::{BudgetWarningLevel, estimate_cost},
    model_router::{
        CapabilityRequirements, ModelCapabilities, ModelMetadata, ModelRouter, ModelRouterConfig,
        RouterPolicy,
    },
    policy::SandboxScope,
    storage::MemoryEventStore,
};
use std::sync::Arc;

#[test]
fn budget_enforcement_prevents_overrun() {
    let config = BudgetConfig {
        max_tokens: Some(1000),
        max_turns: Some(5),
        max_wall_time: None,
        context_limit: Some(8192),
        ..Default::default()
    };

    let mut checker = BudgetChecker::new(config);

    // First turn OK
    assert!(checker.check_all(200).is_ok());
    checker.record_turn(200);
    assert_eq!(checker.state().tokens_used, 200);
    assert_eq!(checker.state().turns_used, 1);

    // Second turn OK
    assert!(checker.check_all(300).is_ok());
    checker.record_turn(300);
    assert_eq!(checker.state().tokens_used, 500);

    // Third turn would exceed
    let result = checker.check_all(600);
    assert!(matches!(
        result,
        Err(BudgetError::TokenLimitExceeded { .. })
    ));
}

#[test]
fn model_router_selects_by_policy() {
    let local_model = ModelMetadata {
        provider_id: "ollama".to_string(),
        model_id: "llama3:8b".to_string(),
        capabilities: ModelCapabilities {
            tools: true,
            reasoning: false,
            vision: false,
            context_window: 8192,
        },
        cost_per_mtok: Some((0.0, 0.0)),
        is_local: true,
        avg_latency_ms: Some(500),
        health: 1.0,
    };

    let cloud_model = ModelMetadata {
        provider_id: "openai".to_string(),
        model_id: "gpt-4o".to_string(),
        capabilities: ModelCapabilities {
            tools: true,
            reasoning: false,
            vision: true,
            context_window: 128000,
        },
        cost_per_mtok: Some((5.0, 15.0)),
        is_local: false,
        avg_latency_ms: Some(2000),
        health: 1.0,
    };

    // LocalFirst policy prefers local
    let config = ModelRouterConfig {
        policy: RouterPolicy::LocalFirst,
        models: vec![local_model.clone(), cloud_model.clone()],
        fallback_chain: vec![],
    };
    let router = ModelRouter::new(config);

    let requirements = CapabilityRequirements {
        tools: true,
        ..Default::default()
    };

    let selection = router
        .select_model(&requirements, &BudgetConfig::default())
        .unwrap();
    assert_eq!(selection.model_id, "llama3:8b");

    // QualityFirst policy prefers larger context
    let config = ModelRouterConfig {
        policy: RouterPolicy::QualityFirst,
        models: vec![local_model.clone(), cloud_model.clone()],
        fallback_chain: vec![],
    };
    let router = ModelRouter::new(config);

    let selection = router
        .select_model(&requirements, &BudgetConfig::default())
        .unwrap();
    assert_eq!(selection.model_id, "gpt-4o");

    // Vision requirement filters models
    let requirements = CapabilityRequirements {
        vision: true,
        ..Default::default()
    };

    let selection = router
        .select_model(&requirements, &BudgetConfig::default())
        .unwrap();
    assert_eq!(selection.model_id, "gpt-4o"); // Only gpt-4o has vision
}

#[test]
fn cost_estimation_and_warnings() {
    let model = ModelMetadata {
        provider_id: "openai".to_string(),
        model_id: "gpt-4o".to_string(),
        capabilities: ModelCapabilities {
            tools: true,
            reasoning: false,
            vision: false,
            context_window: 128000,
        },
        cost_per_mtok: Some((5.0, 15.0)),
        is_local: false,
        avg_latency_ms: Some(2000),
        health: 1.0,
    };

    // Estimate cost for typical request
    let input_tokens = 5000;
    let output_tokens = 1000;
    let cost = estimate_cost(&model, input_tokens, output_tokens).unwrap();
    assert!((cost - 0.04).abs() < 0.001); // $0.025 + $0.015 = $0.04

    // Budget warnings at thresholds
    assert!(matches!(
        BudgetWarningLevel::from_usage_percent(30),
        BudgetWarningLevel::None
    ));
    assert!(matches!(
        BudgetWarningLevel::from_usage_percent(85),
        BudgetWarningLevel::Medium
    ));
    assert!(matches!(
        BudgetWarningLevel::from_usage_percent(97),
        BudgetWarningLevel::High
    ));

    let warning = BudgetWarningLevel::High.message(97);
    assert!(warning.unwrap().contains("critical"));
}

#[test]
fn vertical_slice_budget_router_integration() {
    // Setup: store, runtime, budget, router
    let store = Arc::new(MemoryEventStore::default());
    let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
    let mut runtime = AgentRuntime::new(store.clone(), policy);

    let budget_config = BudgetConfig {
        max_tokens: Some(10000),
        max_turns: Some(10),
        ..Default::default()
    };
    runtime.set_budget(budget_config.clone()).unwrap();

    let models = vec![
        ModelMetadata {
            provider_id: "ollama".to_string(),
            model_id: "llama3:8b".to_string(),
            capabilities: ModelCapabilities {
                tools: true,
                reasoning: false,
                vision: false,
                context_window: 8192,
            },
            cost_per_mtok: Some((0.0, 0.0)),
            is_local: true,
            avg_latency_ms: Some(500),
            health: 1.0,
        },
        ModelMetadata {
            provider_id: "openai".to_string(),
            model_id: "gpt-4o".to_string(),
            capabilities: ModelCapabilities {
                tools: true,
                reasoning: false,
                vision: false,
                context_window: 128000,
            },
            cost_per_mtok: Some((5.0, 15.0)),
            is_local: false,
            avg_latency_ms: Some(2000),
            health: 1.0,
        },
    ];

    let router_config = ModelRouterConfig {
        policy: RouterPolicy::LocalFirst,
        models,
        fallback_chain: vec!["llama3:8b".to_string(), "gpt-4o".to_string()],
    };
    let router = ModelRouter::new(router_config);

    // Turn 1: Select local model
    let requirements = CapabilityRequirements {
        tools: true,
        ..Default::default()
    };
    let selection = router.select_model(&requirements, &budget_config).unwrap();
    assert_eq!(selection.model_id, "llama3:8b");

    // Check budget before turn
    runtime.check_budget(2000).unwrap();

    // Execute turn and record
    runtime.record_turn(2000).unwrap();
    assert_eq!(runtime.budget_state().unwrap().turns_used, 1);
    assert_eq!(runtime.budget_state().unwrap().tokens_used, 2000);

    // Turn 2: Still within budget
    runtime.check_budget(3000).unwrap();
    runtime.record_turn(3000).unwrap();
    assert_eq!(runtime.budget_state().unwrap().tokens_used, 5000);

    // Turn 3: Would exceed if we try 6000 more
    let result = runtime.check_budget(6000);
    assert!(result.is_err());

    // Turn 3: OK with 4000
    runtime.check_budget(4000).unwrap();
    runtime.record_turn(4000).unwrap();
    assert_eq!(runtime.budget_state().unwrap().tokens_used, 9000);

    // Fallback if local model fails (technical failure: unavailable, rate limit, etc.)
    let fallback = router.fallback("llama3:8b").unwrap();
    assert_eq!(fallback.model_id, "gpt-4o");

    // Budget state persisted in runtime
    let state = runtime.budget_state().unwrap();
    assert_eq!(state.tokens_used, 9000);
    assert_eq!(state.turns_used, 3);
}
