use crate::agent_plugins_adapter::AgentPluginsAdapter;
use crate::agent_skills_adapter::AgentSkillsAdapter;
use crate::claude_code_adapter::ClaudeCodeAdapter;
use crate::cursor_adapter::CursorAdapter;
use crate::extension_compat::{
    CanonicalModuleSpec, CompatibilityMatrix, ExtensionSource, ImportCapability, ImportResult,
    McpModule,
};
use crate::mcp_adapter::McpAdapter;
use anyhow::Result;
use std::path::Path;

/// Extension compatibility adapter
pub struct ExtensionAdapter {
    matrices: Vec<CompatibilityMatrix>,
}

impl ExtensionAdapter {
    pub fn new() -> Self {
        Self {
            matrices: CompatibilityMatrix::all(),
        }
    }

    /// Get compatibility matrix for a source
    pub fn get_matrix(&self, source: &ExtensionSource) -> Option<&CompatibilityMatrix> {
        self.matrices.iter().find(|m| &m.source == source)
    }

    /// Import extension from path
    pub async fn import(&self, source: ExtensionSource, path: &Path) -> Result<ImportResult> {
        let matrix = self
            .get_matrix(&source)
            .ok_or_else(|| anyhow::anyhow!("No compatibility matrix for {:?}", source))?;

        let mut warnings = Vec::new();
        let mut errors = Vec::new();

        // Check if source is supported at all
        let overall_capability = self.assess_overall_capability(matrix);

        if overall_capability == ImportCapability::Incompatible {
            errors.push(format!("Source {:?} is incompatible", source));
            return Ok(ImportResult {
                source,
                capability: ImportCapability::Incompatible,
                canonical: None,
                warnings,
                errors,
            });
        }

        if overall_capability == ImportCapability::Unsupported {
            warnings.push(format!(
                "Source {:?} is not yet supported. Import is a no-op.",
                source
            ));
            return Ok(ImportResult {
                source,
                capability: ImportCapability::Unsupported,
                canonical: None,
                warnings,
                errors,
            });
        }

        // Attempt real import for Agent Skills
        match source {
            ExtensionSource::AgentSkills => {
                let skill_path = if path.is_dir() {
                    path.join("SKILL.md")
                } else {
                    path.to_path_buf()
                };
                if !skill_path.is_file() {
                    warnings.push(format!("SKILL.md not found at {:?}", skill_path));
                    return Ok(ImportResult {
                        source,
                        capability: ImportCapability::Unsupported,
                        canonical: None,
                        warnings,
                        errors,
                    });
                }

                match AgentSkillsAdapter::import(&skill_path).await {
                    Ok((_skill, spec)) => Ok(ImportResult {
                        source,
                        capability: ImportCapability::Supported,
                        canonical: Some(spec),
                        warnings,
                        errors,
                    }),
                    Err(e) => {
                        errors.push(format!("Failed to parse SKILL.md: {}", e));
                        Ok(ImportResult {
                            source,
                            capability: ImportCapability::Incompatible,
                            canonical: None,
                            warnings,
                            errors,
                        })
                    }
                }
            }
            ExtensionSource::Mcp => {
                if !path.is_file() {
                    warnings.push(format!("MCP config not found at {:?}", path));
                    return Ok(ImportResult {
                        source,
                        capability: ImportCapability::Unsupported,
                        canonical: None,
                        warnings,
                        errors,
                    });
                }
                match std::fs::read(path) {
                    Ok(bytes) => match serde_json::from_slice::<McpModule>(&bytes) {
                        Ok(module) => Ok(McpAdapter::import(&module).await),
                        Err(e) => {
                            errors.push(format!("Failed to parse MCP config: {}", e));
                            Ok(ImportResult {
                                source,
                                capability: ImportCapability::Incompatible,
                                canonical: None,
                                warnings,
                                errors,
                            })
                        }
                    },
                    Err(e) => {
                        errors.push(format!("Failed to read MCP config: {}", e));
                        Ok(ImportResult {
                            source,
                            capability: ImportCapability::Incompatible,
                            canonical: None,
                            warnings,
                            errors,
                        })
                    }
                }
            }
            ExtensionSource::AgentPlugins => {
                if !path.exists() {
                    warnings.push(format!("Plugin path not found at {:?}", path));
                    return Ok(ImportResult {
                        source,
                        capability: ImportCapability::Unsupported,
                        canonical: None,
                        warnings,
                        errors,
                    });
                }

                match AgentPluginsAdapter::import(path).await {
                    Ok(spec) => Ok(ImportResult {
                        source,
                        capability: ImportCapability::Supported,
                        canonical: Some(spec),
                        warnings,
                        errors,
                    }),
                    Err(e) => {
                        errors.push(format!("Failed to import plugin: {}", e));
                        Ok(ImportResult {
                            source,
                            capability: ImportCapability::Incompatible,
                            canonical: None,
                            warnings,
                            errors,
                        })
                    }
                }
            }
            ExtensionSource::ClaudeCode => {
                if !path.exists() {
                    warnings.push(format!("Claude Code path not found at {:?}", path));
                    return Ok(ImportResult {
                        source,
                        capability: ImportCapability::Unsupported,
                        canonical: None,
                        warnings,
                        errors,
                    });
                }

                match ClaudeCodeAdapter::import(path).await {
                    Ok(spec) => Ok(ImportResult {
                        source,
                        capability: ImportCapability::Supported,
                        canonical: Some(spec),
                        warnings,
                        errors,
                    }),
                    Err(e) => {
                        errors.push(format!("Failed to import Claude Code extension: {}", e));
                        Ok(ImportResult {
                            source,
                            capability: ImportCapability::Incompatible,
                            canonical: None,
                            warnings,
                            errors,
                        })
                    }
                }
            }
            ExtensionSource::Cursor => {
                if !path.exists() {
                    warnings.push(format!("Cursor path not found at {:?}", path));
                    return Ok(ImportResult {
                        source,
                        capability: ImportCapability::Unsupported,
                        canonical: None,
                        warnings,
                        errors,
                    });
                }

                match CursorAdapter::import(path).await {
                    Ok(spec) => Ok(ImportResult {
                        source,
                        capability: ImportCapability::Supported,
                        canonical: Some(spec),
                        warnings,
                        errors,
                    }),
                    Err(e) => {
                        errors.push(format!("Failed to import Cursor extension: {}", e));
                        Ok(ImportResult {
                            source,
                            capability: ImportCapability::Incompatible,
                            canonical: None,
                            warnings,
                            errors,
                        })
                    }
                }
            }
            _ => {
                // Other sources still unsupported
                warnings.push(format!(
                    "Import from {:?} at {:?} not yet implemented",
                    source, path
                ));

                Ok(ImportResult {
                    source,
                    capability: overall_capability,
                    canonical: None,
                    warnings,
                    errors,
                })
            }
        }
    }

    /// Assess overall capability from matrix
    fn assess_overall_capability(&self, matrix: &CompatibilityMatrix) -> ImportCapability {
        if matrix.capabilities.is_empty() {
            return ImportCapability::Incompatible;
        }

        let has_supported = matrix
            .capabilities
            .values()
            .any(|c| *c == ImportCapability::Supported);
        let has_partial = matrix
            .capabilities
            .values()
            .any(|c| *c == ImportCapability::Partial);
        let all_incompatible = matrix
            .capabilities
            .values()
            .all(|c| *c == ImportCapability::Incompatible);
        let all_unsupported = matrix
            .capabilities
            .values()
            .all(|c| *c == ImportCapability::Unsupported);

        if all_incompatible {
            ImportCapability::Incompatible
        } else if all_unsupported {
            ImportCapability::Unsupported
        } else if has_supported {
            if has_partial {
                ImportCapability::Partial
            } else {
                ImportCapability::Supported
            }
        } else if has_partial {
            ImportCapability::Partial
        } else {
            ImportCapability::Unsupported
        }
    }

    /// List all available compatibility matrices
    pub fn list_matrices(&self) -> &[CompatibilityMatrix] {
        &self.matrices
    }

    /// Validate canonical module spec
    pub fn validate_spec(&self, spec: &CanonicalModuleSpec) -> Result<()> {
        if spec.id.is_empty() {
            anyhow::bail!("Module ID cannot be empty");
        }

        if spec.name.is_empty() {
            anyhow::bail!("Module name cannot be empty");
        }

        if spec.version.is_empty() {
            anyhow::bail!("Module version cannot be empty");
        }

        Ok(())
    }
}

impl Default for ExtensionAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Extension registry for imported modules
pub struct ExtensionRegistry {
    modules: std::sync::Arc<std::sync::RwLock<Vec<CanonicalModuleSpec>>>,
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self {
            modules: std::sync::Arc::new(std::sync::RwLock::new(Vec::new())),
        }
    }

    /// Register imported module
    pub fn register(&self, spec: CanonicalModuleSpec) -> Result<()> {
        let mut modules = self.modules.write().unwrap();

        if modules.iter().any(|m| m.id == spec.id) {
            anyhow::bail!("Module {} already registered", spec.id);
        }

        modules.push(spec);
        Ok(())
    }

    /// Get module by ID
    pub fn get(&self, id: &str) -> Option<CanonicalModuleSpec> {
        let modules = self.modules.read().unwrap();
        modules.iter().find(|m| m.id == id).cloned()
    }

    /// List all registered modules
    pub fn list(&self) -> Vec<CanonicalModuleSpec> {
        let modules = self.modules.read().unwrap();
        modules.clone()
    }

    /// List modules by source
    pub fn list_by_source(&self, source: &ExtensionSource) -> Vec<CanonicalModuleSpec> {
        let modules = self.modules.read().unwrap();
        modules
            .iter()
            .filter(|m| &m.source == source)
            .cloned()
            .collect()
    }

    /// Remove module
    pub fn remove(&self, id: &str) -> Result<()> {
        let mut modules = self.modules.write().unwrap();
        let before = modules.len();
        modules.retain(|m| m.id != id);

        if modules.len() == before {
            anyhow::bail!("Module {} not found", id);
        }

        Ok(())
    }
}

impl Default for ExtensionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_compat::CanonicalModuleKind;
    use std::collections::HashMap;

    #[test]
    fn adapter_has_all_matrices() {
        let adapter = ExtensionAdapter::new();
        let matrices = adapter.list_matrices();
        assert_eq!(matrices.len(), 7);
    }

    #[test]
    fn adapter_finds_matrix_by_source() {
        let adapter = ExtensionAdapter::new();
        let matrix = adapter.get_matrix(&ExtensionSource::Mcp);
        assert!(matrix.is_some());
        assert_eq!(matrix.unwrap().source, ExtensionSource::Mcp);
    }

    #[test]
    fn validate_spec_rejects_empty_fields() {
        let adapter = ExtensionAdapter::new();

        let invalid = CanonicalModuleSpec {
            id: "".to_string(),
            name: "Test".to_string(),
            version: "1.0.0".to_string(),
            source: ExtensionSource::Native,
            kind: CanonicalModuleKind::Skill,
            capabilities: vec![],
            metadata: HashMap::new(),
        };

        assert!(adapter.validate_spec(&invalid).is_err());
    }

    #[test]
    fn registry_registers_and_retrieves() {
        let registry = ExtensionRegistry::new();

        let spec = CanonicalModuleSpec {
            id: "test-module".to_string(),
            name: "Test Module".to_string(),
            version: "1.0.0".to_string(),
            source: ExtensionSource::Native,
            kind: CanonicalModuleKind::Skill,
            capabilities: vec![],
            metadata: HashMap::new(),
        };

        registry.register(spec.clone()).unwrap();

        let retrieved = registry.get("test-module");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().id, "test-module");
    }

    #[test]
    fn registry_prevents_duplicate_ids() {
        let registry = ExtensionRegistry::new();

        let spec = CanonicalModuleSpec {
            id: "duplicate".to_string(),
            name: "Test".to_string(),
            version: "1.0.0".to_string(),
            source: ExtensionSource::Native,
            kind: CanonicalModuleKind::Skill,
            capabilities: vec![],
            metadata: HashMap::new(),
        };

        registry.register(spec.clone()).unwrap();
        let result = registry.register(spec);
        assert!(result.is_err());
    }

    #[test]
    fn registry_lists_by_source() {
        let registry = ExtensionRegistry::new();

        let native = CanonicalModuleSpec {
            id: "native-1".to_string(),
            name: "Native".to_string(),
            version: "1.0.0".to_string(),
            source: ExtensionSource::Native,
            kind: CanonicalModuleKind::Skill,
            capabilities: vec![],
            metadata: HashMap::new(),
        };

        let mcp = CanonicalModuleSpec {
            id: "mcp-1".to_string(),
            name: "MCP".to_string(),
            version: "1.0.0".to_string(),
            source: ExtensionSource::Mcp,
            kind: CanonicalModuleKind::McpServer,
            capabilities: vec![],
            metadata: HashMap::new(),
        };

        registry.register(native).unwrap();
        registry.register(mcp).unwrap();

        let native_modules = registry.list_by_source(&ExtensionSource::Native);
        assert_eq!(native_modules.len(), 1);

        let mcp_modules = registry.list_by_source(&ExtensionSource::Mcp);
        assert_eq!(mcp_modules.len(), 1);
    }

    #[tokio::test]
    async fn import_unsupported_source_warns() {
        let adapter = ExtensionAdapter::new();
        let result = adapter
            .import(ExtensionSource::AgentSkills, Path::new("/tmp/test"))
            .await
            .unwrap();

        assert_eq!(result.capability, ImportCapability::Unsupported);
        assert!(!result.warnings.is_empty());
    }

    #[tokio::test]
    async fn import_mcp_missing_config_warns() {
        let adapter = ExtensionAdapter::new();
        let result = adapter
            .import(ExtensionSource::Mcp, Path::new("/tmp/nonexistent-mcp.json"))
            .await
            .unwrap();

        assert_eq!(result.capability, ImportCapability::Unsupported);
        assert!(!result.warnings.is_empty());
    }

    #[tokio::test]
    async fn import_agent_plugins_supported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.path().join(".claude-plugin/plugin.json"),
            r#"{"name": "test-plugin", "commands": ["commands/run.md"]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("commands")).unwrap();
        std::fs::write(
            dir.path().join("commands/run.md"),
            "---\nname: run\n---\necho hi\n",
        )
        .unwrap();

        let adapter = ExtensionAdapter::new();
        let result = adapter
            .import(ExtensionSource::AgentPlugins, dir.path())
            .await
            .unwrap();

        assert_eq!(result.capability, ImportCapability::Supported);
        assert!(result.errors.is_empty());
        let spec = result.canonical.unwrap();
        assert_eq!(spec.id, "test-plugin");
        assert_eq!(spec.source, ExtensionSource::AgentPlugins);
        assert!(spec.capabilities.contains(&"commands".to_string()));
    }

    #[tokio::test]
    async fn import_agent_plugins_missing_path_warns() {
        let adapter = ExtensionAdapter::new();
        let result = adapter
            .import(
                ExtensionSource::AgentPlugins,
                Path::new("/nonexistent/plugin"),
            )
            .await
            .unwrap();

        assert_eq!(result.capability, ImportCapability::Unsupported);
        assert!(!result.warnings.is_empty());
    }

    #[tokio::test]
    async fn import_claude_code_supported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude/commands")).unwrap();
        std::fs::write(
            dir.path().join(".claude/commands/review.md"),
            "---\nname: review\ndescription: Review code\n---\nrun review\n",
        )
        .unwrap();

        let adapter = ExtensionAdapter::new();
        let result = adapter
            .import(ExtensionSource::ClaudeCode, dir.path())
            .await
            .unwrap();

        assert_eq!(result.capability, ImportCapability::Supported);
        assert!(result.errors.is_empty());
        let spec = result.canonical.unwrap();
        assert_eq!(spec.source, ExtensionSource::ClaudeCode);
        assert!(spec.capabilities.contains(&"commands".to_string()));
    }

    #[tokio::test]
    async fn import_claude_code_missing_path_warns() {
        let adapter = ExtensionAdapter::new();
        let result = adapter
            .import(
                ExtensionSource::ClaudeCode,
                Path::new("/nonexistent/claude-code"),
            )
            .await
            .unwrap();

        assert_eq!(result.capability, ImportCapability::Unsupported);
        assert!(!result.warnings.is_empty());
    }

    #[tokio::test]
    async fn import_cursor_supported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".cursor/commands")).unwrap();
        std::fs::write(
            dir.path().join(".cursor/commands/review.md"),
            "---\nname: review\ndescription: Review code\n---\nrun review\n",
        )
        .unwrap();

        let adapter = ExtensionAdapter::new();
        let result = adapter
            .import(ExtensionSource::Cursor, dir.path())
            .await
            .unwrap();

        assert_eq!(result.capability, ImportCapability::Supported);
        assert!(result.errors.is_empty());
        let spec = result.canonical.unwrap();
        assert_eq!(spec.source, ExtensionSource::Cursor);
        assert!(spec.capabilities.contains(&"commands".to_string()));
    }

    #[tokio::test]
    async fn import_cursor_missing_path_warns() {
        let adapter = ExtensionAdapter::new();
        let result = adapter
            .import(ExtensionSource::Cursor, Path::new("/nonexistent/cursor"))
            .await
            .unwrap();

        assert_eq!(result.capability, ImportCapability::Unsupported);
        assert!(!result.warnings.is_empty());
    }
}
