//! CLI wrappers for extension lifecycle plan + install + remove + enable +
//! disable + unload + list + doctor + repair + migrate.
//!
//! Offline (no daemon IPC): wraps plan/apply/remove/enable/disable/unload/
//! doctor/repair/migrate. CLI is presentation/control over the same daemon
//! SoT when `--data-dir` / `$IMPETUS_DATA_DIR` is used (no `--root`).

use anyhow::{Context, Result};
use clap::ValueEnum;
use impetus_core::{
    ExtensionHost, ExtensionInstallIntent, ExtensionInstallLayout, ExtensionLifecycleStatus,
    ExtensionRuntime, ExtensionStateStore, InstallPlan, OwnershipStore, apply_install,
    build_effective_inventory, daemon_extension_state_db, disable_install, doctor_install,
    enable_install, load_daemon_extension_runtime, migrate_legacy_to_daemon,
    plan_install_with_layout, remove_install, repair_install, unload_install,
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

/// Where CLI reads/writes install state.
#[derive(Debug, Clone)]
enum ControlPlane {
    /// Project `.impetus/` layout (legacy / `--root`).
    Workspace { root: PathBuf },
    /// Daemon `$IMPETUS_DATA_DIR` layout (canonical SoT).
    Daemon { data_root: PathBuf },
}

impl ControlPlane {
    fn layout(&self) -> ExtensionInstallLayout {
        match self {
            Self::Workspace { .. } => ExtensionInstallLayout::Workspace,
            Self::Daemon { .. } => ExtensionInstallLayout::Daemon,
        }
    }

    fn target_root(&self) -> &Path {
        match self {
            Self::Workspace { root } => root,
            Self::Daemon { data_root } => data_root,
        }
    }

    fn open_stores(&self) -> Result<(OwnershipStore, ExtensionStateStore)> {
        match self {
            Self::Workspace { root } => {
                let base = root.join(".impetus");
                let ownership = OwnershipStore::open(base.join("ownership.db"))
                    .context("open ownership store")?;
                let state = ExtensionStateStore::open(base.join("install_state.db"))
                    .context("open install state store")?;
                Ok((ownership, state))
            }
            Self::Daemon { data_root } => {
                let ownership =
                    OwnershipStore::open(data_root.join("extensions").join("ownership.db"))
                        .context("open daemon ownership store")?;
                let state = ExtensionStateStore::open(daemon_extension_state_db(data_root))
                    .context("open daemon install state store")?;
                Ok((ownership, state))
            }
        }
    }
}

/// Resolve control plane: `--root` → workspace; else `--data-dir` /
/// `$IMPETUS_DATA_DIR` → daemon; else cwd workspace.
fn resolve_control_plane(root: Option<&Path>, data_dir: Option<&Path>) -> Result<ControlPlane> {
    if let Some(path) = root {
        let root = path
            .canonicalize()
            .with_context(|| format!("canonicalize target root {}", path.display()))?;
        return Ok(ControlPlane::Workspace { root });
    }
    if let Some(path) = data_dir {
        std::fs::create_dir_all(path)
            .with_context(|| format!("create data dir {}", path.display()))?;
        let data_root = path
            .canonicalize()
            .with_context(|| format!("canonicalize data dir {}", path.display()))?;
        return Ok(ControlPlane::Daemon { data_root });
    }
    if let Ok(raw) = std::env::var("IMPETUS_DATA_DIR") {
        let path = PathBuf::from(raw);
        std::fs::create_dir_all(&path)
            .with_context(|| format!("create IMPETUS_DATA_DIR {}", path.display()))?;
        let data_root = path
            .canonicalize()
            .with_context(|| format!("canonicalize IMPETUS_DATA_DIR {}", path.display()))?;
        return Ok(ControlPlane::Daemon { data_root });
    }
    let root = std::env::current_dir()
        .context("current directory")?
        .canonicalize()
        .context("canonicalize current directory")?;
    Ok(ControlPlane::Workspace { root })
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
pub async fn plan(
    kind: ExtensionKind,
    path: &Path,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    json: bool,
) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let intent = intent_for(kind, path);
    let plan = plan_install_with_layout(&intent, plane.target_root(), plane.layout())
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
    data_dir: Option<&Path>,
    json: bool,
) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let intent = intent_for(kind, path);
    let plan = plan_install_with_layout(&intent, plane.target_root(), plane.layout())
        .await
        .with_context(|| format!("plan install from {}", path.display()))?;

    let (ownership, state_store) = plane.open_stores()?;
    let state = apply_install(&plan, &ownership, &state_store).context("apply install")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&state)?);
    } else {
        println!("Installed extension");
        println!("  installation_id: {}", state.installation_id);
        println!("  module_id: {}", state.resolution.module_id);
        println!("  module_name: {}", state.resolution.module_name);
        println!("  version: {}", state.resolution.version);
        println!("  layout: {:?}", plane.layout());
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
pub fn remove(
    installation_id: &str,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    json: bool,
) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let (ownership, state_store) = plane.open_stores()?;
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
pub fn enable(
    installation_id: &str,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    json: bool,
) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let (ownership, state_store) = plane.open_stores()?;
    let result = enable_install(installation_id, &ownership, &state_store)
        .with_context(|| format!("enable install {installation_id}"))?;
    print_lifecycle(&result, "Enabled", json)
}

/// Disable install: sideline files; not loaded on restart.
pub fn disable(
    installation_id: &str,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    json: bool,
) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let (ownership, state_store) = plane.open_stores()?;
    let result = disable_install(installation_id, &ownership, &state_store)
        .with_context(|| format!("disable install {installation_id}"))?;
    print_lifecycle(&result, "Disabled", json)
}

/// Unload install: sideline files + drop from runtime reload set.
pub fn unload(
    installation_id: &str,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    json: bool,
) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let (ownership, state_store) = plane.open_stores()?;
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

/// List install states (+ effective inventory when on daemon SoT).
pub fn list(root: Option<&Path>, data_dir: Option<&Path>, json: bool) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let (_ownership, state_store) = plane.open_stores()?;
    let states = state_store.list_all().context("list install states")?;
    let runtime = ExtensionRuntime::reload_from_store(&state_store).context("reload runtime")?;

    if let ControlPlane::Daemon { data_root } = &plane {
        let mut host = ExtensionHost::with_persist_root(data_root);
        let discovery =
            impetus_core::ExtensionDiscoveryRoots::from_data_and_workspace(data_root, None);
        let _ = host.reload(&discovery);
        let inventory = build_effective_inventory(data_root, &runtime, Some(&host));
        if json {
            println!("{}", serde_json::to_string_pretty(&inventory)?);
            return Ok(());
        }
        let active: Vec<_> = inventory.active_unshadowed().collect();
        println!(
            "Effective inventory ({} row(s); {} unshadowed; daemon SoT {})",
            inventory.entries.len(),
            active.len(),
            data_root.display()
        );
        if inventory.entries.is_empty() {
            println!("  (none)");
            return Ok(());
        }
        for row in &inventory.entries {
            let shadow = if row.shadowed { " shadowed" } else { "" };
            println!(
                "  [{:?}/{:?}] {}  {} v{}{shadow}",
                row.kind, row.origin, row.key, row.name, row.version
            );
            println!("    status: {}", row.status);
            if let Some(path) = &row.path {
                println!("    path: {path}");
            }
        }
        return Ok(());
    }

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

/// Migrate workspace `.impetus/` Enabled installs into daemon SoT.
pub fn migrate(from: Option<&Path>, data_dir: Option<&Path>, json: bool) -> Result<()> {
    let legacy_root = match from {
        Some(path) => path
            .canonicalize()
            .with_context(|| format!("canonicalize --from {}", path.display()))?,
        None => std::env::current_dir()
            .context("current directory")?
            .canonicalize()
            .context("canonicalize current directory")?,
    };
    let data_root = match data_dir {
        Some(path) => {
            std::fs::create_dir_all(path)?;
            path.canonicalize()
                .with_context(|| format!("canonicalize --data-dir {}", path.display()))?
        }
        None => {
            let raw = std::env::var("IMPETUS_DATA_DIR").context(
                "migrate requires --data-dir or IMPETUS_DATA_DIR (daemon-owned inventory SoT)",
            )?;
            let path = PathBuf::from(raw);
            std::fs::create_dir_all(&path)?;
            path.canonicalize()
                .context("canonicalize IMPETUS_DATA_DIR")?
        }
    };

    let marker = migrate_legacy_to_daemon(&legacy_root, &data_root)
        .with_context(|| format!("migrate from {}", legacy_root.display()))?;

    // Restart seam check: daemon reload sees migrated Enabled rows.
    let runtime = load_daemon_extension_runtime(&data_root)
        .context("reload daemon extension runtime after migrate")?;
    let loaded = runtime.loaded_ids().len();

    if json {
        #[derive(Serialize)]
        struct Out<'a> {
            marker: &'a impetus_core::LegacyMigrationMarker,
            loaded_on_restart: usize,
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Out {
                marker: &marker,
                loaded_on_restart: loaded,
            })?
        );
    } else {
        println!("Migrated legacy inventory → daemon SoT");
        println!("  from: {}", marker.from_root);
        println!("  data: {}", data_root.display());
        println!("  skills: {}", marker.migrated_skills.len());
        for id in &marker.migrated_skills {
            println!("    + {id}");
        }
        println!("  mcp: {}", marker.migrated_mcp.len());
        for id in &marker.migrated_mcp {
            println!("    + {id}");
        }
        println!("  skipped_existing: {}", marker.skipped_existing.len());
        for id in &marker.skipped_existing {
            println!("    = {id}");
        }
        println!("  loaded_on_restart: {loaded}");
    }
    Ok(())
}

/// Report install-state + ownership health (read-only).
pub fn doctor(
    installation_id: Option<&str>,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    json: bool,
) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let (ownership, state_store) = plane.open_stores()?;
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
        let status = if install.healthy { "ok" } else { "fail" };
        println!(
            "  [{status}] {} (state_present={})",
            install.installation_id, install.state_present
        );
        for path in &install.paths {
            println!("    {} → {:?}", path.path, path.status);
        }
    }
}

/// Restore owned paths from recorded source (digest mismatch needs force).
pub fn repair(
    installation_id: &str,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    force: bool,
    json: bool,
) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let (ownership, state_store) = plane.open_stores()?;
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
        println!("  skipped_ok: {}", result.skipped_ok.len());
    }
    Ok(())
}
