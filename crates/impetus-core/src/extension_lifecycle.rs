//! Extension install planning and apply.
//!
//! Lifecycle (TODO P1 §2):
//! `Manifest → ResolutionPlan → InstallPlan → Apply → ExtensionState`
//!
//! Covers **ResolutionPlan → InstallPlan → Apply → ExtensionState**:
//! dry-run plan, register-before-write ownership, and durable install state.
//! CLI: `impetus extension plan | install | remove | doctor | repair`.

use crate::agent_skills_adapter::AgentSkillsAdapter;
use crate::extension_compat::{ExtensionSource, McpModule, McpTransport};
use crate::extension_manifest::{ExtensionManifest, ExtensionManifestError, ExtensionManifestKind};
use crate::ownership::{OwnershipError, OwnershipRecord, OwnershipStore, content_digest, path_key};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

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
    /// Validated `impetus.extension.v1` manifest (id/kind/version/digest/capabilities).
    pub manifest: ExtensionManifest,
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
    #[error("invalid extension manifest: {0}")]
    InvalidManifest(#[from] ExtensionManifestError),
    #[error("path key error: {0}")]
    PathKey(String),
}

/// Durable result of applying an [`InstallPlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionState {
    pub installation_id: String,
    pub resolution: ResolutionPlan,
    pub created_paths: Vec<PathBuf>,
    pub modified_paths: Vec<PathBuf>,
    /// Ownership rows written or updated for this install.
    pub ownership: Vec<OwnershipRecord>,
}

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ownership error: {0}")]
    Ownership(#[from] OwnershipError),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("install state already exists for id: {0}")]
    StateExists(String),
    #[error("path key error: {0}")]
    PathKey(String),
}

/// SQLite-backed install-state store (lookup by `installation_id`).
pub struct ExtensionStateStore {
    conn: Arc<Mutex<Connection>>,
}

impl ExtensionStateStore {
    /// Open or create the install-state database at `db_path`.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, ApplyError> {
        let db_path = db_path.as_ref();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(db_path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS extension_install_state (
                installation_id TEXT PRIMARY KEY NOT NULL,
                state_json TEXT NOT NULL,
                created_unix_ms INTEGER NOT NULL
            )",
            [],
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Persist a completed install state.
    pub fn put(&self, state: &ExtensionState) -> Result<(), ApplyError> {
        let json = serde_json::to_string(state)
            .map_err(|e| ApplyError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        let created_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_millis() as i64;

        let conn = self.conn.lock().expect("install state db lock");
        match conn.execute(
            "INSERT INTO extension_install_state
                (installation_id, state_json, created_unix_ms)
             VALUES (?1, ?2, ?3)",
            params![&state.installation_id, &json, created_unix_ms],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(ApplyError::StateExists(state.installation_id.clone()))
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Lookup install state by `installation_id`.
    pub fn get(&self, installation_id: &str) -> Result<Option<ExtensionState>, ApplyError> {
        let conn = self.conn.lock().expect("install state db lock");
        let row: Option<String> = conn
            .query_row(
                "SELECT state_json FROM extension_install_state WHERE installation_id = ?1",
                params![installation_id],
                |row| row.get(0),
            )
            .optional()?;
        match row {
            Some(json) => {
                let state = serde_json::from_str(&json).map_err(|e| {
                    ApplyError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                })?;
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }

    /// Delete install state by `installation_id`. Returns whether a row existed.
    pub fn delete(&self, installation_id: &str) -> Result<bool, ApplyError> {
        let conn = self.conn.lock().expect("install state db lock");
        let n = conn.execute(
            "DELETE FROM extension_install_state WHERE installation_id = ?1",
            params![installation_id],
        )?;
        Ok(n > 0)
    }

    /// List all persisted install states (ordered by creation time).
    pub fn list_all(&self) -> Result<Vec<ExtensionState>, ApplyError> {
        let conn = self.conn.lock().expect("install state db lock");
        let mut stmt = conn.prepare(
            "SELECT state_json FROM extension_install_state ORDER BY created_unix_ms, installation_id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            let json = row?;
            let state = serde_json::from_str(&json).map_err(|e| {
                ApplyError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            })?;
            out.push(state);
        }
        Ok(out)
    }
}

/// Result of a successful [`remove_install`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveResult {
    pub installation_id: String,
    pub removed_paths: Vec<PathBuf>,
}

#[derive(Debug, Error)]
pub enum RemoveError {
    #[error(transparent)]
    Apply(#[from] ApplyError),
    #[error(transparent)]
    Ownership(#[from] OwnershipError),
    #[error("install state not found for id: {0}")]
    StateNotFound(String),
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

    let bytes = std::fs::read(&skill_md)?;
    let digest = content_digest(&bytes);
    let manifest = ExtensionManifest::new(
        skill.id.clone(),
        ExtensionManifestKind::Skill,
        spec.version.clone(),
        digest,
        vec!["instructions".to_string(), "triggers".to_string()],
    )?;

    let dest = skill_dest(target_root, &spec.id);
    let resolution = ResolutionPlan {
        source: ExtensionSource::AgentSkills,
        module_id: skill.id,
        module_name: spec.name,
        version: spec.version,
        source_path: skill_md.canonicalize().unwrap_or_else(|_| skill_md.clone()),
    };
    classify_plan(resolution, manifest, vec![dest])
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
    let digest = content_digest(&bytes);
    // Ponytail: MCP config JSON has no version field; stable placeholder under extension.v1.
    let version = "1.0.0".to_string();
    let manifest = ExtensionManifest::new(
        module_id.clone(),
        ExtensionManifestKind::McpConfig,
        version.clone(),
        digest,
        mcp_capability_tokens(&module),
    )?;

    let dest = mcp_dest(target_root, &module_id);
    let resolution = ResolutionPlan {
        source: ExtensionSource::Mcp,
        module_id,
        module_name: module.name,
        version,
        source_path: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
    };
    // Dry-run: parse config only — no MCP server spawn.
    classify_plan(resolution, manifest, vec![dest])
}

fn mcp_capability_tokens(module: &McpModule) -> Vec<String> {
    let mut caps = vec!["mcp".to_string()];
    caps.push(match module.transport {
        McpTransport::Stdio => "stdio".to_string(),
        McpTransport::Http => "http".to_string(),
        McpTransport::Sse => "sse".to_string(),
    });
    if module.capabilities.tools {
        caps.push("tools".to_string());
    }
    if module.capabilities.resources {
        caps.push("resources".to_string());
    }
    if module.capabilities.prompts {
        caps.push("prompts".to_string());
    }
    if module.capabilities.sampling {
        caps.push("sampling".to_string());
    }
    caps
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
    manifest: ExtensionManifest,
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
        manifest,
        created_paths,
        modified_paths,
    })
}

const OWNER_IMPETUS: &str = "impetus";

/// Apply a dry-run plan: write files, record ownership, persist install state.
///
/// Register-before-write for new paths. Owned modified paths go through
/// [`OwnershipStore::repair`] (digest must still match unless unrelated edits).
/// Unowned destinations refuse overwrite via [`OwnershipStore::ensure_can_overwrite`].
pub fn apply_install(
    plan: &InstallPlan,
    ownership: &OwnershipStore,
    state_store: &ExtensionStateStore,
) -> Result<ExtensionState, ApplyError> {
    let installation_id = Uuid::new_v4().to_string();
    let bytes = std::fs::read(&plan.resolution.source_path)?;
    let digest = content_digest(&bytes);
    let source = provenance(&plan.resolution);
    let mut ownership_records = Vec::new();

    for dest in &plan.created_paths {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let key = path_key(dest).map_err(|e| ApplyError::PathKey(e.to_string()))?;
        let record = OwnershipRecord {
            path: key,
            owner: OWNER_IMPETUS.into(),
            source: source.clone(),
            digest: digest.clone(),
            version: plan.resolution.version.clone(),
            installation_id: installation_id.clone(),
        };
        ownership.create(&record)?;
        std::fs::write(dest, &bytes)?;
        ownership_records.push(record);
    }

    for dest in &plan.modified_paths {
        ownership.ensure_can_overwrite(dest)?;
        let updated = ownership.repair(dest, &bytes, false)?;
        ownership.rebind_installation(
            &updated.path,
            &installation_id,
            &source,
            &plan.resolution.version,
        )?;
        let rebound = ownership
            .get_by_path(&updated.path)?
            .ok_or_else(|| OwnershipError::NotOwned(updated.path.clone()))?;
        ownership_records.push(rebound);
    }

    ownership_records.sort_by(|a, b| a.path.cmp(&b.path));

    let state = ExtensionState {
        installation_id,
        resolution: plan.resolution.clone(),
        created_paths: plan.created_paths.clone(),
        modified_paths: plan.modified_paths.clone(),
        ownership: ownership_records,
    };
    state_store.put(&state)?;
    Ok(state)
}

/// Uninstall an extension by `installation_id` using ownership proof.
///
/// Requires a persisted install-state row. Removes each path returned by
/// [`OwnershipStore::list_by_installation_id`] via [`OwnershipStore::uninstall`]
/// (digest + owner must match). Deletes install state only after all owned
/// paths are cleared. Digest mismatch leaves remaining paths and state intact.
pub fn remove_install(
    installation_id: &str,
    ownership: &OwnershipStore,
    state_store: &ExtensionStateStore,
) -> Result<RemoveResult, RemoveError> {
    if state_store.get(installation_id)?.is_none() {
        return Err(RemoveError::StateNotFound(installation_id.to_string()));
    }

    let records = ownership.list_by_installation_id(installation_id)?;
    let mut removed_paths = Vec::with_capacity(records.len());
    for record in &records {
        let path = PathBuf::from(&record.path);
        ownership.uninstall(&path, OWNER_IMPETUS)?;
        removed_paths.push(path);
    }

    state_store.delete(installation_id)?;
    Ok(RemoveResult {
        installation_id: installation_id.to_string(),
        removed_paths,
    })
}

/// On-disk health of one owned path relative to its ownership record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PathHealthStatus {
    Ok,
    Missing,
    DigestMismatch { expected: String, actual: String },
    Unreadable { reason: String },
}

impl PathHealthStatus {
    fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Health of a single owned path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathHealthReport {
    pub path: String,
    #[serde(flatten)]
    pub status: PathHealthStatus,
}

/// Health of one installation (install state + ownership paths).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallHealthReport {
    pub installation_id: String,
    pub state_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<ResolutionPlan>,
    pub paths: Vec<PathHealthReport>,
    pub healthy: bool,
}

/// Aggregate doctor report for one or more installations under a root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub installations: Vec<InstallHealthReport>,
    pub healthy: bool,
}

#[derive(Debug, Error)]
pub enum DoctorError {
    #[error(transparent)]
    Apply(#[from] ApplyError),
    #[error(transparent)]
    Ownership(#[from] OwnershipError),
    #[error("no install state or ownership records for id: {0}")]
    NotFound(String),
}

/// Result of a successful [`repair_install`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairResult {
    pub installation_id: String,
    pub repaired_paths: Vec<PathBuf>,
    /// Owned paths whose on-disk digest already matched the record (untouched).
    pub skipped_ok: Vec<PathBuf>,
}

#[derive(Debug, Error)]
pub enum RepairError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Apply(#[from] ApplyError),
    #[error(transparent)]
    Ownership(#[from] OwnershipError),
    #[error("install state not found for id: {0}")]
    StateNotFound(String),
    #[error("install source missing at {}", .0.display())]
    SourceMissing(PathBuf),
}

/// Restore owned paths for `installation_id` from the recorded `source_path`.
///
/// Uses [`OwnershipStore::repair`]: missing files restore without `force`;
/// digest mismatch (unrelated user edits) refuses unless `force` is true.
/// Paths already matching the ownership digest are skipped.
pub fn repair_install(
    installation_id: &str,
    ownership: &OwnershipStore,
    state_store: &ExtensionStateStore,
    force: bool,
) -> Result<RepairResult, RepairError> {
    let state = state_store
        .get(installation_id)?
        .ok_or_else(|| RepairError::StateNotFound(installation_id.to_string()))?;

    let source = &state.resolution.source_path;
    if !source.is_file() {
        return Err(RepairError::SourceMissing(source.clone()));
    }
    let bytes = fs::read(source)?;

    let records = ownership.list_by_installation_id(installation_id)?;
    let mut repaired_paths = Vec::new();
    let mut skipped_ok = Vec::new();

    for record in &records {
        let path = PathBuf::from(&record.path);
        let health = check_owned_path(record);
        if matches!(health.status, PathHealthStatus::Ok) {
            skipped_ok.push(path);
            continue;
        }
        ownership.repair(&path, &bytes, force)?;
        repaired_paths.push(path);
    }

    repaired_paths.sort();
    skipped_ok.sort();
    Ok(RepairResult {
        installation_id: installation_id.to_string(),
        repaired_paths,
        skipped_ok,
    })
}

/// Report install-state + ownership health (read-only; no repair).
///
/// - `Some(id)` — one installation; errors if neither state nor ownership rows exist.
/// - `None` — every row in the install-state store under the open DBs.
pub fn doctor_install(
    installation_id: Option<&str>,
    ownership: &OwnershipStore,
    state_store: &ExtensionStateStore,
) -> Result<DoctorReport, DoctorError> {
    let installations = match installation_id {
        Some(id) => {
            let report = doctor_one(id, ownership, state_store)?;
            if !report.state_present && report.paths.is_empty() {
                return Err(DoctorError::NotFound(id.to_string()));
            }
            vec![report]
        }
        None => {
            let states = state_store.list_all()?;
            let mut out = Vec::with_capacity(states.len());
            for state in states {
                out.push(doctor_one(&state.installation_id, ownership, state_store)?);
            }
            out
        }
    };
    let healthy = installations.iter().all(|r| r.healthy);
    Ok(DoctorReport {
        installations,
        healthy,
    })
}

fn doctor_one(
    installation_id: &str,
    ownership: &OwnershipStore,
    state_store: &ExtensionStateStore,
) -> Result<InstallHealthReport, DoctorError> {
    let state = state_store.get(installation_id)?;
    let records = ownership.list_by_installation_id(installation_id)?;
    let paths: Vec<PathHealthReport> = records.iter().map(check_owned_path).collect();
    let healthy = paths.iter().all(|p| p.status.is_ok());
    Ok(InstallHealthReport {
        installation_id: installation_id.to_string(),
        state_present: state.is_some(),
        resolution: state.map(|s| s.resolution),
        paths,
        healthy,
    })
}

fn check_owned_path(record: &OwnershipRecord) -> PathHealthReport {
    let path = PathBuf::from(&record.path);
    let status = if !path.exists() {
        PathHealthStatus::Missing
    } else {
        match fs::read(&path) {
            Ok(bytes) => {
                let actual = content_digest(&bytes);
                if actual == record.digest {
                    PathHealthStatus::Ok
                } else {
                    PathHealthStatus::DigestMismatch {
                        expected: record.digest.clone(),
                        actual,
                    }
                }
            }
            Err(err) => PathHealthStatus::Unreadable {
                reason: err.to_string(),
            },
        }
    };
    PathHealthReport {
        path: record.path.clone(),
        status,
    }
}

fn provenance(resolution: &ResolutionPlan) -> String {
    let source = match &resolution.source {
        ExtensionSource::Native => "native",
        ExtensionSource::AgentSkills => "agent_skills",
        ExtensionSource::Mcp => "mcp",
        ExtensionSource::AgentPlugins => "agent_plugins",
        ExtensionSource::ClaudeCode => "claude_code",
        ExtensionSource::Codex => "codex",
        ExtensionSource::Cursor => "cursor",
        ExtensionSource::DeepSeekHarness => "deepseek_harness",
        ExtensionSource::Custom(name) => name.as_str(),
    };
    format!(
        "extension://{}/{}/{}",
        source, resolution.module_id, resolution.version
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::{OwnershipError, OwnershipStore, content_digest, path_key};
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
        assert_eq!(plan.manifest.id, "demo-skill");
        assert_eq!(
            plan.manifest.kind,
            crate::extension_manifest::ExtensionManifestKind::Skill
        );
        assert_eq!(plan.manifest.version, plan.resolution.version);
        assert!(!plan.manifest.version.is_empty());
        assert!(plan.manifest.digest.starts_with("sha256:"));
        assert_eq!(
            plan.manifest.capabilities,
            vec!["instructions".to_string(), "triggers".to_string()]
        );
        plan.manifest.validate().expect("manifest valid");
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
        assert_eq!(plan.manifest.id, "filesystem");
        assert_eq!(
            plan.manifest.kind,
            crate::extension_manifest::ExtensionManifestKind::McpConfig
        );
        assert_eq!(
            plan.manifest.capabilities,
            vec!["mcp".to_string(), "stdio".to_string(), "tools".to_string()]
        );
        plan.manifest.validate().expect("manifest valid");
        assert_eq!(plan.modified_paths, Vec::<PathBuf>::new());
        assert_eq!(plan.created_paths.len(), 1);
        assert!(
            plan.created_paths[0].ends_with(Path::new(".impetus/mcp/filesystem.json")),
            "unexpected dest {:?}",
            plan.created_paths[0]
        );
        assert!(!plan.created_paths[0].exists());
    }

    fn open_stores(dir: &Path) -> (OwnershipStore, ExtensionStateStore) {
        let ownership = OwnershipStore::open(dir.join("ownership.db")).expect("ownership store");
        let state = ExtensionStateStore::open(dir.join("install_state.db")).expect("state store");
        (ownership, state)
    }

    #[tokio::test]
    async fn skill_apply_writes_ownership_and_persists_state() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let skill_md = write_skill(src.path(), "demo-skill", "Do the thing.");
        let (ownership, state_store) = open_stores(target.path());

        let plan = plan_install(
            &ExtensionInstallIntent::Skill {
                path: skill_md.parent().unwrap().to_path_buf(),
            },
            target.path(),
        )
        .await
        .expect("plan");

        let state = apply_install(&plan, &ownership, &state_store).expect("apply");

        assert_eq!(state.created_paths, plan.created_paths);
        assert!(state.modified_paths.is_empty());
        assert_eq!(state.ownership.len(), 1);
        assert!(!state.installation_id.is_empty());

        let dest = &state.created_paths[0];
        assert!(dest.exists(), "apply must write skill file");
        let written = fs::read_to_string(dest).expect("read dest");
        assert!(written.contains("Do the thing."));

        let owned = ownership
            .get_by_path(&state.ownership[0].path)
            .expect("lookup")
            .expect("owned");
        assert_eq!(owned.installation_id, state.installation_id);
        assert_eq!(owned.digest, content_digest(&fs::read(dest).unwrap()));

        let listed = ownership
            .list_by_installation_id(&state.installation_id)
            .expect("list");
        assert_eq!(listed, state.ownership);

        let loaded = state_store
            .get(&state.installation_id)
            .expect("get state")
            .expect("present");
        assert_eq!(loaded, state);
    }

    #[tokio::test]
    async fn mcp_apply_writes_and_lookup_by_installation_id() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let config = write_mcp_config(src.path(), "filesystem");
        let (ownership, state_store) = open_stores(target.path());

        let plan = plan_install(
            &ExtensionInstallIntent::McpConfig {
                path: config.clone(),
            },
            target.path(),
        )
        .await
        .expect("plan");

        let state = apply_install(&plan, &ownership, &state_store).expect("apply");
        let dest = &state.created_paths[0];
        assert!(dest.exists());
        assert_eq!(
            fs::read(dest).unwrap(),
            fs::read(&config).unwrap(),
            "MCP config must be copied byte-for-byte"
        );

        let loaded = state_store
            .get(&state.installation_id)
            .expect("get")
            .expect("present");
        assert_eq!(loaded.resolution.module_id, "filesystem");
        assert_eq!(
            ownership
                .list_by_installation_id(&state.installation_id)
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn apply_refuses_unowned_existing_destination() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let skill_md = write_skill(src.path(), "demo-skill", "v2");
        let dest = skill_dest(target.path(), "demo-skill");
        fs::create_dir_all(dest.parent().unwrap()).expect("dest dir");
        fs::write(&dest, "pre-existing user").expect("seed");

        let (ownership, state_store) = open_stores(target.path());
        let plan = plan_install(
            &ExtensionInstallIntent::Skill {
                path: skill_md.clone(),
            },
            target.path(),
        )
        .await
        .expect("plan");
        assert_eq!(plan.modified_paths.len(), 1);

        let err = apply_install(&plan, &ownership, &state_store).expect_err("unowned");
        assert!(matches!(
            err,
            ApplyError::Ownership(OwnershipError::UnownedDestination(_))
        ));
        assert_eq!(fs::read_to_string(&dest).unwrap(), "pre-existing user");
        assert!(state_store.get("anything").unwrap().is_none());
    }

    #[tokio::test]
    async fn remove_install_deletes_owned_paths_and_state() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let skill_md = write_skill(src.path(), "demo-skill", "remove me");
        let (ownership, state_store) = open_stores(target.path());

        let plan = plan_install(
            &ExtensionInstallIntent::Skill {
                path: skill_md.parent().unwrap().to_path_buf(),
            },
            target.path(),
        )
        .await
        .expect("plan");
        let state = apply_install(&plan, &ownership, &state_store).expect("apply");
        let dest = state.created_paths[0].clone();
        assert!(dest.exists());

        let result =
            remove_install(&state.installation_id, &ownership, &state_store).expect("remove");
        assert_eq!(result.installation_id, state.installation_id);
        assert_eq!(result.removed_paths.len(), 1);
        assert_eq!(
            path_key(&result.removed_paths[0]).unwrap(),
            path_key(&dest).unwrap()
        );
        assert!(!dest.exists(), "owned file must be deleted");
        assert!(
            ownership
                .list_by_installation_id(&state.installation_id)
                .unwrap()
                .is_empty()
        );
        assert!(state_store.get(&state.installation_id).unwrap().is_none());
    }

    #[tokio::test]
    async fn remove_install_refuses_digest_mismatch_keeps_state() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let skill_md = write_skill(src.path(), "demo-skill", "original");
        let (ownership, state_store) = open_stores(target.path());

        let plan = plan_install(
            &ExtensionInstallIntent::Skill {
                path: skill_md.parent().unwrap().to_path_buf(),
            },
            target.path(),
        )
        .await
        .expect("plan");
        let state = apply_install(&plan, &ownership, &state_store).expect("apply");
        let dest = &state.created_paths[0];
        fs::write(dest, "user edited").expect("tamper");

        let err = remove_install(&state.installation_id, &ownership, &state_store)
            .expect_err("digest mismatch");
        assert!(matches!(
            err,
            RemoveError::Ownership(OwnershipError::DigestMismatch(_))
        ));
        assert_eq!(fs::read_to_string(dest).unwrap(), "user edited");
        assert!(
            state_store.get(&state.installation_id).unwrap().is_some(),
            "state must remain for retry after mismatch"
        );
        assert_eq!(
            ownership
                .list_by_installation_id(&state.installation_id)
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn remove_install_unknown_id_errors() {
        let target = tempfile::tempdir().expect("target");
        let (ownership, state_store) = open_stores(target.path());
        let err = remove_install("missing-id", &ownership, &state_store).expect_err("missing");
        assert!(matches!(err, RemoveError::StateNotFound(_)));
    }

    #[tokio::test]
    async fn doctor_reports_ok_for_healthy_install() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let skill_md = write_skill(src.path(), "healthy-skill", "Stay healthy.");
        let (ownership, state_store) = open_stores(target.path());

        let plan = plan_install(
            &ExtensionInstallIntent::Skill { path: skill_md },
            target.path(),
        )
        .await
        .expect("plan");
        let state = apply_install(&plan, &ownership, &state_store).expect("apply");

        let report =
            doctor_install(Some(&state.installation_id), &ownership, &state_store).expect("doctor");
        assert!(report.healthy);
        assert_eq!(report.installations.len(), 1);
        let one = &report.installations[0];
        assert!(one.state_present);
        assert!(one.healthy);
        assert_eq!(one.paths.len(), 1);
        assert!(matches!(one.paths[0].status, PathHealthStatus::Ok));
    }

    #[tokio::test]
    async fn doctor_detects_missing_and_digest_mismatch() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let skill_md = write_skill(src.path(), "broken-skill", "Will break.");
        let (ownership, state_store) = open_stores(target.path());

        let plan = plan_install(
            &ExtensionInstallIntent::Skill { path: skill_md },
            target.path(),
        )
        .await
        .expect("plan");
        let state = apply_install(&plan, &ownership, &state_store).expect("apply");
        let owned_path = PathBuf::from(&state.ownership[0].path);

        fs::write(&owned_path, b"tampered").expect("tamper");
        let mismatch = doctor_install(Some(&state.installation_id), &ownership, &state_store)
            .expect("doctor mismatch");
        assert!(!mismatch.healthy);
        assert!(matches!(
            mismatch.installations[0].paths[0].status,
            PathHealthStatus::DigestMismatch { .. }
        ));

        fs::remove_file(&owned_path).expect("delete");
        let missing = doctor_install(Some(&state.installation_id), &ownership, &state_store)
            .expect("doctor missing");
        assert!(!missing.healthy);
        assert!(matches!(
            missing.installations[0].paths[0].status,
            PathHealthStatus::Missing
        ));
    }

    #[tokio::test]
    async fn doctor_lists_all_under_store() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let (ownership, state_store) = open_stores(target.path());

        for name in ["a-skill", "b-skill"] {
            let skill_md = write_skill(src.path(), name, "body");
            let plan = plan_install(
                &ExtensionInstallIntent::Skill { path: skill_md },
                target.path(),
            )
            .await
            .expect("plan");
            apply_install(&plan, &ownership, &state_store).expect("apply");
        }

        let report = doctor_install(None, &ownership, &state_store).expect("doctor all");
        assert!(report.healthy);
        assert_eq!(report.installations.len(), 2);
    }

    #[tokio::test]
    async fn doctor_unknown_id_errors() {
        let target = tempfile::tempdir().expect("target");
        let (ownership, state_store) = open_stores(target.path());
        let err =
            doctor_install(Some("missing-id"), &ownership, &state_store).expect_err("missing");
        assert!(matches!(err, DoctorError::NotFound(_)));
    }

    #[tokio::test]
    async fn repair_refuses_digest_mismatch_without_force() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let skill_md = write_skill(src.path(), "repair-skill", "original body");
        let (ownership, state_store) = open_stores(target.path());

        let plan = plan_install(
            &ExtensionInstallIntent::Skill { path: skill_md },
            target.path(),
        )
        .await
        .expect("plan");
        let state = apply_install(&plan, &ownership, &state_store).expect("apply");
        let dest = PathBuf::from(&state.ownership[0].path);
        fs::write(&dest, "user edited").expect("tamper");

        let err = repair_install(&state.installation_id, &ownership, &state_store, false)
            .expect_err("digest mismatch");
        assert!(matches!(
            err,
            RepairError::Ownership(OwnershipError::DigestMismatch(_))
        ));
        assert_eq!(fs::read_to_string(&dest).unwrap(), "user edited");
    }

    #[tokio::test]
    async fn repair_overwrites_digest_mismatch_with_force() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let skill_md = write_skill(src.path(), "force-skill", "canonical");
        let (ownership, state_store) = open_stores(target.path());

        let plan = plan_install(
            &ExtensionInstallIntent::Skill {
                path: skill_md.clone(),
            },
            target.path(),
        )
        .await
        .expect("plan");
        let state = apply_install(&plan, &ownership, &state_store).expect("apply");
        let dest = PathBuf::from(&state.ownership[0].path);
        fs::write(&dest, "user edited").expect("tamper");

        let result = repair_install(&state.installation_id, &ownership, &state_store, true)
            .expect("force repair");
        assert_eq!(result.repaired_paths.len(), 1);
        assert_eq!(
            fs::read(&dest).unwrap(),
            fs::read(&skill_md).unwrap(),
            "dest restored from source"
        );
        let report =
            doctor_install(Some(&state.installation_id), &ownership, &state_store).expect("doctor");
        assert!(report.healthy);
    }

    #[tokio::test]
    async fn repair_restores_missing_without_force() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let skill_md = write_skill(src.path(), "missing-skill", "restore me");
        let (ownership, state_store) = open_stores(target.path());

        let plan = plan_install(
            &ExtensionInstallIntent::Skill {
                path: skill_md.clone(),
            },
            target.path(),
        )
        .await
        .expect("plan");
        let state = apply_install(&plan, &ownership, &state_store).expect("apply");
        let dest = PathBuf::from(&state.ownership[0].path);
        fs::remove_file(&dest).expect("delete owned");

        let result = repair_install(&state.installation_id, &ownership, &state_store, false)
            .expect("restore missing");
        assert_eq!(result.repaired_paths.len(), 1);
        assert!(dest.exists());
        assert_eq!(fs::read(&dest).unwrap(), fs::read(&skill_md).unwrap());
    }

    #[tokio::test]
    async fn repair_skips_healthy_paths() {
        let src = tempfile::tempdir().expect("src");
        let target = tempfile::tempdir().expect("target");
        let skill_md = write_skill(src.path(), "ok-skill", "already fine");
        let (ownership, state_store) = open_stores(target.path());

        let plan = plan_install(
            &ExtensionInstallIntent::Skill { path: skill_md },
            target.path(),
        )
        .await
        .expect("plan");
        let state = apply_install(&plan, &ownership, &state_store).expect("apply");

        let result = repair_install(&state.installation_id, &ownership, &state_store, false)
            .expect("noop repair");
        assert!(result.repaired_paths.is_empty());
        assert_eq!(result.skipped_ok.len(), 1);
    }

    #[tokio::test]
    async fn repair_unknown_id_errors() {
        let target = tempfile::tempdir().expect("target");
        let (ownership, state_store) = open_stores(target.path());
        let err =
            repair_install("missing-id", &ownership, &state_store, false).expect_err("missing");
        assert!(matches!(err, RepairError::StateNotFound(_)));
    }
}
