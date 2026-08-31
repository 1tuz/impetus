use crate::budget::BudgetState;
use crate::events::{Event, EventPayload, SessionEvent, legacy_payload};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::broadcast;
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
    pub parent_session_id: Option<Uuid>,
    pub fork_sequence: Option<u64>,
}

pub trait EventStore: Send + Sync {
    fn create_session(&self) -> Result<Uuid, StoreError>;
    fn append(&self, event: &Event) -> Result<(), StoreError>;
    fn append_next(&self, session_id: Uuid, payload: EventPayload) -> Result<Event, StoreError>;
    fn list(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError>;
    fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError>;

    /// Delete session and all its events.
    /// Returns Ok(()) even if session doesn't exist (idempotent).
    fn delete_session(&self, session_id: Uuid) -> Result<(), StoreError>;

    /// Fork session up to given sequence number (inclusive).
    /// Creates new session with events copied from source up to checkpoint.
    fn fork_session(
        &self,
        source_session_id: Uuid,
        up_to_sequence: u64,
    ) -> Result<Uuid, StoreError>;

    /// Subscribe to event notifications.
    /// Returns a receiver that gets (session_id, sequence) on every append.
    /// Channel size is 100; old notifications may be dropped but cursors handle gaps.
    fn subscribe_notifications(&self) -> broadcast::Receiver<(Uuid, u64)>;

    /// Get budget state for session.
    fn get_budget_state(&self, session_id: Uuid) -> Result<BudgetState, StoreError>;

    /// Update budget state for session.
    fn update_budget_state(&self, session_id: Uuid, state: &BudgetState) -> Result<(), StoreError>;
}

#[derive(Debug, Clone)]
struct MemorySessionMetadata {
    id: Uuid,
    parent_session_id: Option<Uuid>,
    fork_sequence: Option<u64>,
}

pub struct MemoryEventStore {
    events: Mutex<Vec<Event>>,
    sessions: Mutex<Vec<MemorySessionMetadata>>,
    notifier: broadcast::Sender<(Uuid, u64)>,
}

impl Default for MemoryEventStore {
    fn default() -> Self {
        let (notifier, _) = broadcast::channel(100);
        Self {
            events: Mutex::default(),
            sessions: Mutex::default(),
            notifier,
        }
    }
}

impl EventStore for MemoryEventStore {
    fn create_session(&self) -> Result<Uuid, StoreError> {
        let session_id = Uuid::new_v4();
        let event = Event::new(session_id, 1, EventPayload::Session(SessionEvent::Created));
        self.events
            .lock()
            .map_err(|_| StoreError::Poisoned)?
            .push(event);
        self.sessions
            .lock()
            .map_err(|_| StoreError::Poisoned)?
            .push(MemorySessionMetadata {
                id: session_id,
                parent_session_id: None,
                fork_sequence: None,
            });
        Ok(session_id)
    }

    fn append(&self, event: &Event) -> Result<(), StoreError> {
        self.events
            .lock()
            .map_err(|_| StoreError::Poisoned)?
            .push(event.clone());
        let _ = self.notifier.send((event.session_id, event.sequence));
        Ok(())
    }

    fn append_next(&self, session_id: Uuid, payload: EventPayload) -> Result<Event, StoreError> {
        // Get logical last sequence by reading full history (prefix + suffix)
        let current_events = self.list(session_id)?;
        let last_sequence = current_events.last().map(|e| e.sequence).unwrap_or(0);
        let sequence = last_sequence + 1;

        let event = Event::new(session_id, sequence, payload);
        self.events
            .lock()
            .map_err(|_| StoreError::Poisoned)?
            .push(event.clone());
        let _ = self.notifier.send((event.session_id, event.sequence));
        Ok(event)
    }
    fn list(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError> {
        let events = self.events.lock().map_err(|_| StoreError::Poisoned)?;
        let sessions = self.sessions.lock().map_err(|_| StoreError::Poisoned)?;

        // Build ancestry chain
        let mut ancestry = Vec::new();
        let mut current_id = session_id;
        let mut current_fork_seq: Option<u64> = None;

        loop {
            let session_meta = sessions.iter().find(|s| s.id == current_id);
            if let Some(meta) = session_meta {
                if let Some(parent_id) = meta.parent_session_id {
                    ancestry.push((current_id, current_fork_seq));
                    current_id = parent_id;
                    current_fork_seq = meta.fork_sequence;
                } else {
                    // Root session
                    ancestry.push((current_id, current_fork_seq));
                    break;
                }
            } else {
                // No metadata found, treat as root
                ancestry.push((current_id, current_fork_seq));
                break;
            }
        }

        // Reverse to get root -> ... -> current
        ancestry.reverse();

        // Collect events with logical renumbering
        let mut all_events = Vec::new();
        let mut logical_sequence = 0u64;

        for (idx, (sid, up_to_seq_opt)) in ancestry.iter().enumerate() {
            let is_current = idx == ancestry.len() - 1;
            let session_events: Vec<Event> = events
                .iter()
                .filter(|e| {
                    if e.session_id != *sid {
                        return false;
                    }
                    if is_current {
                        true
                    } else if let Some(up_to) = up_to_seq_opt {
                        e.sequence <= *up_to
                    } else {
                        false
                    }
                })
                .cloned()
                .collect();

            for mut event in session_events {
                logical_sequence += 1;
                event.sequence = logical_sequence;
                all_events.push(event);
            }
        }

        Ok(all_events)
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError> {
        let events = self.events.lock().map_err(|_| StoreError::Poisoned)?;
        let sessions = self.sessions.lock().map_err(|_| StoreError::Poisoned)?;
        let mut sessions_map = std::collections::BTreeMap::<Uuid, (u64, u64)>::new();
        for event in events.iter() {
            sessions_map
                .entry(event.session_id)
                .and_modify(|times| times.1 = times.1.max(event.at_unix_ms))
                .or_insert((event.at_unix_ms, event.at_unix_ms));
        }
        Ok(sessions_map
            .into_iter()
            .map(|(id, (created_at_unix_ms, updated_at_unix_ms))| {
                let meta = sessions.iter().find(|s| s.id == id);
                SessionInfo {
                    id,
                    created_at_unix_ms,
                    updated_at_unix_ms,
                    parent_session_id: meta.and_then(|m| m.parent_session_id),
                    fork_sequence: meta.and_then(|m| m.fork_sequence),
                }
            })
            .collect())
    }

    fn fork_session(
        &self,
        source_session_id: Uuid,
        up_to_sequence: u64,
    ) -> Result<Uuid, StoreError> {
        let events = self.events.lock().map_err(|_| StoreError::Poisoned)?;
        let mut sessions = self.sessions.lock().map_err(|_| StoreError::Poisoned)?;

        // Verify source session exists and has events up to checkpoint
        let source_exists = events
            .iter()
            .any(|e| e.session_id == source_session_id && e.sequence <= up_to_sequence);

        if !source_exists {
            return Err(StoreError::MissingSession(source_session_id));
        }

        // Create new session with parent reference (no event copying, no SessionCreated)
        let new_session_id = Uuid::new_v4();
        sessions.push(MemorySessionMetadata {
            id: new_session_id,
            parent_session_id: Some(source_session_id),
            fork_sequence: Some(up_to_sequence),
        });

        Ok(new_session_id)
    }

    fn delete_session(&self, session_id: Uuid) -> Result<(), StoreError> {
        let mut events = self.events.lock().map_err(|_| StoreError::Poisoned)?;
        events.retain(|e| e.session_id != session_id);
        Ok(())
    }

    fn subscribe_notifications(&self) -> broadcast::Receiver<(Uuid, u64)> {
        self.notifier.subscribe()
    }

    fn get_budget_state(&self, _session_id: Uuid) -> Result<BudgetState, StoreError> {
        // In-memory store: return fresh state (not persisted)
        Ok(BudgetState::new())
    }

    fn update_budget_state(
        &self,
        _session_id: Uuid,
        _state: &BudgetState,
    ) -> Result<(), StoreError> {
        // In-memory store: no-op (not persisted)
        Ok(())
    }
}

pub struct SqliteEventStore {
    connection: Mutex<Connection>,
    notifier: broadcast::Sender<(Uuid, u64)>,
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

        // Budget state columns
        add_column_if_missing(&connection, "sessions", "turns_used", "INTEGER DEFAULT 0")?;
        add_column_if_missing(&connection, "sessions", "tokens_used", "INTEGER DEFAULT 0")?;
        add_column_if_missing(
            &connection,
            "sessions",
            "compaction_count",
            "INTEGER DEFAULT 0",
        )?;

        // Session DAG columns
        add_column_if_missing(&connection, "sessions", "parent_session_id", "TEXT")?;
        add_column_if_missing(&connection, "sessions", "fork_sequence", "INTEGER")?;

        connection.execute_batch(
            "INSERT OR IGNORE INTO sessions (id, created_at_unix_ms, updated_at_unix_ms)
             SELECT session_id, MIN(at_unix_ms), MAX(at_unix_ms) FROM events GROUP BY session_id;",
        )?;
        let (notifier, _) = broadcast::channel(100);
        Ok(Arc::new(Self {
            connection: Mutex::new(connection),
            notifier,
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
        let _ = self.notifier.send((event.session_id, event.sequence));
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
        let _ = self.notifier.send((event.session_id, event.sequence));
        Ok(())
    }

    fn append_next(&self, session_id: Uuid, payload: EventPayload) -> Result<Event, StoreError> {
        // Get logical last sequence by reading full history (prefix + suffix)
        let current_events = self.list(session_id)?;
        let last_sequence = current_events.last().map(|e| e.sequence).unwrap_or(0);
        let next_sequence = last_sequence + 1;

        let mut conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

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
        let _ = self.notifier.send((event.session_id, event.sequence));
        Ok(event)
    }

    fn list(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError> {
        let conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;

        // Build ancestry chain: current session <- parent <- grandparent ...
        let mut ancestry = Vec::new();
        let mut current_id = session_id;
        let mut current_fork_seq: Option<u64> = None;

        loop {
            let parent_info: Option<(String, u64)> = conn
                .query_row(
                    "SELECT parent_session_id, fork_sequence FROM sessions WHERE id = ?1",
                    params![current_id.to_string()],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<u64>>(1)?,
                        ))
                    },
                )
                .optional()?
                .and_then(|(p, f)| p.zip(f));

            if let Some((parent_id_str, fork_seq)) = parent_info {
                let parent_id =
                    Uuid::parse_str(&parent_id_str).map_err(|_| StoreError::InvalidUuid {
                        field: "parent_session_id",
                        value: parent_id_str.to_string(),
                    })?;
                ancestry.push((current_id, current_fork_seq));
                current_id = parent_id;
                current_fork_seq = Some(fork_seq);
            } else {
                // Root session
                ancestry.push((current_id, current_fork_seq));
                break;
            }
        }

        // Reverse to get root -> ... -> current
        ancestry.reverse();

        // Collect events: for each ancestor, read events up to fork_sequence (or all for current)
        let mut all_events = Vec::new();
        let mut logical_sequence = 0u64;

        for (idx, (sid, up_to_seq_opt)) in ancestry.iter().enumerate() {
            let is_current = idx == ancestry.len() - 1;
            let events_query = if is_current {
                // Current session: read all its own events
                let mut stmt = conn.prepare(
                    "SELECT id, session_id, sequence, at_unix_ms, kind_json, body_json, schema_version, payload_json \
                     FROM events WHERE session_id = ?1 ORDER BY sequence"
                )?;
                stmt.query_map([sid.to_string()], |row| {
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
                })?
                .collect::<Result<Vec<_>, _>>()?
            } else {
                // Ancestor: read events up to fork_sequence
                let up_to = up_to_seq_opt.ok_or_else(|| StoreError::MissingSession(*sid))?;
                let mut stmt = conn.prepare(
                    "SELECT id, session_id, sequence, at_unix_ms, kind_json, body_json, schema_version, payload_json \
                     FROM events WHERE session_id = ?1 AND sequence <= ?2 ORDER BY sequence"
                )?;
                stmt.query_map(params![sid.to_string(), up_to], |row| {
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
                })?
                .collect::<Result<Vec<_>, _>>()?
            };

            for row in events_query {
                let mut event = decode_event(row)?;
                // Renumber to logical sequence
                logical_sequence += 1;
                event.sequence = logical_sequence;
                all_events.push(event);
            }
        }

        Ok(all_events)
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError> {
        let conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = conn.prepare(
            "SELECT id, created_at_unix_ms, updated_at_unix_ms, parent_session_id, fork_sequence \
             FROM sessions ORDER BY updated_at_unix_ms DESC, id",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<u64>>(4)?,
                ))
            })?
            .map(|row| {
                let (id, created_at_unix_ms, updated_at_unix_ms, parent_id_str, fork_sequence) =
                    row?;
                let id = Uuid::parse_str(&id).map_err(|_| StoreError::InvalidUuid {
                    field: "session id",
                    value: id,
                })?;
                let parent_session_id = parent_id_str
                    .map(|s| {
                        Uuid::parse_str(&s).map_err(|_| StoreError::InvalidUuid {
                            field: "parent_session_id",
                            value: s,
                        })
                    })
                    .transpose()?;
                Ok(SessionInfo {
                    id,
                    created_at_unix_ms,
                    updated_at_unix_ms,
                    parent_session_id,
                    fork_sequence,
                })
            })
            .collect()
    }

    fn fork_session(
        &self,
        source_session_id: Uuid,
        up_to_sequence: u64,
    ) -> Result<Uuid, StoreError> {
        let mut conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // Verify source session exists and has events up to checkpoint
        let exists: bool = transaction
            .query_row(
                "SELECT 1 FROM events WHERE session_id = ?1 AND sequence <= ?2 LIMIT 1",
                params![source_session_id.to_string(), up_to_sequence],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);

        if !exists {
            return Err(StoreError::MissingSession(source_session_id));
        }

        // Create new session with parent reference (no event copying, no SessionCreated)
        let new_session_id = Uuid::new_v4();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_millis() as u64;

        transaction.execute(
            "INSERT INTO sessions (id, created_at_unix_ms, updated_at_unix_ms, parent_session_id, fork_sequence) \
             VALUES (?1, ?2, ?2, ?3, ?4)",
            params![new_session_id.to_string(), now, source_session_id.to_string(), up_to_sequence],
        )?;

        transaction.commit()?;
        Ok(new_session_id)
    }

    fn delete_session(&self, session_id: Uuid) -> Result<(), StoreError> {
        let mut conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM events WHERE session_id = ?",
            [session_id.to_string()],
        )?;
        transaction.execute(
            "DELETE FROM sessions WHERE id = ?",
            [session_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn subscribe_notifications(&self) -> broadcast::Receiver<(Uuid, u64)> {
        self.notifier.subscribe()
    }

    fn get_budget_state(&self, session_id: Uuid) -> Result<BudgetState, StoreError> {
        let connection = self.connection.lock().unwrap();
        let mut stmt = connection.prepare(
            "SELECT turns_used, tokens_used, compaction_count, created_at_unix_ms FROM sessions WHERE id = ?",
        )?;
        let session_id_str = session_id.to_string();
        let result = stmt.query_row([&session_id_str], |row| {
            let turns_used: i64 = row.get(0)?;
            let tokens_used: i64 = row.get(1)?;
            let compaction_count: i64 = row.get(2)?;
            let created_at_unix_ms: i64 = row.get(3)?;

            let elapsed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64
                - created_at_unix_ms;

            let started_at =
                std::time::Instant::now() - std::time::Duration::from_millis(elapsed.max(0) as u64);

            Ok(BudgetState {
                turns_used: turns_used as u32,
                tokens_used: tokens_used as u64,
                compaction_count: compaction_count as u32,
                started_at,
            })
        });

        match result {
            Ok(state) => Ok(state),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(BudgetState::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn update_budget_state(&self, session_id: Uuid, state: &BudgetState) -> Result<(), StoreError> {
        let connection = self.connection.lock().unwrap();
        let session_id_str = session_id.to_string();
        connection.execute(
            "UPDATE sessions SET turns_used = ?, tokens_used = ?, compaction_count = ?, updated_at_unix_ms = ? WHERE id = ?",
            rusqlite::params![
                state.turns_used as i64,
                state.tokens_used as i64,
                state.compaction_count as i64,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as i64,
                session_id_str,
            ],
        )?;
        Ok(())
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
    use crate::{EventPayload, IntentEvent, NoticeEvent, SessionEvent};

    #[test]
    fn sqlite_events_survive_reopen() {
        let test_root =
            std::env::temp_dir().join(format!("impetus-sqlite-reopen-{}", Uuid::new_v4()));
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
        let test_root = std::env::temp_dir().join(format!("impetus-legacy-{}", Uuid::new_v4()));
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
        let test_root = std::env::temp_dir().join(format!("impetus-malformed-{}", Uuid::new_v4()));
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

    #[test]
    fn memory_store_fork_creates_independent_session() {
        let store = MemoryEventStore::default();
        let source_id = store.create_session().expect("create source session");

        store
            .append_next(
                source_id,
                EventPayload::Intent(IntentEvent {
                    text: "step1".into(),
                }),
            )
            .expect("append event 1");
        store
            .append_next(
                source_id,
                EventPayload::Intent(IntentEvent {
                    text: "step2".into(),
                }),
            )
            .expect("append event 2");
        store
            .append_next(
                source_id,
                EventPayload::Intent(IntentEvent {
                    text: "step3".into(),
                }),
            )
            .expect("append event 3");

        let forked_id = store
            .fork_session(source_id, 2)
            .expect("fork up to sequence 2");
        assert_ne!(source_id, forked_id);

        let forked_events = store.list(forked_id).expect("list forked events");
        assert_eq!(forked_events.len(), 2);
        assert!(matches!(
            &forked_events[0].payload,
            EventPayload::Session(SessionEvent::Created)
        ));
        assert!(
            matches!(&forked_events[1].payload, EventPayload::Intent(intent) if intent.text == "step1")
        );

        // Source session unchanged
        let source_events = store.list(source_id).expect("list source events");
        assert_eq!(source_events.len(), 4);
    }

    #[test]
    fn sqlite_store_fork_creates_independent_session() {
        let test_root = std::env::temp_dir().join(format!("impetus-fork-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&test_root).expect("create isolated test directory");
        let database = test_root.join("events.sqlite3");
        let store = SqliteEventStore::open(&database).expect("open sqlite event store");

        let source_id = store.create_session().expect("create source session");
        store
            .append_next(
                source_id,
                EventPayload::Intent(IntentEvent {
                    text: "step1".into(),
                }),
            )
            .expect("append event 1");
        store
            .append_next(
                source_id,
                EventPayload::Intent(IntentEvent {
                    text: "step2".into(),
                }),
            )
            .expect("append event 2");
        store
            .append_next(
                source_id,
                EventPayload::Intent(IntentEvent {
                    text: "step3".into(),
                }),
            )
            .expect("append event 3");

        let forked_id = store
            .fork_session(source_id, 2)
            .expect("fork up to sequence 2");
        assert_ne!(source_id, forked_id);

        let forked_events = store.list(forked_id).expect("list forked events");
        assert_eq!(forked_events.len(), 2);
        assert!(matches!(
            &forked_events[0].payload,
            EventPayload::Session(SessionEvent::Created)
        ));
        assert!(
            matches!(&forked_events[1].payload, EventPayload::Intent(intent) if intent.text == "step1")
        );

        // Forked session appears in session list
        let sessions = store.list_sessions().expect("list sessions");
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().any(|s| s.id == forked_id));

        std::fs::remove_dir_all(test_root).expect("remove isolated test directory");
    }

    #[test]
    fn fork_nonexistent_session_returns_error() {
        let store = MemoryEventStore::default();
        let nonexistent = Uuid::new_v4();
        assert!(matches!(
            store.fork_session(nonexistent, 1),
            Err(StoreError::MissingSession(id)) if id == nonexistent
        ));
    }

    #[test]
    fn fork_bounded_memory_does_not_copy_full_history() {
        let store = MemoryEventStore::default();
        let source_id = store.create_session().expect("create source session");

        // Simulate large history
        for i in 1..=100 {
            store
                .append_next(
                    source_id,
                    EventPayload::Intent(IntentEvent {
                        text: format!("event{}", i),
                    }),
                )
                .expect("append event");
        }

        let source_events = store.list(source_id).expect("list source");
        assert_eq!(source_events.len(), 101); // Created + 100 intents

        // Fork only first 10 events
        let forked_id = store
            .fork_session(source_id, 10)
            .expect("fork with bounded history");
        let forked_events = store.list(forked_id).expect("list forked");

        // Forked session has only 10 events, not 101
        assert_eq!(forked_events.len(), 10);
        assert!(
            matches!(&forked_events[9].payload, EventPayload::Intent(intent) if intent.text == "event9")
        );
    }
}
