use anyhow::{Context, Result};
use impetus_core::{ExtensionAdapter, ExtensionRegistry, ExtensionSource, ImportCapability};
use std::path::{Path, PathBuf};

/// Default skills search paths
fn default_skills_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // ~/.agents/skills/
    if let Ok(home) = std::env::var("HOME") {
        paths.push(PathBuf::from(home).join(".agents/skills"));
    }

    // ./.agents/skills/ (project-local)
    paths.push(PathBuf::from(".agents/skills"));

    paths
}

/// Discover SKILL.md files in a directory
fn discover_skills(dir: &Path) -> Vec<PathBuf> {
    let mut skills = Vec::new();

    if !dir.is_dir() {
        return skills;
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let skill_md = path.join("SKILL.md");
                if skill_md.is_file() {
                    skills.push(skill_md);
                }
            } else if path.file_name().map(|n| n == "SKILL.md").unwrap_or(false) {
                skills.push(path);
            }
        }
    }

    skills.sort();
    skills
}

/// List all discovered skills
pub async fn list_skills() -> Result<()> {
    let adapter = ExtensionAdapter::new();
    let registry = ExtensionRegistry::new();
    let mut total = 0;

    println!("Agent Skills Discovery\n");

    for search_path in default_skills_paths() {
        if !search_path.exists() {
            continue;
        }

        println!("Search path: {}", search_path.display());

        let skill_files = discover_skills(&search_path);
        if skill_files.is_empty() {
            println!("  (no skills found)\n");
            continue;
        }

        for skill_path in &skill_files {
            let parent = skill_path.parent().unwrap_or(skill_path);
            let result = adapter.import(ExtensionSource::AgentSkills, parent).await?;

            match (&result.capability, &result.canonical) {
                (ImportCapability::Supported, Some(spec)) => {
                    registry.register(spec.clone())?;
                    println!("  ✓ {}", spec.id);
                    if !spec.capabilities.is_empty() {
                        println!("    capabilities: {}", spec.capabilities.join(", "));
                    }
                    if !result.warnings.is_empty() {
                        for w in &result.warnings {
                            println!("    ⚠ {}", w);
                        }
                    }
                    total += 1;
                }
                _ => {
                    let name = parent
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    println!("  ✗ {} — {:?}", name, result.capability);
                    for e in &result.errors {
                        println!("    error: {}", e);
                    }
                }
            }
        }
        println!();
    }

    println!("Total: {} skill(s) discovered", total);
    Ok(())
}

/// Import a specific skill by path
pub async fn import_skill(path: &Path) -> Result<()> {
    let adapter = ExtensionAdapter::new();

    let skill_path = if path.is_dir() {
        path.join("SKILL.md")
    } else {
        path.to_path_buf()
    };

    if !skill_path.is_file() {
        anyhow::bail!("SKILL.md not found at {}", skill_path.display());
    }

    let parent = skill_path.parent().unwrap_or(&skill_path);
    let result = adapter
        .import(ExtensionSource::AgentSkills, parent)
        .await
        .context("Failed to import skill")?;

    match (&result.capability, &result.canonical) {
        (ImportCapability::Supported, Some(spec)) => {
            println!("Imported skill:");
            println!("  ID: {}", spec.id);
            println!("  Name: {}", spec.name);
            println!("  Version: {}", spec.version);
            println!("  Capabilities: {}", spec.capabilities.join(", "));
            if !spec.metadata.is_empty() {
                println!("  Metadata:");
                for (k, v) in &spec.metadata {
                    println!("    {}: {}", k, v);
                }
            }
            for w in &result.warnings {
                println!("  ⚠ {}", w);
            }
            Ok(())
        }
        _ => {
            anyhow::bail!(
                "Import failed: {:?}\nErrors: {:?}\nWarnings: {:?}",
                result.capability,
                result.errors,
                result.warnings
            );
        }
    }
}

/// Show details of a specific skill
pub async fn show_skill(name: &str) -> Result<()> {
    let adapter = ExtensionAdapter::new();

    for search_path in default_skills_paths() {
        let skill_dir = search_path.join(name);
        if !skill_dir.is_dir() {
            continue;
        }

        let result = adapter
            .import(ExtensionSource::AgentSkills, &skill_dir)
            .await?;

        if let (ImportCapability::Supported, Some(spec)) = (&result.capability, &result.canonical) {
            println!("Skill: {}", spec.name);
            println!("ID: {}", spec.id);
            println!("Version: {}", spec.version);
            println!("Source: {:?}", spec.source);
            println!("Kind: {:?}", spec.kind);
            println!("Capabilities: {}", spec.capabilities.join(", "));

            if !spec.metadata.is_empty() {
                println!("\nMetadata:");
                for (k, v) in &spec.metadata {
                    println!("  {}: {}", k, v);
                }
            }

            // Show instructions preview
            use impetus_core::AgentSkillsAdapter;
            let skill_md = skill_dir.join("SKILL.md");
            if skill_md.is_file() {
                let (skill, _) = AgentSkillsAdapter::import(&skill_md).await?;
                if !skill.instructions.is_empty() {
                    let content = &skill.instructions[0].content;
                    let preview_len = content.len().min(500);
                    println!("\nInstructions (first {} chars):", preview_len);
                    println!("{}", &content[..preview_len]);
                    if content.len() > 500 {
                        println!("... ({} total chars)", content.len());
                    }
                }
            }

            return Ok(());
        }
    }

    anyhow::bail!("Skill '{}' not found in default search paths", name)
}
