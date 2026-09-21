//! Managed git worktree lifecycle with durable session binding.
//!
//! Vertical slice (TODO P1 §5): create → resume → stop → close → stale → salvage;
//! identity survives compaction via `CompactionStructuralState.worktree_id`;
//! Build-role agents prefer isolated worktrees with attached permissions.
//! Diff / merge-ready / conflict checks come later.
//!
//! Uses the system `git` CLI (no new git dependency). Bindings survive daemon
//! restart via SQLite; `worktree_id` identity is retained through stop/close/stale.
//!
//! ## Build-role permissions hook
//!
//! [`WorktreeManager::create_for_role`] with [`AgentWorkRole::Build`] creates an
//! isolated worktree and persists [`WorktreeAttachedPermissions`]. Callers that
//! wire EffectSeam / PolicyEngine should use
//! [`WorktreeAttachedPermissions::to_sandbox_scope`] or
//! [`WorktreeManager::enforce_write`] so sandbox admission matches the binding.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

use crate::SandboxScope;

/// Lifecycle state for a managed worktree binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeLifecycleState {
    Active,
    Stopped,
    /// Binding still present, but disk/git registration looks abandoned.
    Stale,
    Closed,
}

impl WorktreeLifecycleState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Stopped => "stopped",
            Self::Stale => "stale",
            Self::Closed => "closed",
        }
    }

    fn parse(raw: &str) -> Result<Self, WorktreeError> {
        match raw {
            "active" => Ok(Self::Active),
            "stopped" => Ok(Self::Stopped),
            "stale" => Ok(Self::Stale),
            "closed" => Ok(Self::Closed),
            other => Err(WorktreeError::CorruptState(other.to_string())),
        }
    }
}

/// Why a managed binding was classified as abandoned/stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleReason {
    /// Bound path is gone (git entry may still be prunable).
    PathMissing,
    /// Path still exists but is not listed by `git worktree list`.
    NotRegistered,
}

/// Detection result for an abandoned/stale managed worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleReport {
    pub binding: WorktreeBinding,
    pub reason: StaleReason,
    /// `true` when the worktree directory still exists and can be re-registered.
    pub recoverable: bool,
}

/// Agent role that may own a managed worktree binding.
///
/// Only [`AgentWorkRole::Build`] prefers an isolated worktree; other roles are
/// listed so callers can ask [`AgentWorkRole::prefers_isolated_worktree`] without
/// inventing a parallel enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorkRole {
    Explore,
    Research,
    Build,
    Review,
}

impl AgentWorkRole {
    /// Build agents must not write the main tree; they get an isolated worktree.
    pub fn prefers_isolated_worktree(self) -> bool {
        matches!(self, Self::Build)
    }
}

/// Permission / sandbox metadata attached to a worktree binding.
///
/// For Build role, `workspace_root` and `write_roots` default to the isolated
/// worktree path so EffectSeam path-scope matches the binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeAttachedPermissions {
    pub role: AgentWorkRole,
    /// Primary sandbox workspace (isolated worktree path for Build).
    pub workspace_root: PathBuf,
    /// Paths where writes are admitted; Build defaults to `[workspace_root]`.
    pub write_roots: Vec<PathBuf>,
    #[serde(default)]
    pub allow_network: bool,
    #[serde(default)]
    pub allow_web_outbound: bool,
    #[serde(default)]
    pub allow_private_network: bool,
}

impl WorktreeAttachedPermissions {
    /// Build-role defaults: isolated worktree as sole sandbox + write root.
    pub fn for_build(worktree_path: PathBuf) -> Self {
        Self {
            role: AgentWorkRole::Build,
            write_roots: vec![worktree_path.clone()],
            workspace_root: worktree_path,
            allow_network: false,
            allow_web_outbound: false,
            allow_private_network: false,
        }
    }

    /// Hook for EffectSeam / PolicyEngine wiring.
    pub fn to_sandbox_scope(&self) -> SandboxScope {
        SandboxScope {
            workspace_root: self.workspace_root.clone(),
            allow_network: self.allow_network,
            allowed_hosts: vec![],
            allow_web_outbound: self.allow_web_outbound,
            allow_private_network: self.allow_private_network,
        }
    }

    /// Path-scope write admission against attached `write_roots`.
    pub fn admits_write(&self, candidate: &Path) -> bool {
        self.write_roots
            .iter()
            .any(|root| SandboxScope::local_workspace(root).contains_write_target(candidate))
    }

    /// Path-scope read admission against attached `workspace_root`.
    pub fn admits_read(&self, candidate: &Path) -> bool {
        self.to_sandbox_scope().contains(candidate)
    }
}

/// Durable session ↔ worktree binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeBinding {
    /// Stable identity (survives stop/close; fits `CompactionStructuralState.worktree_id`).
    pub worktree_id: String,
    pub session_id: Uuid,
    pub path: PathBuf,
    pub branch: String,
    pub repo_root: PathBuf,
    pub state: WorktreeLifecycleState,
    /// Present when created via [`WorktreeManager::create_for_role`] (Build).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<WorktreeAttachedPermissions>,
}

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("git failed: {0}")]
    Git(String),
    #[error("session already has an open worktree binding: {0}")]
    AlreadyBound(Uuid),
    #[error("no worktree binding for session: {0}")]
    NotFound(Uuid),
    #[error("no worktree binding with id: {0}")]
    NotFoundId(String),
    #[error("invalid lifecycle transition from {from:?} via {op}")]
    InvalidTransition {
        from: WorktreeLifecycleState,
        op: &'static str,
    },
    #[error("worktree path missing on disk: {0}")]
    PathMissing(String),
    #[error("stale worktree is recoverable; salvage instead of cleanup: {0}")]
    RecoverableNeedsSalvage(String),
    #[error("stale worktree is not recoverable: {0}")]
    NotRecoverable(String),
    #[error("corrupt binding state: {0}")]
    CorruptState(String),
    #[error("worktree binding has no attached permissions")]
    MissingPermissions,
    #[error("path outside attached worktree permissions: {0}")]
    OutsidePermissions(String),
    #[error("role {0:?} does not prefer isolated worktrees")]
    RoleDoesNotPreferIsolated(AgentWorkRole),
}

/// Creates, stops, resumes, closes, detects stale, and salvages git worktrees.
pub struct WorktreeManager {
    conn: Arc<Mutex<Connection>>,
    worktrees_root: PathBuf,
}

impl WorktreeManager {
    /// Open or create the binding store. Worktrees are placed under `worktrees_root`.
    pub fn open(
        db_path: impl AsRef<Path>,
        worktrees_root: impl Into<PathBuf>,
    ) -> Result<Self, WorktreeError> {
        let db_path = db_path.as_ref();
        let worktrees_root = worktrees_root.into();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(&worktrees_root)?;

        let conn = Connection::open(db_path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS worktree_bindings (
                worktree_id TEXT PRIMARY KEY NOT NULL,
                session_id TEXT NOT NULL,
                path TEXT NOT NULL,
                branch TEXT NOT NULL,
                repo_root TEXT NOT NULL,
                state TEXT NOT NULL,
                created_unix_ms INTEGER NOT NULL,
                updated_unix_ms INTEGER NOT NULL,
                permissions_json TEXT
            )",
            [],
        )?;
        Self::migrate_permissions_column(&conn)?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_worktree_bindings_session
             ON worktree_bindings(session_id)",
            [],
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            worktrees_root,
        })
    }

    /// Older DBs lack `permissions_json`; add it when missing.
    fn migrate_permissions_column(conn: &Connection) -> Result<(), WorktreeError> {
        let mut stmt = conn.prepare("PRAGMA table_info(worktree_bindings)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        if !cols.iter().any(|c| c == "permissions_json") {
            conn.execute(
                "ALTER TABLE worktree_bindings ADD COLUMN permissions_json TEXT",
                [],
            )?;
        }
        Ok(())
    }

    /// Create a new git worktree bound to `session_id`.
    ///
    /// Fails if the session already has a non-closed binding.
    /// No role permissions are attached — use [`Self::create_for_role`] for Build.
    pub fn create(
        &self,
        session_id: Uuid,
        repo_root: &Path,
    ) -> Result<WorktreeBinding, WorktreeError> {
        self.create_inner(session_id, repo_root, None)
    }

    /// Create an isolated worktree for a role that prefers isolation (Build).
    ///
    /// Attaches [`WorktreeAttachedPermissions`] so sandbox/write scope defaults
    /// to the worktree path. Other roles return [`WorktreeError::RoleDoesNotPreferIsolated`].
    pub fn create_for_role(
        &self,
        session_id: Uuid,
        repo_root: &Path,
        role: AgentWorkRole,
    ) -> Result<WorktreeBinding, WorktreeError> {
        if !role.prefers_isolated_worktree() {
            return Err(WorktreeError::RoleDoesNotPreferIsolated(role));
        }
        self.create_inner(session_id, repo_root, Some(role))
    }

    /// Enforce write admission using the binding's attached permissions.
    ///
    /// Documented hook for EffectSeam / ToolOrchestrator: call before execution
    /// when a build-role worktree binding is active.
    pub fn enforce_write(binding: &WorktreeBinding, path: &Path) -> Result<(), WorktreeError> {
        let perms = binding
            .permissions
            .as_ref()
            .ok_or(WorktreeError::MissingPermissions)?;
        if perms.admits_write(path) {
            Ok(())
        } else {
            Err(WorktreeError::OutsidePermissions(
                path.to_string_lossy().into_owned(),
            ))
        }
    }

    /// Sandbox scope from attached permissions (Build bindings).
    pub fn attached_sandbox_scope(
        binding: &WorktreeBinding,
    ) -> Result<SandboxScope, WorktreeError> {
        binding
            .permissions
            .as_ref()
            .map(WorktreeAttachedPermissions::to_sandbox_scope)
            .ok_or(WorktreeError::MissingPermissions)
    }

    fn create_inner(
        &self,
        session_id: Uuid,
        repo_root: &Path,
        role: Option<AgentWorkRole>,
    ) -> Result<WorktreeBinding, WorktreeError> {
        if let Some(existing) = self.get_open_by_session(session_id)? {
            return Err(WorktreeError::AlreadyBound(existing.session_id));
        }

        let repo_root = absolute_path(repo_root)?;
        let worktree_id = Uuid::new_v4().to_string();
        let short = &worktree_id[..8];
        let branch = format!("impetus/wt-{short}");
        let path = self.worktrees_root.join(&worktree_id);

        git(
            &repo_root,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                path.to_str()
                    .ok_or_else(|| WorktreeError::Git("worktree path is not valid UTF-8".into()))?,
            ],
        )?;

        let permissions = role.map(|r| match r {
            AgentWorkRole::Build => WorktreeAttachedPermissions::for_build(path.clone()),
            AgentWorkRole::Explore | AgentWorkRole::Research | AgentWorkRole::Review => {
                WorktreeAttachedPermissions {
                    role: r,
                    write_roots: vec![path.clone()],
                    workspace_root: path.clone(),
                    allow_network: false,
                    allow_web_outbound: false,
                    allow_private_network: false,
                }
            }
        });

        let binding = WorktreeBinding {
            worktree_id: worktree_id.clone(),
            session_id,
            path: path.clone(),
            branch: branch.clone(),
            repo_root: repo_root.clone(),
            state: WorktreeLifecycleState::Active,
            permissions,
        };

        self.insert_binding(&binding)?;
        Ok(binding)
    }

    fn insert_binding(&self, binding: &WorktreeBinding) -> Result<(), WorktreeError> {
        let now = now_unix_ms();
        let permissions_json =
            match &binding.permissions {
                Some(p) => Some(serde_json::to_string(p).map_err(|e| {
                    WorktreeError::CorruptState(format!("permissions serialize: {e}"))
                })?),
                None => None,
            };
        let conn = self.conn.lock().expect("worktree db lock");
        conn.execute(
            "INSERT INTO worktree_bindings
                (worktree_id, session_id, path, branch, repo_root, state,
                 created_unix_ms, updated_unix_ms, permissions_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &binding.worktree_id,
                binding.session_id.to_string(),
                binding.path.to_string_lossy().as_ref(),
                &binding.branch,
                binding.repo_root.to_string_lossy().as_ref(),
                binding.state.as_str(),
                now as i64,
                now as i64,
                permissions_json,
            ],
        )?;
        Ok(())
    }

    /// Resume a stopped (or still-active after restart) binding.
    ///
    /// Verifies the worktree path still exists and sets state to Active.
    pub fn resume(&self, session_id: Uuid) -> Result<WorktreeBinding, WorktreeError> {
        let binding = self.require_open(session_id)?;
        match binding.state {
            WorktreeLifecycleState::Active | WorktreeLifecycleState::Stopped => {}
            WorktreeLifecycleState::Stale | WorktreeLifecycleState::Closed => {
                return Err(WorktreeError::InvalidTransition {
                    from: binding.state,
                    op: "resume",
                });
            }
        }
        if !binding.path.exists() {
            return Err(WorktreeError::PathMissing(
                binding.path.to_string_lossy().into_owned(),
            ));
        }
        self.set_state(&binding.worktree_id, WorktreeLifecycleState::Active)?;
        self.get_by_worktree_id(&binding.worktree_id)?
            .ok_or_else(|| WorktreeError::NotFound(session_id))
    }

    /// Mark the binding Stopped; leave the worktree on disk.
    pub fn stop(&self, session_id: Uuid) -> Result<WorktreeBinding, WorktreeError> {
        let binding = self.require_open(session_id)?;
        if binding.state != WorktreeLifecycleState::Active {
            return Err(WorktreeError::InvalidTransition {
                from: binding.state,
                op: "stop",
            });
        }
        self.set_state(&binding.worktree_id, WorktreeLifecycleState::Stopped)?;
        self.get_by_worktree_id(&binding.worktree_id)?
            .ok_or_else(|| WorktreeError::NotFound(session_id))
    }

    /// Remove the git worktree and mark the binding Closed.
    ///
    /// Accepts Active, Stopped, or Stale. `worktree_id` is retained after close.
    pub fn close(&self, session_id: Uuid) -> Result<WorktreeBinding, WorktreeError> {
        let binding = self.require_open(session_id)?;
        match binding.state {
            WorktreeLifecycleState::Active
            | WorktreeLifecycleState::Stopped
            | WorktreeLifecycleState::Stale => {}
            WorktreeLifecycleState::Closed => {
                return Err(WorktreeError::InvalidTransition {
                    from: binding.state,
                    op: "close",
                });
            }
        }
        self.remove_worktree_disk(&binding)?;
        self.set_state(&binding.worktree_id, WorktreeLifecycleState::Closed)?;
        self.get_by_worktree_id(&binding.worktree_id)?
            .ok_or_else(|| WorktreeError::NotFound(session_id))
    }

    /// Scan non-closed bindings for abandoned/stale conditions.
    ///
    /// Does not mutate durable state. Path-missing → not recoverable;
    /// path present but absent from `git worktree list` → recoverable.
    pub fn detect_stale(&self) -> Result<Vec<StaleReport>, WorktreeError> {
        let mut reports = Vec::new();
        for binding in self.list_non_closed()? {
            if let Some(report) = self.classify_stale(&binding)? {
                reports.push(report);
            }
        }
        Ok(reports)
    }

    /// Detect abandoned bindings and mark them `Stale` without deleting disk contents.
    pub fn mark_stale(&self) -> Result<Vec<StaleReport>, WorktreeError> {
        let reports = self.detect_stale()?;
        let mut marked = Vec::new();
        for report in reports {
            if report.binding.state != WorktreeLifecycleState::Stale {
                self.set_state(&report.binding.worktree_id, WorktreeLifecycleState::Stale)?;
            }
            let binding = self
                .get_by_worktree_id(&report.binding.worktree_id)?
                .ok_or_else(|| WorktreeError::NotFoundId(report.binding.worktree_id.clone()))?;
            marked.push(StaleReport {
                binding,
                reason: report.reason,
                recoverable: report.recoverable,
            });
        }
        Ok(marked)
    }

    /// Safe cleanup for a non-recoverable stale binding: prune/remove managed disk
    /// state and mark Closed. Refuses recoverable stale worktrees (use [`Self::salvage`]).
    pub fn cleanup_stale(&self, worktree_id: &str) -> Result<WorktreeBinding, WorktreeError> {
        let binding = self
            .get_by_worktree_id(worktree_id)?
            .ok_or_else(|| WorktreeError::NotFoundId(worktree_id.to_string()))?;
        if binding.state != WorktreeLifecycleState::Stale {
            return Err(WorktreeError::InvalidTransition {
                from: binding.state,
                op: "cleanup_stale",
            });
        }
        let report = self.classify_stale(&binding)?.unwrap_or(StaleReport {
            binding: binding.clone(),
            reason: StaleReason::PathMissing,
            recoverable: binding.path.exists(),
        });
        if report.recoverable {
            return Err(WorktreeError::RecoverableNeedsSalvage(
                worktree_id.to_string(),
            ));
        }
        self.remove_worktree_disk(&binding)?;
        self.set_state(worktree_id, WorktreeLifecycleState::Closed)?;
        self.get_by_worktree_id(worktree_id)?
            .ok_or_else(|| WorktreeError::NotFoundId(worktree_id.to_string()))
    }

    /// Re-register a recoverable abandoned worktree and set state to Active.
    ///
    /// Preserves on-disk files (including untracked) via relocate → `git worktree add`
    /// → overlay restore. Keeps the same `worktree_id`.
    pub fn salvage(&self, worktree_id: &str) -> Result<WorktreeBinding, WorktreeError> {
        let binding = self
            .get_by_worktree_id(worktree_id)?
            .ok_or_else(|| WorktreeError::NotFoundId(worktree_id.to_string()))?;
        match binding.state {
            WorktreeLifecycleState::Stale
            | WorktreeLifecycleState::Active
            | WorktreeLifecycleState::Stopped => {}
            WorktreeLifecycleState::Closed => {
                return Err(WorktreeError::InvalidTransition {
                    from: binding.state,
                    op: "salvage",
                });
            }
        }
        if !binding.path.exists() {
            return Err(WorktreeError::NotRecoverable(worktree_id.to_string()));
        }
        if self.is_registered(&binding)? {
            // Already healthy on disk; just ensure Active.
            self.set_state(worktree_id, WorktreeLifecycleState::Active)?;
            return self
                .get_by_worktree_id(worktree_id)?
                .ok_or_else(|| WorktreeError::NotFoundId(worktree_id.to_string()));
        }

        let path_str = binding
            .path
            .to_str()
            .ok_or_else(|| WorktreeError::Git("worktree path is not valid UTF-8".into()))?
            .to_string();
        let bak = binding
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{worktree_id}.salvage-bak"));
        if bak.exists() {
            std::fs::remove_dir_all(&bak)?;
        }
        std::fs::rename(&binding.path, &bak)?;
        let add_result = git(
            &binding.repo_root,
            &["worktree", "add", &path_str, &binding.branch],
        );
        if let Err(err) = add_result {
            // Best-effort restore of the abandoned tree.
            let _ = std::fs::rename(&bak, &binding.path);
            return Err(err);
        }
        copy_tree_overlay_skip_git(&bak, &binding.path)?;
        std::fs::remove_dir_all(&bak)?;

        self.set_state(worktree_id, WorktreeLifecycleState::Active)?;
        self.get_by_worktree_id(worktree_id)?
            .ok_or_else(|| WorktreeError::NotFoundId(worktree_id.to_string()))
    }

    /// Lookup any binding (including Closed) for a session — newest by update time.
    pub fn get_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<WorktreeBinding>, WorktreeError> {
        let conn = self.conn.lock().expect("worktree db lock");
        let row = conn
            .query_row(
                "SELECT worktree_id, session_id, path, branch, repo_root, state, permissions_json
                 FROM worktree_bindings
                 WHERE session_id = ?1
                 ORDER BY updated_unix_ms DESC
                 LIMIT 1",
                params![session_id.to_string()],
                row_to_binding,
            )
            .optional()?;
        Ok(row)
    }

    /// Lookup by stable worktree identity.
    pub fn get_by_worktree_id(
        &self,
        worktree_id: &str,
    ) -> Result<Option<WorktreeBinding>, WorktreeError> {
        let conn = self.conn.lock().expect("worktree db lock");
        let row = conn
            .query_row(
                "SELECT worktree_id, session_id, path, branch, repo_root, state, permissions_json
                 FROM worktree_bindings WHERE worktree_id = ?1",
                params![worktree_id],
                row_to_binding,
            )
            .optional()?;
        Ok(row)
    }

    /// Resolve session ↔ worktree binding after compaction using structural identity.
    ///
    /// Prefer `structural.worktree_id` when present; fall back to session lookup.
    /// Binding row is unchanged by CompactionCompleted — this only verifies it still
    /// resolves to the same identity.
    pub fn resolve_after_compaction(
        &self,
        session_id: Uuid,
        structural: &crate::CompactionStructuralState,
    ) -> Result<WorktreeBinding, WorktreeError> {
        match structural.worktree_id.as_deref() {
            Some(id) => {
                let binding = self
                    .get_by_worktree_id(id)?
                    .ok_or_else(|| WorktreeError::NotFoundId(id.to_string()))?;
                if binding.session_id != session_id {
                    return Err(WorktreeError::NotFoundId(id.to_string()));
                }
                Ok(binding)
            }
            None => self
                .get_by_session(session_id)?
                .ok_or(WorktreeError::NotFound(session_id)),
        }
    }

    fn classify_stale(
        &self,
        binding: &WorktreeBinding,
    ) -> Result<Option<StaleReport>, WorktreeError> {
        if binding.state == WorktreeLifecycleState::Closed {
            return Ok(None);
        }
        let path_exists = binding.path.exists();
        if !path_exists {
            return Ok(Some(StaleReport {
                binding: binding.clone(),
                reason: StaleReason::PathMissing,
                recoverable: false,
            }));
        }
        if !self.is_registered(binding)? {
            return Ok(Some(StaleReport {
                binding: binding.clone(),
                reason: StaleReason::NotRegistered,
                recoverable: true,
            }));
        }
        Ok(None)
    }

    fn is_registered(&self, binding: &WorktreeBinding) -> Result<bool, WorktreeError> {
        if !binding.repo_root.exists() {
            return Ok(false);
        }
        let registered = git_worktree_paths(&binding.repo_root)?;
        let canonical = absolute_path(&binding.path)?;
        Ok(registered.iter().any(|p| paths_equal(p, &canonical)))
    }

    fn remove_worktree_disk(&self, binding: &WorktreeBinding) -> Result<(), WorktreeError> {
        if binding.path.exists() {
            let path_str = binding.path.to_string_lossy();
            // Prefer git remove; fall back to prune + rmdir if registration is gone.
            match git(
                &binding.repo_root,
                &["worktree", "remove", "--force", path_str.as_ref()],
            ) {
                Ok(_) => {}
                Err(_) => {
                    if binding.path.exists() {
                        std::fs::remove_dir_all(&binding.path)?;
                    }
                    let _ = git(&binding.repo_root, &["worktree", "prune"]);
                }
            }
        } else if binding.repo_root.exists() {
            let _ = git(&binding.repo_root, &["worktree", "prune"]);
        }
        Ok(())
    }

    fn list_non_closed(&self) -> Result<Vec<WorktreeBinding>, WorktreeError> {
        let conn = self.conn.lock().expect("worktree db lock");
        let mut stmt = conn.prepare(
            "SELECT worktree_id, session_id, path, branch, repo_root, state, permissions_json
             FROM worktree_bindings
             WHERE state != 'closed'
             ORDER BY updated_unix_ms ASC",
        )?;
        let rows = stmt.query_map([], row_to_binding)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn get_open_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<WorktreeBinding>, WorktreeError> {
        let conn = self.conn.lock().expect("worktree db lock");
        let row = conn
            .query_row(
                "SELECT worktree_id, session_id, path, branch, repo_root, state, permissions_json
                 FROM worktree_bindings
                 WHERE session_id = ?1 AND state != 'closed'
                 ORDER BY updated_unix_ms DESC
                 LIMIT 1",
                params![session_id.to_string()],
                row_to_binding,
            )
            .optional()?;
        Ok(row)
    }

    fn require_open(&self, session_id: Uuid) -> Result<WorktreeBinding, WorktreeError> {
        self.get_open_by_session(session_id)?
            .ok_or(WorktreeError::NotFound(session_id))
    }

    fn set_state(
        &self,
        worktree_id: &str,
        state: WorktreeLifecycleState,
    ) -> Result<(), WorktreeError> {
        let conn = self.conn.lock().expect("worktree db lock");
        let n = conn.execute(
            "UPDATE worktree_bindings SET state = ?1, updated_unix_ms = ?2
             WHERE worktree_id = ?3",
            params![state.as_str(), now_unix_ms() as i64, worktree_id],
        )?;
        if n == 0 {
            return Err(WorktreeError::CorruptState(format!(
                "missing worktree_id {worktree_id}"
            )));
        }
        Ok(())
    }
}

fn row_to_binding(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorktreeBinding> {
    let state_raw: String = row.get(5)?;
    let state = WorktreeLifecycleState::parse(&state_raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                err.to_string(),
            )),
        )
    })?;
    let session_raw: String = row.get(1)?;
    let session_id = Uuid::parse_str(&session_raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(err))
    })?;
    let permissions_json: Option<String> = row.get(6)?;
    let permissions = match permissions_json {
        Some(raw) if !raw.is_empty() => Some(serde_json::from_str(&raw).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    err.to_string(),
                )),
            )
        })?),
        _ => None,
    };
    Ok(WorktreeBinding {
        worktree_id: row.get(0)?,
        session_id,
        path: PathBuf::from(row.get::<_, String>(2)?),
        branch: row.get(3)?,
        repo_root: PathBuf::from(row.get::<_, String>(4)?),
        state,
        permissions,
    })
}

fn absolute_path(path: &Path) -> Result<PathBuf, WorktreeError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(absolute.canonicalize().unwrap_or(absolute))
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let ca = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let cb = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

fn git_worktree_paths(repo_root: &Path) -> Result<HashSet<PathBuf>, WorktreeError> {
    let output = git(repo_root, &["worktree", "list", "--porcelain"])?;
    let mut paths = HashSet::new();
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            paths.insert(PathBuf::from(rest));
        }
    }
    Ok(paths)
}

fn copy_tree_overlay_skip_git(src: &Path, dst: &Path) -> Result<(), WorktreeError> {
    fn walk(src: &Path, dst: &Path) -> Result<(), WorktreeError> {
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == ".git" {
                continue;
            }
            let from = entry.path();
            let to = dst.join(&name);
            let ft = entry.file_type()?;
            if ft.is_dir() {
                std::fs::create_dir_all(&to)?;
                walk(&from, &to)?;
            } else if ft.is_file() {
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&from, &to)?;
            }
            // Skip symlinks — managed worktrees should not rely on them.
        }
        Ok(())
    }
    walk(src, dst)
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(WorktreeError::Git(detail));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_git_repo(dir: &Path) {
        // `-b main` avoids master/main drift across git versions (2.28+).
        git(dir, &["init", "-b", "main"]).expect("git init");
        git(dir, &["config", "user.email", "test@example.com"]).expect("email");
        git(dir, &["config", "user.name", "Test"]).expect("name");
        std::fs::write(dir.join("README"), b"seed").expect("write");
        git(dir, &["add", "README"]).expect("add");
        git(dir, &["commit", "-m", "seed"]).expect("commit");
    }

    fn temp_manager() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        WorktreeManager,
        PathBuf,
    ) {
        let store_dir = tempfile::tempdir().expect("store temp");
        let repo_dir = tempfile::tempdir().expect("repo temp");
        init_git_repo(repo_dir.path());
        let worktrees = store_dir.path().join("worktrees");
        let manager = WorktreeManager::open(store_dir.path().join("worktrees.db"), &worktrees)
            .expect("open manager");
        (store_dir, repo_dir, manager, worktrees)
    }

    /// Break git registration while keeping the worktree directory and files.
    fn break_registration(binding: &WorktreeBinding) {
        let git_file = binding.path.join(".git");
        let contents = std::fs::read_to_string(&git_file).expect("read .git");
        let admin = contents
            .trim()
            .strip_prefix("gitdir: ")
            .expect("gitdir prefix")
            .to_string();
        let _ = std::fs::remove_dir_all(&admin);
        let _ = std::fs::remove_file(&git_file);
        // Drop stale admin metadata if pruneable entries remain.
        let _ = git(&binding.repo_root, &["worktree", "prune"]);
    }

    #[test]
    fn create_binds_session_and_makes_path() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let binding = manager.create(session, repo.path()).expect("create");

        assert_eq!(binding.session_id, session);
        assert_eq!(binding.state, WorktreeLifecycleState::Active);
        assert!(binding.path.is_dir());
        assert!(binding.path.join("README").is_file());
        assert!(binding.branch.starts_with("impetus/wt-"));
    }

    #[test]
    fn stop_resume_preserves_identity_and_path() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let created = manager.create(session, repo.path()).expect("create");
        let worktree_id = created.worktree_id.clone();
        let path = created.path.clone();

        let stopped = manager.stop(session).expect("stop");
        assert_eq!(stopped.state, WorktreeLifecycleState::Stopped);
        assert_eq!(stopped.worktree_id, worktree_id);
        assert!(path.is_dir());

        let resumed = manager.resume(session).expect("resume");
        assert_eq!(resumed.state, WorktreeLifecycleState::Active);
        assert_eq!(resumed.worktree_id, worktree_id);
        assert_eq!(resumed.path, path);
    }

    #[test]
    fn close_removes_worktree_keeps_identity() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let created = manager.create(session, repo.path()).expect("create");
        let worktree_id = created.worktree_id.clone();
        let path = created.path.clone();

        let closed = manager.close(session).expect("close");
        assert_eq!(closed.state, WorktreeLifecycleState::Closed);
        assert_eq!(closed.worktree_id, worktree_id);
        assert!(!path.exists());

        let by_id = manager
            .get_by_worktree_id(&worktree_id)
            .expect("lookup")
            .expect("present");
        assert_eq!(by_id.state, WorktreeLifecycleState::Closed);
        assert_eq!(by_id.session_id, session);
    }

    #[test]
    fn binding_survives_store_reopen() {
        let store_dir = tempfile::tempdir().expect("store");
        let repo_dir = tempfile::tempdir().expect("repo");
        init_git_repo(repo_dir.path());
        let db = store_dir.path().join("worktrees.db");
        let worktrees = store_dir.path().join("worktrees");
        let session = Uuid::new_v4();
        let worktree_id;

        {
            let manager = WorktreeManager::open(&db, &worktrees).expect("open");
            let created = manager.create(session, repo_dir.path()).expect("create");
            worktree_id = created.worktree_id.clone();
            manager.stop(session).expect("stop");
        }

        let reopened = WorktreeManager::open(&db, &worktrees).expect("reopen");
        let resumed = reopened.resume(session).expect("resume after reopen");
        assert_eq!(resumed.worktree_id, worktree_id);
        assert_eq!(resumed.state, WorktreeLifecycleState::Active);
    }

    #[test]
    fn create_refuses_second_open_binding() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        manager.create(session, repo.path()).expect("first");
        let err = manager.create(session, repo.path()).expect_err("second");
        assert!(matches!(err, WorktreeError::AlreadyBound(_)));
    }

    #[test]
    fn resume_rejects_closed() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        manager.create(session, repo.path()).expect("create");
        manager.close(session).expect("close");
        let err = manager.resume(session).expect_err("resume closed");
        assert!(matches!(err, WorktreeError::NotFound(_)));
    }

    #[test]
    fn detect_stale_path_missing() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let created = manager.create(session, repo.path()).expect("create");
        std::fs::remove_dir_all(&created.path).expect("rm path");
        let _ = git(repo.path(), &["worktree", "prune"]);

        let reports = manager.detect_stale().expect("detect");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].reason, StaleReason::PathMissing);
        assert!(!reports[0].recoverable);
        assert_eq!(reports[0].binding.worktree_id, created.worktree_id);
        // Detection must not mutate durable state.
        let still = manager
            .get_by_worktree_id(&created.worktree_id)
            .expect("lookup")
            .expect("present");
        assert_eq!(still.state, WorktreeLifecycleState::Active);
    }

    #[test]
    fn mark_stale_does_not_delete_disk() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let created = manager.create(session, repo.path()).expect("create");
        std::fs::write(created.path.join("notes.txt"), b"keep").expect("notes");
        break_registration(&created);
        assert!(created.path.join("notes.txt").is_file());

        let marked = manager.mark_stale().expect("mark");
        assert_eq!(marked.len(), 1);
        assert_eq!(marked[0].reason, StaleReason::NotRegistered);
        assert!(marked[0].recoverable);
        assert_eq!(marked[0].binding.state, WorktreeLifecycleState::Stale);
        assert!(created.path.join("notes.txt").is_file());
        assert!(created.path.is_dir());
    }

    #[test]
    fn cleanup_stale_closes_non_recoverable() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let created = manager.create(session, repo.path()).expect("create");
        let worktree_id = created.worktree_id.clone();
        std::fs::remove_dir_all(&created.path).expect("rm path");
        let _ = git(repo.path(), &["worktree", "prune"]);

        let marked = manager.mark_stale().expect("mark");
        assert_eq!(marked.len(), 1);
        assert!(!marked[0].recoverable);

        let closed = manager.cleanup_stale(&worktree_id).expect("cleanup");
        assert_eq!(closed.state, WorktreeLifecycleState::Closed);
        assert!(!created.path.exists());
    }

    #[test]
    fn cleanup_refuses_recoverable() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let created = manager.create(session, repo.path()).expect("create");
        break_registration(&created);
        manager.mark_stale().expect("mark");

        let err = manager
            .cleanup_stale(&created.worktree_id)
            .expect_err("must refuse");
        assert!(matches!(err, WorktreeError::RecoverableNeedsSalvage(_)));
        assert!(created.path.is_dir());
    }

    #[test]
    fn salvage_restores_unregistered_with_files() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let created = manager.create(session, repo.path()).expect("create");
        let worktree_id = created.worktree_id.clone();
        std::fs::write(created.path.join("wip.rs"), b"fn main() {}").expect("wip");
        break_registration(&created);
        manager.mark_stale().expect("mark");

        let salvaged = manager.salvage(&worktree_id).expect("salvage");
        assert_eq!(salvaged.state, WorktreeLifecycleState::Active);
        assert_eq!(salvaged.worktree_id, worktree_id);
        assert!(salvaged.path.join("wip.rs").is_file());
        assert!(salvaged.path.join("README").is_file());
        assert!(manager.is_registered(&salvaged).expect("registered"));
        assert!(manager.detect_stale().expect("detect").is_empty());
    }

    #[test]
    fn healthy_binding_not_reported_stale() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        manager.create(session, repo.path()).expect("create");
        assert!(manager.detect_stale().expect("detect").is_empty());
        assert!(manager.mark_stale().expect("mark").is_empty());
    }

    #[test]
    fn identity_survives_compaction_structural_and_resume() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let created = manager.create(session, repo.path()).expect("create");
        let worktree_id = created.worktree_id.clone();

        // Simulated CompactionCompleted structural snapshot.
        let structural = crate::CompactionStructuralState {
            workspace_root: repo.path().to_path_buf(),
            parent_session_id: None,
            allow_network: false,
            allow_web_outbound: false,
            allow_private_network: false,
            allowed_hosts: vec![],
            turns_used: 3,
            tokens_used: 900,
            compaction_count: 1,
            worktree_id: Some(worktree_id.clone()),
        };

        let resolved = manager
            .resolve_after_compaction(session, &structural)
            .expect("resolve after compaction");
        assert_eq!(resolved.worktree_id, worktree_id);
        assert_eq!(resolved.session_id, session);
        assert_eq!(resolved.state, WorktreeLifecycleState::Active);

        let stopped = manager.stop(session).expect("stop");
        assert_eq!(stopped.worktree_id, worktree_id);

        let resumed = manager.resume(session).expect("resume");
        assert_eq!(resumed.worktree_id, worktree_id);

        let after_resume = manager
            .resolve_after_compaction(session, &structural)
            .expect("resolve after resume");
        assert_eq!(after_resume.worktree_id, worktree_id);
        assert_eq!(after_resume.state, WorktreeLifecycleState::Active);
    }

    #[test]
    fn resolve_after_compaction_rejects_foreign_session() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let created = manager.create(session, repo.path()).expect("create");
        let structural = crate::CompactionStructuralState {
            workspace_root: repo.path().to_path_buf(),
            parent_session_id: None,
            allow_network: false,
            allow_web_outbound: false,
            allow_private_network: false,
            allowed_hosts: vec![],
            turns_used: 0,
            tokens_used: 0,
            compaction_count: 1,
            worktree_id: Some(created.worktree_id.clone()),
        };
        let err = manager
            .resolve_after_compaction(Uuid::new_v4(), &structural)
            .expect_err("foreign session");
        assert!(matches!(err, WorktreeError::NotFoundId(_)));
    }

    #[test]
    fn build_role_creates_isolated_worktree_with_permissions() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let binding = manager
            .create_for_role(session, repo.path(), AgentWorkRole::Build)
            .expect("create build");

        assert_eq!(binding.state, WorktreeLifecycleState::Active);
        assert!(binding.path.is_dir());
        // Isolated: worktree path is not the repo root.
        assert_ne!(
            absolute_path(&binding.path).unwrap(),
            absolute_path(repo.path()).unwrap()
        );

        let perms = binding.permissions.as_ref().expect("permissions");
        assert_eq!(perms.role, AgentWorkRole::Build);
        assert_eq!(perms.workspace_root, binding.path);
        assert_eq!(perms.write_roots, vec![binding.path.clone()]);
        assert!(!perms.allow_network);

        let scope = WorktreeManager::attached_sandbox_scope(&binding).expect("scope");
        assert_eq!(scope.workspace_root, binding.path);
    }

    #[test]
    fn build_role_permissions_survive_reopen() {
        let store_dir = tempfile::tempdir().expect("store");
        let repo_dir = tempfile::tempdir().expect("repo");
        init_git_repo(repo_dir.path());
        let db = store_dir.path().join("worktrees.db");
        let worktrees = store_dir.path().join("worktrees");
        let session = Uuid::new_v4();
        let worktree_id;
        let path;

        {
            let manager = WorktreeManager::open(&db, &worktrees).expect("open");
            let created = manager
                .create_for_role(session, repo_dir.path(), AgentWorkRole::Build)
                .expect("create");
            worktree_id = created.worktree_id.clone();
            path = created.path.clone();
        }

        let reopened = WorktreeManager::open(&db, &worktrees).expect("reopen");
        let binding = reopened
            .get_by_worktree_id(&worktree_id)
            .expect("lookup")
            .expect("present");
        let perms = binding.permissions.as_ref().expect("permissions");
        assert_eq!(perms.role, AgentWorkRole::Build);
        assert_eq!(perms.workspace_root, path);
        assert_eq!(perms.write_roots, vec![path]);
    }

    #[test]
    fn enforce_write_admits_worktree_denies_outside() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let binding = manager
            .create_for_role(session, repo.path(), AgentWorkRole::Build)
            .expect("create build");

        let inside = binding.path.join("src/main.rs");
        WorktreeManager::enforce_write(&binding, &inside).expect("inside ok");

        let outside = repo.path().join("README");
        let err = WorktreeManager::enforce_write(&binding, &outside).expect_err("outside");
        assert!(matches!(err, WorktreeError::OutsidePermissions(_)));
    }

    #[test]
    fn plain_create_has_no_permissions_enforce_fails() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        let binding = manager.create(session, repo.path()).expect("create");
        assert!(binding.permissions.is_none());
        let err =
            WorktreeManager::enforce_write(&binding, &binding.path.join("x")).expect_err("missing");
        assert!(matches!(err, WorktreeError::MissingPermissions));
    }

    #[test]
    fn non_build_role_refuses_isolated_create() {
        let (_store, repo, manager, _) = temp_manager();
        let session = Uuid::new_v4();
        for role in [
            AgentWorkRole::Explore,
            AgentWorkRole::Research,
            AgentWorkRole::Review,
        ] {
            let err = manager
                .create_for_role(session, repo.path(), role)
                .expect_err("non-build");
            assert!(matches!(err, WorktreeError::RoleDoesNotPreferIsolated(_)));
            assert!(!role.prefers_isolated_worktree());
        }
        assert!(AgentWorkRole::Build.prefers_isolated_worktree());
    }
}
