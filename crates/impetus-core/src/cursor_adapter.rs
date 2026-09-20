//! Cursor extensions adapter.
//!
//! Imports a Cursor project extension layout — `.cursor/rules/**/*.{md,mdc}`
//! rules, `.cursor/commands/**/*.md` slash commands,
//! `.cursor/agents/**/*.md` agents, and skills under
//! `.cursor/skills/<name>/SKILL.md` or `.agents/skills/<name>/SKILL.md` —
//! into a canonical module spec. Project-local plugins have no stable
//! layout, so the capability matrix reports plugins as Unsupported.

use crate::agent_plugins_adapter::{
    PluginCommandEntry, collect_markdown, parse_frontmatter, plugin_command_entry, read_skill_name,
    slugify,
};
use crate::extension_compat::{CanonicalModuleKind, CanonicalModuleSpec, ExtensionSource};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Cursor extension adapter
pub struct CursorAdapter;

impl CursorAdapter {
    /// Import an extension from path.
    ///
    /// The path is the project root containing `.cursor/` and/or
    /// `.agents/skills/`. At least one supported item must be present.
    pub async fn import(root: &Path) -> Result<CanonicalModuleSpec> {
        if !root.is_dir() {
            anyhow::bail!(
                "{} is not a directory (expected a Cursor project root)",
                root.display()
            );
        }

        let dir_name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "cursor".to_string());

        let rules = Self::discover_rules(root);
        let commands = Self::discover_commands(root);
        let agents = Self::discover_agents(root);
        let skills = Self::discover_skills(root).await?;

        if rules.is_empty() && commands.is_empty() && agents.is_empty() && skills.is_empty() {
            anyhow::bail!(
                "{} contains no .cursor/rules, .cursor/commands, .cursor/agents, .cursor/skills or .agents/skills",
                root.display()
            );
        }

        let mut capabilities = Vec::new();
        if !commands.is_empty() {
            capabilities.push("commands".to_string());
        }
        if !agents.is_empty() {
            capabilities.push("agents".to_string());
        }
        if !skills.is_empty() {
            capabilities.push("skills".to_string());
        }
        if !rules.is_empty() {
            // Keep matrix/runtime vocabulary aligned: capability key is `rules`.
            capabilities.push("rules".to_string());
        }

        let mut metadata = HashMap::new();
        if !commands.is_empty() {
            let names: Vec<String> = commands.iter().map(|c| c.name.clone()).collect();
            metadata.insert("commands".to_string(), serde_json::json!(names));
        }
        if !agents.is_empty() {
            let names: Vec<String> = agents.iter().map(|c| c.name.clone()).collect();
            metadata.insert("agents".to_string(), serde_json::json!(names));
        }
        if !skills.is_empty() {
            let names: Vec<String> = skills.iter().map(|c| c.name.clone()).collect();
            metadata.insert("skills".to_string(), serde_json::json!(names));
        }
        if !rules.is_empty() {
            let names: Vec<String> = rules.iter().map(|c| c.name.clone()).collect();
            metadata.insert("rules".to_string(), serde_json::json!(names));
        }
        metadata.insert(
            "extension_path".to_string(),
            serde_json::json!(root.display().to_string()),
        );

        Ok(CanonicalModuleSpec {
            id: slugify(&dir_name),
            name: dir_name.clone(),
            version: "1.0.0".to_string(),
            source: ExtensionSource::Cursor,
            kind: CanonicalModuleKind::Plugin,
            capabilities,
            metadata,
        })
    }

    /// Discover `.cursor/rules/**/*.{md,mdc}` rule files.
    fn discover_rules(root: &Path) -> Vec<PluginCommandEntry> {
        let mut found = Vec::new();
        collect_rule_files(&root.join(".cursor").join("rules"), &mut found);
        found.sort();

        let mut rules = Vec::new();
        for path in found {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            rules.push(plugin_command_entry(&rel, &path));
        }
        rules
    }

    /// Discover `.cursor/commands/**/*.md` slash commands.
    fn discover_commands(root: &Path) -> Vec<PluginCommandEntry> {
        let mut found = Vec::new();
        collect_markdown(&root.join(".cursor").join("commands"), &mut found);
        found.sort();

        let mut commands = Vec::new();
        for path in found {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            commands.push(plugin_command_entry(&rel, &path));
        }
        commands
    }

    /// Discover `.cursor/agents/**/*.md` agents.
    fn discover_agents(root: &Path) -> Vec<PluginCommandEntry> {
        let mut found = Vec::new();
        collect_markdown(&root.join(".cursor").join("agents"), &mut found);
        found.sort();

        let mut agents = Vec::new();
        for path in found {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            agents.push(plugin_command_entry(&rel, &path));
        }
        agents
    }

    /// Discover skills under `.cursor/skills/<name>/SKILL.md` and
    /// `.agents/skills/<name>/SKILL.md`, using the skill directory name
    /// as the skill id when frontmatter has no `name`.
    async fn discover_skills(root: &Path) -> Result<Vec<PluginCommandEntry>> {
        let mut skills = Vec::new();
        Self::collect_skills_from(&root.join(".cursor").join("skills"), &mut skills).await?;
        Self::collect_skills_from(&root.join(".agents").join("skills"), &mut skills).await?;
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills.dedup_by(|a, b| a.name == b.name);
        Ok(skills)
    }

    async fn collect_skills_from(
        skills_dir: &Path,
        skills: &mut Vec<PluginCommandEntry>,
    ) -> Result<()> {
        if !skills_dir.is_dir() {
            return Ok(());
        }

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
        Ok(())
    }
}

/// Collect `.md` and `.mdc` files under a directory, recursively.
fn collect_rule_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rule_files(&path, out);
            } else if path
                .extension()
                .map(|e| e == "md" || e == "mdc")
                .unwrap_or(false)
            {
                out.push(path);
            }
        }
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
    async fn import_cursor_extension_with_rules_commands_agents_skills() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".cursor/rules/rust.mdc",
            "---\ndescription: Rust conventions\nglobs: \"**/*.rs\"\n---\nPrefer Result over panic.\n",
        );
        write_file(
            dir.path(),
            ".cursor/rules/docs.md",
            "---\nname: docs\ndescription: Doc style\n---\nKeep docs short.\n",
        );
        write_file(
            dir.path(),
            ".cursor/commands/review.md",
            "---\nname: review\ndescription: Review code\n---\nrun review\n",
        );
        write_file(
            dir.path(),
            ".cursor/agents/reviewer.md",
            "---\nname: reviewer\ndescription: Reviews code\n---\nYou are a reviewer.\n",
        );
        write_file(
            dir.path(),
            ".cursor/skills/rust/SKILL.md",
            "---\nname: rust\ndescription: Rust coding helpers\n---\n# Rust\nGuidance for Rust work.\n",
        );
        write_file(
            dir.path(),
            ".agents/skills/caveman/SKILL.md",
            "---\nname: caveman\ndescription: Terse replies\n---\n# Caveman\nSpeak terse.\n",
        );

        let spec = CursorAdapter::import(dir.path()).await.unwrap();
        assert_eq!(spec.source, ExtensionSource::Cursor);
        assert_eq!(spec.kind, CanonicalModuleKind::Plugin);
        assert_eq!(
            spec.capabilities,
            vec!["commands", "agents", "skills", "rules"]
        );
        let commands = spec.metadata["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0], "review");
        let agents = spec.metadata["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0], "reviewer");
        let skills = spec.metadata["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 2);
        assert!(skills.iter().any(|s| s == "rust"));
        assert!(skills.iter().any(|s| s == "caveman"));
        let rules = spec.metadata["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 2);
        assert!(rules.iter().any(|r| r == "rust" || r == "docs"));
        assert!(spec.metadata.contains_key("extension_path"));
    }

    #[tokio::test]
    async fn import_cursor_extension_empty_tree_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = CursorAdapter::import(dir.path()).await.unwrap_err();
        assert!(err.to_string().contains("no .cursor/rules"));
    }

    #[tokio::test]
    async fn import_cursor_extension_not_a_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "file.md", "# hi\n");
        let err = CursorAdapter::import(&dir.path().join("file.md"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a directory"));
    }

    #[tokio::test]
    async fn import_cursor_rules_only_reports_rules_capability() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".cursor/rules/style.mdc",
            "---\ndescription: Style\n---\nUse short lines.\n",
        );

        let spec = CursorAdapter::import(dir.path()).await.unwrap();
        assert_eq!(spec.capabilities, vec!["rules"]);
        let rules = spec.metadata["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0], "style");
    }
}
