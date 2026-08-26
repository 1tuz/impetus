use crate::events::Event;
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("storage lock poisoned")]
    Poisoned,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub trait EventStore: Send + Sync {
    fn append(&self, event: &Event) -> Result<(), StoreError>;
    fn list(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError>;
}

#[derive(Default)]
pub struct MemoryEventStore {
    events: Mutex<Vec<Event>>,
}

impl EventStore for MemoryEventStore {
    fn append(&self, event: &Event) -> Result<(), StoreError> {
        self.events
            .lock()
            .map_err(|_| StoreError::Poisoned)?
            .push(event.clone());
        Ok(())
    }
    fn list(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError> {
        Ok(self
            .events
            .lock()
            .map_err(|_| StoreError::Poisoned)?
            .iter()
            .filter(|e| e.session_id == session_id)
            .cloned()
            .collect())
    }
}

pub struct SqliteEventStore {
    connection: Mutex<Connection>,
}

impl SqliteEventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Arc<Self>, StoreError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, sequence INTEGER NOT NULL,
                at_unix_ms INTEGER NOT NULL, kind_json TEXT NOT NULL, body_json TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS events_session_sequence_unique ON events(session_id, sequence);",
        )?;
        Ok(Arc::new(Self {
            connection: Mutex::new(connection),
        }))
    }
}

impl EventStore for SqliteEventStore {
    fn append(&self, event: &Event) -> Result<(), StoreError> {
        let conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT INTO events (id, session_id, sequence, at_unix_ms, kind_json, body_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![event.id.to_string(), event.session_id.to_string(), event.sequence, event.at_unix_ms, serde_json::to_string(&event.kind)?, serde_json::to_string(&event.body)?],
        )?;
        Ok(())
    }

    fn list(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError> {
        let conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = conn.prepare("SELECT id, session_id, sequence, at_unix_ms, kind_json, body_json FROM events WHERE session_id = ?1 ORDER BY sequence")?;
        let rows = statement.query_map([session_id.to_string()], |row| {
            Ok(Event {
                id: Uuid::parse_str(&row.get::<_, String>(0)?)
                    .expect("valid UUID stored by this application"),
                session_id: Uuid::parse_str(&row.get::<_, String>(1)?)
                    .expect("valid UUID stored by this application"),
                sequence: row.get(2)?,
                at_unix_ms: row.get(3)?,
                kind: serde_json::from_str(&row.get::<_, String>(4)?)
                    .expect("valid event kind stored by this application"),
                body: serde_json::from_str(&row.get::<_, String>(5)?)
                    .expect("valid event body stored by this application"),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventKind;

    #[test]
    fn sqlite_events_survive_reopen() {
        let test_root =
            std::env::temp_dir().join(format!("agentic-terminal-sqlite-reopen-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&test_root).expect("create isolated test directory");
        let database = test_root.join("events.sqlite3");
        let session_id = Uuid::new_v4();
        let event = Event::new(
            session_id,
            1,
            EventKind::RuntimeNotice,
            serde_json::json!({ "status": "created" }),
        );

        {
            let store = SqliteEventStore::open(&database).expect("open sqlite event store");
            store.append(&event).expect("append event");
            let duplicate_sequence = Event::new(
                session_id,
                1,
                EventKind::RuntimeNotice,
                serde_json::json!({ "status": "duplicate" }),
            );
            assert!(matches!(
                store.append(&duplicate_sequence),
                Err(StoreError::Sqlite(_))
            ));
        }
        {
            let reopened = SqliteEventStore::open(&database).expect("reopen sqlite event store");
            assert_eq!(reopened.list(session_id).expect("list events"), vec![event]);
        }

        std::fs::remove_dir_all(test_root).expect("remove isolated test directory");
    }
}
