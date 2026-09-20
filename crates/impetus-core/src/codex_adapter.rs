//! Codex extensions/plugins/skills adapter.
//!
//! Imports a Codex project layout — root `AGENTS.md` instructions,
//! `.agents/skills/<name>/SKILL.md` skills, and optional
//! `.codex-plugin/plugin.json` plus package `skills/` — into a canonical
//! module spec. MCP servers and hooks are part of some Codex layouts but
//! are not imported yet; the capability matrix reports them as Unsupported.

use crate::agent_plugins_adapter::{
    PluginCommandEntry, parse_frontmatter, read_skill_name, slugify,
};
use crate::extension_compat::{CanonicalModuleKind, CanonicalModuleSpec, ExtensionSource};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Optional `.codex-plugin/plugin.json` fields used for discovery metadata.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexPluginManifest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// Codex extension adapter
pub struct CodexAdapter;

impl CodexAdapter {
    /// Import an extension from path.
    ///
    /// The path is the project root containing `AGENTS.md`, `.agents/skills/`,
    /// and/or `.codex-plugin/plugin.json`. At least one supported item must
    /// be present.
    pub async fn import(root: &Path) -> Result<CanonicalModuleSpec> {
        if !root.is_dir() {
            anyhow::bail!(
                "{} is not a directory (expected a Codex project root)",
                root.display()
            );
        }

        let dir_name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "codex".to_string());

        let agents_md = root.join("AGENTS.md");
        let has_instructions = agents_md.is_file();
        let mut skills = Self::discover_skills(&root.join(".agents").join("skills")).await?;

        let plugin_manifest_path = root.join(".codex-plugin").join("plugin.json");
        let has_plugin = plugin_manifest_path.is_file();
        let mut manifest = CodexPluginManifest::default();
        if has_plugin {
            let content = tokio::fs::read_to_string(&plugin_manifest_path)
                .await
                .with_context(|| format!("Failed to read {}", plugin_manifest_path.display()))?;
            manifest = serde_json::from_str(&content).context("Failed to parse plugin.json")?;
            let plugin_skills = Self::discover_skills(&root.join("skills")).await?;
            for skill in plugin_skills {
                if !skills.iter().any(|s| s.name == skill.name) {
                    skills.push(skill);
                }
            }
            skills.sort_by(|a, b| a.name.cmp(&b.name));
        }

        if !has_instructions && skills.is_empty() && !has_plugin {
            anyhow::bail!(
                "{} contains no AGENTS.md, .agents/skills, or .codex-plugin/plugin.json",
                root.display()
            );
        }

        let mut capabilities = Vec::new();
        if has_instructions {
            capabilities.push("instructions".to_string());
        }
        if !skills.is_empty() {
            capabilities.push("skills".to_string());
        }
        if has_plugin {
            capabilities.push("plugins".to_string());
        }

        let mut metadata = HashMap::new();
        if !skills.is_empty() {
            let names: Vec<String> = skills.iter().map(|c| c.name.clone()).collect();
            metadata.insert("skills".to_string(), serde_json::json!(names));
        }
        if has_plugin {
            if let Some(desc) = manifest.description.as_deref() {
                metadata.insert("description".to_string(), serde_json::json!(desc));
            }
            if let Some(name) = manifest.name.as_deref() {
                metadata.insert("plugin".to_string(), serde_json::json!(name));
            }
        }
        metadata.insert(
            "extension_path".to_string(),
            serde_json::json!(root.display().to_string()),
        );

        let id = manifest
            .name
            .as_deref()
            .map(slugify)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| slugify(&dir_name));
        let name = manifest
            .display_name
            .clone()
            .or(manifest.name.clone())
            .unwrap_or(dir_name);
        let version = manifest
            .version
            .clone()
            .unwrap_or_else(|| "1.0.0".to_string());

        Ok(CanonicalModuleSpec {
            id,
            name,
            version,
            source: ExtensionSource::Codex,
            kind: CanonicalModuleKind::Plugin,
            capabilities,
            metadata,
        })
    }

    /// Discover `<skills_dir>/<name>/SKILL.md` skills.
    async fn discover_skills(skills_dir: &Path) -> Result<Vec<PluginCommandEntry>> {
        if !skills_dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut skills = Vec::new();
        for entry in std::fs::read_dir(skills_dir)
            .with_context(|| format!("Failed to read {}", skills_dir.display()))?
            .flatten()
        {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let skill_md = path.join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }
            let name = read_skill_name(&skill_md)
                .await
                .unwrap_or_else(|| slugify(&path.display().to_string()));
            let description = std::fs::read_to_string(&skill_md).ok().and_then(|content| {
                parse_frontmatter(&content).and_then(|fm| {
                    fm.get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
            });
            skills.push(PluginCommandEntry {
                name,
                description,
                path: skill_md,
            });
        }
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(skills)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[tokio::test]
    async fn import_codex_with_agents_md_and_skills() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "AGENTS.md", "# Project\nRepo instructions.\n");
        write_file(
            dir.path(),
            ".agents/skills/rust/SKILL.md",
            "---\nname: rust\ndescription: Rust coding helpers\n---\n# Rust\nGuidance for Rust work.\n",
        );

        let spec = CodexAdapter::import(dir.path()).await.unwrap();
        assert_eq!(spec.source, ExtensionSource::Codex);
        assert_eq!(spec.kind, CanonicalModuleKind::Plugin);
        assert_eq!(spec.capabilities, vec!["instructions", "skills"]);
        let skills = spec.metadata["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0], "rust");
        assert!(spec.metadata.contains_key("extension_path"));
    }

    #[tokio::test]
    async fn import_codex_with_plugin_json_and_package_skills() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".codex-plugin/plugin.json",
            r#"{
                "name": "codex-helpers",
                "displayName": "Codex Helpers",
                "version": "0.2.0",
                "description": "Helper plugin"
            }"#,
        );
        write_file(
            dir.path(),
            "skills/review/SKILL.md",
            "---\nname: review\ndescription: Review helpers\n---\n# Review\n",
        );

        let spec = CodexAdapter::import(dir.path()).await.unwrap();
        assert_eq!(spec.id, "codex-helpers");
        assert_eq!(spec.name, "Codex Helpers");
        assert_eq!(spec.version, "0.2.0");
        assert!(spec.capabilities.contains(&"plugins".to_string()));
        assert!(spec.capabilities.contains(&"skills".to_string()));
        assert!(!spec.capabilities.contains(&"extensions".to_string()));
        assert_eq!(spec.metadata.get("plugin").unwrap(), "codex-helpers");
        assert_eq!(spec.metadata.get("description").unwrap(), "Helper plugin");
        let skills = spec.metadata["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0], "review");
    }

    #[tokio::test]
    async fn import_codex_merges_agents_and_plugin_skills() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".agents/skills/alpha/SKILL.md",
            "---\nname: alpha\n---\n# Alpha\n",
        );
        write_file(
            dir.path(),
            ".codex-plugin/plugin.json",
            r#"{"name": "merged"}"#,
        );
        write_file(
            dir.path(),
            "skills/beta/SKILL.md",
            "---\nname: beta\n---\n# Beta\n",
        );

        let spec = CodexAdapter::import(dir.path()).await.unwrap();
        let skills = spec.metadata["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0], "alpha");
        assert_eq!(skills[1], "beta");
    }

    #[tokio::test]
    async fn import_codex_empty_tree_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = CodexAdapter::import(dir.path()).await.unwrap_err();
        assert!(err.to_string().contains("no AGENTS.md"));
    }

    #[tokio::test]
    async fn import_codex_not_a_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "file.md", "# hi\n");
        let err = CodexAdapter::import(&dir.path().join("file.md"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a directory"));
    }

    #[tokio::test]
    async fn import_codex_plugin_json_alone_ok() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".codex-plugin/plugin.json",
            r#"{"name": "bare-plugin"}"#,
        );
        let spec = CodexAdapter::import(dir.path()).await.unwrap();
        assert_eq!(spec.id, "bare-plugin");
        assert!(spec.capabilities.contains(&"plugins".to_string()));
        assert!(!spec.capabilities.contains(&"skills".to_string()));
    }
}
