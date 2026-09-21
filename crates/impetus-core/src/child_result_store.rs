//! Durable child-run results for parent resume (TODO P1 §7).
//!
//! Vertical slice: SQLite store keyed by `child_id` + `parent_id`, plus a
//! parent-resume gate stub that refuses resume when required results are
//! missing. Labels / status only — never secrets, tokens, or raw tool payloads.
//! Does **not** spawn children, cap concurrency, or wire AgentScheduler.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thiserror::Error;

use crate::subagent_metadata::{ChildRunMetadata, SubagentRole};

/// Outcome label for a finished child run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildResultStatus {
    Completed,
    Failed,
    Cancelled,
}

impl ChildResultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Durable child result — reference labels only, no secret material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildResult {
    pub child_id: String,
    pub parent_id: String,
    /// Role label (`SubagentRole::as_str`), not a free-form secret.
    pub role_label: String,
    pub status: ChildResultStatus,
    /// Short human/status label (e.g. "tests-green"); never a token/key.
    pub summary_label: String,
    /// Optional durable artifact id labels (SHA refs / store keys), not bodies.
    pub artifact_ref_labels: Vec<String>,
    pub recorded_unix_ms: u64,
}

impl ChildResult {
    /// Build a result from validated child metadata + outcome labels.
    pub fn from_metadata(
        child_id: impl Into<String>,
        metadata: &ChildRunMetadata,
        status: ChildResultStatus,
        summary_label: impl Into<String>,
    ) -> Self {
        Self {
            child_id: child_id.into(),
            parent_id: metadata.parent_id.clone(),
            role_label: metadata.role.as_str().to_string(),
            status,
            summary_label: summary_label.into(),
            artifact_ref_labels: Vec::new(),
            recorded_unix_ms: 0,
        }
    }

    /// Role parsed from stored label when it matches a known [`SubagentRole`].
    pub fn role(&self) -> Option<SubagentRole> {
        match self.role_label.as_str() {
            "Explore" => Some(SubagentRole::Explore),
            "Research" => Some(SubagentRole::Research),
            "Build" => Some(SubagentRole::Build),
            "Review" => Some(SubagentRole::Review),
            _ => None,
        }
    }
}

/// Store / gate failures.
#[derive(Debug, Error)]
pub enum ChildResultError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("child_id must be non-empty")]
    EmptyChildId,
    #[error("parent_id must be non-empty")]
    EmptyParentId,
    #[error("role_label must be non-empty")]
    EmptyRoleLabel,
    #[error("summary_label must be non-empty")]
    EmptySummaryLabel,
    #[error("unknown child result status: {0}")]
    UnknownStatus(String),
    #[error("invalid artifact_ref_labels JSON: {0}")]
    InvalidArtifactLabels(String),
    #[error("parent {parent_id} missing required child results: {missing:?}")]
    MissingChildResults {
        parent_id: String,
        missing: Vec<String>,
    },
}

/// SQLite-backed durable child result store.
pub struct ChildResultStore {
    conn: Arc<Mutex<Connection>>,
}

impl ChildResultStore {
    /// Open or create the child-result database at `db_path`.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, ChildResultError> {
        let db_path = db_path.as_ref();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(db_path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS child_results (
                child_id TEXT PRIMARY KEY NOT NULL,
                parent_id TEXT NOT NULL,
                role_label TEXT NOT NULL,
                status TEXT NOT NULL,
                summary_label TEXT NOT NULL,
                artifact_ref_labels_json TEXT NOT NULL,
                recorded_unix_ms INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_child_results_parent
             ON child_results(parent_id)",
            [],
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Persist (or replace) a finished child result. Labels only.
    pub fn record_result(&self, result: &ChildResult) -> Result<(), ChildResultError> {
        validate_result(result)?;
        let recorded_unix_ms = if result.recorded_unix_ms == 0 {
            now_unix_ms()
        } else {
            result.recorded_unix_ms
        };
        let artifact_json = serde_json::to_string(&result.artifact_ref_labels)
            .map_err(|err| ChildResultError::InvalidArtifactLabels(err.to_string()))?;

        let conn = self.conn.lock().expect("child result db lock");
        conn.execute(
            "INSERT INTO child_results
                (child_id, parent_id, role_label, status, summary_label,
                 artifact_ref_labels_json, recorded_unix_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(child_id) DO UPDATE SET
                parent_id = excluded.parent_id,
                role_label = excluded.role_label,
                status = excluded.status,
                summary_label = excluded.summary_label,
                artifact_ref_labels_json = excluded.artifact_ref_labels_json,
                recorded_unix_ms = excluded.recorded_unix_ms",
            params![
                &result.child_id,
                &result.parent_id,
                &result.role_label,
                result.status.as_str(),
                &result.summary_label,
                artifact_json,
                recorded_unix_ms as i64,
            ],
        )?;
        Ok(())
    }

    /// Load a single child result by child id.
    pub fn load_result(&self, child_id: &str) -> Result<Option<ChildResult>, ChildResultError> {
        if child_id.trim().is_empty() {
            return Err(ChildResultError::EmptyChildId);
        }
        let conn = self.conn.lock().expect("child result db lock");
        let row = conn
            .query_row(
                "SELECT child_id, parent_id, role_label, status, summary_label,
                        artifact_ref_labels_json, recorded_unix_ms
                 FROM child_results WHERE child_id = ?1",
                params![child_id],
                row_to_result,
            )
            .optional()?;
        match row {
            Some(Ok(result)) => Ok(Some(result)),
            Some(Err(err)) => Err(err),
            None => Ok(None),
        }
    }

    /// List all stored results for a parent, ordered by child_id.
    pub fn list_by_parent(&self, parent_id: &str) -> Result<Vec<ChildResult>, ChildResultError> {
        if parent_id.trim().is_empty() {
            return Err(ChildResultError::EmptyParentId);
        }
        let conn = self.conn.lock().expect("child result db lock");
        let mut stmt = conn.prepare(
            "SELECT child_id, parent_id, role_label, status, summary_label,
                    artifact_ref_labels_json, recorded_unix_ms
             FROM child_results WHERE parent_id = ?1
             ORDER BY child_id",
        )?;
        let rows = stmt.query_map(params![parent_id], row_to_result)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    /// Parent-resume gate stub: refuse if any required child result is missing
    /// or belongs to a different parent.
    pub fn gate_parent_resume(
        &self,
        parent_id: &str,
        required_child_ids: &[&str],
    ) -> Result<(), ChildResultError> {
        if parent_id.trim().is_empty() {
            return Err(ChildResultError::EmptyParentId);
        }
        let mut missing = Vec::new();
        for child_id in required_child_ids {
            if child_id.trim().is_empty() {
                return Err(ChildResultError::EmptyChildId);
            }
            match self.load_result(child_id)? {
                Some(result) if result.parent_id == parent_id => {}
                Some(_) | None => missing.push((*child_id).to_string()),
            }
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(ChildResultError::MissingChildResults {
                parent_id: parent_id.to_string(),
                missing,
            })
        }
    }
}

fn validate_result(result: &ChildResult) -> Result<(), ChildResultError> {
    if result.child_id.trim().is_empty() {
        return Err(ChildResultError::EmptyChildId);
    }
    if result.parent_id.trim().is_empty() {
        return Err(ChildResultError::EmptyParentId);
    }
    if result.role_label.trim().is_empty() {
        return Err(ChildResultError::EmptyRoleLabel);
    }
    if result.summary_label.trim().is_empty() {
        return Err(ChildResultError::EmptySummaryLabel);
    }
    Ok(())
}

fn row_to_result(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<ChildResult, ChildResultError>> {
    let status_raw: String = row.get(3)?;
    let Some(status) = ChildResultStatus::parse(&status_raw) else {
        return Ok(Err(ChildResultError::UnknownStatus(status_raw)));
    };
    let artifact_json: String = row.get(5)?;
    let artifact_ref_labels: Vec<String> = match serde_json::from_str(&artifact_json) {
        Ok(labels) => labels,
        Err(err) => {
            return Ok(Err(ChildResultError::InvalidArtifactLabels(
                err.to_string(),
            )));
        }
    };
    Ok(Ok(ChildResult {
        child_id: row.get(0)?,
        parent_id: row.get(1)?,
        role_label: row.get(2)?,
        status,
        summary_label: row.get(4)?,
        artifact_ref_labels,
        recorded_unix_ms: row.get::<_, i64>(6)? as u64,
    }))
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
    use std::path::PathBuf;

    fn temp_store() -> (tempfile::TempDir, ChildResultStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ChildResultStore::open(dir.path().join("child_results.db")).expect("open");
        (dir, store)
    }

    fn explore_meta(parent_id: &str) -> ChildRunMetadata {
        ChildRunMetadata {
            parent_id: parent_id.into(),
            cwd: PathBuf::from("/tmp/ws"),
            worktree: None,
            allowed_tools: vec!["read".into()],
            write_roots: vec![],
            max_tokens: 1_000,
            max_time: 60_000,
            max_depth: 1,
            role: SubagentRole::Explore,
        }
        .try_validated()
        .expect("valid explore metadata")
    }

    #[test]
    fn record_load_and_list_by_parent() {
        let (_dir, store) = temp_store();
        let meta = explore_meta("parent-a");
        let mut a =
            ChildResult::from_metadata("child-1", &meta, ChildResultStatus::Completed, "ok");
        a.artifact_ref_labels = vec!["artifact:sha256:abc".into()];
        let b = ChildResult::from_metadata(
            "child-2",
            &meta,
            ChildResultStatus::Failed,
            "timeout-label",
        );

        store.record_result(&a).expect("record a");
        store.record_result(&b).expect("record b");

        let loaded = store
            .load_result("child-1")
            .expect("load")
            .expect("present");
        assert_eq!(loaded.child_id, "child-1");
        assert_eq!(loaded.parent_id, "parent-a");
        assert_eq!(loaded.role_label, "Explore");
        assert_eq!(loaded.role(), Some(SubagentRole::Explore));
        assert_eq!(loaded.status, ChildResultStatus::Completed);
        assert_eq!(loaded.summary_label, "ok");
        assert_eq!(loaded.artifact_ref_labels, vec!["artifact:sha256:abc"]);
        assert!(loaded.recorded_unix_ms > 0);

        let listed = store.list_by_parent("parent-a").expect("list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].child_id, "child-1");
        assert_eq!(listed[1].child_id, "child-2");
        assert!(store.list_by_parent("other").expect("empty").is_empty());
    }

    #[test]
    fn record_result_replaces_same_child_id() {
        let (_dir, store) = temp_store();
        let meta = explore_meta("parent-a");
        store
            .record_result(&ChildResult::from_metadata(
                "child-1",
                &meta,
                ChildResultStatus::Failed,
                "first",
            ))
            .expect("first");
        store
            .record_result(&ChildResult::from_metadata(
                "child-1",
                &meta,
                ChildResultStatus::Completed,
                "retry-ok",
            ))
            .expect("replace");

        let loaded = store
            .load_result("child-1")
            .expect("load")
            .expect("present");
        assert_eq!(loaded.status, ChildResultStatus::Completed);
        assert_eq!(loaded.summary_label, "retry-ok");
        assert_eq!(store.list_by_parent("parent-a").expect("list").len(), 1);
    }

    #[test]
    fn gate_parent_resume_refuses_missing_results() {
        let (_dir, store) = temp_store();
        let meta = explore_meta("parent-a");
        store
            .record_result(&ChildResult::from_metadata(
                "child-1",
                &meta,
                ChildResultStatus::Completed,
                "ok",
            ))
            .expect("record");

        store
            .gate_parent_resume("parent-a", &["child-1"])
            .expect("required present");

        let err = store
            .gate_parent_resume("parent-a", &["child-1", "child-2"])
            .expect_err("missing child-2");
        match err {
            ChildResultError::MissingChildResults { parent_id, missing } => {
                assert_eq!(parent_id, "parent-a");
                assert_eq!(missing, vec!["child-2".to_string()]);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn gate_parent_resume_refuses_wrong_parent() {
        let (_dir, store) = temp_store();
        let meta = explore_meta("parent-a");
        store
            .record_result(&ChildResult::from_metadata(
                "child-1",
                &meta,
                ChildResultStatus::Completed,
                "ok",
            ))
            .expect("record");

        let err = store
            .gate_parent_resume("parent-b", &["child-1"])
            .expect_err("wrong parent");
        assert!(matches!(err, ChildResultError::MissingChildResults { .. }));
    }

    #[test]
    fn rejects_empty_ids_and_labels() {
        let (_dir, store) = temp_store();
        let meta = explore_meta("parent-a");
        let mut bad =
            ChildResult::from_metadata("child-1", &meta, ChildResultStatus::Completed, "ok");
        bad.child_id = "  ".into();
        assert!(matches!(
            store.record_result(&bad),
            Err(ChildResultError::EmptyChildId)
        ));

        assert!(matches!(
            store.load_result("  "),
            Err(ChildResultError::EmptyChildId)
        ));
        assert!(matches!(
            store.list_by_parent(""),
            Err(ChildResultError::EmptyParentId)
        ));
    }
}
