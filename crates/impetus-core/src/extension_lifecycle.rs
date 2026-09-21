//! Extension install planning (dry-run).
//!
//! Lifecycle (TODO P1 §2):
//! `Manifest → ResolutionPlan → InstallPlan → Apply → ExtensionState`
//!
//! This module covers **ResolutionPlan → InstallPlan** only: resolve an install
//! intent via existing adapters and report created/modified paths **without**
//! writing. Apply / CLI / persist install state are out of scope.

use crate::agent_skills_adapter::AgentSkillsAdapter;
use crate::extension_compat::{ExtensionSource, McpModule};
use crate::ownership::path_key;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Intent to install an extension from a local source path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionInstallIntent {
    /// Skill directory or `SKILL.md` file (Agent Skills adapter).
    Skill { path: PathBuf },
    /// MCP server config JSON matching [`McpModule`] (parse only; no server spawn).
    McpConfig { path: PathBuf },
}

/// Resolved identity of what would be installed (before path actions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionPlan {
    pub source: ExtensionSource,
    pub module_id: String,
    pub module_name: String,
    pub version: String,
    /// Absolute source path that was resolved.
    pub source_path: PathBuf,
}

/// Concrete filesystem mutations that Apply would perform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallPlan {
    pub resolution: ResolutionPlan,
    /// Paths that do not exist yet and would be created.
    pub created_paths: Vec<PathBuf>,
    /// Paths that already exist and would be overwritten (subject to ownership).
    pub modified_paths: Vec<PathBuf>,
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to resolve install intent: {0}")]
    Resolve(String),
    #[error("invalid MCP config: {0}")]
    InvalidMcpConfig(String),
    #[error("path key error: {0}")]
    PathKey(String),
}

/// Plan install destinations under `target_root` without writing anything.
///
/// Destinations (project-scoped layout used by instruction resolver):
/// - Skill → `{target_root}/.impetus/skills/{id}/SKILL.md`
/// - MCP   → `{target_root}/.impetus/mcp/{id}.json`
pub async fn plan_install(
    intent: &ExtensionInstallIntent,
    target_root: &Path,
) -> Result<InstallPlan, PlanError> {
    match intent {
        ExtensionInstallIntent::Skill { path } => plan_skill(path, target_root).await,
        ExtensionInstallIntent::McpConfig { path } => plan_mcp_config(path, target_root),
    }
}

async fn plan_skill(path: &Path, target_root: &Path) -> Result<InstallPlan, PlanError> {
    let skill_md = if path.is_dir() {
        path.join("SKILL.md")
    } else {
        path.to_path_buf()
    };
    if !skill_md.is_file() {
        return Err(PlanError::Resolve(format!(
            "SKILL.md not found at {}",
            skill_md.display()
        )));
    }

    let (skill, spec) = AgentSkillsAdapter::import(&skill_md)
        .await
        .map_err(|e| PlanError::Resolve(e.to_string()))?;

    let dest = skill_dest(target_root, &spec.id);
    let resolution = ResolutionPlan {
        source: ExtensionSource::AgentSkills,
        module_id: skill.id,
        module_name: spec.name,
        version: spec.version,
        source_path: skill_md.canonicalize().unwrap_or_else(|_| skill_md.clone()),
    };
    classify_plan(resolution, vec![dest])
}

fn plan_mcp_config(path: &Path, target_root: &Path) -> Result<InstallPlan, PlanError> {
    if !path.is_file() {
        return Err(PlanError::Resolve(format!(
            "MCP config not found at {}",
            path.display()
        )));
    }

    let bytes = std::fs::read(path)?;
    let module: McpModule =
        serde_json::from_slice(&bytes).map_err(|e| PlanError::InvalidMcpConfig(e.to_string()))?;

    let module_id = sanitize_id(&module.name);
    let dest = mcp_dest(target_root, &module_id);
    let resolution = ResolutionPlan {
        source: ExtensionSource::Mcp,
        module_id,
        module_name: module.name,
        // Ponytail: MCP config has no version field yet; placeholder until extension.v1.
        version: "1.0.0".to_string(),
        source_path: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
    };
    // Dry-run: parse config only — no MCP server spawn.
    classify_plan(resolution, vec![dest])
}

fn skill_dest(target_root: &Path, skill_id: &str) -> PathBuf {
    target_root
        .join(".impetus")
        .join("skills")
        .join(skill_id)
        .join("SKILL.md")
}

fn mcp_dest(target_root: &Path, module_id: &str) -> PathBuf {
    target_root
        .join(".impetus")
        .join("mcp")
        .join(format!("{module_id}.json"))
}

fn sanitize_id(name: &str) -> String {
    let trimmed = name.trim().to_lowercase().replace(' ', "-");
    if trimmed.is_empty() {
        "unnamed".to_string()
    } else {
        trimmed
    }
}

fn classify_plan(
    resolution: ResolutionPlan,
    destinations: Vec<PathBuf>,
) -> Result<InstallPlan, PlanError> {
    let mut created_paths = Vec::new();
    let mut modified_paths = Vec::new();

    for dest in destinations {
        let key = path_key(&dest).map_err(|e| PlanError::PathKey(e.to_string()))?;
        let path = PathBuf::from(&key);
        if path.exists() {
            modified_paths.push(path);
        } else {
            created_paths.push(path);
        }
    }

    created_paths.sort();
    modified_paths.sort();

    Ok(InstallPlan {
        resolution,
        created_paths,
        modified_paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::SystemTime;

    fn write_skill(dir: &Path, name: &str, body: &str) -> PathBuf {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).expect("skill dir");
        let skill_md = skill_dir.join("SKILL.md");
        fs::write(
            &skill_md,
            format!(
                "---\nname: {name}\ndescription: test skill\nversion: \"0.1.0\"\n---\n\n{body}\n"
            ),
        )
        .expect("write skill");
        skill_md
    }

    fn write_mcp_config(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(format!("{name}.json"));
        let json = serde_json::json!({
            "name": name,
            "command": "true",
            "args": [],
            "env": {},
            "transport": "stdio",
            "capabilities": {
                "tools": true,
                "resources": false,
                "prompts": false,
                "sampling": false
            }
        });
        fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).expect("write mcp");
        path
    }

    fn snapshot_tree(root: &Path) -> Vec<(PathBuf, Option<SystemTime>, u64)> {
        let mut entries = Vec::new();
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, Option<SystemTime>, u64)>) {
            let Ok(rd) = fs::read_dir(dir) else {
                return;
            };
            for entry in rd.flatten() {
                let path = entry.path();
                let meta = entry.metadata().ok();
                let mtime = meta.as_ref().and_then(|m| m.modified().ok());
                let len = meta.map(|m| m.len()).unwrap_or(0);
                out.push((path.clone(), mtime, len));
                if path.is_dir() {
                    walk(&path, out);
                }
            }
        }
        walk(root, &mut entries);
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    #[tokio::test]
    async fn skill_dry_run_returns_create_plan_disk_unchanged() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let skill_md = write_skill(src.path(), "demo-skill", "Do the thing.");

        let before = snapshot_tree(target.path());
        let plan = plan_install(
            &ExtensionInstallIntent::Skill {
                path: skill_md.parent().unwrap().to_path_buf(),
            },
            target.path(),
        )
        .await
        .expect("plan");
        let after = snapshot_tree(target.path());

        assert_eq!(before, after, "dry-run must not mutate target tree");
        assert_eq!(plan.resolution.source, ExtensionSource::AgentSkills);
        assert_eq!(plan.resolution.module_id, "demo-skill");
        assert_eq!(plan.modified_paths, Vec::<PathBuf>::new());
        assert_eq!(plan.created_paths.len(), 1);
        assert!(
            plan.created_paths[0].ends_with(Path::new(".impetus/skills/demo-skill/SKILL.md")),
            "unexpected dest {:?}",
            plan.created_paths[0]
        );
        assert!(!plan.created_paths[0].exists());
    }

    #[tokio::test]
    async fn skill_dry_run_marks_existing_dest_as_modified() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let skill_md = write_skill(src.path(), "demo-skill", "v1");

        let dest = skill_dest(target.path(), "demo-skill");
        fs::create_dir_all(dest.parent().unwrap()).expect("dest dir");
        fs::write(&dest, "pre-existing").expect("seed dest");

        let before = snapshot_tree(target.path());
        let plan = plan_install(
            &ExtensionInstallIntent::Skill {
                path: skill_md.clone(),
            },
            target.path(),
        )
        .await
        .expect("plan");
        let after = snapshot_tree(target.path());

        assert_eq!(before, after, "dry-run must not mutate target tree");
        assert!(plan.created_paths.is_empty());
        assert_eq!(plan.modified_paths.len(), 1);
        assert_eq!(
            fs::read_to_string(&dest).unwrap(),
            "pre-existing",
            "existing file must stay untouched"
        );
    }

    #[tokio::test]
    async fn mcp_dry_run_returns_create_plan_disk_unchanged() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let config = write_mcp_config(src.path(), "filesystem");

        let before = snapshot_tree(target.path());
        let plan = plan_install(
            &ExtensionInstallIntent::McpConfig { path: config },
            target.path(),
        )
        .await
        .expect("plan");
        let after = snapshot_tree(target.path());

        assert_eq!(before, after, "dry-run must not mutate target tree");
        assert_eq!(plan.resolution.source, ExtensionSource::Mcp);
        assert_eq!(plan.resolution.module_id, "filesystem");
        assert_eq!(plan.modified_paths, Vec::<PathBuf>::new());
        assert_eq!(plan.created_paths.len(), 1);
        assert!(
            plan.created_paths[0].ends_with(Path::new(".impetus/mcp/filesystem.json")),
            "unexpected dest {:?}",
            plan.created_paths[0]
        );
        assert!(!plan.created_paths[0].exists());
    }
}
