//! Claude Code extensions adapter.
//!
//! Imports a Claude Code project extension layout — `.claude/commands/*.md`
//! slash commands, `.claude/agents/*.md` subagents, `.claude/skills/<name>/`
//! skills, and a root `CLAUDE.md` instruction file — into a canonical
//! module spec. Hooks (`settings.json`) and MCP servers (`.mcp.json`) are
//! part of the Claude Code layout but are not imported yet; the capability
//! matrix reports them as Unsupported.

use crate::agent_plugins_adapter::{
    PluginCommandEntry, collect_markdown, parse_frontmatter, plugin_command_entry, read_skill_name,
    slugify,
};
use crate::extension_compat::{CanonicalModuleKind, CanonicalModuleSpec, ExtensionSource};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

/// Claude Code extension adapter
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    /// Import an extension from path.
    ///
    /// The path is the project root containing `.claude/` and/or a
    /// `CLAUDE.md` file. At least one supported item must be present.
    pub async fn import(root: &Path) -> Result<CanonicalModuleSpec> {
        if !root.is_dir() {
            anyhow::bail!(
                "{} is not a directory (expected a Claude Code project root)",
                root.display()
            );
        }

        let dir_name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "claude-code".to_string());

        let commands = Self::discover_commands(root);
        let agents = Self::discover_agents(root);
        let skills = Self::discover_skills(root).await?;
        let claude_md = root.join("CLAUDE.md");
        let has_instructions = claude_md.is_file();

        if commands.is_empty() && agents.is_empty() && skills.is_empty() && !has_instructions {
            anyhow::bail!(
                "{} contains no .claude/commands, .claude/agents, .claude/skills or CLAUDE.md",
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
        if has_instructions {
            capabilities.push("instructions".to_string());
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
        metadata.insert(
            "extension_path".to_string(),
            serde_json::json!(root.display().to_string()),
        );

        Ok(CanonicalModuleSpec {
            id: slugify(&dir_name),
            name: dir_name.clone(),
            version: "1.0.0".to_string(),
            source: ExtensionSource::ClaudeCode,
            kind: CanonicalModuleKind::Plugin,
            capabilities,
            metadata,
        })
    }

    /// Discover `.claude/commands/**/*.md` slash commands.
    fn discover_commands(root: &Path) -> Vec<PluginCommandEntry> {
        let mut found = Vec::new();
        collect_markdown(&root.join(".claude").join("commands"), &mut found);
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

    /// Discover `.claude/agents/**/*.md` subagents.
    fn discover_agents(root: &Path) -> Vec<PluginCommandEntry> {
        let mut found = Vec::new();
        collect_markdown(&root.join(".claude").join("agents"), &mut found);
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

    /// Discover `.claude/skills/<name>/SKILL.md` skills, using the skill
    /// directory name as the skill id.
    async fn discover_skills(root: &Path) -> Result<Vec<PluginCommandEntry>> {
        let skills_dir = root.join(".claude").join("skills");
        if !skills_dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut skills = Vec::new();
        for entry in std::fs::read_dir(&skills_dir)
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
    async fn import_claude_code_extension_with_commands_and_agents() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".claude/commands/review.md",
            "---\nname: review\nargument-hint: \"[file]\"\ndescription: Review code\nalowed-tools: []\n---\nrun review \"$@\"\n",
        );
        write_file(
            dir.path(),
            ".claude/agents/reviewer.md",
            "---\nname: reviewer\ndescription: Reviews code\nmodel: opus\ntools: Read, Edit\n---\nYou are a reviewer.\n",
        );
        write_file(
            dir.path(),
            ".claude/skills/rust/SKILL.md",
            "---\nname: rust\ndescription: Rust coding helpers\n---\n# Rust\nGuidance for Rust work.\n",
        );
        write_file(dir.path(), "CLAUDE.md", "# Project\nRepo instructions.\n");

        let spec = ClaudeCodeAdapter::import(dir.path()).await.unwrap();
        assert_eq!(spec.source, ExtensionSource::ClaudeCode);
        assert_eq!(spec.kind, CanonicalModuleKind::Plugin);
        assert_eq!(
            spec.capabilities,
            vec!["commands", "agents", "skills", "instructions"]
        );
        let commands = spec.metadata["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0], "review");
        let agents = spec.metadata["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0], "reviewer");
        let skills = spec.metadata["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0], "rust");
    }

    #[tokio::test]
    async fn import_claude_code_extension_empty_tree_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = ClaudeCodeAdapter::import(dir.path()).await.unwrap_err();
        assert!(err.to_string().contains("no .claude/commands"));
    }

    #[tokio::test]
    async fn import_claude_code_extension_not_a_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "file.md", "# hi\n");
        let err = ClaudeCodeAdapter::import(&dir.path().join("file.md"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a directory"));
    }
}
