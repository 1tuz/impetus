use crate::module::{ModuleDescriptor, ModuleKind, ModuleState};
use crate::module_ipc::ExternalModule;
use crate::module_registry::ModuleRegistry;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::UnixStream;
use tokio::sync::RwLock;

/// Module lifecycle manager
pub struct ModuleLifecycle {
    registry: Arc<ModuleRegistry>,
    external_modules: Arc<RwLock<HashMap<String, Arc<ExternalModule>>>>,
    module_search_paths: Vec<PathBuf>,
    socket_dir: PathBuf,
}

impl ModuleLifecycle {
    pub fn new(registry: Arc<ModuleRegistry>) -> Self {
        Self {
            registry,
            external_modules: Arc::new(RwLock::new(HashMap::new())),
            module_search_paths: vec![],
            socket_dir: std::env::temp_dir().join("impetus-modules"),
        }
    }

    /// Create lifecycle manager with custom configuration
    pub fn with_config(
        registry: Arc<ModuleRegistry>,
        module_search_paths: Vec<PathBuf>,
        socket_dir: PathBuf,
    ) -> Self {
        Self {
            registry,
            external_modules: Arc::new(RwLock::new(HashMap::new())),
            module_search_paths,
            socket_dir,
        }
    }

    /// Discover modules from configured sources
    pub async fn discover(&self) -> Result<Vec<ModuleDescriptor>> {
        let mut discovered = Vec::new();

        // Scan configured search paths for module binaries
        for search_path in &self.module_search_paths {
            if !search_path.exists() {
                continue;
            }

            // Look for executables
            if let Ok(entries) = std::fs::read_dir(search_path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && is_executable(&path) {
                        // Try to infer module descriptor from binary name
                        if let Some(module_id) = path.file_stem().and_then(|s| s.to_str()) {
                            let descriptor = ModuleDescriptor {
                                id: module_id.to_string(),
                                name: module_id.to_string(),
                                version: "unknown".to_string(),
                                kind: ModuleKind::Custom,
                                provides: vec![],
                                requires: vec![],
                                capabilities: vec![],
                                permissions: Default::default(),
                            };
                            discovered.push(descriptor);
                        }
                    }
                }
            }
        }

        Ok(discovered)
    }

    /// Probe module capabilities and availability
    pub async fn probe(&self, module_id: &str) -> Result<()> {
        self.registry
            .update_state(module_id, ModuleState::Probing)?;

        // Check if module has external runtime
        let external_modules = self.external_modules.read().await;
        if let Some(external_module) = external_modules.get(module_id) {
            // Try to connect and probe real capabilities
            let socket_path = &external_module.descriptor().id;
            let socket_full_path = self.socket_dir.join(format!("{}.sock", socket_path));

            match UnixStream::connect(&socket_full_path).await {
                Ok(mut stream) => {
                    // Real capability probe via IPC
                    match external_module.probe_capabilities(&mut stream).await {
                        Ok(probes) => {
                            let all_available = probes.iter().all(|p| p.available);
                            let new_state = if all_available {
                                ModuleState::Ready
                            } else {
                                ModuleState::Degraded
                            };
                            self.registry.update_state(module_id, new_state)?;
                            return Ok(());
                        }
                        Err(_) => {
                            // IPC probe failed - mark as degraded
                            self.registry
                                .update_state(module_id, ModuleState::Degraded)?;
                            return Ok(());
                        }
                    }
                }
                Err(_) => {
                    // Can't connect - mark as unavailable
                    self.registry.update_state(module_id, ModuleState::Failed)?;
                    return Ok(());
                }
            }
        }

        // Fallback: probe from registry descriptor
        let probes = self.registry.probe_capabilities(module_id)?;
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
    pub async fn start(&self, module_id: &str, binary_path: Option<PathBuf>) -> Result<()> {
        // Check compatibility before starting
        let harness_version = env!("CARGO_PKG_VERSION");
        let compat = self
            .registry
            .check_compatibility(module_id, harness_version)?;
        if compat.overall == crate::module::Compatibility::Incompatible {
            anyhow::bail!(
                "Module {} is incompatible with harness version {}",
                module_id,
                harness_version
            );
        }

        self.registry
            .update_state(module_id, ModuleState::Starting)?;

        // Get module descriptor
        let descriptor = self
            .registry
            .get_module(module_id)
            .ok_or_else(|| anyhow::anyhow!("Module {} not found", module_id))?;

        // If binary path provided, spawn external process
        if let Some(bin_path) = binary_path {
            // Create socket directory if needed
            tokio::fs::create_dir_all(&self.socket_dir).await?;

            let socket_path = self.socket_dir.join(format!("{}.sock", module_id));

            // Clean up old socket if exists
            let _ = tokio::fs::remove_file(&socket_path).await;

            let external_module = Arc::new(ExternalModule::new(descriptor, socket_path));

            // Spawn the module process
            external_module.spawn(bin_path).await?;

            // Store external module handle
            self.external_modules
                .write()
                .await
                .insert(module_id.to_string(), external_module);
        }

        self.registry
            .update_state(module_id, ModuleState::Running)?;
        Ok(())
    }

    /// Stop a module
    pub async fn stop(&self, module_id: &str) -> Result<()> {
        self.registry
            .update_state(module_id, ModuleState::Stopping)?;

        // If external module, shutdown via IPC
        let mut external_modules = self.external_modules.write().await;
        if let Some(external_module) = external_modules.get(module_id) {
            let socket_path = self.socket_dir.join(format!("{}.sock", module_id));

            // Try graceful shutdown
            if let Ok(mut stream) = UnixStream::connect(&socket_path).await {
                let _ = external_module.shutdown(&mut stream).await;
            } else {
                // Force kill if can't connect
                let _ = external_module.kill().await;
            }

            // Remove from tracking
            external_modules.remove(module_id);

            // Clean up socket
            let _ = tokio::fs::remove_file(&socket_path).await;
        }

        self.registry
            .update_state(module_id, ModuleState::Stopped)?;
        Ok(())
    }

    /// Check module health
    pub async fn health_check(&self, module_id: &str) -> Result<()> {
        use crate::module::ModuleHealth;

        // Check if external module process is alive
        let external_modules = self.external_modules.read().await;
        if let Some(external_module) = external_modules.get(module_id) {
            let socket_path = self.socket_dir.join(format!("{}.sock", module_id));

            // Try health check ping
            match UnixStream::connect(&socket_path).await {
                Ok(mut stream) => {
                    match external_module.health_check(&mut stream).await {
                        Ok(_) => {
                            // Process alive and responding
                            let probes = self.registry.probe_capabilities(module_id)?;
                            let health = ModuleHealth {
                                module_id: module_id.to_string(),
                                state: ModuleState::Running,
                                capabilities: probes,
                                last_check: chrono::Utc::now().to_rfc3339(),
                                error: None,
                            };
                            self.registry.update_health(module_id, health)?;
                            self.registry
                                .update_state(module_id, ModuleState::Running)?;
                            return Ok(());
                        }
                        Err(e) => {
                            // Process not responding - mark as degraded
                            let probes = self.registry.probe_capabilities(module_id)?;
                            let health = ModuleHealth {
                                module_id: module_id.to_string(),
                                state: ModuleState::Degraded,
                                capabilities: probes,
                                last_check: chrono::Utc::now().to_rfc3339(),
                                error: Some(format!("Health check failed: {}", e)),
                            };
                            self.registry.update_health(module_id, health)?;
                            self.registry
                                .update_state(module_id, ModuleState::Degraded)?;
                            return Ok(());
                        }
                    }
                }
                Err(_) => {
                    // Can't connect - process likely crashed
                    let probes = self.registry.probe_capabilities(module_id)?;
                    let health = ModuleHealth {
                        module_id: module_id.to_string(),
                        state: ModuleState::Failed,
                        capabilities: probes,
                        last_check: chrono::Utc::now().to_rfc3339(),
                        error: Some("Process crashed or unreachable".to_string()),
                    };
                    self.registry.update_health(module_id, health)?;
                    self.registry.update_state(module_id, ModuleState::Failed)?;
                    return Ok(());
                }
            }
        }

        // Fallback for non-external modules
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
    pub async fn restart(&self, module_id: &str, binary_path: Option<PathBuf>) -> Result<()> {
        self.stop(module_id).await?;
        self.start(module_id, binary_path).await?;
        Ok(())
    }
}

// Helper function to check if a path is executable
#[cfg(unix)]
fn is_executable(path: &PathBuf) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = std::fs::metadata(path) {
        let permissions = metadata.permissions();
        permissions.mode() & 0o111 != 0
    } else {
        false
    }
}

#[cfg(not(unix))]
fn is_executable(_path: &PathBuf) -> bool {
    // On non-Unix, assume files are executable if they exist
    true
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

        // Start (without binary path - internal module)
        lifecycle.start("test-module", None).await.unwrap();

        // Health check
        lifecycle.health_check("test-module").await.unwrap();
        let health = registry.get_health("test-module").unwrap();
        assert_eq!(health.state, ModuleState::Running);

        // Stop
        lifecycle.stop("test-module").await.unwrap();
    }
}
