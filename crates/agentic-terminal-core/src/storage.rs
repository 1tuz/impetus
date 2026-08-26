use crate::events::{Event, EventPayload, SessionEvent, legacy_payload};
use rusqlite::{Connection, TransactionBehavior, params};
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
    #[error("event `{event_id}` has unsupported schema version {schema_version}")]
    UnsupportedSchema { event_id: Uuid, schema_version: u16 },
    #[error("event `{event_id}` has malformed payload: {source}")]
    MalformedPayload {
        event_id: Uuid,
        source: serde_json::Error,
    },
    #[error("event has invalid {field} UUID `{value}`")]
    InvalidUuid { field: &'static str, value: String },
    #[error("session `{0}` does not exist")]
    MissingSession(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: Uuid,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

pub trait EventStore: Send + Sync {
    fn create_session(&self) -> Result<Uuid, StoreError>;
    fn append(&self, event: &Event) -> Result<(), StoreError>;
    fn append_next(&self, session_id: Uuid, payload: EventPayload) -> Result<Event, StoreError>;
    fn list(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError>;
    fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError>;
}

#[derive(Default)]
pub struct MemoryEventStore {
    events: Mutex<Vec<Event>>,
}

impl EventStore for MemoryEventStore {
    fn create_session(&self) -> Result<Uuid, StoreError> {
        let session_id = Uuid::new_v4();
        let event = Event::new(session_id, 1, EventPayload::Session(SessionEvent::Created));
        self.events
            .lock()
            .map_err(|_| StoreError::Poisoned)?
            .push(event);
        Ok(session_id)
    }

    fn append(&self, event: &Event) -> Result<(), StoreError> {
        self.events
            .lock()
            .map_err(|_| StoreError::Poisoned)?
            .push(event.clone());
        Ok(())
    }

    fn append_next(&self, session_id: Uuid, payload: EventPayload) -> Result<Event, StoreError> {
        let mut events = self.events.lock().map_err(|_| StoreError::Poisoned)?;
        let mut sequences = events
            .iter()
            .filter(|event| event.session_id == session_id)
            .map(|event| event.sequence);
        let Some(last_sequence) = sequences
            .next()
            .map(|first| sequences.fold(first, u64::max))
        else {
            return Err(StoreError::MissingSession(session_id));
        };
        let sequence = last_sequence + 1;
        let event = Event::new(session_id, sequence, payload);
        events.push(event.clone());
        Ok(event)
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

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError> {
        let events = self.events.lock().map_err(|_| StoreError::Poisoned)?;
        let mut sessions = std::collections::BTreeMap::<Uuid, (u64, u64)>::new();
        for event in events.iter() {
            sessions
                .entry(event.session_id)
                .and_modify(|times| times.1 = times.1.max(event.at_unix_ms))
                .or_insert((event.at_unix_ms, event.at_unix_ms));
        }
        Ok(sessions
            .into_iter()
            .map(
                |(id, (created_at_unix_ms, updated_at_unix_ms))| SessionInfo {
                    id,
                    created_at_unix_ms,
                    updated_at_unix_ms,
                },
            )
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
                at_unix_ms INTEGER NOT NULL, kind_json TEXT NOT NULL, body_json TEXT NOT NULL,
                schema_version INTEGER, payload_json TEXT
             );
             CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY, created_at_unix_ms INTEGER NOT NULL, updated_at_unix_ms INTEGER NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS events_session_sequence_unique ON events(session_id, sequence);",
        )?;
        add_column_if_missing(&connection, "events", "schema_version", "INTEGER")?;
        add_column_if_missing(&connection, "events", "payload_json", "TEXT")?;
        connection.execute_batch(
            "INSERT OR IGNORE INTO sessions (id, created_at_unix_ms, updated_at_unix_ms)
             SELECT session_id, MIN(at_unix_ms), MAX(at_unix_ms) FROM events GROUP BY session_id;",
        )?;
        Ok(Arc::new(Self {
            connection: Mutex::new(connection),
        }))
    }
}

impl EventStore for SqliteEventStore {
    fn create_session(&self) -> Result<Uuid, StoreError> {
        let mut conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session_id = Uuid::new_v4();
        let event = Event::new(session_id, 1, EventPayload::Session(SessionEvent::Created));
        transaction.execute(
            "INSERT INTO sessions (id, created_at_unix_ms, updated_at_unix_ms) VALUES (?1, ?2, ?2)",
            params![session_id.to_string(), event.at_unix_ms],
        )?;
        insert_event(&transaction, &event)?;
        transaction.commit()?;
        Ok(session_id)
    }

    fn append(&self, event: &Event) -> Result<(), StoreError> {
        let mut conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO sessions (id, created_at_unix_ms, updated_at_unix_ms) VALUES (?1, ?2, ?2)",
            params![event.session_id.to_string(), event.at_unix_ms],
        )?;
        insert_event(&transaction, event)?;
        transaction.execute(
            "UPDATE sessions SET updated_at_unix_ms = MAX(updated_at_unix_ms, ?2) WHERE id = ?1",
            params![event.session_id.to_string(), event.at_unix_ms],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn append_next(&self, session_id: Uuid, payload: EventPayload) -> Result<Event, StoreError> {
        let mut conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let next_sequence = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM events WHERE session_id = ?1",
            [session_id.to_string()],
            |row| row.get::<_, u64>(0),
        )?;
        let event = Event::new(session_id, next_sequence, payload);
        insert_event(&transaction, &event)?;
        let changed = transaction.execute(
            "UPDATE sessions SET updated_at_unix_ms = MAX(updated_at_unix_ms, ?2) WHERE id = ?1",
            params![session_id.to_string(), event.at_unix_ms],
        )?;
        if changed == 0 {
            return Err(StoreError::MissingSession(session_id));
        }
        transaction.commit()?;
        Ok(event)
    }

    fn list(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError> {
        let conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = conn.prepare("SELECT id, session_id, sequence, at_unix_ms, kind_json, body_json, schema_version, payload_json FROM events WHERE session_id = ?1 ORDER BY sequence")?;
        let rows = statement.query_map([session_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<u16>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?;
        rows.map(|row| row.map_err(StoreError::from).and_then(decode_event))
            .collect()
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError> {
        let conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = conn.prepare("SELECT id, created_at_unix_ms, updated_at_unix_ms FROM sessions ORDER BY updated_at_unix_ms DESC, id")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
            })?
            .map(|row| {
                let (id, created_at_unix_ms, updated_at_unix_ms) = row?;
                let id = Uuid::parse_str(&id).map_err(|_| StoreError::InvalidUuid {
                    field: "session id",
                    value: id,
                })?;
                Ok(SessionInfo {
                    id,
                    created_at_unix_ms,
                    updated_at_unix_ms,
                })
            })
            .collect()
    }
}

fn insert_event(transaction: &rusqlite::Transaction<'_>, event: &Event) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO events (id, session_id, sequence, at_unix_ms, kind_json, body_json, schema_version, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![event.id.to_string(), event.session_id.to_string(), event.sequence, event.at_unix_ms, "typed", "{}", event.schema_version, serde_json::to_string(&event.payload)?],
    )?;
    Ok(())
}

fn add_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), StoreError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let known = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !known.iter().any(|known_column| known_column == column) {
        connection.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

type StoredRow = (
    String,
    String,
    u64,
    u64,
    String,
    String,
    Option<u16>,
    Option<String>,
);

fn decode_event(row: StoredRow) -> Result<Event, StoreError> {
    let (id, session_id, sequence, at_unix_ms, kind_json, body_json, schema_version, payload_json) =
        row;
    let id = Uuid::parse_str(&id).map_err(|_| StoreError::InvalidUuid {
        field: "id",
        value: id,
    })?;
    let session_id = Uuid::parse_str(&session_id).map_err(|_| StoreError::InvalidUuid {
        field: "session_id",
        value: session_id,
    })?;
    let (schema_version, payload) = match (schema_version, payload_json) {
        (Some(version), Some(payload_json)) => {
            if version != crate::EVENT_SCHEMA_VERSION {
                return Err(StoreError::UnsupportedSchema {
                    event_id: id,
                    schema_version: version,
                });
            }
            let payload =
                serde_json::from_str::<EventPayload>(&payload_json).map_err(|source| {
                    StoreError::MalformedPayload {
                        event_id: id,
                        source,
                    }
                })?;
            (version, payload)
        }
        _ => {
            let kind = serde_json::from_str::<String>(&kind_json).map_err(|source| {
                StoreError::MalformedPayload {
                    event_id: id,
                    source,
                }
            })?;
            let body = serde_json::from_str(&body_json).map_err(|source| {
                StoreError::MalformedPayload {
                    event_id: id,
                    source,
                }
            })?;
            let payload =
                legacy_payload(&kind, body).map_err(|source| StoreError::MalformedPayload {
                    event_id: id,
                    source,
                })?;
            (crate::EVENT_SCHEMA_VERSION, payload)
        }
    };
    Ok(Event::with_metadata(
        schema_version,
        id,
        session_id,
        sequence,
        at_unix_ms,
        payload,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventPayload, NoticeEvent};

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
            EventPayload::Notice(NoticeEvent::Runtime {
                message: "created".into(),
            }),
        );

        {
            let store = SqliteEventStore::open(&database).expect("open sqlite event store");
            store.append(&event).expect("append event");
            let duplicate_sequence = Event::new(
                session_id,
                1,
                EventPayload::Notice(NoticeEvent::Runtime {
                    message: "duplicate".into(),
                }),
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

    #[test]
    fn legacy_rows_replay_after_schema_migration() {
        let test_root =
            std::env::temp_dir().join(format!("agentic-terminal-legacy-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&test_root).expect("create isolated test directory");
        let database = test_root.join("events.sqlite3");
        let session_id = Uuid::new_v4();
        let event_id = Uuid::new_v4();
        {
            let connection = Connection::open(&database).expect("open legacy database");
            connection.execute_batch("CREATE TABLE events (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, sequence INTEGER NOT NULL, at_unix_ms INTEGER NOT NULL, kind_json TEXT NOT NULL, body_json TEXT NOT NULL);") .expect("create legacy table");
            connection
                .execute(
                    "INSERT INTO events VALUES (?1, ?2, 1, 0, ?3, ?4)",
                    params![
                        event_id.to_string(),
                        session_id.to_string(),
                        "\"user_intent\"",
                        "{\"text\":\"legacy\"}"
                    ],
                )
                .expect("insert legacy event");
        }
        let store = SqliteEventStore::open(&database).expect("migrate legacy database");
        assert!(
            matches!(store.list(session_id).expect("read migrated event").as_slice(), [Event { payload: EventPayload::Intent(intent), .. }] if intent.text == "legacy")
        );
        std::fs::remove_dir_all(test_root).expect("remove isolated test directory");
    }

    #[test]
    fn malformed_typed_payload_has_typed_error() {
        let test_root =
            std::env::temp_dir().join(format!("agentic-terminal-malformed-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&test_root).expect("create isolated test directory");
        let database = test_root.join("events.sqlite3");
        let session_id = Uuid::new_v4();
        let event_id = Uuid::new_v4();
        let store = SqliteEventStore::open(&database).expect("open sqlite event store");
        store
            .connection
            .lock()
            .expect("lock test connection")
            .execute(
                "INSERT INTO events (id, session_id, sequence, at_unix_ms, kind_json, body_json, schema_version, payload_json) VALUES (?1, ?2, 1, 0, 'typed', '{}', 1, '{bad')",
                params![event_id.to_string(), session_id.to_string()],
            )
            .expect("insert malformed payload");
        assert!(matches!(
            store.list(session_id),
            Err(StoreError::MalformedPayload { event_id: actual, .. }) if actual == event_id
        ));
        std::fs::remove_dir_all(test_root).expect("remove isolated test directory");
    }
}
