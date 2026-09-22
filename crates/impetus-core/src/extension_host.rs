//! Extension package host: discovery → validate → compat → load → activate.
//!
//! Distinct from legacy CLI Skill/MCP [`crate::ExtensionRuntime`] inventory.
//! Authors depend on [`impetus_extension_sdk`]; this module is the daemon-side
//! loader and capability registry feed for AgentLoop.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use impetus_extension_sdk::{
    CURRENT_SUPPORTED_RANGE, ExtensionCapabilityKind, ExtensionEntrypoint, ExtensionId,
    ExtensionPackageManifest, ExtensionPermission,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::extension_capability_registry::ExtensionCapabilityRegistry;
use crate::extension_host_process::{HostProcessSession, spawn_and_initialize};
use crate::extension_policy::permission_eval;
use crate::policy::SandboxScope;

/// Durable disable SoT under `$IMPETUS_DATA_DIR/extensions/disabled_packages.json`.
const DISABLED_PACKAGES_REL: &str = "extensions/disabled_packages.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct DisabledPackagesFile {
    #[serde(default = "disabled_file_version")]
    version: u32,
    #[serde(default)]
    ids: BTreeSet<String>,
}

fn disabled_file_version() -> u32 {
    1
}

/// Where a discovered package was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionPackageSource {
    Global,
    Workspace,
    Dev,
}

/// Lifecycle phase for a loaded package (host-owned; not author ABI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionHostPhase {
    Discovered,
    Validated,
    Compatible,
    Loaded,
    Active,
    Failed,
    Disabled,
}

/// Standard discovery roots (user-writable only; no root-owned defaults).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionDiscoveryRoots {
    pub global_packages: PathBuf,
    pub workspace_extensions: Option<PathBuf>,
    pub dev_packages: PathBuf,
}

impl ExtensionDiscoveryRoots {
    /// Canonical layout under `$IMPETUS_DATA_DIR` (+ optional workspace).
    pub fn from_data_and_workspace(data_root: &Path, workspace: Option<&Path>) -> Self {
        Self {
            global_packages: data_root.join("extensions").join("packages"),
            workspace_extensions: workspace.map(|w| w.join(".impetus").join("extensions")),
            dev_packages: data_root.join("extensions").join("dev"),
        }
    }
}

/// One package directory that contains `extension.toml` or `extension.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPackage {
    pub path: PathBuf,
    pub source: ExtensionPackageSource,
}

/// Loaded package state in the host registry.
#[derive(Debug, Clone)]
pub struct LoadedExtension {
    pub id: ExtensionId,
    pub path: PathBuf,
    pub source: ExtensionPackageSource,
    pub manifest: ExtensionPackageManifest,
    pub phase: ExtensionHostPhase,
    pub last_error: Option<String>,
}

/// Host error — per-package; never panics the daemon.
#[derive(Debug, Error)]
pub enum ExtensionHostError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest error: {0}")]
    Manifest(String),
    #[error("extension `{0}` not found in host")]
    NotFound(String),
    #[error("extension `{0}` is disabled")]
    Disabled(String),
    #[error("extension `{id}` is not activatable in phase {phase:?}")]
    BadPhase {
        id: String,
        phase: ExtensionHostPhase,
    },
}

/// In-memory package host + capability registry view.
#[derive(Debug, Default)]
pub struct ExtensionHost {
    loaded: BTreeMap<String, LoadedExtension>,
    disabled_ids: BTreeSet<String>,
    /// When set, enable/disable persist to `{root}/extensions/disabled_packages.json`.
    persist_root: Option<PathBuf>,
    /// Sandbox scope for activate-time permission_eval (default: no network).
    scope: Option<SandboxScope>,
    /// Live `host_process` children keyed by extension id.
    host_processes: BTreeMap<String, HostProcessSession>,
}

impl ExtensionHost {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Host with durable disable SoT under `data_root`.
    pub fn with_persist_root(data_root: impl Into<PathBuf>) -> Self {
        let mut host = Self::empty();
        host.persist_root = Some(data_root.into());
        host.load_disabled_from_disk();
        host
    }

    /// Replace sandbox scope used for permission_eval on activate.
    pub fn set_scope(&mut self, scope: SandboxScope) {
        self.scope = Some(scope);
    }

    pub fn disabled_ids(&self) -> &BTreeSet<String> {
        &self.disabled_ids
    }

    fn disabled_path(&self) -> Option<PathBuf> {
        self.persist_root
            .as_ref()
            .map(|root| root.join(DISABLED_PACKAGES_REL))
    }

    fn load_disabled_from_disk(&mut self) {
        let Some(path) = self.disabled_path() else {
            return;
        };
        let Ok(bytes) = fs::read(&path) else {
            return;
        };
        let Ok(file) = serde_json::from_slice::<DisabledPackagesFile>(&bytes) else {
            return;
        };
        self.disabled_ids = file.ids;
    }

    fn persist_disabled(&self) -> Result<(), ExtensionHostError> {
        let Some(path) = self.disabled_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = DisabledPackagesFile {
            version: 1,
            ids: self.disabled_ids.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| {
            ExtensionHostError::Manifest(format!("serialize disabled_packages: {e}"))
        })?;
        fs::write(&path, bytes)?;
        Ok(())
    }

    fn scope_ref(&self) -> SandboxScope {
        self.scope.clone().unwrap_or_else(|| {
            SandboxScope::local_workspace(PathBuf::from(".")).with_network(false)
        })
    }

    /// Scan immediate child directories for package manifests.
    pub fn discover(roots: &ExtensionDiscoveryRoots) -> Vec<DiscoveredPackage> {
        let mut out = Vec::new();
        scan_root(
            &roots.global_packages,
            ExtensionPackageSource::Global,
            &mut out,
        );
        if let Some(ws) = &roots.workspace_extensions {
            scan_root(ws, ExtensionPackageSource::Workspace, &mut out);
        }
        scan_root(&roots.dev_packages, ExtensionPackageSource::Dev, &mut out);
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// Parse + validate + compat-check one package (does not insert).
    pub fn load_package(
        path: &Path,
        source: ExtensionPackageSource,
    ) -> Result<LoadedExtension, ExtensionHostError> {
        let manifest = read_manifest(path)?;
        // validate() includes CURRENT_SUPPORTED_RANGE compat.
        manifest
            .validate()
            .map_err(|e| ExtensionHostError::Manifest(e.to_string()))?;
        let _ = CURRENT_SUPPORTED_RANGE; // documented host range
        Ok(LoadedExtension {
            id: manifest.id.clone(),
            path: path.to_path_buf(),
            source,
            manifest,
            phase: ExtensionHostPhase::Loaded,
            last_error: None,
        })
    }

    /// Rediscover roots; preserve disable set; isolate per-package failures.
    pub fn reload(
        &mut self,
        roots: &ExtensionDiscoveryRoots,
    ) -> Vec<(PathBuf, Result<(), String>)> {
        let disabled = self.disabled_ids.clone();
        let scope = self.scope_ref();
        let mut results = Vec::new();
        let mut next = BTreeMap::new();
        let mut pending_host: BTreeMap<String, HostProcessSession> = BTreeMap::new();
        for discovered in Self::discover(roots) {
            match Self::load_package(&discovered.path, discovered.source) {
                Ok(mut loaded) => {
                    let id = loaded.id.as_str().to_string();
                    if next.contains_key(&id) {
                        results.push((
                            discovered.path,
                            Err(format!(
                                "duplicate extension id `{id}` across discovery roots"
                            )),
                        ));
                        continue;
                    }
                    if disabled.contains(&id) {
                        loaded.phase = ExtensionHostPhase::Disabled;
                    } else {
                        loaded.phase = match &loaded.manifest.entrypoint {
                            ExtensionEntrypoint::InstructionPack { .. } => {
                                match permission_eval(&loaded.manifest.permissions, &scope) {
                                    Ok(()) => ExtensionHostPhase::Active,
                                    Err(reason) => {
                                        loaded.last_error = Some(reason);
                                        ExtensionHostPhase::Failed
                                    }
                                }
                            }
                            ExtensionEntrypoint::McpBridge { module_id } => {
                                match permission_eval(&loaded.manifest.permissions, &scope) {
                                    Ok(()) => match self.enable_mcp_module(module_id) {
                                        Ok(()) => ExtensionHostPhase::Active,
                                        Err(err) => {
                                            loaded.last_error = Some(err.to_string());
                                            ExtensionHostPhase::Loaded
                                        }
                                    },
                                    Err(reason) => {
                                        loaded.last_error = Some(reason);
                                        ExtensionHostPhase::Failed
                                    }
                                }
                            }
                            ExtensionEntrypoint::HostProcess { command, args } => {
                                match permission_eval(&loaded.manifest.permissions, &scope) {
                                    Ok(()) => {
                                        if !loaded
                                            .manifest
                                            .permissions
                                            .contains(&ExtensionPermission::ProcessSpawn)
                                        {
                                            loaded.last_error = Some(
                                                "host_process requires permission `process_spawn`"
                                                    .into(),
                                            );
                                            ExtensionHostPhase::Failed
                                        } else {
                                            match spawn_and_initialize(
                                                &loaded.path,
                                                &id,
                                                loaded.manifest.extension_api_version,
                                                command,
                                                args,
                                            ) {
                                                Ok(session) => {
                                                    pending_host.insert(id.clone(), session);
                                                    ExtensionHostPhase::Active
                                                }
                                                Err(err) => {
                                                    loaded.last_error = Some(err.to_string());
                                                    ExtensionHostPhase::Failed
                                                }
                                            }
                                        }
                                    }
                                    Err(reason) => {
                                        loaded.last_error = Some(reason);
                                        ExtensionHostPhase::Failed
                                    }
                                }
                            }
                        };
                    }
                    next.insert(id, loaded);
                    results.push((discovered.path, Ok(())));
                }
                Err(err) => {
                    results.push((discovered.path, Err(err.to_string())));
                }
            }
        }
        for (_id, session) in std::mem::take(&mut self.host_processes) {
            session.shutdown();
        }
        self.loaded = next;
        self.disabled_ids = disabled;
        self.host_processes = pending_host;
        results
    }

    pub fn get(&self, id: &str) -> Option<&LoadedExtension> {
        self.loaded.get(id)
    }

    pub fn list(&self) -> Vec<&LoadedExtension> {
        self.loaded.values().collect()
    }

    pub fn enable(&mut self, id: &str) -> Result<(), ExtensionHostError> {
        self.activate(id)
    }

    pub fn disable(&mut self, id: &str) -> Result<(), ExtensionHostError> {
        self.deactivate(id)
    }

    pub fn activate(&mut self, id: &str) -> Result<(), ExtensionHostError> {
        let scope = self.scope_ref();
        let Some(ext) = self.loaded.get(id) else {
            return Err(ExtensionHostError::NotFound(id.to_string()));
        };
        match ext.phase {
            ExtensionHostPhase::Loaded
            | ExtensionHostPhase::Compatible
            | ExtensionHostPhase::Validated
            | ExtensionHostPhase::Disabled
            | ExtensionHostPhase::Failed
            | ExtensionHostPhase::Active => {}
            other => {
                return Err(ExtensionHostError::BadPhase {
                    id: id.to_string(),
                    phase: other,
                });
            }
        }
        let entry = ext.manifest.entrypoint.clone();
        let permissions = ext.manifest.permissions.clone();
        match entry {
            ExtensionEntrypoint::InstructionPack { .. } => {
                if let Err(reason) = permission_eval(&permissions, &scope) {
                    let ext = self.loaded.get_mut(id).expect("present");
                    ext.phase = ExtensionHostPhase::Failed;
                    ext.last_error = Some(reason.clone());
                    return Err(ExtensionHostError::Manifest(reason));
                }
                let ext = self.loaded.get_mut(id).expect("present");
                ext.phase = ExtensionHostPhase::Active;
                ext.last_error = None;
            }
            ExtensionEntrypoint::McpBridge { module_id } => {
                if let Err(reason) = permission_eval(&permissions, &scope) {
                    let ext = self.loaded.get_mut(id).expect("present");
                    ext.phase = ExtensionHostPhase::Failed;
                    ext.last_error = Some(reason.clone());
                    return Err(ExtensionHostError::Manifest(reason));
                }
                if let Err(err) = self.enable_mcp_module(&module_id) {
                    let ext = self.loaded.get_mut(id).expect("present");
                    ext.phase = ExtensionHostPhase::Loaded;
                    ext.last_error = Some(err.to_string());
                    return Err(err);
                }
                let ext = self.loaded.get_mut(id).expect("present");
                ext.phase = ExtensionHostPhase::Active;
                ext.last_error = None;
            }
            ExtensionEntrypoint::HostProcess { command, args } => {
                if let Err(reason) = permission_eval(&permissions, &scope) {
                    let ext = self.loaded.get_mut(id).expect("present");
                    ext.phase = ExtensionHostPhase::Failed;
                    ext.last_error = Some(reason.clone());
                    return Err(ExtensionHostError::Manifest(reason));
                }
                if !permissions.contains(&ExtensionPermission::ProcessSpawn) {
                    let reason = "host_process requires permission `process_spawn`".to_string();
                    let ext = self.loaded.get_mut(id).expect("present");
                    ext.phase = ExtensionHostPhase::Failed;
                    ext.last_error = Some(reason.clone());
                    return Err(ExtensionHostError::Manifest(reason));
                }
                let path = self.loaded.get(id).expect("present").path.clone();
                let api = self
                    .loaded
                    .get(id)
                    .expect("present")
                    .manifest
                    .extension_api_version;
                if let Some(old) = self.host_processes.remove(id) {
                    old.shutdown();
                }
                match spawn_and_initialize(&path, id, api, &command, &args) {
                    Ok(session) => {
                        self.host_processes.insert(id.to_string(), session);
                        let ext = self.loaded.get_mut(id).expect("present");
                        ext.phase = ExtensionHostPhase::Active;
                        ext.last_error = None;
                    }
                    Err(err) => {
                        let ext = self.loaded.get_mut(id).expect("present");
                        ext.phase = ExtensionHostPhase::Failed;
                        ext.last_error = Some(err.to_string());
                        return Err(ExtensionHostError::Manifest(err.to_string()));
                    }
                }
            }
        }
        self.disabled_ids.remove(id);
        self.persist_disabled()?;
        Ok(())
    }

    pub fn deactivate(&mut self, id: &str) -> Result<(), ExtensionHostError> {
        let Some(ext) = self.loaded.get(id) else {
            return Err(ExtensionHostError::NotFound(id.to_string()));
        };
        let mcp_module = match &ext.manifest.entrypoint {
            ExtensionEntrypoint::McpBridge { module_id } => Some(module_id.clone()),
            _ => None,
        };
        if let Some(module_id) = mcp_module {
            // Best-effort: package still disables even if MCP SoT row already gone.
            let _ = self.disable_mcp_module(&module_id);
        }
        if let Some(session) = self.host_processes.remove(id) {
            session.shutdown();
        }
        let ext = self.loaded.get_mut(id).expect("present");
        ext.phase = ExtensionHostPhase::Disabled;
        self.disabled_ids.insert(id.to_string());
        self.persist_disabled()?;
        Ok(())
    }

    fn enable_mcp_module(&self, module_id: &str) -> Result<(), ExtensionHostError> {
        let Some(root) = &self.persist_root else {
            return Err(ExtensionHostError::Manifest(
                "mcp_bridge activate requires daemon data root (persist_root)".into(),
            ));
        };
        crate::set_daemon_mcp_enabled(root, module_id, true).map_err(|e| {
            ExtensionHostError::Manifest(format!(
                "mcp_bridge module `{module_id}`: {e} (upsert via mcp_manage first)"
            ))
        })
    }

    fn disable_mcp_module(&self, module_id: &str) -> Result<(), ExtensionHostError> {
        let Some(root) = &self.persist_root else {
            return Ok(());
        };
        crate::set_daemon_mcp_enabled(root, module_id, false).map_err(|e| {
            ExtensionHostError::Manifest(format!("mcp_bridge disable `{module_id}`: {e}"))
        })
    }

    /// Absolute instruction-pack roots for **Active** packages only.
    ///
    /// Prefer [`Self::capability_registry`] for AgentLoop consumers.
    pub fn instruction_pack_roots(&self) -> Vec<PathBuf> {
        self.capability_registry().skill_roots
    }

    /// Typed registry view for AgentLoop / Context (not install_state scans).
    pub fn capability_registry(&self) -> ExtensionCapabilityRegistry {
        let mut skill_roots = Vec::new();
        let mut capabilities = Vec::new();
        let mut permissions = Vec::new();
        for ext in self.loaded.values() {
            if ext.phase != ExtensionHostPhase::Active {
                continue;
            }
            for cap in &ext.manifest.capabilities {
                capabilities.push((ext.id.clone(), *cap));
            }
            for perm in &ext.manifest.permissions {
                permissions.push((ext.id.clone(), *perm));
            }
            if let ExtensionEntrypoint::InstructionPack { root } = &ext.manifest.entrypoint {
                let candidate = ext.path.join(root);
                if contained_under(&ext.path, &candidate) {
                    skill_roots.push(candidate);
                }
            }
        }
        skill_roots.sort();
        let mut host_process_ids: Vec<String> = self.host_processes.keys().cloned().collect();
        host_process_ids.sort();
        ExtensionCapabilityRegistry {
            skill_roots,
            capabilities,
            permissions,
            host_process_ids,
        }
    }

    pub fn active_capabilities(&self) -> Vec<(ExtensionId, ExtensionCapabilityKind)> {
        self.capability_registry().capabilities
    }

    pub fn active_permissions(&self) -> Vec<(ExtensionId, ExtensionPermission)> {
        self.capability_registry().permissions
    }
}

fn contained_under(package_root: &Path, candidate: &Path) -> bool {
    let Ok(root) = package_root.canonicalize() else {
        return false;
    };
    let Ok(path) = candidate.canonicalize() else {
        // Not yet created is OK if lexical join stays under root.
        let lexical = package_root.join(candidate.strip_prefix(package_root).unwrap_or(candidate));
        return lexical.starts_with(package_root)
            && !candidate
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir));
    };
    path.starts_with(&root)
}

fn scan_root(root: &Path, source: ExtensionPackageSource, out: &mut Vec<DiscoveredPackage>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if package_manifest_path(&path).is_some() {
            out.push(DiscoveredPackage { path, source });
        }
    }
}

fn package_manifest_path(dir: &Path) -> Option<PathBuf> {
    let toml = dir.join("extension.toml");
    if toml.is_file() {
        return Some(toml);
    }
    let json = dir.join("extension.json");
    if json.is_file() {
        return Some(json);
    }
    None
}

fn read_manifest(dir: &Path) -> Result<ExtensionPackageManifest, ExtensionHostError> {
    let path = package_manifest_path(dir).ok_or_else(|| {
        ExtensionHostError::Manifest(format!("no extension.toml/json in {}", dir.display()))
    })?;
    let text = fs::read_to_string(&path)?;
    if path.extension().and_then(|e| e.to_str()) == Some("json") {
        ExtensionPackageManifest::from_json_str(&text)
            .map_err(|e| ExtensionHostError::Manifest(e.to_string()))
    } else {
        ExtensionPackageManifest::from_toml_str(&text)
            .map_err(|e| ExtensionHostError::Manifest(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_demo_pack(dir: &Path, api_version: u32) {
        fs::create_dir_all(dir.join("skills")).unwrap();
        fs::write(
            dir.join("extension.toml"),
            format!(
                r#"
schema_version = 1
id = "demo-pack"
name = "Demo Pack"
version = "0.1.0"
description = "fixture"
author = "impetus"
extension_api_version = {api_version}
capabilities = ["skill_provider"]
permissions = ["filesystem_read"]

[entrypoint]
kind = "instruction_pack"
root = "skills"
"#
            ),
        )
        .unwrap();
        fs::write(
            dir.join("skills").join("SKILL.md"),
            "---\nid: demo\nscope: global\n---\n# Demo\n",
        )
        .unwrap();
    }

    #[test]
    fn discover_load_activate_deactivate_isolation() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let workspace = tmp.path().join("ws");
        let roots = ExtensionDiscoveryRoots::from_data_and_workspace(&data, Some(&workspace));
        fs::create_dir_all(&roots.global_packages).unwrap();
        fs::create_dir_all(roots.workspace_extensions.as_ref().unwrap()).unwrap();

        let good = roots.global_packages.join("demo-pack");
        write_demo_pack(&good, 1);

        let bad = roots
            .workspace_extensions
            .as_ref()
            .unwrap()
            .join("bad-pack");
        write_demo_pack(&bad, 99);
        // Force bad id file for isolation — overwrite with invalid api already 99.

        let mut host = ExtensionHost::empty();
        let results = host.reload(&roots);
        assert_eq!(results.len(), 2);
        let oks: Vec<_> = results.iter().filter(|(_, r)| r.is_ok()).collect();
        let errs: Vec<_> = results.iter().filter(|(_, r)| r.is_err()).collect();
        assert_eq!(oks.len(), 1);
        assert_eq!(errs.len(), 1);

        host.activate("demo-pack").expect("activate");
        let roots_active = host.instruction_pack_roots();
        assert_eq!(roots_active.len(), 1);
        assert!(roots_active[0].ends_with("skills"));
        assert!(!host.active_capabilities().is_empty());

        // reload auto-activates non-disabled packs
        let _ = host.reload(&roots);
        assert_eq!(host.instruction_pack_roots().len(), 1);

        host.deactivate("demo-pack").expect("deactivate");
        assert!(host.instruction_pack_roots().is_empty());
        assert!(host.active_capabilities().is_empty());
    }

    #[test]
    fn durable_disable_survives_new_host() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let roots = ExtensionDiscoveryRoots::from_data_and_workspace(&data, None);
        fs::create_dir_all(&roots.global_packages).unwrap();
        write_demo_pack(&roots.global_packages.join("demo-pack"), 1);

        let mut host = ExtensionHost::with_persist_root(&data);
        let _ = host.reload(&roots);
        assert_eq!(host.instruction_pack_roots().len(), 1);
        host.deactivate("demo-pack").expect("disable");
        assert!(
            data.join("extensions")
                .join("disabled_packages.json")
                .is_file()
        );

        let mut host2 = ExtensionHost::with_persist_root(&data);
        let _ = host2.reload(&roots);
        assert!(host2.instruction_pack_roots().is_empty());
        assert_eq!(
            host2.get("demo-pack").map(|e| e.phase),
            Some(ExtensionHostPhase::Disabled)
        );

        host2.activate("demo-pack").expect("re-enable");
        let mut host3 = ExtensionHost::with_persist_root(&data);
        let _ = host3.reload(&roots);
        assert_eq!(host3.instruction_pack_roots().len(), 1);
    }

    #[test]
    fn network_permission_blocks_activate_when_scope_denies() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let roots = ExtensionDiscoveryRoots::from_data_and_workspace(&data, None);
        fs::create_dir_all(&roots.global_packages).unwrap();
        let pack = roots.global_packages.join("net-pack");
        fs::create_dir_all(pack.join("skills")).unwrap();
        fs::write(
            pack.join("extension.toml"),
            r#"
schema_version = 1
id = "net-pack"
name = "net"
version = "0.1.0"
description = "needs network"
author = "impetus"
extension_api_version = 1
capabilities = ["skill_provider"]
permissions = ["network"]

[entrypoint]
kind = "instruction_pack"
root = "skills"
"#,
        )
        .unwrap();
        fs::write(
            pack.join("skills").join("SKILL.md"),
            "---\nid: net\nscope: global\n---\n# Net\n",
        )
        .unwrap();

        let mut host = ExtensionHost::empty();
        host.set_scope(crate::SandboxScope::local_workspace(tmp.path()).with_network(false));
        let results = host.reload(&roots);
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_ok());
        assert_eq!(
            host.get("net-pack").map(|e| e.phase),
            Some(ExtensionHostPhase::Failed)
        );
        assert!(host.instruction_pack_roots().is_empty());
    }

    #[test]
    fn mcp_bridge_activates_via_daemon_mcp_sot() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let roots = ExtensionDiscoveryRoots::from_data_and_workspace(&data, None);
        fs::create_dir_all(&roots.global_packages).unwrap();
        let pack = roots.global_packages.join("bridge-pack");
        fs::create_dir_all(&pack).unwrap();
        fs::write(
            pack.join("extension.toml"),
            r#"
schema_version = 1
id = "bridge-pack"
name = "bridge"
version = "0.1.0"
description = "mcp bridge"
author = "impetus"
extension_api_version = 1
capabilities = ["mcp_integration"]
permissions = ["mcp"]

[entrypoint]
kind = "mcp_bridge"
module_id = "echo"
"#,
        )
        .unwrap();

        // Seed MCP SoT as disabled; activate must flip to enabled.
        let mcp_dir = data.join("mcp");
        fs::create_dir_all(&mcp_dir).unwrap();
        crate::upsert_daemon_mcp_server(
            &data,
            &impetus_protocol::McpServerUpsert {
                id: "echo".into(),
                name: "echo".into(),
                command: "true".into(),
                args: vec![],
                transport: impetus_protocol::McpTransport::Stdio,
                capabilities: impetus_protocol::McpCapabilities {
                    tools: true,
                    ..impetus_protocol::McpCapabilities::default()
                },
                env_keys: vec![],
            },
        )
        .expect("seed mcp");
        crate::set_daemon_mcp_enabled(&data, "echo", false).expect("pre-disable");
        assert!(mcp_dir.join("echo.json.disabled").is_file());

        let mut host = ExtensionHost::with_persist_root(&data);
        let _ = host.reload(&roots);
        assert_eq!(
            host.get("bridge-pack").map(|e| e.phase),
            Some(ExtensionHostPhase::Active)
        );
        assert!(mcp_dir.join("echo.json").is_file());
        assert!(!mcp_dir.join("echo.json.disabled").exists());

        host.deactivate("bridge-pack").expect("disable bridge");
        assert_eq!(
            host.get("bridge-pack").map(|e| e.phase),
            Some(ExtensionHostPhase::Disabled)
        );
        assert!(mcp_dir.join("echo.json.disabled").is_file());
    }

    #[test]
    fn host_process_activates_with_initialize_handshake() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let roots = ExtensionDiscoveryRoots::from_data_and_workspace(&data, None);
        fs::create_dir_all(&roots.global_packages).unwrap();
        let pack = roots.global_packages.join("proc-pack");
        fs::create_dir_all(&pack).unwrap();
        fs::write(
            pack.join("extension.toml"),
            r#"
schema_version = 1
id = "proc-pack"
name = "proc"
version = "0.1.0"
description = "host process"
author = "impetus"
extension_api_version = 1
capabilities = ["tool"]
permissions = ["process_spawn"]

[entrypoint]
kind = "host_process"
command = "./ext.sh"
args = []
"#,
        )
        .unwrap();
        let script = pack.join("ext.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *extension/initialize*)
      echo '{"jsonrpc":"2.0","id":1,"result":{"protocol_version":1,"name":"proc"}}'
      ;;
    *extension/shutdown*)
      echo '{"jsonrpc":"2.0","id":2,"result":null}'
      exit 0
      ;;
  esac
done
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();

        let mut host = ExtensionHost::with_persist_root(&data);
        let _ = host.reload(&roots);
        assert_eq!(
            host.get("proc-pack").map(|e| e.phase),
            Some(ExtensionHostPhase::Active)
        );
        let reg = host.capability_registry();
        assert!(reg.host_process_ids.iter().any(|id| id == "proc-pack"));
        host.deactivate("proc-pack").expect("disable");
        assert!(host.capability_registry().host_process_ids.is_empty());
    }

    #[test]
    fn rejects_outdated_api_on_load_package() {
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("old");
        write_demo_pack(&pack, 99);
        let err = ExtensionHost::load_package(&pack, ExtensionPackageSource::Dev).unwrap_err();
        assert!(err.to_string().contains("API version") || err.to_string().contains("above"));
    }
}
