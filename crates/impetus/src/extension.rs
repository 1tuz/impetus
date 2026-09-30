//! CLI wrappers for extension lifecycle plan + install + remove + enable +
//! disable + unload + list + doctor + repair + migrate.
//!
//! Package path (`kind=package`): when daemon sock is live and negotiates
//! `extension_manage`, install/remove go through
//! `InstallExtensionPackage` / `RemoveExtensionPackage` IPC. Sock down →
//! offline `ExtensionHost` copy under `$IMPETUS_DATA_DIR/extensions/packages/`.
//!
//! Legacy Skill/MCP: offline FS plan/apply under `--root` / `--data-dir`; after
//! mutate, best-effort `ReloadExtensionPackages` when sock live.

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use impetus_client::{HarnessClient, UnixSocketTransport};
use impetus_core::{
    ExtensionDiscoveryRoots, ExtensionHost, ExtensionInstallIntent, ExtensionInstallLayout,
    ExtensionLifecycleStatus, ExtensionPackageSource, ExtensionRuntime, ExtensionStateStore,
    InstallPlan, OwnershipStore, apply_install, build_effective_inventory,
    daemon_extension_state_db, disable_install, doctor_install, enable_install,
    load_daemon_extension_runtime, migrate_legacy_to_daemon, plan_install_with_layout,
    remove_install, repair_install, unload_install,
};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExtensionKind {
    /// Agent Skills SKILL.md (file or skill directory)
    Skill,
    /// MCP server config JSON (parse only; no server spawn)
    Mcp,
    /// Package directory with `extension.toml` / `extension.json` (daemon SoT)
    Package,
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

/// Data root for package install/remove (always daemon packages dir).
fn resolve_package_data_root(data_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = data_dir {
        std::fs::create_dir_all(path)
            .with_context(|| format!("create data dir {}", path.display()))?;
        return path
            .canonicalize()
            .with_context(|| format!("canonicalize data dir {}", path.display()));
    }
    if let Ok(raw) = std::env::var("IMPETUS_DATA_DIR") {
        let path = PathBuf::from(raw);
        std::fs::create_dir_all(&path)
            .with_context(|| format!("create IMPETUS_DATA_DIR {}", path.display()))?;
        return path.canonicalize().context("canonicalize IMPETUS_DATA_DIR");
    }
    let path = crate::daemon::default_data_root();
    std::fs::create_dir_all(&path)
        .with_context(|| format!("create default data root {}", path.display()))?;
    path.canonicalize()
        .context("canonicalize default data root")
}

fn intent_for(kind: ExtensionKind, path: &Path) -> Result<ExtensionInstallIntent> {
    match kind {
        ExtensionKind::Skill => Ok(ExtensionInstallIntent::Skill {
            path: path.to_path_buf(),
        }),
        ExtensionKind::Mcp => Ok(ExtensionInstallIntent::McpConfig {
            path: path.to_path_buf(),
        }),
        ExtensionKind::Package => bail!(
            "package kind uses ExtensionHost / daemon IPC, not legacy InstallPlan (use install package <dir>)"
        ),
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

/// Connect when sock exists and `extension_manage` negotiated; else `None`.
async fn try_extension_manage_client() -> Option<UnixSocketTransport> {
    let socket_path = crate::daemon::discover_socket_path();
    if !Path::new(&socket_path).exists() {
        return None;
    }
    let client = match UnixSocketTransport::connect(&socket_path).await {
        Ok(client) => client,
        Err(_) => return None,
    };
    if !client
        .negotiated_capabilities()
        .iter()
        .any(|cap| cap == "extension_manage")
    {
        return None;
    }
    Some(client)
}

/// Best-effort daemon rediscover after offline Skill/MCP mutate.
async fn reload_packages_best_effort() {
    let Some(client) = try_extension_manage_client().await else {
        return;
    };
    let _ = client.reload_extension_packages().await;
}

/// Dry-run plan: print InstallPlan; do not write.
///
/// Package kind: validate `extension.toml` / `extension.json` only (no FS copy).
pub async fn plan(
    kind: ExtensionKind,
    path: &Path,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    json: bool,
) -> Result<()> {
    if kind == ExtensionKind::Package {
        return plan_package(path, json);
    }
    let plane = resolve_control_plane(root, data_dir)?;
    let intent = intent_for(kind, path)?;
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

fn plan_package(path: &Path, json: bool) -> Result<()> {
    let loaded = ExtensionHost::load_package(path, ExtensionPackageSource::Dev)
        .with_context(|| format!("validate package at {}", path.display()))?;
    let caps: Vec<String> = loaded
        .manifest
        .capabilities
        .iter()
        .map(|c| c.as_str().to_string())
        .collect();
    if json {
        #[derive(Serialize)]
        struct Out<'a> {
            id: &'a str,
            name: &'a str,
            version: &'a str,
            extension_api_version: u32,
            capabilities: &'a [String],
            source_path: String,
            dry_run: bool,
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Out {
                id: loaded.id.as_str(),
                name: &loaded.manifest.name,
                version: &loaded.manifest.version,
                extension_api_version: loaded.manifest.extension_api_version,
                capabilities: &caps,
                source_path: path.display().to_string(),
                dry_run: true,
            })?
        );
    } else {
        println!("Package plan (dry-run; no copy)");
        println!("  id: {}", loaded.id.as_str());
        println!("  name: {}", loaded.manifest.name);
        println!("  version: {}", loaded.manifest.version);
        println!(
            "  extension_api_version: {}",
            loaded.manifest.extension_api_version
        );
        println!("  capabilities: {caps:?}");
        println!("  source_path: {}", path.display());
        println!(
            "  dest: $IMPETUS_DATA_DIR/extensions/packages/{}/",
            loaded.id.as_str()
        );
    }
    Ok(())
}

/// Plan then apply: write files, ownership, install state.
///
/// Package: daemon IPC when sock live; else offline ExtensionHost install.
pub async fn install(
    kind: ExtensionKind,
    path: &Path,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    replace: bool,
    json: bool,
) -> Result<()> {
    if kind == ExtensionKind::Package {
        return install_package(path, data_dir, replace, json).await;
    }
    let plane = resolve_control_plane(root, data_dir)?;
    let intent = intent_for(kind, path)?;
    let plan = plan_install_with_layout(&intent, plane.target_root(), plane.layout())
        .await
        .with_context(|| format!("plan install from {}", path.display()))?;

    let (ownership, state_store) = plane.open_stores()?;
    let state = apply_install(&plan, &ownership, &state_store).context("apply install")?;
    reload_packages_best_effort().await;

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

async fn install_package(
    path: &Path,
    data_dir: Option<&Path>,
    replace: bool,
    json: bool,
) -> Result<()> {
    let source = path
        .canonicalize()
        .with_context(|| format!("canonicalize package source {}", path.display()))?;
    if !source.is_dir() {
        bail!("package source must be a directory: {}", source.display());
    }

    if let Some(client) = try_extension_manage_client().await {
        let package = client
            .install_extension_package(source.clone(), replace)
            .await
            .context("daemon InstallExtensionPackage")?;
        return print_package_installed(&package, "daemon_ipc", json);
    }

    let data_root = resolve_package_data_root(data_dir)?;
    let roots = ExtensionDiscoveryRoots::from_data_and_workspace(&data_root, None);
    let mut host = ExtensionHost::with_persist_root(&data_root);
    let id = host
        .install_package(&source, replace, &roots)
        .with_context(|| format!("offline install package from {}", source.display()))?;
    let loaded = host
        .get(&id)
        .with_context(|| format!("package `{id}` missing after offline install"))?;
    #[derive(Serialize)]
    struct OfflinePkg<'a> {
        id: &'a str,
        name: &'a str,
        version: &'a str,
        phase: String,
        source: &'a str,
        admission: &'a str,
    }
    let row = OfflinePkg {
        id: loaded.id.as_str(),
        name: &loaded.manifest.name,
        version: &loaded.manifest.version,
        phase: format!("{:?}", loaded.phase).to_lowercase(),
        source: "global",
        admission: "offline_host",
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&row)?);
    } else {
        println!("Installed extension package (offline ExtensionHost)");
        println!("  id: {}", row.id);
        println!("  name: {}", row.name);
        println!("  version: {}", row.version);
        println!("  phase: {}", row.phase);
        println!("  data: {}", data_root.display());
    }
    Ok(())
}

fn print_package_installed(
    package: &impetus_client::protocol::ExtensionPackageInfo,
    admission: &str,
    json: bool,
) -> Result<()> {
    if json {
        #[derive(Serialize)]
        struct Out<'a> {
            package: &'a impetus_client::protocol::ExtensionPackageInfo,
            admission: &'a str,
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Out { package, admission })?
        );
    } else {
        println!("Installed extension package ({admission})");
        println!("  id: {}", package.id);
        println!("  name: {}", package.name);
        println!("  version: {}", package.version);
        println!("  phase: {}", package.phase);
        println!("  source: {}", package.source);
        println!("  capabilities: {:?}", package.capabilities);
    }
    Ok(())
}

/// Remove install by `installation_id`, or package id when `package=true`.
pub async fn remove(
    installation_id: &str,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    package: bool,
    json: bool,
) -> Result<()> {
    if package {
        return remove_package(installation_id, data_dir, json).await;
    }
    let plane = resolve_control_plane(root, data_dir)?;
    let (ownership, state_store) = plane.open_stores()?;
    let result = remove_install(installation_id, &ownership, &state_store)
        .with_context(|| format!("remove install {installation_id}"))?;
    reload_packages_best_effort().await;

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

async fn remove_package(id: &str, data_dir: Option<&Path>, json: bool) -> Result<()> {
    if let Some(client) = try_extension_manage_client().await {
        let (loaded, failed) = client
            .remove_extension_package(id)
            .await
            .context("daemon RemoveExtensionPackage")?;
        if json {
            #[derive(Serialize)]
            struct Out {
                id: String,
                loaded: u32,
                failed: u32,
                admission: &'static str,
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&Out {
                    id: id.to_string(),
                    loaded,
                    failed,
                    admission: "daemon_ipc",
                })?
            );
        } else {
            println!("Removed extension package (daemon_ipc)");
            println!("  id: {id}");
            println!("  reload loaded: {loaded}");
            println!("  reload failed: {failed}");
        }
        return Ok(());
    }

    let data_root = resolve_package_data_root(data_dir)?;
    let roots = ExtensionDiscoveryRoots::from_data_and_workspace(&data_root, None);
    let mut host = ExtensionHost::with_persist_root(&data_root);
    let _ = host.reload(&roots);
    let results = host
        .remove_package(id, &roots)
        .with_context(|| format!("offline remove package `{id}`"))?;
    let loaded = results.iter().filter(|(_, r)| r.is_ok()).count() as u32;
    let failed = results.iter().filter(|(_, r)| r.is_err()).count() as u32;
    if json {
        #[derive(Serialize)]
        struct Out {
            id: String,
            loaded: u32,
            failed: u32,
            admission: &'static str,
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Out {
                id: id.to_string(),
                loaded,
                failed,
                admission: "offline_host",
            })?
        );
    } else {
        println!("Removed extension package (offline ExtensionHost)");
        println!("  id: {id}");
        println!("  reload loaded: {loaded}");
        println!("  reload failed: {failed}");
        println!("  data: {}", data_root.display());
    }
    Ok(())
}

/// Enable a disabled or unloaded install (restore sidelined files).
pub async fn enable(
    installation_id: &str,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    json: bool,
) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let (ownership, state_store) = plane.open_stores()?;
    let result = enable_install(installation_id, &ownership, &state_store)
        .with_context(|| format!("enable install {installation_id}"))?;
    reload_packages_best_effort().await;
    print_lifecycle(&result, "Enabled", json)
}

/// Disable install: sideline files; not loaded on restart.
pub async fn disable(
    installation_id: &str,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    json: bool,
) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let (ownership, state_store) = plane.open_stores()?;
    let result = disable_install(installation_id, &ownership, &state_store)
        .with_context(|| format!("disable install {installation_id}"))?;
    reload_packages_best_effort().await;
    print_lifecycle(&result, "Disabled", json)
}

/// Unload install: sideline files + drop from runtime reload set.
pub async fn unload(
    installation_id: &str,
    root: Option<&Path>,
    data_dir: Option<&Path>,
    json: bool,
) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let (ownership, state_store) = plane.open_stores()?;
    let result = unload_install(installation_id, &ownership, &state_store)
        .with_context(|| format!("unload install {installation_id}"))?;
    reload_packages_best_effort().await;
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
///
/// When sock live + `extension_manage`, also print daemon package inventory
/// (cheap ListExtensionPackages).
pub async fn list(root: Option<&Path>, data_dir: Option<&Path>, json: bool) -> Result<()> {
    let plane = resolve_control_plane(root, data_dir)?;
    let (_ownership, state_store) = plane.open_stores()?;
    let states = state_store.list_all().context("list install states")?;
    let runtime = ExtensionRuntime::reload_from_store(&state_store).context("reload runtime")?;

    let ipc_packages = if let Some(client) = try_extension_manage_client().await {
        client.list_extension_packages().await.ok()
    } else {
        None
    };

    if let ControlPlane::Daemon { data_root } = &plane {
        let mut host = ExtensionHost::with_persist_root(data_root);
        let discovery =
            impetus_core::ExtensionDiscoveryRoots::from_data_and_workspace(data_root, None);
        let _ = host.reload(&discovery);
        let inventory = build_effective_inventory(data_root, &runtime, Some(&host));
        if json {
            #[derive(Serialize)]
            struct Out<'a> {
                inventory: &'a impetus_core::EffectiveInventory,
                #[serde(skip_serializing_if = "Option::is_none")]
                daemon_packages: Option<&'a Vec<impetus_client::protocol::ExtensionPackageInfo>>,
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&Out {
                    inventory: &inventory,
                    daemon_packages: ipc_packages.as_ref(),
                })?
            );
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
        } else {
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
        }
        if let Some(packages) = &ipc_packages {
            println!("Daemon packages (IPC; {}):", packages.len());
            for p in packages {
                println!(
                    "  [{}] {}  {} v{} ({})",
                    p.phase, p.id, p.name, p.version, p.source
                );
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
        #[derive(Serialize)]
        struct Out<'a> {
            installs: &'a [Row<'a>],
            #[serde(skip_serializing_if = "Option::is_none")]
            daemon_packages: Option<&'a Vec<impetus_client::protocol::ExtensionPackageInfo>>,
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Out {
                installs: &rows,
                daemon_packages: ipc_packages.as_ref(),
            })?
        );
    } else {
        println!(
            "Extensions ({} install(s); {} loaded on restart)",
            rows.len(),
            runtime.loaded_ids().len()
        );
        if rows.is_empty() {
            println!("  (none)");
        } else {
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
        if let Some(packages) = &ipc_packages {
            println!("Daemon packages (IPC; {}):", packages.len());
            for p in packages {
                println!(
                    "  [{}] {}  {} v{} ({})",
                    p.phase, p.id, p.name, p.version, p.source
                );
            }
        }
    }
    Ok(())
}

/// Migrate workspace `.impetus/` Enabled installs into daemon SoT.
pub async fn migrate(from: Option<&Path>, data_dir: Option<&Path>, json: bool) -> Result<()> {
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
    reload_packages_best_effort().await;

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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;
    use std::fs;

    #[test]
    fn package_kind_is_clap_value() {
        assert_eq!(
            ExtensionKind::from_str("package", true).unwrap(),
            ExtensionKind::Package
        );
    }

    #[tokio::test]
    async fn plan_package_validates_extension_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("demo-pack");
        fs::create_dir_all(pack.join("skills")).unwrap();
        fs::write(
            pack.join("extension.toml"),
            r#"
schema_version = 1
id = "demo-pack"
name = "Demo"
version = "0.1.0"
description = "test"
author = "impetus"
extension_api_version = 1
capabilities = ["skill_provider"]
permissions = ["filesystem_read"]

[entrypoint]
kind = "instruction_pack"
root = "skills"
"#,
        )
        .unwrap();
        fs::write(
            pack.join("skills").join("SKILL.md"),
            "---\nid: demo\nscope: global\n---\n# demo\n",
        )
        .unwrap();

        plan_package(&pack, true).expect("plan package");
    }

    #[tokio::test]
    async fn offline_install_then_remove_package() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let source = tmp.path().join("src");
        fs::create_dir_all(source.join("skills")).unwrap();
        fs::write(
            source.join("extension.toml"),
            r#"
schema_version = 1
id = "cli-demo-pack"
name = "CLI Demo"
version = "0.1.0"
description = "test"
author = "impetus"
extension_api_version = 1
capabilities = ["skill_provider"]
permissions = ["filesystem_read"]

[entrypoint]
kind = "instruction_pack"
root = "skills"
"#,
        )
        .unwrap();
        fs::write(
            source.join("skills").join("SKILL.md"),
            "---\nid: cli-demo\nscope: global\n---\n# cli\n",
        )
        .unwrap();

        // Force offline path: no sock (or sock without our data).
        install_package(&source, Some(&data), false, true)
            .await
            .expect("offline install");
        assert!(
            data.join("extensions")
                .join("packages")
                .join("cli-demo-pack")
                .is_dir()
        );

        remove_package("cli-demo-pack", Some(&data), true)
            .await
            .expect("offline remove");
        assert!(
            !data
                .join("extensions")
                .join("packages")
                .join("cli-demo-pack")
                .exists()
        );
    }
}
