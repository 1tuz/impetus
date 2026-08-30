use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Runtime profile selector
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// Zero-config daily use with safe defaults
    #[default]
    Standard,
    /// Minimal runtime for debugging/benchmarks
    Minimal,
    /// Advanced customization and introspection
    Creator,
}

impl Profile {
    /// Human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            Self::Standard => "Zero-config daily use with safe defaults",
            Self::Minimal => "Minimal runtime for debugging and benchmarks",
            Self::Creator => "Advanced customization and introspection enabled",
        }
    }

    /// Default service bindings for this profile
    pub fn default_bindings(&self) -> ServiceBindings {
        match self {
            Self::Standard => ServiceBindings {
                agent_loop: ServiceBinding::Builtin {
                    variant: "standard".to_string(),
                },
                scheduler: ServiceBinding::Builtin {
                    variant: "standard".to_string(),
                },
                model_router: ServiceBinding::Builtin {
                    variant: "balanced".to_string(),
                },
                context: ServiceBinding::Builtin {
                    variant: "lazy".to_string(),
                },
                reference: ServiceBinding::Builtin {
                    variant: "yaml".to_string(),
                },
                memory: ServiceBinding::Builtin {
                    variant: "standard".to_string(),
                },
                policy: ServiceBinding::Builtin {
                    variant: "standard".to_string(),
                },
                tools: ServiceBinding::Builtin {
                    variant: "standard".to_string(),
                },
                output_reducer: ServiceBinding::Builtin {
                    variant: "standard".to_string(),
                },
                custom: HashMap::new(),
            },
            Self::Minimal => ServiceBindings {
                agent_loop: ServiceBinding::Builtin {
                    variant: "minimal".to_string(),
                },
                scheduler: ServiceBinding::Builtin {
                    variant: "sync".to_string(),
                },
                model_router: ServiceBinding::Builtin {
                    variant: "direct".to_string(),
                },
                context: ServiceBinding::Disabled,
                reference: ServiceBinding::Disabled,
                memory: ServiceBinding::Disabled,
                policy: ServiceBinding::Builtin {
                    variant: "permissive".to_string(),
                },
                tools: ServiceBinding::Builtin {
                    variant: "minimal".to_string(),
                },
                output_reducer: ServiceBinding::Disabled,
                custom: HashMap::new(),
            },
            Self::Creator => ServiceBindings {
                agent_loop: ServiceBinding::Builtin {
                    variant: "standard".to_string(),
                },
                scheduler: ServiceBinding::Builtin {
                    variant: "standard".to_string(),
                },
                model_router: ServiceBinding::Builtin {
                    variant: "balanced".to_string(),
                },
                context: ServiceBinding::Builtin {
                    variant: "lazy".to_string(),
                },
                reference: ServiceBinding::Builtin {
                    variant: "yaml".to_string(),
                },
                memory: ServiceBinding::Builtin {
                    variant: "standard".to_string(),
                },
                policy: ServiceBinding::Builtin {
                    variant: "standard".to_string(),
                },
                tools: ServiceBinding::Builtin {
                    variant: "standard".to_string(),
                },
                output_reducer: ServiceBinding::Builtin {
                    variant: "standard".to_string(),
                },
                custom: HashMap::new(),
            },
        }
    }
}

/// Service binding specification
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "type", content = "value")]
pub enum ServiceBinding {
    /// Built-in implementation with variant name
    Builtin { variant: String },
    /// Custom module by ID
    Custom { module_id: String },
    /// External module via IPC
    External { module_id: String },
    /// Service disabled
    Disabled,
}

/// Complete service binding configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceBindings {
    pub agent_loop: ServiceBinding,
    pub scheduler: ServiceBinding,
    pub model_router: ServiceBinding,
    pub context: ServiceBinding,
    pub reference: ServiceBinding,
    pub memory: ServiceBinding,
    pub policy: ServiceBinding,
    pub tools: ServiceBinding,
    pub output_reducer: ServiceBinding,
    #[serde(default)]
    pub custom: HashMap<String, ServiceBinding>,
}

impl Default for ServiceBindings {
    fn default() -> Self {
        Profile::default().default_bindings()
    }
}

/// Profile configuration with overrides
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfileConfig {
    #[serde(default)]
    pub profile: Profile,
    #[serde(default)]
    pub services: Option<ServiceBindings>,
}

impl ProfileConfig {
    /// Resolve effective service bindings (profile defaults + overrides)
    pub fn resolve_bindings(&self) -> ServiceBindings {
        let mut bindings = self.profile.default_bindings();

        if let Some(overrides) = &self.services {
            // Apply all non-default overrides
            bindings.agent_loop = overrides.agent_loop.clone();
            bindings.scheduler = overrides.scheduler.clone();
            bindings.model_router = overrides.model_router.clone();
            bindings.context = overrides.context.clone();
            bindings.reference = overrides.reference.clone();
            bindings.memory = overrides.memory.clone();
            bindings.policy = overrides.policy.clone();
            bindings.tools = overrides.tools.clone();
            bindings.output_reducer = overrides.output_reducer.clone();
            // Merge custom bindings
            bindings.custom.extend(overrides.custom.clone());
        }

        bindings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_profile_is_standard() {
        assert_eq!(Profile::default(), Profile::Standard);
    }

    #[test]
    fn test_standard_profile_bindings() {
        let bindings = Profile::Standard.default_bindings();
        assert_eq!(
            bindings.agent_loop,
            ServiceBinding::Builtin {
                variant: "standard".to_string()
            }
        );
        assert_eq!(
            bindings.model_router,
            ServiceBinding::Builtin {
                variant: "balanced".to_string()
            }
        );
    }

    #[test]
    fn test_minimal_profile_disables_services() {
        let bindings = Profile::Minimal.default_bindings();
        assert_eq!(bindings.context, ServiceBinding::Disabled);
        assert_eq!(bindings.memory, ServiceBinding::Disabled);
    }

    #[test]
    fn test_profile_config_resolve_with_overrides() {
        let config = ProfileConfig {
            profile: Profile::Standard,
            services: Some(ServiceBindings {
                agent_loop: ServiceBinding::Custom {
                    module_id: "my-loop".to_string(),
                },
                ..Profile::Standard.default_bindings()
            }),
        };

        let resolved = config.resolve_bindings();
        assert_eq!(
            resolved.agent_loop,
            ServiceBinding::Custom {
                module_id: "my-loop".to_string()
            }
        );
        // Other services remain default
        assert_eq!(
            resolved.scheduler,
            ServiceBinding::Builtin {
                variant: "standard".to_string()
            }
        );
    }

    #[test]
    fn test_profile_descriptions() {
        assert!(!Profile::Standard.description().is_empty());
        assert!(!Profile::Minimal.description().is_empty());
        assert!(!Profile::Creator.description().is_empty());
    }
}
