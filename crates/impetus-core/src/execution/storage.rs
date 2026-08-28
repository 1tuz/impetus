//! Durable storage for PTY sessions using SQLite.

use super::{PtySession, PtySessionId, PtySessionState};
use crate::ActionOrigin;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PtySessionStoreError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("session not found: {0:?}")]
    NotFound(PtySessionId),
    #[error("invalid state encoding: {0}")]
    InvalidState(String),
    #[error("invalid origin encoding: {0}")]
    InvalidOrigin(String),
}

#[derive(Debug, Clone)]
pub struct PtySessionRecord {
    pub id: PtySessionId,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    pub env: Vec<(String, String)>,
    pub state: PtySessionState,
    pub origin: ActionOrigin,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

impl From<PtySession> for PtySessionRecord {
    fn from(session: PtySession) -> Self {
        Self {
            id: session.id,
            command: session.command,
            args: session.args,
            working_dir: session.working_dir,
            env: session.env,
            state: session.state,
            origin: session.origin,
            created_at_unix_ms: session.created_at_unix_ms,
            updated_at_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time after epoch")
                .as_millis() as u64,
        }
    }
}

impl From<PtySessionRecord> for PtySession {
    fn from(record: PtySessionRecord) -> Self {
        Self {
            id: record.id,
            command: record.command,
            args: record.args,
            working_dir: record.working_dir,
            env: record.env,
            state: record.state,
            origin: record.origin,
            created_at_unix_ms: record.created_at_unix_ms,
        }
    }
}

/// Trait for PTY session storage.
#[async_trait::async_trait]
pub trait PtySessionStore: Send + Sync {
    async fn save_session(&self, session: &PtySessionRecord) -> Result<(), PtySessionStoreError>;
    async fn get_session(
        &self,
        id: PtySessionId,
    ) -> Result<Option<PtySessionRecord>, PtySessionStoreError>;
    async fn list_sessions(&self) -> Result<Vec<PtySessionRecord>, PtySessionStoreError>;
    async fn update_state(
        &self,
        id: PtySessionId,
        state: &PtySessionState,
    ) -> Result<(), PtySessionStoreError>;
    async fn delete_session(&self, id: PtySessionId) -> Result<(), PtySessionStoreError>;
}

/// SQLite implementation of PtySessionStore.
pub struct SqlitePtySessionStore {
    db_path: PathBuf,
}

impl SqlitePtySessionStore {
    pub fn new(db_path: impl AsRef<Path>) -> Result<Self, PtySessionStoreError> {
        let db_path = db_path.as_ref().to_path_buf();
        let conn = Connection::open(&db_path)?;

        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS pty_sessions (
                id INTEGER PRIMARY KEY,
                command TEXT NOT NULL,
                args TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                env TEXT NOT NULL,
                state_type TEXT NOT NULL,
                state_data TEXT,
                origin TEXT NOT NULL,
                created_at_unix_ms INTEGER NOT NULL,
                updated_at_unix_ms INTEGER NOT NULL
            )
            "#,
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_pty_sessions_state ON pty_sessions(state_type)",
            [],
        )?;

        Ok(Self { db_path })
    }

    fn connection(&self) -> Result<Connection, PtySessionStoreError> {
        Ok(Connection::open(&self.db_path)?)
    }

    fn encode_state(state: &PtySessionState) -> (String, Option<String>) {
        match state {
            PtySessionState::Starting => ("starting".into(), None),
            PtySessionState::Running { pid } => ("running".into(), Some(pid.to_string())),
            PtySessionState::Detached { pid } => ("detached".into(), Some(pid.to_string())),
            PtySessionState::Exited { exit_code } => (
                "exited".into(),
                Some(exit_code.map(|c| c.to_string()).unwrap_or_default()),
            ),
            PtySessionState::Failed { reason } => ("failed".into(), Some(reason.clone())),
        }
    }

    fn decode_state(
        state_type: &str,
        state_data: Option<&str>,
    ) -> Result<PtySessionState, PtySessionStoreError> {
        match state_type {
            "starting" => Ok(PtySessionState::Starting),
            "running" => {
                let pid = state_data
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| PtySessionStoreError::InvalidState("missing pid".into()))?;
                Ok(PtySessionState::Running { pid })
            }
            "detached" => {
                let pid = state_data
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| PtySessionStoreError::InvalidState("missing pid".into()))?;
                Ok(PtySessionState::Detached { pid })
            }
            "exited" => {
                let exit_code =
                    state_data.and_then(|s| if s.is_empty() { None } else { s.parse().ok() });
                Ok(PtySessionState::Exited { exit_code })
            }
            "failed" => {
                let reason = state_data
                    .ok_or_else(|| PtySessionStoreError::InvalidState("missing reason".into()))?;
                Ok(PtySessionState::Failed {
                    reason: reason.into(),
                })
            }
            _ => Err(PtySessionStoreError::InvalidState(format!(
                "unknown state type: {}",
                state_type
            ))),
        }
    }

    fn encode_origin(origin: &ActionOrigin) -> &'static str {
        match origin {
            ActionOrigin::User => "user",
            ActionOrigin::Agent => "agent",
        }
    }

    fn decode_origin(origin: &str) -> Result<ActionOrigin, PtySessionStoreError> {
        match origin {
            "user" => Ok(ActionOrigin::User),
            "agent" => Ok(ActionOrigin::Agent),
            _ => Err(PtySessionStoreError::InvalidOrigin(format!(
                "unknown origin: {}",
                origin
            ))),
        }
    }
}

#[async_trait::async_trait]
impl PtySessionStore for SqlitePtySessionStore {
    async fn save_session(&self, session: &PtySessionRecord) -> Result<(), PtySessionStoreError> {
        let conn = self.connection()?;
        let (state_type, state_data) = Self::encode_state(&session.state);
        let origin = Self::encode_origin(&session.origin);

        conn.execute(
            r#"
            INSERT INTO pty_sessions 
            (id, command, args, working_dir, env, state_type, state_data, origin, created_at_unix_ms, updated_at_unix_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(id) DO UPDATE SET
                state_type = ?6,
                state_data = ?7,
                updated_at_unix_ms = ?10
            "#,
            params![
                session.id.0,
                session.command,
                serde_json::to_string(&session.args).unwrap(),
                session.working_dir.display().to_string(),
                serde_json::to_string(&session.env).unwrap(),
                state_type,
                state_data,
                origin,
                session.created_at_unix_ms,
                session.updated_at_unix_ms,
            ],
        )?;

        Ok(())
    }

    async fn get_session(
        &self,
        id: PtySessionId,
    ) -> Result<Option<PtySessionRecord>, PtySessionStoreError> {
        let conn = self.connection()?;

        let result = conn
            .query_row(
                r#"
                SELECT id, command, args, working_dir, env, state_type, state_data, origin, created_at_unix_ms, updated_at_unix_ms
                FROM pty_sessions
                WHERE id = ?1
                "#,
                params![id.0],
                |row| {
                    let state_type: String = row.get(5)?;
                    let state_data: Option<String> = row.get(6)?;
                    let origin_str: String = row.get(7)?;

                    Ok((
                        PtySessionId(row.get(0)?),
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        state_type,
                        state_data,
                        origin_str,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .optional()?;

        match result {
            None => Ok(None),
            Some((
                id,
                command,
                args_json,
                working_dir,
                env_json,
                state_type,
                state_data,
                origin_str,
                created_at,
                updated_at,
            )) => {
                let args: Vec<String> = serde_json::from_str(&args_json).unwrap_or_default();
                let env: Vec<(String, String)> =
                    serde_json::from_str(&env_json).unwrap_or_default();
                let state = Self::decode_state(&state_type, state_data.as_deref())?;
                let origin = Self::decode_origin(&origin_str)?;

                Ok(Some(PtySessionRecord {
                    id,
                    command,
                    args,
                    working_dir: PathBuf::from(working_dir),
                    env,
                    state,
                    origin,
                    created_at_unix_ms: created_at,
                    updated_at_unix_ms: updated_at,
                }))
            }
        }
    }

    async fn list_sessions(&self) -> Result<Vec<PtySessionRecord>, PtySessionStoreError> {
        let conn = self.connection()?;

        let mut stmt = conn.prepare(
            r#"
            SELECT id, command, args, working_dir, env, state_type, state_data, origin, created_at_unix_ms, updated_at_unix_ms
            FROM pty_sessions
            ORDER BY created_at_unix_ms DESC
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            let state_type: String = row.get(5)?;
            let state_data: Option<String> = row.get(6)?;
            let origin_str: String = row.get(7)?;

            Ok((
                PtySessionId(row.get(0)?),
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                state_type,
                state_data,
                origin_str,
                row.get(8)?,
                row.get(9)?,
            ))
        })?;

        let mut sessions = Vec::new();
        for row_result in rows {
            let (
                id,
                command,
                args_json,
                working_dir,
                env_json,
                state_type,
                state_data,
                origin_str,
                created_at,
                updated_at,
            ) = row_result?;
            let args: Vec<String> = serde_json::from_str(&args_json).unwrap_or_default();
            let env: Vec<(String, String)> = serde_json::from_str(&env_json).unwrap_or_default();
            let state = Self::decode_state(&state_type, state_data.as_deref())?;
            let origin = Self::decode_origin(&origin_str)?;

            sessions.push(PtySessionRecord {
                id,
                command,
                args,
                working_dir: PathBuf::from(working_dir),
                env,
                state,
                origin,
                created_at_unix_ms: created_at,
                updated_at_unix_ms: updated_at,
            });
        }

        Ok(sessions)
    }

    async fn update_state(
        &self,
        id: PtySessionId,
        state: &PtySessionState,
    ) -> Result<(), PtySessionStoreError> {
        let conn = self.connection()?;
        let (state_type, state_data) = Self::encode_state(state);
        let updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_millis() as u64;

        let rows_affected = conn.execute(
            "UPDATE pty_sessions SET state_type = ?1, state_data = ?2, updated_at_unix_ms = ?3 WHERE id = ?4",
            params![state_type, state_data, updated_at, id.0],
        )?;

        if rows_affected == 0 {
            return Err(PtySessionStoreError::NotFound(id));
        }

        Ok(())
    }

    async fn delete_session(&self, id: PtySessionId) -> Result<(), PtySessionStoreError> {
        let conn = self.connection()?;

        let rows_affected =
            conn.execute("DELETE FROM pty_sessions WHERE id = ?1", params![id.0])?;

        if rows_affected == 0 {
            return Err(PtySessionStoreError::NotFound(id));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActionOrigin;

    fn temp_db() -> SqlitePtySessionStore {
        let temp_file =
            std::env::temp_dir().join(format!("pty_sessions_test_{}.db", std::process::id()));
        SqlitePtySessionStore::new(&temp_file).unwrap()
    }

    #[tokio::test]
    async fn save_and_retrieve_session() {
        let store = temp_db();
        let session = PtySessionRecord {
            id: PtySessionId(1),
            command: "bash".into(),
            args: vec!["-c".into(), "echo test".into()],
            working_dir: PathBuf::from("/tmp"),
            env: vec![("VAR".into(), "value".into())],
            state: PtySessionState::Running { pid: 12345 },
            origin: ActionOrigin::User,
            created_at_unix_ms: 1000,
            updated_at_unix_ms: 1000,
        };

        store.save_session(&session).await.unwrap();

        let retrieved = store.get_session(PtySessionId(1)).await.unwrap();
        assert!(retrieved.is_some());

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.command, "bash");
        assert_eq!(retrieved.args, vec!["-c", "echo test"]);
        assert!(matches!(
            retrieved.state,
            PtySessionState::Running { pid: 12345 }
        ));
    }

    #[tokio::test]
    async fn update_session_state() {
        let store = temp_db();
        let session = PtySessionRecord {
            id: PtySessionId(2),
            command: "bash".into(),
            args: vec![],
            working_dir: PathBuf::from("/tmp"),
            env: vec![],
            state: PtySessionState::Starting,
            origin: ActionOrigin::Agent,
            created_at_unix_ms: 2000,
            updated_at_unix_ms: 2000,
        };

        store.save_session(&session).await.unwrap();

        store
            .update_state(PtySessionId(2), &PtySessionState::Running { pid: 99999 })
            .await
            .unwrap();

        let retrieved = store.get_session(PtySessionId(2)).await.unwrap().unwrap();
        assert!(matches!(
            retrieved.state,
            PtySessionState::Running { pid: 99999 }
        ));
    }

    #[tokio::test]
    async fn list_sessions_ordered() {
        let store = temp_db();

        for i in 1..=3 {
            let session = PtySessionRecord {
                id: PtySessionId(i + 100),
                command: format!("cmd{}", i),
                args: vec![],
                working_dir: PathBuf::from("/tmp"),
                env: vec![],
                state: PtySessionState::Starting,
                origin: ActionOrigin::User,
                created_at_unix_ms: i * 1000,
                updated_at_unix_ms: i * 1000,
            };
            store.save_session(&session).await.unwrap();
        }

        let sessions = store.list_sessions().await.unwrap();
        assert!(sessions.len() >= 3);

        // Descending order by created_at
        assert!(sessions[0].created_at_unix_ms >= sessions[1].created_at_unix_ms);
    }

    #[tokio::test]
    async fn delete_session() {
        let store = temp_db();
        let session = PtySessionRecord {
            id: PtySessionId(200),
            command: "rm".into(),
            args: vec![],
            working_dir: PathBuf::from("/tmp"),
            env: vec![],
            state: PtySessionState::Exited { exit_code: Some(0) },
            origin: ActionOrigin::User,
            created_at_unix_ms: 5000,
            updated_at_unix_ms: 5000,
        };

        store.save_session(&session).await.unwrap();
        store.delete_session(PtySessionId(200)).await.unwrap();

        let retrieved = store.get_session(PtySessionId(200)).await.unwrap();
        assert!(retrieved.is_none());
    }
}
