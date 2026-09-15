//! Agent Plugins adapter.
//!
//! Imports Anthropic Agent Plugins packages — `.claude-plugin/plugin.json`
//! manifest + `commands/*.md` entry scripts + optional `CLAUDE.md` skill —
//! into a canonical module spec. Format follows the upstream plugins
//! reference: unknown top-level manifest fields are ignored, which keeps a
//! single manifest usable across Claude Code / VS Code / Cursor / npm
//! ecosystems. Hooks, MCP servers, LSP servers and monitors declared in the
//! manifest are not imported yet (capability matrix reports them as
//! Unsupported).

use crate::extension_compat::{CanonicalModuleKind, CanonicalModuleSpec, ExtensionSource};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// plugin.json field that may be a single path or an array of paths.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

/// `.claude-plugin/plugin.json` manifest (camelCase per upstream spec).
/// All fields optional; plugin discovery is also valid without a manifest
/// (skills-directory style: CLAUDE.md + commands/*.md).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginManifest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    commands: Vec<OneOrMany>,
}

/// A discovered plugin command entry script.
#[derive(Debug, Clone)]
pub struct PluginCommandEntry {
    pub name: String,
    pub description: Option<String>,
    pub path: PathBuf,
}

/// Agent Plugins adapter
pub struct AgentPluginsAdapter;

impl AgentPluginsAdapter {
    /// Import a plugin package from path.
    ///
    /// The path may be a plugin directory (manifest at
    /// `.claude-plugin/plugin.json`, or auto-discovery without one) or a
    /// `plugin.json` manifest file directly.
    pub async fn import(path: &Path) -> Result<CanonicalModuleSpec> {
        let (plugin_root, manifest_path) = Self::locate(path)?;
        let mut manifest = PluginManifest::default();

        if let Some(manifest_path) = manifest_path {
            let content = tokio::fs::read_to_string(&manifest_path)
                .await
                .context("Failed to read plugin.json")?;
            manifest = serde_json::from_str(&content).context("Failed to parse plugin.json")?;
        }

        Self::build_spec(&plugin_root, &manifest).await
    }

    /// Discover command entry scripts for a plugin.
    ///
    /// Uses manifest-declared `commands` paths when present (existing files
    /// only), otherwise scans `commands/` recursively for `*.md`.
    fn discover_commands(root: &Path, manifest: &PluginManifest) -> Vec<PluginCommandEntry> {
        let mut commands = Vec::new();

        let declared: Vec<String> = manifest
            .commands
            .iter()
            .cloned()
            .flat_map(OneOrMany::into_vec)
            .map(|p| normalize_path(&p))
            .collect();

        if !declared.is_empty() {
            for rel in declared {
                let path = root.join(&rel);
                if path.is_file() && path.extension().map(|e| e == "md").unwrap_or(false) {
                    commands.push(plugin_command_entry(&rel, &path));
                }
            }
            commands.sort_by(|a, b| a.name.cmp(&b.name));
            return commands;
        }

        let commands_dir = root.join("commands");
        if commands_dir.is_dir() {
            let mut found = Vec::new();
            collect_markdown(&commands_dir, &mut found);
            found.sort();
            for path in found {
                commands.push(plugin_command_entry(&path.display().to_string(), &path));
            }
        }

        commands
    }

    /// Locate the plugin root and optional manifest path for an import path.
    fn locate(path: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
        if path.is_dir() {
            let manifest = path.join(".claude-plugin").join("plugin.json");
            if manifest.is_file() {
                return Ok((path.to_path_buf(), Some(manifest)));
            }
            // Skills-directory style plugin without manifest
            return Ok((path.to_path_buf(), None));
        }

        if path.is_file()
            && path
                .file_name()
                .map(|n| n == "plugin.json")
                .unwrap_or(false)
        {
            // `<plugin>/.claude-plugin/plugin.json` resolves to `<plugin>`;
            // a bare manifest file resolves to its own directory.
            let dir = path
                .parent()
                .context("plugin.json has no parent directory")?;
            let root = match dir
                .file_name()
                .map(|n| n == ".claude-plugin")
                .unwrap_or(false)
            {
                true => dir.parent().unwrap_or(dir),
                false => dir,
            };
            return Ok((root.to_path_buf(), Some(path.to_path_buf())));
        }

        anyhow::bail!(
            "{} is neither a plugin directory nor a plugin.json file",
            path.display()
        )
    }

    /// Build a canonical module spec from plugin root + manifest.
    async fn build_spec(root: &Path, manifest: &PluginManifest) -> Result<CanonicalModuleSpec> {
        let dir_name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "plugin".to_string());

        let id = manifest
            .name
            .as_deref()
            .map(slugify)
            .or_else(|| Some(slugify(&dir_name)))
            .context("plugin name cannot be empty")?;
        if id.is_empty() {
            anyhow::bail!("plugin name cannot be empty");
        }

        let commands = Self::discover_commands(root, manifest);
        let has_commands = !commands.is_empty();

        // CLAUDE.md at plugin root loads as a single plugin skill.
        let skill_md = root.join("CLAUDE.md");
        let has_skill = skill_md.is_file();
        let skill_name = if has_skill {
            read_skill_name(&skill_md)
                .await
                .unwrap_or_else(|| id.clone())
        } else {
            String::new()
        };

        let mut capabilities = Vec::new();
        if has_commands {
            capabilities.push("commands".to_string());
        }
        if has_skill {
            capabilities.push("skills".to_string());
        }
        if capabilities.is_empty() {
            anyhow::bail!(
                "{} contains no plugin commands or CLAUDE.md skill",
                root.display()
            );
        }

        let mut metadata = HashMap::new();
        if let Some(desc) = manifest.description.as_deref() {
            metadata.insert("description".to_string(), serde_json::json!(desc));
        }
        if has_skill {
            metadata.insert("skill".to_string(), serde_json::json!(skill_name));
        }
        if has_commands {
            let names: Vec<String> = commands.iter().map(|c| c.name.clone()).collect();
            metadata.insert("commands".to_string(), serde_json::json!(names));
        }
        metadata.insert(
            "plugin_path".to_string(),
            serde_json::json!(root.display().to_string()),
        );

        Ok(CanonicalModuleSpec {
            id,
            name: manifest
                .display_name
                .clone()
                .unwrap_or_else(|| dir_name.clone()),
            version: manifest
                .version
                .clone()
                .unwrap_or_else(|| "1.0.0".to_string()),
            source: ExtensionSource::AgentPlugins,
            kind: CanonicalModuleKind::Plugin,
            capabilities,
            metadata,
        })
    }
}

/// Slugify a plugin id (kebab-case). Shared with the Claude Code adapter.
pub(crate) fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = true;
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Normalize a manifest-declared path (strip leading `./`).
pub(crate) fn normalize_path(p: &str) -> String {
    if p.starts_with("./") {
        p.replacen("./", "", 1)
    } else {
        p.to_string()
    }
}

/// Build a command entry for a markdown file, preferring frontmatter
/// name/description with a file-stem fallback for the name. Sync by design:
/// discovery runs in a hot loop over a small local tree.
pub(crate) fn plugin_command_entry(rel: &str, path: &Path) -> PluginCommandEntry {
    let fallback = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            Path::new(rel)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "command".to_string())
        });

    let (name, description) = match std::fs::read_to_string(path) {
        Ok(content) => parse_command_frontmatter(&content)
            .map(|(n, d)| (n.unwrap_or_else(|| fallback.clone()), d))
            .unwrap_or((fallback.clone(), None)),
        Err(_) => (fallback.clone(), None),
    };

    PluginCommandEntry {
        name,
        description,
        path: path.to_path_buf(),
    }
}

/// Parse the YAML frontmatter at the top of a markdown file, if present.
pub(crate) fn parse_frontmatter(content: &str) -> Option<serde_yaml::Value> {
    let lines: Vec<&str> = content.lines().collect();
    let end = parse_frontmatter_end(&lines)?;
    serde_yaml::from_str(&lines[1..end].join("\n")).ok()
}

/// Parse the YAML frontmatter of a command file. Returns (name, description);
/// either may be absent.
fn parse_command_frontmatter(content: &str) -> Option<(Option<String>, Option<String>)> {
    let fm = parse_frontmatter(content)?;
    let name = fm.get("name").and_then(|v| v.as_str()).map(String::from);
    let description = fm
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);
    Some((name, description))
}

/// Index of the closing `---` of a frontmatter block, if present.
fn parse_frontmatter_end(lines: &[&str]) -> Option<usize> {
    if lines.first() != Some(&"---") {
        return None;
    }
    lines[1..].iter().position(|&l| l == "---").map(|i| i + 1)
}

/// Collect markdown files under a directory, recursively.
pub(crate) fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_markdown(&path, out);
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                out.push(path);
            }
        }
    }
}

/// Read the plugin skill name from CLAUDE.md: frontmatter `name` when
/// present, otherwise the first markdown H1 heading.
pub(crate) async fn read_skill_name(path: &Path) -> Option<String> {
    let content = tokio::fs::read_to_string(path).await.ok()?;

    if let Some(name) = parse_frontmatter(&content)
        .and_then(|fm| fm.get("name").and_then(|v| v.as_str()).map(String::from))
    {
        return Some(name);
    }

    content
        .lines()
        .find_map(|l| l.trim().strip_prefix("# "))
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
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
    async fn import_plugin_with_manifest_and_commands() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".claude-plugin/plugin.json",
            r#"{
                "name": "formatter",
                "displayName": "Formatter",
                "version": "1.2.0",
                "description": "Format code",
                "commands": ["commands/format.md", "./commands/lint.md"]
            }"#,
        );
        write_file(
            dir.path(),
            "commands/format.md",
            "---\nname: format\narguments: \"**\"\n---\nruff format \"$@\"\n",
        );
        write_file(
            dir.path(),
            "commands/lint.md",
            "---\nname: lint\n---\nruff check \"$@\"\n",
        );
        write_file(
            dir.path(),
            "CLAUDE.md",
            "# Formatter Skill\n\nUse formatter.\n",
        );

        let spec = AgentPluginsAdapter::import(dir.path()).await.unwrap();

        assert_eq!(spec.id, "formatter");
        assert_eq!(spec.name, "Formatter");
        assert_eq!(spec.version, "1.2.0");
        assert_eq!(spec.source, ExtensionSource::AgentPlugins);
        assert_eq!(spec.kind, CanonicalModuleKind::Plugin);
        assert!(spec.capabilities.contains(&"commands".to_string()));
        assert!(spec.capabilities.contains(&"skills".to_string()));
        assert_eq!(
            spec.metadata
                .get("commands")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(spec.metadata.get("skill").unwrap(), "Formatter Skill");
        assert_eq!(spec.metadata.get("description").unwrap(), "Format code");
    }

    #[tokio::test]
    async fn import_plugin_without_manifest_auto_discovers() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "commands/build.md",
            "---\nname: build\n---\nmake build\n",
        );
        write_file(dir.path(), "CLAUDE.md", "# Build Skill\n");

        let spec = AgentPluginsAdapter::import(dir.path()).await.unwrap();

        let dir_name = dir.path().file_name().unwrap().to_string_lossy();
        assert_eq!(spec.id, slugify(&dir_name));
        assert_eq!(spec.version, "1.0.0");
        assert!(spec.capabilities.contains(&"commands".to_string()));
        assert!(spec.capabilities.contains(&"skills".to_string()));
    }

    #[tokio::test]
    async fn import_via_manifest_file_path() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".claude-plugin/plugin.json",
            r#"{"name": "direct-file", "commands": ["commands/run.md"]}"#,
        );
        write_file(
            dir.path(),
            "commands/run.md",
            "---\nname: run\n---\necho hi\n",
        );

        let manifest_path = dir.path().join(".claude-plugin").join("plugin.json");
        let spec = AgentPluginsAdapter::import(&manifest_path).await.unwrap();
        assert_eq!(spec.id, "direct-file");
        assert!(spec.capabilities.contains(&"commands".to_string()));
    }

    #[tokio::test]
    async fn import_unknown_manifest_fields_ignored() {
        // Upstream: unknown top-level fields (e.g. VS Code manifest keys)
        // must not break parsing.
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".claude-plugin/plugin.json",
            r#"{
                "name": "hybrid",
                "publisher": "vendor",
                "engines": {"vscode": "^1.90.0"},
                "contributes": {"commands": []}
            }"#,
        );
        write_file(dir.path(), "CLAUDE.md", "# Hybrid\n");

        let spec = AgentPluginsAdapter::import(dir.path()).await.unwrap();
        assert_eq!(spec.id, "hybrid");
        assert!(spec.capabilities.contains(&"skills".to_string()));
    }

    #[tokio::test]
    async fn import_empty_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = AgentPluginsAdapter::import(dir.path()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn import_invalid_path_errors() {
        let result = AgentPluginsAdapter::import(Path::new("/nonexistent/plugin")).await;
        assert!(result.is_err());
    }

    #[test]
    fn discovers_nested_commands_recursively() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "commands/a.md", "---\n---\nx\n");
        write_file(dir.path(), "commands/nested/b.md", "---\n---\ny\n");

        let manifest = PluginManifest::default();
        let commands = AgentPluginsAdapter::discover_commands(dir.path(), &manifest);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].name, "a");
        assert_eq!(commands[1].name, "b");
    }

    #[test]
    fn slugify_kebab_case() {
        assert_eq!(slugify("My Awesome Plugin"), "my-awesome-plugin");
        assert_eq!(slugify("already-kebab"), "already-kebab");
        assert_eq!(slugify("  spaced  "), "spaced");
    }
}
