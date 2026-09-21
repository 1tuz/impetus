//! Managed git worktree lifecycle with durable session binding.
//!
//! Vertical slice (TODO P1 §5): create → resume → stop → close.
//! Diff / merge-ready / stale / salvage come later.
//!
//! Uses the system `git` CLI (no new git dependency). Bindings survive daemon
//! restart via SQLite; `worktree_id` identity is retained through stop/close.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

/// Lifecycle state for a managed worktree binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeLifecycleState {
    Active,
    Stopped,
    Closed,
}

impl WorktreeLifecycleState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Stopped => "stopped",
            Self::Closed => "closed",
        }
    }

    fn parse(raw: &str) -> Result<Self, WorktreeError> {
        match raw {
            "active" => Ok(Self::Active),
            "stopped" => Ok(Self::Stopped),
            "closed" => Ok(Self::Closed),
            other => Err(WorktreeError::CorruptState(other.to_string())),
        }
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
    #[error("invalid lifecycle transition from {from:?} via {op}")]
    InvalidTransition {
        from: WorktreeLifecycleState,
        op: &'static str,
    },
    #[error("worktree path missing on disk: {0}")]
    PathMissing(String),
    #[error("corrupt binding state: {0}")]
    CorruptState(String),
}

/// Creates, stops, resumes, and closes git worktrees bound to session ids.
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
                updated_unix_ms INTEGER NOT NULL
            )",
            [],
        )?;
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

    /// Create a new git worktree bound to `session_id`.
    ///
    /// Fails if the session already has a non-closed binding.
    pub fn create(
        &self,
        session_id: Uuid,
        repo_root: &Path,
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

        let now = now_unix_ms();
        let binding = WorktreeBinding {
            worktree_id: worktree_id.clone(),
            session_id,
            path: path.clone(),
            branch: branch.clone(),
            repo_root: repo_root.clone(),
            state: WorktreeLifecycleState::Active,
        };

        {
            let conn = self.conn.lock().expect("worktree db lock");
            conn.execute(
                "INSERT INTO worktree_bindings
                    (worktree_id, session_id, path, branch, repo_root, state,
                     created_unix_ms, updated_unix_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    &binding.worktree_id,
                    session_id.to_string(),
                    path.to_string_lossy().as_ref(),
                    &branch,
                    repo_root.to_string_lossy().as_ref(),
                    WorktreeLifecycleState::Active.as_str(),
                    now as i64,
                    now as i64,
                ],
            )?;
        }

        Ok(binding)
    }

    /// Resume a stopped (or still-active after restart) binding.
    ///
    /// Verifies the worktree path still exists and sets state to Active.
    pub fn resume(&self, session_id: Uuid) -> Result<WorktreeBinding, WorktreeError> {
        let binding = self.require_open(session_id)?;
        match binding.state {
            WorktreeLifecycleState::Active | WorktreeLifecycleState::Stopped => {}
            WorktreeLifecycleState::Closed => {
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
    /// `worktree_id` is retained so callers can still resolve identity after close.
    pub fn close(&self, session_id: Uuid) -> Result<WorktreeBinding, WorktreeError> {
        let binding = self.require_open(session_id)?;
        if binding.path.exists() {
            let path_str = binding.path.to_string_lossy();
            git(
                &binding.repo_root,
                &["worktree", "remove", "--force", path_str.as_ref()],
            )?;
        }
        self.set_state(&binding.worktree_id, WorktreeLifecycleState::Closed)?;
        self.get_by_worktree_id(&binding.worktree_id)?
            .ok_or_else(|| WorktreeError::NotFound(session_id))
    }

    /// Lookup any binding (including Closed) for a session — newest by update time.
    pub fn get_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<WorktreeBinding>, WorktreeError> {
        let conn = self.conn.lock().expect("worktree db lock");
        let row = conn
            .query_row(
                "SELECT worktree_id, session_id, path, branch, repo_root, state
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
                "SELECT worktree_id, session_id, path, branch, repo_root, state
                 FROM worktree_bindings WHERE worktree_id = ?1",
                params![worktree_id],
                row_to_binding,
            )
            .optional()?;
        Ok(row)
    }

    fn get_open_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<WorktreeBinding>, WorktreeError> {
        let conn = self.conn.lock().expect("worktree db lock");
        let row = conn
            .query_row(
                "SELECT worktree_id, session_id, path, branch, repo_root, state
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
    Ok(WorktreeBinding {
        worktree_id: row.get(0)?,
        session_id,
        path: PathBuf::from(row.get::<_, String>(2)?),
        branch: row.get(3)?,
        repo_root: PathBuf::from(row.get::<_, String>(4)?),
        state,
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
}
