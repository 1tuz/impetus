use impetus_core::{
    Profile, ProfileConfig, ServiceBinding, ServiceBindings, ServiceProvider, ServiceProviderKind,
};

#[test]
fn test_profile_defaults() {
    assert_eq!(Profile::default(), Profile::Standard);

    let standard = Profile::Standard.default_bindings();
    assert_eq!(
        standard.agent_loop,
        ServiceBinding::Builtin {
            variant: "standard".to_string()
        }
    );

    let minimal = Profile::Minimal.default_bindings();
    assert_eq!(minimal.context, ServiceBinding::Disabled);
    assert_eq!(minimal.memory, ServiceBinding::Disabled);
}

#[test]
fn test_profile_config_resolution() {
    let config = ProfileConfig {
        profile: Profile::Standard,
        services: Some(ServiceBindings {
            agent_loop: ServiceBinding::Custom {
                module_id: "my-agent-loop".to_string(),
            },
            context: ServiceBinding::External {
                module_id: "custom-context".to_string(),
            },
            ..Profile::Standard.default_bindings()
        }),
    };

    let resolved = config.resolve_bindings();

    // Overridden services
    assert_eq!(
        resolved.agent_loop,
        ServiceBinding::Custom {
            module_id: "my-agent-loop".to_string()
        }
    );
    assert_eq!(
        resolved.context,
        ServiceBinding::External {
            module_id: "custom-context".to_string()
        }
    );

    // Default services unchanged
    assert_eq!(
        resolved.scheduler,
        ServiceBinding::Builtin {
            variant: "standard".to_string()
        }
    );
}

#[test]
fn test_service_provider_kinds() {
    #[derive(Debug, Clone)]
    struct TestService;

    let builtin = ServiceProvider::builtin(TestService);
    assert_eq!(builtin.kind(), ServiceProviderKind::Builtin);
    assert!(builtin.as_builtin().is_some());

    let handle =
        impetus_core::ExternalServiceHandle::new("test".to_string(), "/tmp/test.sock".to_string());
    let external: ServiceProvider<TestService> = ServiceProvider::external(handle);
    assert_eq!(external.kind(), ServiceProviderKind::External);
    assert!(external.as_builtin().is_none());
}

#[test]
fn test_resolved_service_states() {
    #[derive(Debug, Clone)]
    struct TestService {
        #[allow(dead_code)]
        name: String,
    }

    let ready = impetus_core::ResolvedService::Ready(TestService {
        name: "primary".to_string(),
    });
    assert!(ready.is_ready());
    assert!(!ready.is_degraded());
    assert!(ready.service().is_some());

    let degraded = impetus_core::ResolvedService::Degraded {
        fallback: TestService {
            name: "fallback".to_string(),
        },
        reason: "Primary unavailable".to_string(),
    };
    assert!(!degraded.is_ready());
    assert!(degraded.is_degraded());
    assert!(degraded.service().is_some());

    let unavailable: impetus_core::ResolvedService<TestService> =
        impetus_core::ResolvedService::Unavailable {
            reason: "Not configured".to_string(),
        };
    assert!(!unavailable.is_ready());
    assert!(!unavailable.is_degraded());
    assert!(unavailable.service().is_none());
}

#[test]
fn test_profile_descriptions() {
    assert_eq!(
        Profile::Standard.description(),
        "Zero-config daily use with safe defaults"
    );
    assert_eq!(
        Profile::Minimal.description(),
        "Minimal runtime for debugging and benchmarks"
    );
    assert_eq!(
        Profile::Creator.description(),
        "Advanced customization and introspection enabled"
    );
}

#[test]
fn test_service_binding_serialization() {
    let builtin = ServiceBinding::Builtin {
        variant: "standard".to_string(),
    };
    let json = serde_json::to_string(&builtin).unwrap();
    assert!(json.contains("builtin"));
    assert!(json.contains("standard"));

    let custom = ServiceBinding::Custom {
        module_id: "my-module".to_string(),
    };
    let json = serde_json::to_string(&custom).unwrap();
    assert!(json.contains("custom"));
    assert!(json.contains("my-module"));

    let disabled = ServiceBinding::Disabled;
    let json = serde_json::to_string(&disabled).unwrap();
    assert!(json.contains("disabled"));
}

#[test]
fn test_profile_config_no_overrides() {
    let config = ProfileConfig {
        profile: Profile::Standard,
        services: None,
    };

    let resolved = config.resolve_bindings();
    let defaults = Profile::Standard.default_bindings();

    assert_eq!(resolved.agent_loop, defaults.agent_loop);
    assert_eq!(resolved.scheduler, defaults.scheduler);
    assert_eq!(resolved.model_router, defaults.model_router);
}
