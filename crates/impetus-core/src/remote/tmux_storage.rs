//! Durable storage for tmux sessions using SQLite.

use super::tmux::{TmuxSession, TmuxSessionId, TmuxSessionState};
use crate::{ActionOrigin, SSHProfile};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TmuxSessionStoreError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("session not found: {0:?}")]
    NotFound(TmuxSessionId),
    #[error("invalid state encoding: {0}")]
    InvalidState(String),
    #[error("invalid origin encoding: {0}")]
    InvalidOrigin(String),
    #[error("serialization error: {0}")]
    Serialization(String),
}

#[derive(Debug, Clone)]
pub struct TmuxSessionRecord {
    pub id: TmuxSessionId,
    pub name: String,
    pub ssh_profile: SSHProfile,
    pub working_dir: Option<PathBuf>,
    pub initial_command: Option<String>,
    pub state: TmuxSessionState,
    pub origin: ActionOrigin,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

impl From<TmuxSession> for TmuxSessionRecord {
    fn from(session: TmuxSession) -> Self {
        Self {
            id: session.id,
            name: session.name,
            ssh_profile: session.ssh_profile,
            working_dir: session.working_dir,
            initial_command: session.initial_command,
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

impl From<TmuxSessionRecord> for TmuxSession {
    fn from(record: TmuxSessionRecord) -> Self {
        Self {
            id: record.id,
            name: record.name,
            ssh_profile: record.ssh_profile,
            working_dir: record.working_dir,
            initial_command: record.initial_command,
            state: record.state,
            origin: record.origin,
            created_at_unix_ms: record.created_at_unix_ms,
        }
    }
}

/// Trait for tmux session storage.
#[async_trait::async_trait]
pub trait TmuxSessionStore: Send + Sync {
    async fn save_session(&self, session: &TmuxSessionRecord) -> Result<(), TmuxSessionStoreError>;
    async fn get_session(
        &self,
        id: TmuxSessionId,
    ) -> Result<Option<TmuxSessionRecord>, TmuxSessionStoreError>;
    async fn list_sessions(&self) -> Result<Vec<TmuxSessionRecord>, TmuxSessionStoreError>;
    async fn update_state(
        &self,
        id: TmuxSessionId,
        state: &TmuxSessionState,
    ) -> Result<(), TmuxSessionStoreError>;
    async fn delete_session(&self, id: TmuxSessionId) -> Result<(), TmuxSessionStoreError>;
}

/// SQLite implementation of TmuxSessionStore.
pub struct SqliteTmuxSessionStore {
    db_path: PathBuf,
}

impl SqliteTmuxSessionStore {
    pub fn new(db_path: impl AsRef<Path>) -> Result<Self, TmuxSessionStoreError> {
        let db_path = db_path.as_ref().to_path_buf();
        let conn = Connection::open(&db_path)?;

        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS tmux_sessions (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                ssh_host TEXT NOT NULL,
                ssh_user TEXT NOT NULL,
                ssh_port INTEGER NOT NULL,
                ssh_profile TEXT NOT NULL,
                working_dir TEXT,
                initial_command TEXT,
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
            "CREATE INDEX IF NOT EXISTS idx_tmux_sessions_state ON tmux_sessions(state_type)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tmux_sessions_host ON tmux_sessions(ssh_host, ssh_user)",
            [],
        )?;

        Ok(Self { db_path })
    }

    fn connection(&self) -> Result<Connection, TmuxSessionStoreError> {
        Ok(Connection::open(&self.db_path)?)
    }

    fn encode_state(state: &TmuxSessionState) -> (String, Option<String>) {
        match state {
            TmuxSessionState::Creating => ("creating".into(), None),
            TmuxSessionState::Active { windows } => ("active".into(), Some(windows.to_string())),
            TmuxSessionState::Detached { windows } => {
                ("detached".into(), Some(windows.to_string()))
            }
            TmuxSessionState::Dead => ("dead".into(), None),
        }
    }

    fn decode_state(
        state_type: &str,
        state_data: Option<&str>,
    ) -> Result<TmuxSessionState, TmuxSessionStoreError> {
        match state_type {
            "creating" => Ok(TmuxSessionState::Creating),
            "active" => {
                let windows = state_data
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| TmuxSessionStoreError::InvalidState("missing windows".into()))?;
                Ok(TmuxSessionState::Active { windows })
            }
            "detached" => {
                let windows = state_data
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| TmuxSessionStoreError::InvalidState("missing windows".into()))?;
                Ok(TmuxSessionState::Detached { windows })
            }
            "dead" => Ok(TmuxSessionState::Dead),
            _ => Err(TmuxSessionStoreError::InvalidState(format!(
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

    fn decode_origin(origin: &str) -> Result<ActionOrigin, TmuxSessionStoreError> {
        match origin {
            "user" => Ok(ActionOrigin::User),
            "agent" => Ok(ActionOrigin::Agent),
            _ => Err(TmuxSessionStoreError::InvalidOrigin(format!(
                "unknown origin: {}",
                origin
            ))),
        }
    }
}

#[async_trait::async_trait]
impl TmuxSessionStore for SqliteTmuxSessionStore {
    async fn save_session(&self, session: &TmuxSessionRecord) -> Result<(), TmuxSessionStoreError> {
        let conn = self.connection()?;
        let (state_type, state_data) = Self::encode_state(&session.state);
        let origin = Self::encode_origin(&session.origin);

        let ssh_profile_json = serde_json::to_string(&session.ssh_profile)
            .map_err(|e| TmuxSessionStoreError::Serialization(e.to_string()))?;

        conn.execute(
            r#"
            INSERT INTO tmux_sessions 
            (id, name, ssh_host, ssh_user, ssh_port, ssh_profile, working_dir, initial_command, 
             state_type, state_data, origin, created_at_unix_ms, updated_at_unix_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(id) DO UPDATE SET
                state_type = ?9,
                state_data = ?10,
                updated_at_unix_ms = ?13
            "#,
            params![
                session.id.0,
                session.name,
                session.ssh_profile.host,
                session.ssh_profile.user,
                session.ssh_profile.port,
                ssh_profile_json,
                session
                    .working_dir
                    .as_ref()
                    .map(|p| p.display().to_string()),
                session.initial_command,
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
        id: TmuxSessionId,
    ) -> Result<Option<TmuxSessionRecord>, TmuxSessionStoreError> {
        let conn = self.connection()?;

        let result = conn
            .query_row(
                r#"
                SELECT id, name, ssh_profile, working_dir, initial_command, 
                       state_type, state_data, origin, created_at_unix_ms, updated_at_unix_ms
                FROM tmux_sessions
                WHERE id = ?1
                "#,
                params![id.0],
                |row| {
                    let state_type: String = row.get(5)?;
                    let state_data: Option<String> = row.get(6)?;
                    let origin_str: String = row.get(7)?;

                    Ok((
                        TmuxSessionId(row.get(0)?),
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
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
                name,
                ssh_profile_json,
                working_dir,
                initial_command,
                state_type,
                state_data,
                origin_str,
                created_at,
                updated_at,
            )) => {
                let ssh_profile: SSHProfile = serde_json::from_str(&ssh_profile_json)
                    .map_err(|e| TmuxSessionStoreError::Serialization(e.to_string()))?;
                let state = Self::decode_state(&state_type, state_data.as_deref())?;
                let origin = Self::decode_origin(&origin_str)?;

                Ok(Some(TmuxSessionRecord {
                    id,
                    name,
                    ssh_profile,
                    working_dir: working_dir.map(PathBuf::from),
                    initial_command,
                    state,
                    origin,
                    created_at_unix_ms: created_at,
                    updated_at_unix_ms: updated_at,
                }))
            }
        }
    }

    async fn list_sessions(&self) -> Result<Vec<TmuxSessionRecord>, TmuxSessionStoreError> {
        let conn = self.connection()?;

        let mut stmt = conn.prepare(
            r#"
            SELECT id, name, ssh_profile, working_dir, initial_command,
                   state_type, state_data, origin, created_at_unix_ms, updated_at_unix_ms
            FROM tmux_sessions
            ORDER BY created_at_unix_ms DESC
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            let state_type: String = row.get(5)?;
            let state_data: Option<String> = row.get(6)?;
            let origin_str: String = row.get(7)?;

            Ok((
                TmuxSessionId(row.get(0)?),
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
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
                name,
                ssh_profile_json,
                working_dir,
                initial_command,
                state_type,
                state_data,
                origin_str,
                created_at,
                updated_at,
            ) = row_result?;

            let ssh_profile: SSHProfile = serde_json::from_str(&ssh_profile_json)
                .map_err(|e| TmuxSessionStoreError::Serialization(e.to_string()))?;
            let state = Self::decode_state(&state_type, state_data.as_deref())?;
            let origin = Self::decode_origin(&origin_str)?;

            sessions.push(TmuxSessionRecord {
                id,
                name,
                ssh_profile,
                working_dir: working_dir.map(PathBuf::from),
                initial_command,
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
        id: TmuxSessionId,
        state: &TmuxSessionState,
    ) -> Result<(), TmuxSessionStoreError> {
        let conn = self.connection()?;
        let (state_type, state_data) = Self::encode_state(state);
        let updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_millis() as u64;

        let rows_affected = conn.execute(
            "UPDATE tmux_sessions SET state_type = ?1, state_data = ?2, updated_at_unix_ms = ?3 WHERE id = ?4",
            params![state_type, state_data, updated_at, id.0],
        )?;

        if rows_affected == 0 {
            return Err(TmuxSessionStoreError::NotFound(id));
        }

        Ok(())
    }

    async fn delete_session(&self, id: TmuxSessionId) -> Result<(), TmuxSessionStoreError> {
        let conn = self.connection()?;

        let rows_affected =
            conn.execute("DELETE FROM tmux_sessions WHERE id = ?1", params![id.0])?;

        if rows_affected == 0 {
            return Err(TmuxSessionStoreError::NotFound(id));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionOrigin, SSHKeyReference};

    fn test_profile() -> SSHProfile {
        SSHProfile {
            host: "example.com".into(),
            user: "testuser".into(),
            port: 22,
            host_key_fingerprint: None,
            key_reference: SSHKeyReference::KeychainItem {
                service: "ssh".into(),
                account: "testuser".into(),
            },
        }
    }

    fn temp_db() -> SqliteTmuxSessionStore {
        let temp_file =
            std::env::temp_dir().join(format!("tmux_sessions_test_{}.db", std::process::id()));
        SqliteTmuxSessionStore::new(&temp_file).unwrap()
    }

    #[tokio::test]
    async fn save_and_retrieve_session() {
        let store = temp_db();
        let session = TmuxSessionRecord {
            id: TmuxSessionId(1),
            name: "dev-session".into(),
            ssh_profile: test_profile(),
            working_dir: Some(PathBuf::from("/home/user")),
            initial_command: Some("vim".into()),
            state: TmuxSessionState::Active { windows: 2 },
            origin: ActionOrigin::User,
            created_at_unix_ms: 1000,
            updated_at_unix_ms: 1000,
        };

        store.save_session(&session).await.unwrap();

        let retrieved = store.get_session(TmuxSessionId(1)).await.unwrap();
        assert!(retrieved.is_some());

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.name, "dev-session");
        assert_eq!(retrieved.ssh_profile.host, "example.com");
        assert!(matches!(
            retrieved.state,
            TmuxSessionState::Active { windows: 2 }
        ));
    }

    #[tokio::test]
    async fn update_session_state() {
        let store = temp_db();
        let session = TmuxSessionRecord {
            id: TmuxSessionId(2),
            name: "test".into(),
            ssh_profile: test_profile(),
            working_dir: None,
            initial_command: None,
            state: TmuxSessionState::Creating,
            origin: ActionOrigin::Agent,
            created_at_unix_ms: 2000,
            updated_at_unix_ms: 2000,
        };

        store.save_session(&session).await.unwrap();

        store
            .update_state(TmuxSessionId(2), &TmuxSessionState::Detached { windows: 3 })
            .await
            .unwrap();

        let retrieved = store.get_session(TmuxSessionId(2)).await.unwrap().unwrap();
        assert!(matches!(
            retrieved.state,
            TmuxSessionState::Detached { windows: 3 }
        ));
    }

    #[tokio::test]
    async fn list_sessions_ordered() {
        let store = temp_db();

        for i in 1..=3 {
            let session = TmuxSessionRecord {
                id: TmuxSessionId(i + 100),
                name: format!("session-{}", i),
                ssh_profile: test_profile(),
                working_dir: None,
                initial_command: None,
                state: TmuxSessionState::Creating,
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
        let session = TmuxSessionRecord {
            id: TmuxSessionId(200),
            name: "to-delete".into(),
            ssh_profile: test_profile(),
            working_dir: None,
            initial_command: None,
            state: TmuxSessionState::Dead,
            origin: ActionOrigin::User,
            created_at_unix_ms: 5000,
            updated_at_unix_ms: 5000,
        };

        store.save_session(&session).await.unwrap();
        store.delete_session(TmuxSessionId(200)).await.unwrap();

        let retrieved = store.get_session(TmuxSessionId(200)).await.unwrap();
        assert!(retrieved.is_none());
    }

    #[tokio::test]
    async fn get_nonexistent_session_returns_none() {
        let store = temp_db();
        let result = store.get_session(TmuxSessionId(999999)).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn update_nonexistent_session_returns_error() {
        let store = temp_db();
        let result = store
            .update_state(TmuxSessionId(888888), &TmuxSessionState::Creating)
            .await;
        assert!(matches!(result, Err(TmuxSessionStoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn delete_nonexistent_session_returns_error() {
        let store = temp_db();
        let result = store.delete_session(TmuxSessionId(777777)).await;
        assert!(matches!(result, Err(TmuxSessionStoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn save_session_upserts_existing() {
        let store = temp_db();
        let session = TmuxSessionRecord {
            id: TmuxSessionId(300),
            name: "upsert-test".into(),
            ssh_profile: test_profile(),
            working_dir: None,
            initial_command: None,
            state: TmuxSessionState::Creating,
            origin: ActionOrigin::User,
            created_at_unix_ms: 1000,
            updated_at_unix_ms: 1000,
        };
        store.save_session(&session).await.unwrap();

        let updated = TmuxSessionRecord {
            id: TmuxSessionId(300),
            name: "upsert-test".into(),
            ssh_profile: test_profile(),
            working_dir: Some(PathBuf::from("/new/dir")),
            initial_command: None,
            state: TmuxSessionState::Active { windows: 5 },
            origin: ActionOrigin::User,
            created_at_unix_ms: 1000,
            updated_at_unix_ms: 2000,
        };
        store.save_session(&updated).await.unwrap();

        let retrieved = store
            .get_session(TmuxSessionId(300))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            retrieved.state,
            TmuxSessionState::Active { windows: 5 }
        ));
        assert_eq!(retrieved.updated_at_unix_ms, 2000);
    }

    #[tokio::test]
    async fn all_state_variants_round_trip() {
        let store = temp_db();
        let states = [
            TmuxSessionState::Creating,
            TmuxSessionState::Active { windows: 1 },
            TmuxSessionState::Detached { windows: 2 },
            TmuxSessionState::Dead,
        ];

        for (i, state) in states.iter().enumerate() {
            let session = TmuxSessionRecord {
                id: TmuxSessionId(400 + i as u64),
                name: format!("state-{}", i),
                ssh_profile: test_profile(),
                working_dir: None,
                initial_command: None,
                state: state.clone(),
                origin: ActionOrigin::Agent,
                created_at_unix_ms: 1000,
                updated_at_unix_ms: 1000,
            };
            store.save_session(&session).await.unwrap();

            let retrieved = store
                .get_session(TmuxSessionId(400 + i as u64))
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
            let session = TmuxSessionRecord {
                id: TmuxSessionId(500 + i as u64),
                name: "origin-test".into(),
                ssh_profile: test_profile(),
                working_dir: None,
                initial_command: None,
                state: TmuxSessionState::Creating,
                origin: *origin,
                created_at_unix_ms: 1000,
                updated_at_unix_ms: 1000,
            };
            store.save_session(&session).await.unwrap();

            let retrieved = store
                .get_session(TmuxSessionId(500 + i as u64))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retrieved.origin, *origin);
        }
    }

    #[tokio::test]
    async fn working_dir_and_initial_command_persist() {
        let store = temp_db();
        let session = TmuxSessionRecord {
            id: TmuxSessionId(600),
            name: "full-session".into(),
            ssh_profile: test_profile(),
            working_dir: Some(PathBuf::from("/opt/project")),
            initial_command: Some("htop".into()),
            state: TmuxSessionState::Active { windows: 1 },
            origin: ActionOrigin::User,
            created_at_unix_ms: 1000,
            updated_at_unix_ms: 1000,
        };
        store.save_session(&session).await.unwrap();

        let retrieved = store
            .get_session(TmuxSessionId(600))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.working_dir, Some(PathBuf::from("/opt/project")));
        assert_eq!(retrieved.initial_command, Some("htop".into()));
    }

    #[test]
    fn tmux_session_record_from_tmux_session() {
        let session = TmuxSession {
            id: TmuxSessionId(1),
            name: "test".into(),
            ssh_profile: test_profile(),
            working_dir: Some(PathBuf::from("/home")),
            initial_command: Some("bash".into()),
            state: TmuxSessionState::Active { windows: 3 },
            origin: ActionOrigin::User,
            created_at_unix_ms: 1234567890,
        };

        let record: TmuxSessionRecord = session.clone().into();
        assert_eq!(record.id, session.id);
        assert_eq!(record.name, session.name);
        assert_eq!(record.ssh_profile.host, session.ssh_profile.host);
        assert_eq!(record.working_dir, session.working_dir);
        assert_eq!(record.initial_command, session.initial_command);
        assert_eq!(record.state, session.state);
        assert_eq!(record.origin, session.origin);
        assert_eq!(record.created_at_unix_ms, session.created_at_unix_ms);
        assert!(record.updated_at_unix_ms >= session.created_at_unix_ms);
    }

    #[test]
    fn tmux_session_from_tmux_session_record() {
        let record = TmuxSessionRecord {
            id: TmuxSessionId(2),
            name: "record-test".into(),
            ssh_profile: test_profile(),
            working_dir: Some(PathBuf::from("/var")),
            initial_command: Some("vim".into()),
            state: TmuxSessionState::Detached { windows: 2 },
            origin: ActionOrigin::Agent,
            created_at_unix_ms: 9876543210,
            updated_at_unix_ms: 9876543220,
        };

        let session: TmuxSession = record.clone().into();
        assert_eq!(session.id, record.id);
        assert_eq!(session.name, record.name);
        assert_eq!(session.ssh_profile.host, record.ssh_profile.host);
        assert_eq!(session.working_dir, record.working_dir);
        assert_eq!(session.initial_command, record.initial_command);
        assert_eq!(session.state, record.state);
        assert_eq!(session.origin, record.origin);
        assert_eq!(session.created_at_unix_ms, record.created_at_unix_ms);
    }
}
