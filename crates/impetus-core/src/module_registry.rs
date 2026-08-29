use crate::module::{
    CapabilityProbe, Compatibility, CompatibilityReport, ModuleDescriptor, ModuleHealth,
    ModuleState,
};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Module registry for lifecycle management
pub struct ModuleRegistry {
    modules: Arc<RwLock<HashMap<String, RegisteredModule>>>,
}

struct RegisteredModule {
    descriptor: ModuleDescriptor,
    state: ModuleState,
    health: Option<ModuleHealth>,
}

impl ModuleRegistry {
    pub fn new() -> Self {
        Self {
            modules: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a module descriptor
    pub fn register(&self, descriptor: ModuleDescriptor) -> Result<()> {
        let mut modules = self.modules.write().unwrap();
        let module_id = descriptor.id.clone();

        if modules.contains_key(&module_id) {
            anyhow::bail!("Module {} already registered", module_id);
        }

        modules.insert(
            module_id,
            RegisteredModule {
                descriptor,
                state: ModuleState::Discovered,
                health: None,
            },
        );

        Ok(())
    }

    /// List all registered modules
    pub fn list_modules(&self) -> Vec<ModuleDescriptor> {
        let modules = self.modules.read().unwrap();
        modules.values().map(|m| m.descriptor.clone()).collect()
    }

    /// Get module descriptor by ID
    pub fn get_module(&self, module_id: &str) -> Option<ModuleDescriptor> {
        let modules = self.modules.read().unwrap();
        modules.get(module_id).map(|m| m.descriptor.clone())
    }

    /// Update module state
    pub fn update_state(&self, module_id: &str, state: ModuleState) -> Result<()> {
        let mut modules = self.modules.write().unwrap();
        if let Some(module) = modules.get_mut(module_id) {
            module.state = state;
            Ok(())
        } else {
            anyhow::bail!("Module {} not found", module_id)
        }
    }

    /// Update module health
    pub fn update_health(&self, module_id: &str, health: ModuleHealth) -> Result<()> {
        let mut modules = self.modules.write().unwrap();
        if let Some(module) = modules.get_mut(module_id) {
            module.health = Some(health);
            Ok(())
        } else {
            anyhow::bail!("Module {} not found", module_id)
        }
    }

    /// Get module health
    pub fn get_health(&self, module_id: &str) -> Option<ModuleHealth> {
        let modules = self.modules.read().unwrap();
        modules.get(module_id).and_then(|m| m.health.clone())
    }

    /// Probe module capabilities
    pub fn probe_capabilities(&self, module_id: &str) -> Result<Vec<CapabilityProbe>> {
        let modules = self.modules.read().unwrap();
        let module = modules
            .get(module_id)
            .ok_or_else(|| anyhow::anyhow!("Module {} not found", module_id))?;

        // For now, return capabilities from descriptor as available
        let probes = module
            .descriptor
            .capabilities
            .iter()
            .map(|cap| CapabilityProbe {
                capability: cap.clone(),
                available: true,
                version: Some(module.descriptor.version.clone()),
                details: None,
            })
            .collect();

        Ok(probes)
    }

    /// Check module compatibility
    pub fn check_compatibility(
        &self,
        module_id: &str,
        harness_version: &str,
    ) -> Result<CompatibilityReport> {
        let modules = self.modules.read().unwrap();
        let module = modules
            .get(module_id)
            .ok_or_else(|| anyhow::anyhow!("Module {} not found", module_id))?;

        // Simple version check for now
        let overall = if module.descriptor.version == harness_version {
            Compatibility::Compatible
        } else {
            Compatibility::PartiallyCompatible
        };

        Ok(CompatibilityReport {
            module_id: module_id.to_string(),
            harness_version: harness_version.to_string(),
            module_version: module.descriptor.version.clone(),
            overall,
            details: HashMap::new(),
        })
    }

    /// Unregister a module
    pub fn unregister(&self, module_id: &str) -> Result<()> {
        let mut modules = self.modules.write().unwrap();
        if modules.remove(module_id).is_some() {
            Ok(())
        } else {
            anyhow::bail!("Module {} not found", module_id)
        }
    }
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{ModuleKind, ModulePermissions};

    #[test]
    fn register_and_list_modules() {
        let registry = ModuleRegistry::new();

        let descriptor = ModuleDescriptor {
            id: "test-module".to_string(),
            name: "Test Module".to_string(),
            version: "1.0.0".to_string(),
            kind: ModuleKind::Custom,
            provides: vec!["test".to_string()],
            requires: vec![],
            capabilities: vec!["test_cap".to_string()],
            permissions: ModulePermissions::default(),
        };

        registry.register(descriptor.clone()).unwrap();

        let modules = registry.list_modules();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].id, "test-module");
    }

    #[test]
    fn probe_capabilities() {
        let registry = ModuleRegistry::new();

        let descriptor = ModuleDescriptor {
            id: "test-module".to_string(),
            name: "Test Module".to_string(),
            version: "1.0.0".to_string(),
            kind: ModuleKind::Custom,
            provides: vec![],
            requires: vec![],
            capabilities: vec!["cap1".to_string(), "cap2".to_string()],
            permissions: ModulePermissions::default(),
        };

        registry.register(descriptor).unwrap();

        let probes = registry.probe_capabilities("test-module").unwrap();
        assert_eq!(probes.len(), 2);
        assert!(probes.iter().all(|p| p.available));
    }
}
