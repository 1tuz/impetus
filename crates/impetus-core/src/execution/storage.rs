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

    #[tokio::test]
    async fn get_nonexistent_session_returns_none() {
        let store = temp_db();
        let result = store.get_session(PtySessionId(999999)).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn update_nonexistent_session_returns_error() {
        let store = temp_db();
        let result = store
            .update_state(PtySessionId(888888), &PtySessionState::Starting)
            .await;
        assert!(matches!(result, Err(PtySessionStoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn delete_nonexistent_session_returns_error() {
        let store = temp_db();
        let result = store.delete_session(PtySessionId(777777)).await;
        assert!(matches!(result, Err(PtySessionStoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn save_session_upserts_existing() {
        let store = temp_db();
        let session = PtySessionRecord {
            id: PtySessionId(300),
            command: "echo".into(),
            args: vec!["first".into()],
            working_dir: PathBuf::from("/tmp"),
            env: vec![],
            state: PtySessionState::Starting,
            origin: ActionOrigin::User,
            created_at_unix_ms: 1000,
            updated_at_unix_ms: 1000,
        };
        store.save_session(&session).await.unwrap();

        let updated = PtySessionRecord {
            id: PtySessionId(300),
            command: "echo".into(),
            args: vec!["second".into()],
            working_dir: PathBuf::from("/tmp"),
            env: vec![],
            state: PtySessionState::Running { pid: 123 },
            origin: ActionOrigin::User,
            created_at_unix_ms: 1000,
            updated_at_unix_ms: 2000,
        };
        store.save_session(&updated).await.unwrap();

        let retrieved = store.get_session(PtySessionId(300)).await.unwrap().unwrap();
        assert!(matches!(
            retrieved.state,
            PtySessionState::Running { pid: 123 }
        ));
        assert_eq!(retrieved.updated_at_unix_ms, 2000);
    }

    #[tokio::test]
    async fn all_state_variants_round_trip() {
        let store = temp_db();
        let states = [
            PtySessionState::Starting,
            PtySessionState::Running { pid: 111 },
            PtySessionState::Detached { pid: 222 },
            PtySessionState::Exited { exit_code: Some(0) },
            PtySessionState::Exited { exit_code: None },
            PtySessionState::Failed {
                reason: "crash".into(),
            },
        ];

        for (i, state) in states.iter().enumerate() {
            let session = PtySessionRecord {
                id: PtySessionId(400 + i as u64),
                command: "test".into(),
                args: vec![],
                working_dir: PathBuf::from("/tmp"),
                env: vec![],
                state: state.clone(),
                origin: ActionOrigin::Agent,
                created_at_unix_ms: 1000,
                updated_at_unix_ms: 1000,
            };
            store.save_session(&session).await.unwrap();

            let retrieved = store
                .get_session(PtySessionId(400 + i as u64))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retrieved.state, *state);
        }
    }

    #[tokio::test]
    async fn both_origins_round_trip() {
        let store = temp_db();
        for (i, origin) in [ActionOrigin::User, ActionOrigin::Agent].iter().enumerate() {
            let session = PtySessionRecord {
                id: PtySessionId(500 + i as u64),
                command: "test".into(),
                args: vec![],
                working_dir: PathBuf::from("/tmp"),
                env: vec![],
                state: PtySessionState::Starting,
                origin: *origin,
                created_at_unix_ms: 1000,
                updated_at_unix_ms: 1000,
            };
            store.save_session(&session).await.unwrap();

            let retrieved = store
                .get_session(PtySessionId(500 + i as u64))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retrieved.origin, *origin);
        }
    }

    #[test]
    fn decode_invalid_state_type_returns_error() {
        let result = SqlitePtySessionStore::decode_state("invalid_state", None);
        assert!(matches!(result, Err(PtySessionStoreError::InvalidState(_))));
    }

    #[test]
    fn decode_running_without_pid_returns_error() {
        let result = SqlitePtySessionStore::decode_state("running", None);
        assert!(matches!(result, Err(PtySessionStoreError::InvalidState(_))));
    }

    #[test]
    fn decode_detached_without_pid_returns_error() {
        let result = SqlitePtySessionStore::decode_state("detached", None);
        assert!(matches!(result, Err(PtySessionStoreError::InvalidState(_))));
    }

    #[test]
    fn decode_failed_without_reason_returns_error() {
        let result = SqlitePtySessionStore::decode_state("failed", None);
        assert!(matches!(result, Err(PtySessionStoreError::InvalidState(_))));
    }

    #[test]
    fn decode_invalid_origin_returns_error() {
        let result = SqlitePtySessionStore::decode_origin("unknown_origin");
        assert!(matches!(
            result,
            Err(PtySessionStoreError::InvalidOrigin(_))
        ));
    }

    #[test]
    fn encode_decode_state_starting() {
        let state = PtySessionState::Starting;
        let (state_type, state_data) = SqlitePtySessionStore::encode_state(&state);
        let decoded =
            SqlitePtySessionStore::decode_state(&state_type, state_data.as_deref()).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn encode_decode_state_running() {
        let state = PtySessionState::Running { pid: 123 };
        let (state_type, state_data) = SqlitePtySessionStore::encode_state(&state);
        let decoded =
            SqlitePtySessionStore::decode_state(&state_type, state_data.as_deref()).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn encode_decode_state_detached() {
        let state = PtySessionState::Detached { pid: 456 };
        let (state_type, state_data) = SqlitePtySessionStore::encode_state(&state);
        let decoded =
            SqlitePtySessionStore::decode_state(&state_type, state_data.as_deref()).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn encode_decode_state_exited_with_code() {
        let state = PtySessionState::Exited { exit_code: Some(1) };
        let (state_type, state_data) = SqlitePtySessionStore::encode_state(&state);
        let decoded =
            SqlitePtySessionStore::decode_state(&state_type, state_data.as_deref()).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn encode_decode_state_exited_without_code() {
        let state = PtySessionState::Exited { exit_code: None };
        let (state_type, state_data) = SqlitePtySessionStore::encode_state(&state);
        let decoded =
            SqlitePtySessionStore::decode_state(&state_type, state_data.as_deref()).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn encode_decode_state_failed() {
        let state = PtySessionState::Failed {
            reason: "out of memory".into(),
        };
        let (state_type, state_data) = SqlitePtySessionStore::encode_state(&state);
        let decoded =
            SqlitePtySessionStore::decode_state(&state_type, state_data.as_deref()).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn encode_decode_origin_user() {
        let origin = ActionOrigin::User;
        let encoded = SqlitePtySessionStore::encode_origin(&origin);
        let decoded = SqlitePtySessionStore::decode_origin(encoded).unwrap();
        assert_eq!(decoded, origin);
    }

    #[test]
    fn encode_decode_origin_agent() {
        let origin = ActionOrigin::Agent;
        let encoded = SqlitePtySessionStore::encode_origin(&origin);
        let decoded = SqlitePtySessionStore::decode_origin(encoded).unwrap();
        assert_eq!(decoded, origin);
    }

    #[test]
    fn pty_session_record_from_pty_session() {
        let session = PtySession {
            id: PtySessionId(1),
            command: "ls".into(),
            args: vec!["-la".into()],
            working_dir: PathBuf::from("/home"),
            env: vec![("PATH".into(), "/bin".into())],
            state: PtySessionState::Running { pid: 789 },
            origin: ActionOrigin::User,
            created_at_unix_ms: 1234567890,
        };

        let record: PtySessionRecord = session.clone().into();
        assert_eq!(record.id, session.id);
        assert_eq!(record.command, session.command);
        assert_eq!(record.args, session.args);
        assert_eq!(record.working_dir, session.working_dir);
        assert_eq!(record.env, session.env);
        assert_eq!(record.state, session.state);
        assert_eq!(record.origin, session.origin);
        assert_eq!(record.created_at_unix_ms, session.created_at_unix_ms);
        assert!(record.updated_at_unix_ms >= session.created_at_unix_ms);
    }

    #[test]
    fn pty_session_from_pty_session_record() {
        let record = PtySessionRecord {
            id: PtySessionId(2),
            command: "cat".into(),
            args: vec!["file.txt".into()],
            working_dir: PathBuf::from("/var"),
            env: vec![("USER".into(), "test".into())],
            state: PtySessionState::Exited { exit_code: Some(0) },
            origin: ActionOrigin::Agent,
            created_at_unix_ms: 9876543210,
            updated_at_unix_ms: 9876543220,
        };

        let session: PtySession = record.clone().into();
        assert_eq!(session.id, record.id);
        assert_eq!(session.command, record.command);
        assert_eq!(session.args, record.args);
        assert_eq!(session.working_dir, record.working_dir);
        assert_eq!(session.env, record.env);
        assert_eq!(session.state, record.state);
        assert_eq!(session.origin, record.origin);
        assert_eq!(session.created_at_unix_ms, record.created_at_unix_ms);
    }
}
