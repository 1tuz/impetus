use crate::module::{ModuleDescriptor, ModuleState};
use crate::module_registry::ModuleRegistry;
use anyhow::Result;
use std::sync::Arc;

/// Module lifecycle manager
pub struct ModuleLifecycle {
    registry: Arc<ModuleRegistry>,
}

impl ModuleLifecycle {
    pub fn new(registry: Arc<ModuleRegistry>) -> Self {
        Self { registry }
    }

    /// Discover modules from configured sources
    pub async fn discover(&self) -> Result<Vec<ModuleDescriptor>> {
        // Placeholder: would scan configured module directories, registries, etc.
        Ok(vec![])
    }

    /// Probe module capabilities and availability
    pub async fn probe(&self, module_id: &str) -> Result<()> {
        self.registry
            .update_state(module_id, ModuleState::Probing)?;

        // Probe capabilities
        let probes = self.registry.probe_capabilities(module_id)?;

        // Check if all required capabilities are available
        let all_available = probes.iter().all(|p| p.available);

        let new_state = if all_available {
            ModuleState::Ready
        } else {
            ModuleState::Degraded
        };

        self.registry.update_state(module_id, new_state)?;
        Ok(())
    }

    /// Start a module
    pub async fn start(&self, module_id: &str) -> Result<()> {
        // Check compatibility before starting
        let harness_version = env!("CARGO_PKG_VERSION");
        let compat = self.registry.check_compatibility(module_id, harness_version)?;
        if compat.overall == crate::module::Compatibility::Incompatible {
            anyhow::bail!(
                "Module {} is incompatible with harness version {}",
                module_id,
                harness_version
            );
        }

        self.registry
            .update_state(module_id, ModuleState::Starting)?;

        // Placeholder: would initialize module runtime, spawn process, etc.
        // For now, transition directly to Running

        self.registry
            .update_state(module_id, ModuleState::Running)?;
        Ok(())
    }

    /// Stop a module
    pub async fn stop(&self, module_id: &str) -> Result<()> {
        self.registry
            .update_state(module_id, ModuleState::Stopping)?;

        // Placeholder: would gracefully shutdown module, cleanup resources

        self.registry
            .update_state(module_id, ModuleState::Stopped)?;
        Ok(())
    }

    /// Check module health
    pub async fn health_check(&self, module_id: &str) -> Result<()> {
        use crate::module::ModuleHealth;

        let probes = self.registry.probe_capabilities(module_id)?;
        let all_available = probes.iter().all(|p| p.available);

        let state = if all_available {
            ModuleState::Running
        } else {
            ModuleState::Degraded
        };

        let health = ModuleHealth {
            module_id: module_id.to_string(),
            state,
            capabilities: probes,
            last_check: chrono::Utc::now().to_rfc3339(),
            error: None,
        };

        self.registry.update_health(module_id, health)?;
        Ok(())
    }

    /// Restart a module
    pub async fn restart(&self, module_id: &str) -> Result<()> {
        self.stop(module_id).await?;
        self.start(module_id).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{ModuleKind, ModulePermissions};

    #[tokio::test]
    async fn lifecycle_transitions() {
        let registry = Arc::new(ModuleRegistry::new());
        let lifecycle = ModuleLifecycle::new(registry.clone());

        let descriptor = ModuleDescriptor {
            id: "test-module".to_string(),
            name: "Test Module".to_string(),
            version: "1.0.0".to_string(),
            kind: ModuleKind::Custom,
            provides: vec![],
            requires: vec![],
            capabilities: vec!["test_cap".to_string()],
            permissions: ModulePermissions::default(),
        };

        registry.register(descriptor).unwrap();

        // Probe
        lifecycle.probe("test-module").await.unwrap();

        // Start
        lifecycle.start("test-module").await.unwrap();

        // Health check
        lifecycle.health_check("test-module").await.unwrap();
        let health = registry.get_health("test-module").unwrap();
        assert_eq!(health.state, ModuleState::Running);

        // Stop
        lifecycle.stop("test-module").await.unwrap();
    }
}
