//! Daemon-owned effective extension inventory (legacy Skill/MCP + package host).
//!
//! One SoT under `$IMPETUS_DATA_DIR`: CLI is presentation/control; AgentLoop
//! consumes deduped skill roots / MCP modules. Distinct from host_process RPC.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::daemon_wiring::{daemon_extension_state_db, daemon_mcp_dir};
use crate::extension_compat::ExtensionSource;
use crate::extension_host::{ExtensionHost, ExtensionHostPhase};
use crate::extension_id::{
    ExtensionIdError, normalize_extension_id, skill_install_path as workspace_skill_path,
};
use crate::extension_lifecycle::{
    ExtensionLifecycleStatus, ExtensionRuntime, ExtensionState, ExtensionStateStore,
};
use crate::ownership::{OwnershipRecord, OwnershipStore, content_digest, path_key};

/// Where a capability row came from in the unified inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryOrigin {
    /// Active ExtensionHost package (wins over legacy on same capability key).
    PackageHost,
    /// Daemon MCP SoT file (`$IMPETUS_DATA_DIR/mcp/*.json`).
    McpSot,
    /// Legacy CLI install reloaded from daemon `install_state.db`.
    LegacyCli,
}

/// Capability class in the effective inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryKind {
    Skill,
    Mcp,
    Package,
}

/// One row in the daemon-owned effective inventory (labels only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveInventoryEntry {
    /// Stable capability key: skill/mcp module_id or package id.
    pub key: String,
    pub kind: InventoryKind,
    pub origin: InventoryOrigin,
    pub status: String,
    pub version: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// True when another higher-priority origin owns the same key.
    pub shadowed: bool,
}

/// Marker written after a successful legacy → daemon migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyMigrationMarker {
    pub version: u32,
    pub from_root: String,
    pub migrated_skills: Vec<String>,
    pub migrated_mcp: Vec<String>,
    pub skipped_existing: Vec<String>,
}

#[derive(Debug, Error)]
pub enum InventoryError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("install state: {0}")]
    State(String),
    #[error("ownership: {0}")]
    Ownership(String),
    #[error("path key: {0}")]
    PathKey(String),
    #[error(transparent)]
    Id(#[from] ExtensionIdError),
    #[error("legacy install state missing at {}", .0.display())]
    LegacyStateMissing(PathBuf),
    #[error("serialize: {0}")]
    Serialize(String),
}

const OWNER_IMPETUS: &str = "impetus";
const MIGRATION_MARKER_REL: &str = "extensions/legacy_migration.json";
const LEGACY_SKILLS_REL: &str = "extensions/legacy_skills";

/// Daemon layout: `{data_root}/extensions/legacy_skills/{id}/SKILL.md`.
pub fn daemon_legacy_skill_path(
    data_root: &Path,
    skill_id: &str,
) -> Result<PathBuf, ExtensionIdError> {
    let id = normalize_extension_id(skill_id)?;
    let type_root = data_root.join(LEGACY_SKILLS_REL);
    crate::join_under_extension_root(&type_root, Path::new(&id).join("SKILL.md"))
}

/// Parent dirs of Enabled legacy skills under the daemon data root.
pub fn daemon_legacy_skill_roots(data_root: &Path) -> Vec<PathBuf> {
    let root = data_root.join(LEGACY_SKILLS_REL);
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("SKILL.md").is_file() {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn migration_marker_path(data_root: &Path) -> PathBuf {
    data_root.join(MIGRATION_MARKER_REL)
}

fn daemon_ownership_db(data_root: &Path) -> PathBuf {
    data_root.join("extensions").join("ownership.db")
}

fn origin_rank(origin: InventoryOrigin) -> u8 {
    match origin {
        InventoryOrigin::PackageHost => 3,
        InventoryOrigin::McpSot => 2,
        InventoryOrigin::LegacyCli => 1,
    }
}

/// Build one effective inventory: package host > MCP SoT > legacy CLI; shadow losers.
pub fn build_effective_inventory(
    data_root: &Path,
    runtime: &ExtensionRuntime,
    host: Option<&ExtensionHost>,
) -> EffectiveInventory {
    let mut rows: Vec<EffectiveInventoryEntry> = Vec::new();

    if let Some(host) = host {
        for ext in host.list() {
            let status = match ext.phase {
                ExtensionHostPhase::Active => "active",
                ExtensionHostPhase::Disabled => "disabled",
                ExtensionHostPhase::Failed => "failed",
                ExtensionHostPhase::Loaded => "loaded",
                ExtensionHostPhase::Compatible => "compatible",
                ExtensionHostPhase::Validated => "validated",
                ExtensionHostPhase::Discovered => "discovered",
            };
            rows.push(EffectiveInventoryEntry {
                key: ext.id.as_str().to_string(),
                kind: InventoryKind::Package,
                origin: InventoryOrigin::PackageHost,
                status: status.to_string(),
                version: ext.manifest.version.clone(),
                name: ext.manifest.name.clone(),
                path: Some(ext.path.display().to_string()),
                shadowed: false,
            });
            // Active instruction_pack also claims skill capability keys under the pack.
            if ext.phase == ExtensionHostPhase::Active
                && let impetus_extension_sdk::ExtensionEntrypoint::InstructionPack { root } =
                    &ext.manifest.entrypoint
            {
                let skill_root = ext.path.join(root);
                for skill_id in skill_ids_under(&skill_root) {
                    rows.push(EffectiveInventoryEntry {
                        key: skill_id,
                        kind: InventoryKind::Skill,
                        origin: InventoryOrigin::PackageHost,
                        status: "active".into(),
                        version: ext.manifest.version.clone(),
                        name: ext.manifest.name.clone(),
                        path: Some(skill_root.display().to_string()),
                        shadowed: false,
                    });
                }
            }
            if ext.phase == ExtensionHostPhase::Active
                && let impetus_extension_sdk::ExtensionEntrypoint::McpBridge { module_id } =
                    &ext.manifest.entrypoint
            {
                rows.push(EffectiveInventoryEntry {
                    key: module_id.clone(),
                    kind: InventoryKind::Mcp,
                    origin: InventoryOrigin::PackageHost,
                    status: "active".into(),
                    version: ext.manifest.version.clone(),
                    name: ext.manifest.name.clone(),
                    path: Some(
                        daemon_mcp_dir(data_root)
                            .join(format!("{module_id}.json"))
                            .display()
                            .to_string(),
                    ),
                    shadowed: false,
                });
            }
        }
    }

    let mcp_dir = daemon_mcp_dir(data_root);
    if let Ok(entries) = fs::read_dir(&mcp_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                rows.push(EffectiveInventoryEntry {
                    key: stem.to_string(),
                    kind: InventoryKind::Mcp,
                    origin: InventoryOrigin::McpSot,
                    status: "enabled".into(),
                    version: "1.0.0".into(),
                    name: stem.to_string(),
                    path: Some(path.display().to_string()),
                    shadowed: false,
                });
            }
        }
    }

    for state in runtime.loaded_states() {
        let kind = match state.resolution.source {
            ExtensionSource::AgentSkills => InventoryKind::Skill,
            ExtensionSource::Mcp => InventoryKind::Mcp,
            _ => continue,
        };
        let path = state
            .created_paths
            .first()
            .or(state.modified_paths.first())
            .map(|p| p.display().to_string());
        rows.push(EffectiveInventoryEntry {
            key: state.resolution.module_id.clone(),
            kind,
            origin: InventoryOrigin::LegacyCli,
            status: "enabled".into(),
            version: state.resolution.version.clone(),
            name: state.resolution.module_name.clone(),
            path,
            shadowed: false,
        });
    }

    dedup_shadow(&mut rows);
    rows.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| origin_rank(b.origin).cmp(&origin_rank(a.origin)))
    });
    EffectiveInventory { entries: rows }
}

fn skill_ids_under(root: &Path) -> Vec<String> {
    let mut ids = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "SKILL.md") {
                let id = skill_id_from_file(&path).unwrap_or_else(|| {
                    path.parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("skill")
                        .to_string()
                });
                if let Ok(norm) = normalize_extension_id(&id) {
                    ids.push(norm);
                }
            }
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Prefer front-matter `id:` / `name:` so pack-root `skills/SKILL.md` keys correctly.
fn skill_id_from_file(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let rest = text.strip_prefix("---\n")?;
    let (header, _) = rest.split_once("\n---")?;
    for line in header.lines() {
        let line = line.trim();
        if let Some(v) = line
            .strip_prefix("id:")
            .or_else(|| line.strip_prefix("name:"))
        {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn dedup_shadow(rows: &mut [EffectiveInventoryEntry]) {
    let mut best: BTreeMap<(InventoryKind, String), InventoryOrigin> = BTreeMap::new();
    for row in rows.iter() {
        let key = (row.kind, row.key.clone());
        match best.get(&key) {
            Some(existing) if origin_rank(*existing) >= origin_rank(row.origin) => {}
            _ => {
                best.insert(key, row.origin);
            }
        }
    }
    for row in rows.iter_mut() {
        if let Some(winner) = best.get(&(row.kind, row.key.clone()))
            && *winner != row.origin
        {
            row.shadowed = true;
        }
    }
}

/// Snapshot returned by [`build_effective_inventory`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EffectiveInventory {
    pub entries: Vec<EffectiveInventoryEntry>,
}

impl EffectiveInventory {
    pub fn active_unshadowed(&self) -> impl Iterator<Item = &EffectiveInventoryEntry> {
        self.entries.iter().filter(|e| !e.shadowed)
    }

    /// Skill keys claimed by a higher-priority origin (exclude from legacy roots).
    pub fn claimed_skill_keys(&self) -> BTreeSet<String> {
        self.entries
            .iter()
            .filter(|e| e.kind == InventoryKind::Skill && !e.shadowed)
            .map(|e| e.key.clone())
            .collect()
    }
}

/// Skill roots for AgentLoop: Active package roots + unshadowed legacy dirs.
pub fn effective_skill_roots(data_root: &Path, host: Option<&ExtensionHost>) -> Vec<PathBuf> {
    let runtime = load_runtime_quiet(data_root);
    let inventory = build_effective_inventory(data_root, &runtime, host);
    let claimed = inventory.claimed_skill_keys();

    let mut roots = Vec::new();
    if let Some(host) = host {
        roots.extend(host.capability_registry().skill_roots);
    }
    for legacy in daemon_legacy_skill_roots(data_root) {
        let Some(name) = legacy.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(id) = normalize_extension_id(name) else {
            continue;
        };
        // Skip when package host already owns this skill key.
        if claimed.contains(&id)
            && inventory.entries.iter().any(|e| {
                e.kind == InventoryKind::Skill
                    && e.key == id
                    && e.origin == InventoryOrigin::PackageHost
                    && !e.shadowed
            })
        {
            continue;
        }
        roots.push(legacy);
    }
    roots.sort();
    roots.dedup();
    roots
}

fn load_runtime_quiet(data_root: &Path) -> ExtensionRuntime {
    let db = daemon_extension_state_db(data_root);
    if !db.is_file() {
        return ExtensionRuntime::empty();
    }
    let Ok(store) = ExtensionStateStore::open(&db) else {
        return ExtensionRuntime::empty();
    };
    ExtensionRuntime::reload_from_store(&store).unwrap_or_else(|_| ExtensionRuntime::empty())
}

/// Deterministic migration of workspace `.impetus/` installs into daemon SoT.
///
/// Idempotent: existing daemon module_ids / MCP files are skipped (not overwritten).
pub fn migrate_legacy_to_daemon(
    legacy_root: &Path,
    data_root: &Path,
) -> Result<LegacyMigrationMarker, InventoryError> {
    let legacy_state_path = legacy_root.join(".impetus").join("install_state.db");
    if !legacy_state_path.is_file() {
        return Err(InventoryError::LegacyStateMissing(legacy_state_path));
    }

    fs::create_dir_all(data_root.join("extensions"))?;
    fs::create_dir_all(data_root.join(LEGACY_SKILLS_REL))?;
    fs::create_dir_all(daemon_mcp_dir(data_root))?;

    let legacy_store = ExtensionStateStore::open(&legacy_state_path)
        .map_err(|e| InventoryError::State(e.to_string()))?;
    let daemon_store = ExtensionStateStore::open(daemon_extension_state_db(data_root))
        .map_err(|e| InventoryError::State(e.to_string()))?;
    let ownership = OwnershipStore::open(daemon_ownership_db(data_root))
        .map_err(|e| InventoryError::Ownership(e.to_string()))?;

    let existing_keys: BTreeSet<String> = daemon_store
        .list_all()
        .map_err(|e| InventoryError::State(e.to_string()))?
        .into_iter()
        .map(|s| s.resolution.module_id)
        .collect();

    let mut migrated_skills = Vec::new();
    let mut migrated_mcp = Vec::new();
    let mut skipped_existing = Vec::new();

    for state in legacy_store
        .list_all()
        .map_err(|e| InventoryError::State(e.to_string()))?
    {
        if state.status != ExtensionLifecycleStatus::Enabled {
            continue;
        }
        let module_id = state.resolution.module_id.clone();
        if existing_keys.contains(&module_id) {
            skipped_existing.push(module_id);
            continue;
        }

        match state.resolution.source {
            ExtensionSource::AgentSkills => {
                migrate_one_skill(legacy_root, data_root, &state, &daemon_store, &ownership)?;
                migrated_skills.push(module_id);
            }
            ExtensionSource::Mcp => {
                let dest = daemon_mcp_dir(data_root).join(format!("{module_id}.json"));
                if dest.is_file() {
                    skipped_existing.push(module_id);
                    continue;
                }
                migrate_one_mcp(legacy_root, data_root, &state, &daemon_store, &ownership)?;
                migrated_mcp.push(module_id);
            }
            _ => {}
        }
    }

    migrated_skills.sort();
    migrated_mcp.sort();
    skipped_existing.sort();

    let marker = LegacyMigrationMarker {
        version: 1,
        from_root: legacy_root.display().to_string(),
        migrated_skills,
        migrated_mcp,
        skipped_existing,
    };
    let bytes =
        serde_json::to_vec_pretty(&marker).map_err(|e| InventoryError::Serialize(e.to_string()))?;
    fs::write(migration_marker_path(data_root), bytes)?;
    Ok(marker)
}

fn migrate_one_skill(
    legacy_root: &Path,
    data_root: &Path,
    legacy: &ExtensionState,
    daemon_store: &ExtensionStateStore,
    ownership: &OwnershipStore,
) -> Result<(), InventoryError> {
    let module_id = &legacy.resolution.module_id;
    let src = workspace_skill_path(legacy_root, module_id)?;
    // Prefer recorded path when present and readable.
    let src = legacy
        .created_paths
        .iter()
        .chain(legacy.modified_paths.iter())
        .find(|p| p.is_file())
        .cloned()
        .filter(|p| p.ends_with("SKILL.md"))
        .unwrap_or(src);
    if !src.is_file() {
        return Err(InventoryError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("legacy skill missing: {}", src.display()),
        )));
    }
    let dest = daemon_legacy_skill_path(data_root, module_id)?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = fs::read(&src)?;
    // Register-before-write (OwnershipStore refuses unowned on-disk paths).
    persist_migrated_state(legacy, dest.clone(), daemon_store, ownership, &bytes)?;
    fs::write(&dest, &bytes)?;
    Ok(())
}

fn migrate_one_mcp(
    legacy_root: &Path,
    data_root: &Path,
    legacy: &ExtensionState,
    daemon_store: &ExtensionStateStore,
    ownership: &OwnershipStore,
) -> Result<(), InventoryError> {
    let module_id = &legacy.resolution.module_id;
    let dest = daemon_mcp_dir(data_root).join(format!("{module_id}.json"));
    if dest.is_file() {
        // MCP SoT already has this id — do not overwrite; still skip install_state dup via caller.
        return Ok(());
    }
    let src = legacy
        .created_paths
        .iter()
        .chain(legacy.modified_paths.iter())
        .find(|p| p.is_file())
        .cloned()
        .unwrap_or_else(|| {
            legacy_root
                .join(".impetus")
                .join("mcp")
                .join(format!("{module_id}.json"))
        });
    if !src.is_file() {
        return Err(InventoryError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("legacy mcp missing: {}", src.display()),
        )));
    }
    let bytes = fs::read(&src)?;
    persist_migrated_state(legacy, dest.clone(), daemon_store, ownership, &bytes)?;
    fs::write(&dest, &bytes)?;
    Ok(())
}

fn persist_migrated_state(
    legacy: &ExtensionState,
    dest: PathBuf,
    daemon_store: &ExtensionStateStore,
    ownership: &OwnershipStore,
    bytes: &[u8],
) -> Result<(), InventoryError> {
    let digest = content_digest(bytes);
    let key = path_key(&dest).map_err(|e| InventoryError::PathKey(e.to_string()))?;
    let installation_id = uuid::Uuid::new_v4().to_string();
    let record = OwnershipRecord {
        path: key,
        owner: OWNER_IMPETUS.into(),
        source: format!("migrate:{}", legacy.installation_id),
        digest,
        version: legacy.resolution.version.clone(),
        installation_id: installation_id.clone(),
    };
    ownership
        .create(&record)
        .map_err(|e| InventoryError::Ownership(e.to_string()))?;

    let state = ExtensionState {
        installation_id,
        resolution: legacy.resolution.clone(),
        created_paths: vec![dest],
        modified_paths: Vec::new(),
        ownership: vec![record],
        status: ExtensionLifecycleStatus::Enabled,
    };
    daemon_store
        .put(&state)
        .map_err(|e| InventoryError::State(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_lifecycle::{ExtensionInstallIntent, apply_install, plan_install};
    use std::io::Write;

    fn write_skill(dir: &Path, name: &str, body: &str) -> PathBuf {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        let skill_md = skill_dir.join("SKILL.md");
        let mut f = fs::File::create(&skill_md).unwrap();
        write!(
            f,
            "---\nname: {name}\ndescription: test\nversion: \"0.1.0\"\n---\n\n{body}\n"
        )
        .unwrap();
        skill_md
    }

    fn write_mcp(dir: &Path, id: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{id}.json"));
        let json = serde_json::json!({
            "name": id,
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
        fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
        path
    }

    fn open_legacy_stores(root: &Path) -> (OwnershipStore, ExtensionStateStore) {
        let base = root.join(".impetus");
        (
            OwnershipStore::open(base.join("ownership.db")).unwrap(),
            ExtensionStateStore::open(base.join("install_state.db")).unwrap(),
        )
    }

    #[tokio::test]
    async fn migrate_legacy_skill_and_mcp_then_restart_reload() {
        let legacy = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let skill_md = write_skill(src.path(), "demo-skill", "do it");
        let mcp_src = write_mcp(src.path(), "filesystem");

        let (ownership, state_store) = open_legacy_stores(legacy.path());
        let skill_plan = plan_install(
            &ExtensionInstallIntent::Skill {
                path: skill_md.parent().unwrap().to_path_buf(),
            },
            legacy.path(),
        )
        .await
        .unwrap();
        apply_install(&skill_plan, &ownership, &state_store).unwrap();
        let mcp_plan = plan_install(
            &ExtensionInstallIntent::McpConfig {
                path: mcp_src.clone(),
            },
            legacy.path(),
        )
        .await
        .unwrap();
        apply_install(&mcp_plan, &ownership, &state_store).unwrap();

        let marker = migrate_legacy_to_daemon(legacy.path(), data.path()).unwrap();
        assert_eq!(marker.migrated_skills, vec!["demo-skill".to_string()]);
        assert_eq!(marker.migrated_mcp, vec!["filesystem".to_string()]);
        assert!(
            daemon_legacy_skill_path(data.path(), "demo-skill")
                .unwrap()
                .is_file()
        );
        assert!(
            daemon_mcp_dir(data.path())
                .join("filesystem.json")
                .is_file()
        );

        // Restart seam: reload ExtensionRuntime from daemon store.
        let runtime = ExtensionRuntime::reload_from_store(
            &ExtensionStateStore::open(daemon_extension_state_db(data.path())).unwrap(),
        )
        .unwrap();
        assert_eq!(runtime.loaded_ids().len(), 2);
        let inventory = build_effective_inventory(data.path(), &runtime, None);
        let keys: BTreeSet<_> = inventory
            .active_unshadowed()
            .map(|e| (e.kind, e.key.clone()))
            .collect();
        assert!(keys.contains(&(InventoryKind::Skill, "demo-skill".into())));
        assert!(keys.contains(&(InventoryKind::Mcp, "filesystem".into())));

        // Idempotent second pass skips existing.
        let again = migrate_legacy_to_daemon(legacy.path(), data.path()).unwrap();
        assert!(again.migrated_skills.is_empty());
        assert!(again.migrated_mcp.is_empty());
        assert!(again.skipped_existing.contains(&"demo-skill".into()));
        assert!(again.skipped_existing.contains(&"filesystem".into()));
    }

    #[test]
    fn package_host_shadows_legacy_skill_key() {
        let data = tempfile::tempdir().unwrap();
        let pack = data.path().join("extensions/packages/demo-pack");
        fs::create_dir_all(pack.join("skills")).unwrap();
        fs::write(
            pack.join("extension.toml"),
            r#"
schema_version = 1
id = "demo-pack"
name = "Demo"
version = "1.0.0"
description = "fixture"
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
            pack.join("skills/SKILL.md"),
            "---\nid: demo-skill\nscope: global\n---\npack\n",
        )
        .unwrap();

        // Legacy skill same id under daemon legacy_skills.
        let legacy_skill = daemon_legacy_skill_path(data.path(), "demo-skill").unwrap();
        fs::create_dir_all(legacy_skill.parent().unwrap()).unwrap();
        fs::write(
            &legacy_skill,
            "---\nname: demo-skill\nversion: \"0.1.0\"\n---\nlegacy\n",
        )
        .unwrap();

        let mut host = ExtensionHost::with_persist_root(data.path());
        host.set_scope(crate::SandboxScope::local_workspace(data.path()).with_network(false));
        let discovery = crate::ExtensionDiscoveryRoots::from_data_and_workspace(data.path(), None);
        let _ = host.reload(&discovery);
        assert_eq!(
            host.get("demo-pack").map(|e| e.phase),
            Some(ExtensionHostPhase::Active)
        );

        // Plant matching legacy runtime row.
        let store = ExtensionStateStore::open(daemon_extension_state_db(data.path())).unwrap();
        let state = ExtensionState {
            installation_id: "legacy-1".into(),
            resolution: crate::extension_lifecycle::ResolutionPlan {
                source: ExtensionSource::AgentSkills,
                module_id: "demo-skill".into(),
                module_name: "demo-skill".into(),
                version: "0.1.0".into(),
                source_path: legacy_skill.clone(),
            },
            created_paths: vec![legacy_skill],
            modified_paths: vec![],
            ownership: vec![],
            status: ExtensionLifecycleStatus::Enabled,
        };
        store.put(&state).unwrap();
        let runtime = ExtensionRuntime::reload_from_store(&store).unwrap();

        let inventory = build_effective_inventory(data.path(), &runtime, Some(&host));
        let skill_rows: Vec<_> = inventory
            .entries
            .iter()
            .filter(|e| e.kind == InventoryKind::Skill && e.key == "demo-skill")
            .collect();
        assert!(
            skill_rows
                .iter()
                .any(|e| e.origin == InventoryOrigin::PackageHost && !e.shadowed),
            "package must win: {skill_rows:?}"
        );
        assert!(
            skill_rows
                .iter()
                .any(|e| e.origin == InventoryOrigin::LegacyCli && e.shadowed),
            "legacy must be shadowed: {skill_rows:?}"
        );

        let roots = effective_skill_roots(data.path(), Some(&host));
        // Only package skill root — no duplicate legacy activation.
        assert_eq!(roots.len(), 1, "roots={roots:?}");
        assert!(roots[0].ends_with("skills"));
    }

    #[test]
    fn effective_skill_roots_survive_hostless_restart() {
        let data = tempfile::tempdir().unwrap();
        let skill = daemon_legacy_skill_path(data.path(), "alone").unwrap();
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, "---\nname: alone\nversion: \"1.0.0\"\n---\nbody\n").unwrap();
        let store = ExtensionStateStore::open(daemon_extension_state_db(data.path())).unwrap();
        store
            .put(&ExtensionState {
                installation_id: "a1".into(),
                resolution: crate::extension_lifecycle::ResolutionPlan {
                    source: ExtensionSource::AgentSkills,
                    module_id: "alone".into(),
                    module_name: "alone".into(),
                    version: "1.0.0".into(),
                    source_path: skill.clone(),
                },
                created_paths: vec![skill],
                modified_paths: vec![],
                ownership: vec![],
                status: ExtensionLifecycleStatus::Enabled,
            })
            .unwrap();

        let roots = effective_skill_roots(data.path(), None);
        assert_eq!(roots.len(), 1);
        assert!(roots[0].ends_with("alone"));
    }
}
