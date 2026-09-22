//! CLI wrappers for extension lifecycle plan + install + remove + enable +
//! disable + unload + list + doctor + repair.
//!
//! Offline (no daemon IPC): wraps plan/apply/remove/enable/disable/unload/
//! doctor/repair. CLI is the control plane; daemon reloads Enabled rows via
//! `ExtensionRuntime::reload_from_store` on restart.

use anyhow::{Context, Result};
use clap::ValueEnum;
use impetus_core::{
    ExtensionInstallIntent, ExtensionLifecycleStatus, ExtensionRuntime, ExtensionStateStore,
    InstallPlan, OwnershipStore, apply_install, disable_install, doctor_install, enable_install,
    plan_install, remove_install, repair_install, unload_install,
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
    manifest: &'a impetus_core::ExtensionManifest,
    created_paths: &'a [PathBuf],
    modified_paths: &'a [PathBuf],
}

fn print_plan_human(plan: &InstallPlan) {
    let r = &plan.resolution;
    let m = &plan.manifest;
    println!("Install plan (dry-run; no writes)");
    println!("  source: {:?}", r.source);
    println!("  module_id: {}", r.module_id);
    println!("  module_name: {}", r.module_name);
    println!("  version: {}", r.version);
    println!("  source_path: {}", r.source_path.display());
    println!("  manifest.id: {}", m.id);
    println!("  manifest.kind: {:?}", m.kind);
    println!("  manifest.digest: {}", m.digest);
    println!("  manifest.capabilities: {:?}", m.capabilities);
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
            manifest: &plan.manifest,
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

/// Enable a disabled or unloaded install (restore sidelined files).
pub fn enable(installation_id: &str, root: Option<&Path>, json: bool) -> Result<()> {
    let target_root = resolve_target_root(root)?;
    let (ownership, state_store) = open_stores(&target_root)?;
    let result = enable_install(installation_id, &ownership, &state_store)
        .with_context(|| format!("enable install {installation_id}"))?;
    print_lifecycle(&result, "Enabled", json)
}

/// Disable install: sideline files; not loaded on restart.
pub fn disable(installation_id: &str, root: Option<&Path>, json: bool) -> Result<()> {
    let target_root = resolve_target_root(root)?;
    let (ownership, state_store) = open_stores(&target_root)?;
    let result = disable_install(installation_id, &ownership, &state_store)
        .with_context(|| format!("disable install {installation_id}"))?;
    print_lifecycle(&result, "Disabled", json)
}

/// Unload install: sideline files + drop from runtime reload set.
pub fn unload(installation_id: &str, root: Option<&Path>, json: bool) -> Result<()> {
    let target_root = resolve_target_root(root)?;
    let (ownership, state_store) = open_stores(&target_root)?;
    let result = unload_install(installation_id, &ownership, &state_store)
        .with_context(|| format!("unload install {installation_id}"))?;
    print_lifecycle(&result, "Unloaded", json)
}

fn print_lifecycle(result: &impetus_core::LifecycleResult, verb: &str, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
    } else {
        println!("{verb} extension");
        println!("  installation_id: {}", result.installation_id);
        println!("  status: {:?}", result.status);
        println!("  touched: {}", result.touched_paths.len());
        for p in &result.touched_paths {
            println!("    ~ {}", p.display());
        }
    }
    Ok(())
}

/// List install states (+ which would load on restart).
pub fn list(root: Option<&Path>, json: bool) -> Result<()> {
    let target_root = resolve_target_root(root)?;
    let (_ownership, state_store) = open_stores(&target_root)?;
    let states = state_store.list_all().context("list install states")?;
    let runtime = ExtensionRuntime::reload_from_store(&state_store).context("reload runtime")?;

    #[derive(Serialize)]
    struct Row<'a> {
        installation_id: &'a str,
        module_id: &'a str,
        module_name: &'a str,
        version: &'a str,
        status: ExtensionLifecycleStatus,
        loaded_on_restart: bool,
    }

    let rows: Vec<Row<'_>> = states
        .iter()
        .map(|s| Row {
            installation_id: &s.installation_id,
            module_id: &s.resolution.module_id,
            module_name: &s.resolution.module_name,
            version: &s.resolution.version,
            status: s.status,
            loaded_on_restart: runtime.is_loaded(&s.installation_id),
        })
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        println!(
            "Extensions ({} install(s); {} loaded on restart)",
            rows.len(),
            runtime.loaded_ids().len()
        );
        if rows.is_empty() {
            println!("  (none)");
            return Ok(());
        }
        for row in &rows {
            println!(
                "  [{}] {}  {} ({}) v{}",
                format!("{:?}", row.status).to_lowercase(),
                row.installation_id,
                row.module_name,
                row.module_id,
                row.version
            );
            println!("    loaded_on_restart: {}", row.loaded_on_restart);
        }
    }
    Ok(())
}

/// Report install-state + ownership health (read-only).
pub fn doctor(installation_id: Option<&str>, root: Option<&Path>, json: bool) -> Result<()> {
    let target_root = resolve_target_root(root)?;
    let (ownership, state_store) = open_stores(&target_root)?;
    let report =
        doctor_install(installation_id, &ownership, &state_store).context("extension doctor")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_doctor_human(&report);
    }
    Ok(())
}

fn print_doctor_human(report: &impetus_core::DoctorReport) {
    let overall = if report.healthy {
        "healthy"
    } else {
        "unhealthy"
    };
    println!(
        "Extension doctor ({overall}; {} installation(s))",
        report.installations.len()
    );
    if report.installations.is_empty() {
        println!("  (no install state rows under this root)");
        return;
    }
    for install in &report.installations {
        let mark = if install.healthy { "ok" } else { "FAIL" };
        println!("  [{mark}] {}", install.installation_id);
        println!("    state_present: {}", install.state_present);
        if let Some(res) = &install.resolution {
            println!(
                "    module: {} ({}) v{}",
                res.module_name, res.module_id, res.version
            );
        }
        if install.paths.is_empty() {
            println!("    paths: (none)");
            continue;
        }
        for path in &install.paths {
            match &path.status {
                impetus_core::PathHealthStatus::Ok => {
                    println!("    ok  {}", path.path);
                }
                impetus_core::PathHealthStatus::Missing => {
                    println!("    MISSING {}", path.path);
                }
                impetus_core::PathHealthStatus::DigestMismatch { expected, actual } => {
                    println!("    DIGEST {}", path.path);
                    println!("      expected: {expected}");
                    println!("      actual:   {actual}");
                }
                impetus_core::PathHealthStatus::Unreadable { reason } => {
                    println!("    UNREADABLE {} ({reason})", path.path);
                }
            }
        }
    }
}

/// Repair owned paths from recorded source (`--force` for digest mismatch).
pub fn repair(installation_id: &str, root: Option<&Path>, force: bool, json: bool) -> Result<()> {
    let target_root = resolve_target_root(root)?;
    let (ownership, state_store) = open_stores(&target_root)?;
    let result = repair_install(installation_id, &ownership, &state_store, force)
        .with_context(|| format!("repair install {installation_id}"))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Repaired extension");
        println!("  installation_id: {}", result.installation_id);
        println!("  repaired: {}", result.repaired_paths.len());
        for p in &result.repaired_paths {
            println!("    ~ {}", p.display());
        }
        println!("  skipped (ok): {}", result.skipped_ok.len());
        for p in &result.skipped_ok {
            println!("    = {}", p.display());
        }
        if result.repaired_paths.is_empty() && result.skipped_ok.is_empty() {
            println!("  (no ownership paths for this installation)");
        }
    }
    Ok(())
}
