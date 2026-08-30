use crate::extension_compat::{
    CanonicalModuleKind, CanonicalModuleSpec, CanonicalSkill, ExtensionSource, Instruction,
    InstructionContext, InstructionPriority,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Agent Skills frontmatter
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    triggers: Vec<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

/// Agent Skills adapter
pub struct AgentSkillsAdapter;

impl AgentSkillsAdapter {
    /// Parse SKILL.md file
    pub async fn parse_skill(path: &Path) -> Result<CanonicalSkill> {
        let content = tokio::fs::read_to_string(path)
            .await
            .context("Failed to read SKILL.md")?;

        let (frontmatter, body) = Self::extract_frontmatter(&content)?;

        let skill_id = frontmatter.name.to_lowercase().replace(' ', "-");

        let instructions = vec![Instruction {
            content: body.trim().to_string(),
            context: InstructionContext::Task,
            priority: InstructionPriority::High,
        }];

        let mut metadata = HashMap::new();
        if let Some(author) = frontmatter.author {
            metadata.insert("author".to_string(), serde_json::json!(author));
        }
        for (k, v) in frontmatter.extra {
            metadata.insert(k, v);
        }

        Ok(CanonicalSkill {
            id: skill_id.clone(),
            name: frontmatter.name.clone(),
            description: frontmatter.description.clone(),
            instructions,
            tools: vec![],
            triggers: frontmatter.triggers.clone(),
            metadata,
        })
    }

    /// Convert CanonicalSkill to CanonicalModuleSpec
    pub fn to_module_spec(skill: &CanonicalSkill, version: Option<String>) -> CanonicalModuleSpec {
        let mut capabilities = vec!["instructions".to_string()];
        if !skill.triggers.is_empty() {
            capabilities.push("triggers".to_string());
        }
        if !skill.tools.is_empty() {
            capabilities.push("tools".to_string());
        }

        CanonicalModuleSpec {
            id: skill.id.clone(),
            name: skill.name.clone(),
            version: version.unwrap_or_else(|| "1.0.0".to_string()),
            source: ExtensionSource::AgentSkills,
            kind: CanonicalModuleKind::Skill,
            capabilities,
            metadata: skill.metadata.clone(),
        }
    }

    /// Import skill from path
    pub async fn import(path: &Path) -> Result<(CanonicalSkill, CanonicalModuleSpec)> {
        let skill = Self::parse_skill(path).await?;
        let version = skill
            .metadata
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let spec = Self::to_module_spec(&skill, version);
        Ok((skill, spec))
    }

    /// Extract YAML frontmatter and markdown body
    fn extract_frontmatter(content: &str) -> Result<(SkillFrontmatter, String)> {
        let lines: Vec<&str> = content.lines().collect();

        if lines.is_empty() || lines[0] != "---" {
            anyhow::bail!("Missing YAML frontmatter (expected leading ---)");
        }

        let end_idx = lines[1..]
            .iter()
            .position(|&line| line == "---")
            .context("Missing closing --- for frontmatter")?
            + 1;

        let frontmatter_lines = &lines[1..end_idx];
        let frontmatter_text = frontmatter_lines.join("\n");

        let frontmatter: SkillFrontmatter =
            serde_yaml::from_str(&frontmatter_text).context("Failed to parse YAML frontmatter")?;

        let body = lines[end_idx + 1..].join("\n");

        Ok((frontmatter, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn extract_frontmatter_valid() {
        let content = r#"---
name: test-skill
description: A test skill
triggers:
  - test
  - example
---

# Test Skill

This is the skill body.
"#;

        let (frontmatter, body) = AgentSkillsAdapter::extract_frontmatter(content).unwrap();
        assert_eq!(frontmatter.name, "test-skill");
        assert_eq!(frontmatter.description, "A test skill");
        assert_eq!(frontmatter.triggers.len(), 2);
        assert!(body.contains("# Test Skill"));
    }

    #[test]
    fn extract_frontmatter_missing_delimiter() {
        let content = "name: test\ndescription: no delimiters";
        let result = AgentSkillsAdapter::extract_frontmatter(content);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parse_skill_creates_canonical() {
        let content = r#"---
name: Example Skill
description: An example skill for testing
triggers:
  - example
  - test
version: "1.0.0"
author: "Test Author"
---

# Example Skill

Use this skill for testing.
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let skill = AgentSkillsAdapter::parse_skill(file.path()).await.unwrap();

        assert_eq!(skill.id, "example-skill");
        assert_eq!(skill.name, "Example Skill");
        assert_eq!(skill.triggers.len(), 2);
        assert_eq!(skill.instructions.len(), 1);
        assert!(skill.instructions[0].content.contains("Use this skill"));
        assert_eq!(
            skill.metadata.get("author").unwrap().as_str().unwrap(),
            "Test Author"
        );
    }

    #[tokio::test]
    async fn import_produces_both_types() {
        let content = r#"---
name: Import Test
description: Test import
---

Body content.
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let (skill, spec) = AgentSkillsAdapter::import(file.path()).await.unwrap();

        assert_eq!(skill.id, "import-test");
        assert_eq!(spec.id, "import-test");
        assert_eq!(spec.source, ExtensionSource::AgentSkills);
        assert_eq!(spec.kind, CanonicalModuleKind::Skill);
        assert!(spec.capabilities.contains(&"instructions".to_string()));
    }

    #[test]
    fn to_module_spec_includes_capabilities() {
        let skill = CanonicalSkill {
            id: "test".to_string(),
            name: "Test".to_string(),
            description: "Test".to_string(),
            instructions: vec![],
            tools: vec![],
            triggers: vec!["trigger1".to_string()],
            metadata: HashMap::new(),
        };

        let spec = AgentSkillsAdapter::to_module_spec(&skill, None);
        assert!(spec.capabilities.contains(&"instructions".to_string()));
        assert!(spec.capabilities.contains(&"triggers".to_string()));
        assert_eq!(spec.version, "1.0.0");
    }
}
