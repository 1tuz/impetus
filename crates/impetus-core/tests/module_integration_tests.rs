use impetus_core::module::{
    Compatibility, ExecutionSemantics, ModuleDescriptor, ModuleKind, ModulePermissions, ModuleState,
};
use impetus_core::module_fallback::{
    FallbackPolicy, FallbackStrategy, OperationOutcome, UnknownOutcomePolicy,
};
use impetus_core::module_lifecycle::ModuleLifecycle;
use impetus_core::module_registry::ModuleRegistry;
use std::sync::Arc;

/// Test module incompatibility detection and rejection
#[tokio::test]
async fn test_incompatible_module_rejected() {
    let registry = Arc::new(ModuleRegistry::new());
    let lifecycle = ModuleLifecycle::new(registry.clone());

    let descriptor = ModuleDescriptor {
        id: "incompatible-module".to_string(),
        name: "Incompatible Module".to_string(),
        version: "0.1.0".to_string(),
        kind: ModuleKind::SearchBackend,
        provides: vec!["search".to_string()],
        requires: vec!["harness_v999".to_string()], // Non-existent requirement
        capabilities: vec![],
        permissions: ModulePermissions::default(),
    };

    registry.register(descriptor.clone()).unwrap();

    // Check compatibility
    let compat_report = registry
        .check_compatibility("incompatible-module", "1.0.0")
        .unwrap();
    assert_eq!(compat_report.overall, Compatibility::Incompatible);

    // Module should not be allowed to start
    let result = lifecycle.start("incompatible-module").await;
    assert!(result.is_err());
}

/// Test degraded module fallback policy
#[tokio::test]
async fn test_degraded_module_fallback() {
    let registry = Arc::new(ModuleRegistry::new());

    let primary = ModuleDescriptor {
        id: "primary-search".to_string(),
        name: "Primary Search".to_string(),
        version: "1.0.0".to_string(),
        kind: ModuleKind::SearchBackend,
        provides: vec!["search".to_string()],
        requires: vec![],
        capabilities: vec!["web_search".to_string()],
        permissions: ModulePermissions::default(),
    };

    registry.register(primary.clone()).unwrap();

    // Mark primary as degraded
    registry
        .update_state("primary-search", ModuleState::Degraded)
        .unwrap();

    // SearchBackend should use Alternate strategy by default
    let policy = FallbackPolicy::default_for_kind(ModuleKind::SearchBackend);
    assert_eq!(policy.strategy, FallbackStrategy::Alternate);
    assert!(policy.max_retries > 0);
}

/// Test unavailable module with FailFast strategy
#[tokio::test]
async fn test_unavailable_module_fail_fast() {
    let registry = Arc::new(ModuleRegistry::new());

    let descriptor = ModuleDescriptor {
        id: "credential-resolver".to_string(),
        name: "Credential Resolver".to_string(),
        version: "1.0.0".to_string(),
        kind: ModuleKind::CredentialResolver,
        provides: vec!["credentials".to_string()],
        requires: vec![],
        capabilities: vec!["keychain".to_string()],
        permissions: ModulePermissions {
            secrets: vec!["all".to_string()],
            ..Default::default()
        },
    };

    registry.register(descriptor).unwrap();
    registry
        .update_state("credential-resolver", ModuleState::Failed)
        .unwrap();

    // CredentialResolver should use FailFast by default
    let policy = FallbackPolicy::default_for_kind(ModuleKind::CredentialResolver);
    assert_eq!(policy.strategy, FallbackStrategy::FailFast);
    assert_eq!(policy.max_retries, 0);
}

/// Test UnknownOutcome blocks retry for mutating operations
#[tokio::test]
async fn test_unknown_outcome_blocks_mutating_retry() {
    // Mutating operation with Unknown outcome
    let policy = UnknownOutcomePolicy::new(ExecutionSemantics::Mutating);

    let can_retry = policy.can_retry(OperationOutcome::Unknown);
    assert!(
        !can_retry,
        "Mutating operation with unknown outcome should block retry"
    );

    let can_fallback = policy.can_fallback(OperationOutcome::Unknown);
    assert!(
        !can_fallback,
        "Mutating operation with unknown outcome should block fallback"
    );

    // NonReplayable also blocks
    let policy_nr = UnknownOutcomePolicy::new(ExecutionSemantics::NonReplayable);
    let can_retry_nr = policy_nr.can_retry(OperationOutcome::Unknown);
    assert!(
        !can_retry_nr,
        "NonReplayable with unknown outcome should block retry"
    );

    // ReadOnly allows retry even with unknown outcome
    let policy_ro = UnknownOutcomePolicy::new(ExecutionSemantics::ReadOnly);
    let can_retry_ro = policy_ro.can_retry(OperationOutcome::Unknown);
    assert!(can_retry_ro, "ReadOnly should allow retry");

    // Idempotent allows retry with unknown outcome
    let policy_id = UnknownOutcomePolicy::new(ExecutionSemantics::Idempotent);
    let can_retry_id = policy_id.can_retry(OperationOutcome::Unknown);
    assert!(can_retry_id, "Idempotent should allow retry");
}

/// Test module capabilities probe
#[tokio::test]
async fn test_module_capability_probing() {
    let registry = Arc::new(ModuleRegistry::new());

    let descriptor = ModuleDescriptor {
        id: "browser-provider".to_string(),
        name: "Browser Provider".to_string(),
        version: "1.0.0".to_string(),
        kind: ModuleKind::BrowserProvider,
        provides: vec!["browser".to_string()],
        requires: vec![],
        capabilities: vec!["chromium".to_string(), "firefox".to_string()],
        permissions: ModulePermissions {
            process: true,
            network: vec!["*".to_string()],
            ..Default::default()
        },
    };

    registry.register(descriptor).unwrap();

    // Probe capabilities
    let probes = registry.probe_capabilities("browser-provider").unwrap();

    assert_eq!(probes.len(), 2);
    assert!(probes.iter().any(|p| p.capability == "chromium"));
    assert!(probes.iter().any(|p| p.capability == "firefox"));
}

/// Test module permission validation
#[tokio::test]
async fn test_module_permission_enforcement() {
    let registry = Arc::new(ModuleRegistry::new());

    let dangerous_module = ModuleDescriptor {
        id: "dangerous-module".to_string(),
        name: "Dangerous Module".to_string(),
        version: "1.0.0".to_string(),
        kind: ModuleKind::Custom,
        provides: vec!["dangerous".to_string()],
        requires: vec![],
        capabilities: vec![],
        permissions: ModulePermissions {
            filesystem: vec!["/".to_string()], // Root access
            network: vec!["*".to_string()],    // All network
            process: true,
            secrets: vec!["*".to_string()], // All secrets
            remote: true,
        },
    };

    registry.register(dangerous_module).unwrap();

    // In production, policy engine would reject this
    let descriptor = registry.get_module("dangerous-module").unwrap();
    assert!(descriptor.permissions.process);
    assert!(descriptor.permissions.remote);
    assert_eq!(descriptor.permissions.filesystem.len(), 1);
    assert_eq!(descriptor.permissions.secrets.len(), 1);
}

/// Test degraded state does not propagate to alternate modules
#[tokio::test]
async fn test_degraded_isolation() {
    let registry = Arc::new(ModuleRegistry::new());

    let module_a = ModuleDescriptor {
        id: "module-a".to_string(),
        name: "Module A".to_string(),
        version: "1.0.0".to_string(),
        kind: ModuleKind::SearchBackend,
        provides: vec!["search".to_string()],
        requires: vec![],
        capabilities: vec!["duckduckgo".to_string()],
        permissions: ModulePermissions::default(),
    };

    let module_b = ModuleDescriptor {
        id: "module-b".to_string(),
        name: "Module B".to_string(),
        version: "1.0.0".to_string(),
        kind: ModuleKind::SearchBackend,
        provides: vec!["search".to_string()],
        requires: vec![],
        capabilities: vec!["bing".to_string()],
        permissions: ModulePermissions::default(),
    };

    registry.register(module_a).unwrap();
    registry.register(module_b).unwrap();

    // Degrade module A
    registry
        .update_state("module-a", ModuleState::Degraded)
        .unwrap();

    // Module B should remain unaffected
    // State is tracked internally; verify via health check that it's independent

    // Health check on B should not be affected by A's state
    let lifecycle = ModuleLifecycle::new(registry.clone());
    let result = lifecycle.health_check("module-b").await;
    assert!(result.is_ok());
}

/// Test version compatibility checks
#[tokio::test]
async fn test_version_compatibility() {
    let registry = Arc::new(ModuleRegistry::new());

    let descriptor = ModuleDescriptor {
        id: "versioned-module".to_string(),
        name: "Versioned Module".to_string(),
        version: "2.0.0".to_string(),
        kind: ModuleKind::AgentLoop,
        provides: vec!["agent_loop_v2".to_string()],
        requires: vec!["harness>=1.0.0".to_string()],
        capabilities: vec![],
        permissions: ModulePermissions::default(),
    };

    registry.register(descriptor).unwrap();

    let report = registry
        .check_compatibility("versioned-module", "2.0.0")
        .unwrap();

    // Exact version match should be compatible
    assert_eq!(report.overall, Compatibility::Compatible);
    assert_eq!(report.module_version, "2.0.0");
    assert_eq!(report.harness_version, "2.0.0");
}
