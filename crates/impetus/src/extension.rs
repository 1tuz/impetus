//! CLI wrappers for extension lifecycle plan + install + remove.
//!
//! Offline (no daemon): wraps `plan_install` / `apply_install` / `remove_install`.
//! doctor / repair stay out of scope.

use anyhow::{Context, Result};
use clap::ValueEnum;
use impetus_core::{
    ExtensionInstallIntent, ExtensionStateStore, InstallPlan, OwnershipStore, apply_install,
    plan_install, remove_install,
};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExtensionKind {
    /// Agent Skills SKILL.md (file or skill directory)
    Skill,
    /// MCP server config JSON (parse only; no server spawn)
    Mcp,
}

fn resolve_target_root(root: Option<&Path>) -> Result<PathBuf> {
    match root {
        Some(path) => path
            .canonicalize()
            .with_context(|| format!("canonicalize target root {}", path.display())),
        None => std::env::current_dir()
            .context("current directory")?
            .canonicalize()
            .context("canonicalize current directory"),
    }
}

fn intent_for(kind: ExtensionKind, path: &Path) -> ExtensionInstallIntent {
    match kind {
        ExtensionKind::Skill => ExtensionInstallIntent::Skill {
            path: path.to_path_buf(),
        },
        ExtensionKind::Mcp => ExtensionInstallIntent::McpConfig {
            path: path.to_path_buf(),
        },
    }
}

fn impetus_dir(target_root: &Path) -> PathBuf {
    target_root.join(".impetus")
}

fn open_stores(target_root: &Path) -> Result<(OwnershipStore, ExtensionStateStore)> {
    let base = impetus_dir(target_root);
    let ownership =
        OwnershipStore::open(base.join("ownership.db")).context("open ownership store")?;
    let state = ExtensionStateStore::open(base.join("install_state.db"))
        .context("open install state store")?;
    Ok((ownership, state))
}

#[derive(Serialize)]
struct PlanOutput<'a> {
    resolution: &'a impetus_core::ResolutionPlan,
    created_paths: &'a [PathBuf],
    modified_paths: &'a [PathBuf],
}

fn print_plan_human(plan: &InstallPlan) {
    let r = &plan.resolution;
    println!("Install plan (dry-run; no writes)");
    println!("  source: {:?}", r.source);
    println!("  module_id: {}", r.module_id);
    println!("  module_name: {}", r.module_name);
    println!("  version: {}", r.version);
    println!("  source_path: {}", r.source_path.display());
    println!("  create ({}):", plan.created_paths.len());
    for path in &plan.created_paths {
        println!("    + {}", path.display());
    }
    println!("  modify ({}):", plan.modified_paths.len());
    for path in &plan.modified_paths {
        println!("    ~ {}", path.display());
    }
}

/// Dry-run plan: print InstallPlan; do not write.
pub async fn plan(kind: ExtensionKind, path: &Path, root: Option<&Path>, json: bool) -> Result<()> {
    let target_root = resolve_target_root(root)?;
    let intent = intent_for(kind, path);
    let plan = plan_install(&intent, &target_root)
        .await
        .with_context(|| format!("plan install from {}", path.display()))?;

    if json {
        let out = PlanOutput {
            resolution: &plan.resolution,
            created_paths: &plan.created_paths,
            modified_paths: &plan.modified_paths,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print_plan_human(&plan);
    }
    Ok(())
}

/// Plan then apply: write files, ownership, install state.
pub async fn install(
    kind: ExtensionKind,
    path: &Path,
    root: Option<&Path>,
    json: bool,
) -> Result<()> {
    let target_root = resolve_target_root(root)?;
    let intent = intent_for(kind, path);
    let plan = plan_install(&intent, &target_root)
        .await
        .with_context(|| format!("plan install from {}", path.display()))?;

    let (ownership, state_store) = open_stores(&target_root)?;
    let state = apply_install(&plan, &ownership, &state_store).context("apply install")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&state)?);
    } else {
        println!("Installed extension");
        println!("  installation_id: {}", state.installation_id);
        println!("  module_id: {}", state.resolution.module_id);
        println!("  module_name: {}", state.resolution.module_name);
        println!("  version: {}", state.resolution.version);
        println!("  created: {}", state.created_paths.len());
        for p in &state.created_paths {
            println!("    + {}", p.display());
        }
        println!("  modified: {}", state.modified_paths.len());
        for p in &state.modified_paths {
            println!("    ~ {}", p.display());
        }
        println!("  ownership records: {}", state.ownership.len());
    }
    Ok(())
}

/// Remove install by `installation_id` (ownership proof + state cleanup).
pub fn remove(installation_id: &str, root: Option<&Path>, json: bool) -> Result<()> {
    let target_root = resolve_target_root(root)?;
    let (ownership, state_store) = open_stores(&target_root)?;
    let result = remove_install(installation_id, &ownership, &state_store)
        .with_context(|| format!("remove install {installation_id}"))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Removed extension");
        println!("  installation_id: {}", result.installation_id);
        println!("  removed: {}", result.removed_paths.len());
        for p in &result.removed_paths {
            println!("    - {}", p.display());
        }
    }
    Ok(())
}
