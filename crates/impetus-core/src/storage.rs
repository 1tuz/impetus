use crate::budget::BudgetState;
use crate::events::{Event, EventPayload, SessionEvent, legacy_payload};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    #[error("checkpoint `{0}` does not exist")]
    MissingCheckpoint(Uuid),
    #[error("sequence {sequence} is outside session `{session_id}` history (head {head_sequence})")]
    InvalidForkSequence {
        session_id: Uuid,
        sequence: u64,
        head_sequence: u64,
    },
    #[error(
        "event sequence conflict for session `{session_id}`: expected {expected}, got {actual}"
    )]
    SequenceConflict {
        session_id: Uuid,
        expected: u64,
        actual: u64,
    },
    #[error("session `{0}` has child branches and cannot be deleted")]
    SessionHasChildren(Uuid),
    #[error("session ancestry exceeds the supported depth of {0}")]
    AncestryTooDeep(usize),
    #[error("{kind} name must contain 1 to 128 bytes")]
    InvalidName { kind: &'static str },
    #[error("{kind} name `{name}` already exists in session `{session_id}`")]
    NameConflict {
        kind: &'static str,
        name: String,
        session_id: Uuid,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: Uuid,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub parent_session_id: Option<Uuid>,
    pub fork_sequence: Option<u64>,
    pub head_sequence: u64,
    pub branch_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionCheckpoint {
    pub id: Uuid,
    pub session_id: Uuid,
    pub name: String,
    pub sequence: u64,
    pub created_at_unix_ms: u64,
}

const MAX_SESSION_ANCESTRY_DEPTH: usize = 256;
const MAX_BRANCH_NAME_BYTES: usize = 128;
const MAX_CHECKPOINT_NAME_BYTES: usize = 128;

pub trait EventStore: Send + Sync {
    fn create_session(&self) -> Result<Uuid, StoreError>;
    fn append(&self, event: &Event) -> Result<(), StoreError>;
    fn append_next(&self, session_id: Uuid, payload: EventPayload) -> Result<Event, StoreError>;
    fn list(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError>;
    fn list_range(
        &self,
        session_id: Uuid,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<Event>, StoreError>;
    fn session_info(&self, session_id: Uuid) -> Result<SessionInfo, StoreError>;
    fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError>;

    /// Delete session and all its events.
    /// Returns Ok(()) even if session doesn't exist (idempotent).
    fn delete_session(&self, session_id: Uuid) -> Result<(), StoreError>;

    /// Fork session up to given sequence number (inclusive), sharing its immutable prefix.
    fn fork_session(
        &self,
        source_session_id: Uuid,
        up_to_sequence: u64,
    ) -> Result<Uuid, StoreError> {
        self.fork_session_named(source_session_id, up_to_sequence, None)
    }

    fn fork_session_named(
        &self,
        source_session_id: Uuid,
        up_to_sequence: u64,
        branch_name: Option<String>,
    ) -> Result<Uuid, StoreError>;

    fn create_checkpoint(
        &self,
        session_id: Uuid,
        name: &str,
        sequence: Option<u64>,
    ) -> Result<SessionCheckpoint, StoreError>;

    fn list_checkpoints(&self, session_id: Uuid) -> Result<Vec<SessionCheckpoint>, StoreError>;

    /// Restore/revert never rewrites the source. It returns a new branch head.
    fn restore_checkpoint(
        &self,
        checkpoint_id: Uuid,
        branch_name: Option<String>,
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

#[derive(Default)]
struct MemoryStoreState {
    events: Vec<Event>,
    sessions: HashMap<Uuid, SessionInfo>,
    checkpoints: HashMap<Uuid, SessionCheckpoint>,
}

pub struct MemoryEventStore {
    state: Mutex<MemoryStoreState>,
    notifier: broadcast::Sender<(Uuid, u64)>,
}

impl Default for MemoryEventStore {
    fn default() -> Self {
        let (notifier, _) = broadcast::channel(100);
        Self {
            state: Mutex::default(),
            notifier,
        }
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis() as u64
}

fn validate_optional_name(
    name: Option<String>,
    kind: &'static str,
    max_bytes: usize,
) -> Result<Option<String>, StoreError> {
    match name {
        Some(name) if name.trim().is_empty() || name.len() > max_bytes => {
            Err(StoreError::InvalidName { kind })
        }
        Some(name) => Ok(Some(name)),
        None => Ok(None),
    }
}

fn memory_logical_events(
    state: &MemoryStoreState,
    session_id: Uuid,
) -> Result<Vec<Event>, StoreError> {
    let Some(mut session) = state.sessions.get(&session_id) else {
        return Ok(Vec::new());
    };
    let mut lineage = Vec::new();
    let mut visible_through = session.head_sequence;
    let mut visited = HashSet::new();
    loop {
        if lineage.len() >= MAX_SESSION_ANCESTRY_DEPTH || !visited.insert(session.id) {
            return Err(StoreError::AncestryTooDeep(MAX_SESSION_ANCESTRY_DEPTH));
        }
        lineage.push((session.id, visible_through));
        let (Some(parent_id), Some(fork_sequence)) =
            (session.parent_session_id, session.fork_sequence)
        else {
            break;
        };
        session = state
            .sessions
            .get(&parent_id)
            .ok_or(StoreError::MissingSession(parent_id))?;
        visible_through = visible_through
            .min(fork_sequence)
            .min(session.head_sequence);
    }

    let mut events = lineage
        .into_iter()
        .flat_map(|(ancestor_id, limit)| {
            state
                .events
                .iter()
                .filter(move |event| event.session_id == ancestor_id && event.sequence <= limit)
                .cloned()
        })
        .collect::<Vec<_>>();
    events.sort_by_key(|event| event.sequence);
    for event in &mut events {
        event.session_id = session_id;
    }
    Ok(events)
}

fn validate_fork_sequence(session: &SessionInfo, sequence: u64) -> Result<(), StoreError> {
    if sequence == 0 || sequence > session.head_sequence {
        return Err(StoreError::InvalidForkSequence {
            session_id: session.id,
            sequence,
            head_sequence: session.head_sequence,
        });
    }
    Ok(())
}

impl EventStore for MemoryEventStore {
    fn create_session(&self) -> Result<Uuid, StoreError> {
        let session_id = Uuid::new_v4();
        let event = Event::new(session_id, 1, EventPayload::Session(SessionEvent::Created));
        let mut state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        state.sessions.insert(
            session_id,
            SessionInfo {
                id: session_id,
                created_at_unix_ms: event.at_unix_ms,
                updated_at_unix_ms: event.at_unix_ms,
                parent_session_id: None,
                fork_sequence: None,
                head_sequence: 1,
                branch_name: None,
            },
        );
        state.events.push(event);
        Ok(session_id)
    }

    fn append(&self, event: &Event) -> Result<(), StoreError> {
        let mut state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        let expected = state
            .sessions
            .get(&event.session_id)
            .map_or(1, |session| session.head_sequence + 1);
        if event.sequence != expected {
            return Err(StoreError::SequenceConflict {
                session_id: event.session_id,
                expected,
                actual: event.sequence,
            });
        }
        let session = state
            .sessions
            .entry(event.session_id)
            .or_insert_with(|| SessionInfo {
                id: event.session_id,
                created_at_unix_ms: event.at_unix_ms,
                updated_at_unix_ms: event.at_unix_ms,
                parent_session_id: None,
                fork_sequence: None,
                head_sequence: 0,
                branch_name: None,
            });
        session.head_sequence = event.sequence;
        session.updated_at_unix_ms = session.updated_at_unix_ms.max(event.at_unix_ms);
        state.events.push(event.clone());
        let _ = self.notifier.send((event.session_id, event.sequence));
        Ok(())
    }

    fn append_next(&self, session_id: Uuid, payload: EventPayload) -> Result<Event, StoreError> {
        let mut state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        let session = state
            .sessions
            .get_mut(&session_id)
            .ok_or(StoreError::MissingSession(session_id))?;
        let sequence = session.head_sequence + 1;
        let event = Event::new(session_id, sequence, payload);
        session.head_sequence = sequence;
        session.updated_at_unix_ms = session.updated_at_unix_ms.max(event.at_unix_ms);
        state.events.push(event.clone());
        let _ = self.notifier.send((event.session_id, event.sequence));
        Ok(event)
    }

    fn list(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError> {
        let state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        memory_logical_events(&state, session_id)
    }

    fn list_range(
        &self,
        session_id: Uuid,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<Event>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(memory_logical_events(&state, session_id)?
            .into_iter()
            .filter(|event| event.sequence > after_sequence)
            .take(limit)
            .collect())
    }

    fn session_info(&self, session_id: Uuid) -> Result<SessionInfo, StoreError> {
        self.state
            .lock()
            .map_err(|_| StoreError::Poisoned)?
            .sessions
            .get(&session_id)
            .cloned()
            .ok_or(StoreError::MissingSession(session_id))
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError> {
        let mut sessions = self
            .state
            .lock()
            .map_err(|_| StoreError::Poisoned)?
            .sessions
            .values()
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            right
                .updated_at_unix_ms
                .cmp(&left.updated_at_unix_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(sessions)
    }

    fn fork_session_named(
        &self,
        source_session_id: Uuid,
        up_to_sequence: u64,
        branch_name: Option<String>,
    ) -> Result<Uuid, StoreError> {
        let branch_name = validate_optional_name(branch_name, "branch", MAX_BRANCH_NAME_BYTES)?;
        let mut state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        let source = state
            .sessions
            .get(&source_session_id)
            .cloned()
            .ok_or(StoreError::MissingSession(source_session_id))?;
        validate_fork_sequence(&source, up_to_sequence)?;
        let mut depth = 1;
        let mut current = source.parent_session_id;
        while let Some(parent_id) = current {
            depth += 1;
            if depth >= MAX_SESSION_ANCESTRY_DEPTH {
                return Err(StoreError::AncestryTooDeep(MAX_SESSION_ANCESTRY_DEPTH));
            }
            current = state
                .sessions
                .get(&parent_id)
                .ok_or(StoreError::MissingSession(parent_id))?
                .parent_session_id;
        }
        let new_session_id = Uuid::new_v4();
        let now = now_unix_ms();
        state.sessions.insert(
            new_session_id,
            SessionInfo {
                id: new_session_id,
                created_at_unix_ms: now,
                updated_at_unix_ms: now,
                parent_session_id: Some(source_session_id),
                fork_sequence: Some(up_to_sequence),
                head_sequence: up_to_sequence,
                branch_name,
            },
        );
        Ok(new_session_id)
    }

    fn create_checkpoint(
        &self,
        session_id: Uuid,
        name: &str,
        sequence: Option<u64>,
    ) -> Result<SessionCheckpoint, StoreError> {
        let name = validate_optional_name(
            Some(name.to_owned()),
            "checkpoint",
            MAX_CHECKPOINT_NAME_BYTES,
        )?
        .expect("validated checkpoint name");
        let mut state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        let session = state
            .sessions
            .get(&session_id)
            .ok_or(StoreError::MissingSession(session_id))?;
        let sequence = sequence.unwrap_or(session.head_sequence);
        validate_fork_sequence(session, sequence)?;
        if state
            .checkpoints
            .values()
            .any(|checkpoint| checkpoint.session_id == session_id && checkpoint.name == name)
        {
            return Err(StoreError::NameConflict {
                kind: "checkpoint",
                name,
                session_id,
            });
        }
        let checkpoint = SessionCheckpoint {
            id: Uuid::new_v4(),
            session_id,
            name,
            sequence,
            created_at_unix_ms: now_unix_ms(),
        };
        state.checkpoints.insert(checkpoint.id, checkpoint.clone());
        Ok(checkpoint)
    }

    fn list_checkpoints(&self, session_id: Uuid) -> Result<Vec<SessionCheckpoint>, StoreError> {
        let state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        if !state.sessions.contains_key(&session_id) {
            return Err(StoreError::MissingSession(session_id));
        }
        let mut checkpoints = state
            .checkpoints
            .values()
            .filter(|checkpoint| checkpoint.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        checkpoints.sort_by_key(|checkpoint| (checkpoint.sequence, checkpoint.created_at_unix_ms));
        Ok(checkpoints)
    }

    fn restore_checkpoint(
        &self,
        checkpoint_id: Uuid,
        branch_name: Option<String>,
    ) -> Result<Uuid, StoreError> {
        let checkpoint = self
            .state
            .lock()
            .map_err(|_| StoreError::Poisoned)?
            .checkpoints
            .get(&checkpoint_id)
            .cloned()
            .ok_or(StoreError::MissingCheckpoint(checkpoint_id))?;
        self.fork_session_named(checkpoint.session_id, checkpoint.sequence, branch_name)
    }

    fn delete_session(&self, session_id: Uuid) -> Result<(), StoreError> {
        let mut state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        if state
            .sessions
            .values()
            .any(|session| session.parent_session_id == Some(session_id))
        {
            return Err(StoreError::SessionHasChildren(session_id));
        }
        state.events.retain(|event| event.session_id != session_id);
        state
            .checkpoints
            .retain(|_, checkpoint| checkpoint.session_id != session_id);
        state.sessions.remove(&session_id);
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
                id TEXT PRIMARY KEY,
                created_at_unix_ms INTEGER NOT NULL,
                updated_at_unix_ms INTEGER NOT NULL,
                parent_session_id TEXT REFERENCES sessions(id) ON DELETE RESTRICT,
                fork_sequence INTEGER,
                head_sequence INTEGER NOT NULL DEFAULT 0,
                branch_name TEXT
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
        add_column_if_missing(&connection, "sessions", "parent_session_id", "TEXT")?;
        add_column_if_missing(&connection, "sessions", "fork_sequence", "INTEGER")?;
        add_column_if_missing(
            &connection,
            "sessions",
            "head_sequence",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        add_column_if_missing(&connection, "sessions", "branch_name", "TEXT")?;

        connection.execute_batch(
            "INSERT OR IGNORE INTO sessions (id, created_at_unix_ms, updated_at_unix_ms)
             SELECT session_id, MIN(at_unix_ms), MAX(at_unix_ms) FROM events GROUP BY session_id;
             UPDATE sessions
             SET head_sequence = COALESCE(
                (SELECT MAX(events.sequence) FROM events WHERE events.session_id = sessions.id),
                head_sequence
             )
             WHERE parent_session_id IS NULL;
             CREATE TABLE IF NOT EXISTS session_checkpoints (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                sequence INTEGER NOT NULL,
                created_at_unix_ms INTEGER NOT NULL,
                UNIQUE(session_id, name)
             );
             CREATE INDEX IF NOT EXISTS sessions_parent_idx ON sessions(parent_session_id);
             CREATE INDEX IF NOT EXISTS checkpoints_session_sequence_idx
                ON session_checkpoints(session_id, sequence);",
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
            "INSERT INTO sessions (id, created_at_unix_ms, updated_at_unix_ms, head_sequence)
             VALUES (?1, ?2, ?2, 1)",
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
        let session_id = event.session_id.to_string();
        let head_sequence = transaction
            .query_row(
                "SELECT head_sequence FROM sessions WHERE id = ?1",
                [&session_id],
                |row| row.get::<_, u64>(0),
            )
            .optional()?;
        let head_sequence = match head_sequence {
            Some(head_sequence) => head_sequence,
            None => {
                transaction.execute(
                    "INSERT INTO sessions
                     (id, created_at_unix_ms, updated_at_unix_ms, head_sequence)
                     VALUES (?1, ?2, ?2, 0)",
                    params![session_id, event.at_unix_ms],
                )?;
                0
            }
        };
        let expected = head_sequence + 1;
        if event.sequence != expected {
            return Err(StoreError::SequenceConflict {
                session_id: event.session_id,
                expected,
                actual: event.sequence,
            });
        }
        insert_event(&transaction, event)?;
        transaction.execute(
            "UPDATE sessions
             SET updated_at_unix_ms = MAX(updated_at_unix_ms, ?2), head_sequence = ?3
             WHERE id = ?1",
            params![session_id, event.at_unix_ms, event.sequence],
        )?;
        transaction.commit()?;
        let _ = self.notifier.send((event.session_id, event.sequence));
        Ok(())
    }

    fn append_next(&self, session_id: Uuid, payload: EventPayload) -> Result<Event, StoreError> {
        let mut conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session_id_text = session_id.to_string();
        let next_sequence = transaction
            .query_row(
                "SELECT head_sequence + 1 FROM sessions WHERE id = ?1",
                [&session_id_text],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .ok_or(StoreError::MissingSession(session_id))?;
        let event = Event::new(session_id, next_sequence, payload);
        insert_event(&transaction, &event)?;
        transaction.execute(
            "UPDATE sessions
             SET updated_at_unix_ms = MAX(updated_at_unix_ms, ?2), head_sequence = ?3
             WHERE id = ?1",
            params![session_id_text, event.at_unix_ms, event.sequence],
        )?;
        transaction.commit()?;
        let _ = self.notifier.send((event.session_id, event.sequence));
        Ok(event)
    }

    fn list(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError> {
        let conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        query_logical_events(&conn, session_id, 0, usize::MAX)
    }

    fn list_range(
        &self,
        session_id: Uuid,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<Event>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        query_logical_events(&conn, session_id, after_sequence, limit)
    }

    fn session_info(&self, session_id: Uuid) -> Result<SessionInfo, StoreError> {
        let conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        query_session_info(&conn, session_id)?.ok_or(StoreError::MissingSession(session_id))
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError> {
        let conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = conn.prepare(
            "SELECT id, created_at_unix_ms, updated_at_unix_ms, parent_session_id,
                    fork_sequence, head_sequence, branch_name
             FROM sessions ORDER BY updated_at_unix_ms DESC, id",
        )?;
        statement
            .query_map([], session_row)?
            .map(|row| row.map_err(StoreError::from).and_then(decode_session_info))
            .collect()
    }

    fn fork_session_named(
        &self,
        source_session_id: Uuid,
        up_to_sequence: u64,
        branch_name: Option<String>,
    ) -> Result<Uuid, StoreError> {
        let branch_name = validate_optional_name(branch_name, "branch", MAX_BRANCH_NAME_BYTES)?;
        let mut conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let new_session_id =
            insert_fork(&transaction, source_session_id, up_to_sequence, branch_name)?;
        transaction.commit()?;
        Ok(new_session_id)
    }

    fn create_checkpoint(
        &self,
        session_id: Uuid,
        name: &str,
        sequence: Option<u64>,
    ) -> Result<SessionCheckpoint, StoreError> {
        let name = validate_optional_name(
            Some(name.to_owned()),
            "checkpoint",
            MAX_CHECKPOINT_NAME_BYTES,
        )?
        .expect("validated checkpoint name");
        let mut conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = query_session_info(&transaction, session_id)?
            .ok_or(StoreError::MissingSession(session_id))?;
        let sequence = sequence.unwrap_or(session.head_sequence);
        validate_fork_sequence(&session, sequence)?;
        let checkpoint = SessionCheckpoint {
            id: Uuid::new_v4(),
            session_id,
            name,
            sequence,
            created_at_unix_ms: now_unix_ms(),
        };
        if transaction.execute(
            "INSERT OR IGNORE INTO session_checkpoints
                 (id, session_id, name, sequence, created_at_unix_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                checkpoint.id.to_string(),
                checkpoint.session_id.to_string(),
                checkpoint.name,
                checkpoint.sequence,
                checkpoint.created_at_unix_ms
            ],
        )? == 0
        {
            return Err(StoreError::NameConflict {
                kind: "checkpoint",
                name: checkpoint.name,
                session_id,
            });
        }
        transaction.commit()?;
        Ok(checkpoint)
    }

    fn list_checkpoints(&self, session_id: Uuid) -> Result<Vec<SessionCheckpoint>, StoreError> {
        let conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        if query_session_info(&conn, session_id)?.is_none() {
            return Err(StoreError::MissingSession(session_id));
        }
        let mut statement = conn.prepare(
            "SELECT id, session_id, name, sequence, created_at_unix_ms
             FROM session_checkpoints WHERE session_id = ?1
             ORDER BY sequence, created_at_unix_ms, id",
        )?;
        statement
            .query_map([session_id.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .map(|row| row.map_err(StoreError::from).and_then(decode_checkpoint))
            .collect()
    }

    fn restore_checkpoint(
        &self,
        checkpoint_id: Uuid,
        branch_name: Option<String>,
    ) -> Result<Uuid, StoreError> {
        let branch_name = validate_optional_name(branch_name, "branch", MAX_BRANCH_NAME_BYTES)?;
        let mut conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let checkpoint = transaction
            .query_row(
                "SELECT id, session_id, name, sequence, created_at_unix_ms
                 FROM session_checkpoints WHERE id = ?1",
                [checkpoint_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?
            .map(decode_checkpoint)
            .transpose()?
            .ok_or(StoreError::MissingCheckpoint(checkpoint_id))?;
        let new_session_id = insert_fork(
            &transaction,
            checkpoint.session_id,
            checkpoint.sequence,
            branch_name,
        )?;
        transaction.commit()?;
        Ok(new_session_id)
    }

    fn delete_session(&self, session_id: Uuid) -> Result<(), StoreError> {
        let mut conn = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let child_count: u64 = transaction.query_row(
            "SELECT COUNT(*) FROM sessions WHERE parent_session_id = ?1",
            [session_id.to_string()],
            |row| row.get(0),
        )?;
        if child_count != 0 {
            return Err(StoreError::SessionHasChildren(session_id));
        }
        transaction.execute(
            "DELETE FROM session_checkpoints WHERE session_id = ?1",
            [session_id.to_string()],
        )?;
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

type StoredSessionRow = (
    String,
    u64,
    u64,
    Option<String>,
    Option<u64>,
    u64,
    Option<String>,
);

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredSessionRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

fn decode_session_info(row: StoredSessionRow) -> Result<SessionInfo, StoreError> {
    let (
        id,
        created_at_unix_ms,
        updated_at_unix_ms,
        parent_session_id,
        fork_sequence,
        head_sequence,
        branch_name,
    ) = row;
    let id = Uuid::parse_str(&id).map_err(|_| StoreError::InvalidUuid {
        field: "session id",
        value: id,
    })?;
    let parent_session_id = parent_session_id
        .map(|parent| {
            Uuid::parse_str(&parent).map_err(|_| StoreError::InvalidUuid {
                field: "parent session id",
                value: parent,
            })
        })
        .transpose()?;
    Ok(SessionInfo {
        id,
        created_at_unix_ms,
        updated_at_unix_ms,
        parent_session_id,
        fork_sequence,
        head_sequence,
        branch_name,
    })
}

fn query_session_info(
    connection: &Connection,
    session_id: Uuid,
) -> Result<Option<SessionInfo>, StoreError> {
    connection
        .query_row(
            "SELECT id, created_at_unix_ms, updated_at_unix_ms, parent_session_id,
                    fork_sequence, head_sequence, branch_name
             FROM sessions WHERE id = ?1",
            [session_id.to_string()],
            session_row,
        )
        .optional()?
        .map(decode_session_info)
        .transpose()
}

fn query_logical_events(
    connection: &Connection,
    session_id: Uuid,
    after_sequence: u64,
    limit: usize,
) -> Result<Vec<Event>, StoreError> {
    let sql = "WITH RECURSIVE lineage(session_id, visible_through, depth) AS (
                 SELECT id, head_sequence, 1 FROM sessions WHERE id = ?1
                 UNION ALL
                 SELECT parent.id,
                        MIN(lineage.visible_through, current.fork_sequence, parent.head_sequence),
                        lineage.depth + 1
                 FROM lineage
                 JOIN sessions AS current ON current.id = lineage.session_id
                 JOIN sessions AS parent ON parent.id = current.parent_session_id
                 WHERE lineage.depth < ?4
               )
               SELECT events.id, events.session_id, events.sequence, events.at_unix_ms,
                      events.kind_json, events.body_json, events.schema_version, events.payload_json
               FROM lineage
               JOIN events ON events.session_id = lineage.session_id
               WHERE events.sequence <= lineage.visible_through AND events.sequence > ?2
               ORDER BY events.sequence
               LIMIT ?3";
    let bounded_limit = limit.min(i64::MAX as usize) as i64;
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(
        params![
            session_id.to_string(),
            after_sequence,
            bounded_limit,
            MAX_SESSION_ANCESTRY_DEPTH as u64
        ],
        |row| {
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
        },
    )?;
    let mut events = rows
        .map(|row| row.map_err(StoreError::from).and_then(decode_event))
        .collect::<Result<Vec<_>, _>>()?;
    for event in &mut events {
        event.session_id = session_id;
    }
    Ok(events)
}

fn insert_fork(
    transaction: &rusqlite::Transaction<'_>,
    source_session_id: Uuid,
    up_to_sequence: u64,
    branch_name: Option<String>,
) -> Result<Uuid, StoreError> {
    let source = query_session_info(transaction, source_session_id)?
        .ok_or(StoreError::MissingSession(source_session_id))?;
    validate_fork_sequence(&source, up_to_sequence)?;
    let depth: u64 = transaction.query_row(
        "WITH RECURSIVE lineage(id, parent_session_id, depth) AS (
           SELECT id, parent_session_id, 1 FROM sessions WHERE id = ?1
           UNION ALL
           SELECT parent.id, parent.parent_session_id, lineage.depth + 1
           FROM lineage JOIN sessions AS parent ON parent.id = lineage.parent_session_id
           WHERE lineage.depth < ?2
         )
         SELECT COALESCE(MAX(depth), 0) FROM lineage",
        params![
            source_session_id.to_string(),
            MAX_SESSION_ANCESTRY_DEPTH as u64
        ],
        |row| row.get(0),
    )?;
    if depth >= MAX_SESSION_ANCESTRY_DEPTH as u64 {
        return Err(StoreError::AncestryTooDeep(MAX_SESSION_ANCESTRY_DEPTH));
    }
    let new_session_id = Uuid::new_v4();
    let now = now_unix_ms();
    transaction.execute(
        "INSERT INTO sessions
         (id, created_at_unix_ms, updated_at_unix_ms, parent_session_id,
          fork_sequence, head_sequence, branch_name)
         VALUES (?1, ?2, ?2, ?3, ?4, ?4, ?5)",
        params![
            new_session_id.to_string(),
            now,
            source_session_id.to_string(),
            up_to_sequence,
            branch_name
        ],
    )?;
    Ok(new_session_id)
}

type StoredCheckpointRow = (String, String, String, u64, u64);

fn decode_checkpoint(row: StoredCheckpointRow) -> Result<SessionCheckpoint, StoreError> {
    let (id, session_id, name, sequence, created_at_unix_ms) = row;
    let id = Uuid::parse_str(&id).map_err(|_| StoreError::InvalidUuid {
        field: "checkpoint id",
        value: id,
    })?;
    let session_id = Uuid::parse_str(&session_id).map_err(|_| StoreError::InvalidUuid {
        field: "checkpoint session id",
        value: session_id,
    })?;
    Ok(SessionCheckpoint {
        id,
        session_id,
        name,
        sequence,
        created_at_unix_ms,
    })
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
                Err(StoreError::SequenceConflict { .. })
            ));
        }
        {
            let reopened = SqliteEventStore::open(&database).expect("reopen sqlite event store");
            assert_eq!(reopened.list(session_id).expect("list events"), vec![event]);
        }

        std::fs::remove_dir_all(test_root).expect("remove isolated test directory");
    }

    #[test]
    fn rejected_memory_append_does_not_create_ghost_session() {
        let store = MemoryEventStore::default();
        let session_id = Uuid::new_v4();
        let event = Event::new(
            session_id,
            2,
            EventPayload::Notice(NoticeEvent::Runtime {
                message: "out of sequence".into(),
            }),
        );

        assert!(matches!(
            store.append(&event),
            Err(StoreError::SequenceConflict {
                session_id: actual,
                expected: 1,
                actual: 2,
            }) if actual == session_id
        ));
        assert!(matches!(
            store.session_info(session_id),
            Err(StoreError::MissingSession(actual)) if actual == session_id
        ));
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
    fn legacy_copied_forks_migrate_as_independent_roots_without_data_loss() {
        let test_root =
            std::env::temp_dir().join(format!("impetus-legacy-forks-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&test_root).expect("create isolated test directory");
        let database = test_root.join("events.sqlite3");
        let source = Uuid::new_v4();
        let copied_fork = Uuid::new_v4();
        {
            let connection = Connection::open(&database).expect("open legacy database");
            connection
                .execute_batch(
                    "CREATE TABLE events (
                       id TEXT PRIMARY KEY, session_id TEXT NOT NULL, sequence INTEGER NOT NULL,
                       at_unix_ms INTEGER NOT NULL, kind_json TEXT NOT NULL, body_json TEXT NOT NULL
                     );
                     CREATE TABLE sessions (
                       id TEXT PRIMARY KEY, created_at_unix_ms INTEGER NOT NULL,
                       updated_at_unix_ms INTEGER NOT NULL
                     );",
                )
                .expect("create legacy schema");
            for session_id in [source, copied_fork] {
                connection
                    .execute(
                        "INSERT INTO sessions VALUES (?1, 1, 2)",
                        [session_id.to_string()],
                    )
                    .expect("insert legacy session");
                for (sequence, text) in [(1, "created"), (2, "shared copy")] {
                    connection
                        .execute(
                            "INSERT INTO events VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
                            params![
                                Uuid::new_v4().to_string(),
                                session_id.to_string(),
                                sequence,
                                "\"user_intent\"",
                                serde_json::json!({ "text": text }).to_string()
                            ],
                        )
                        .expect("insert copied legacy event");
                }
            }
        }

        let store = SqliteEventStore::open(&database).expect("migrate legacy database");
        for session_id in [source, copied_fork] {
            let info = store.session_info(session_id).expect("migrated metadata");
            assert_eq!(info.parent_session_id, None);
            assert_eq!(info.head_sequence, 2);
            assert_eq!(store.list(session_id).expect("migrated history").len(), 2);
        }
        let physical_events: u64 = store
            .connection
            .lock()
            .expect("lock connection")
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .expect("count migrated events");
        assert_eq!(physical_events, 4);

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

    #[test]
    fn sqlite_fork_shares_prefix_and_branches_diverge() {
        let test_root =
            std::env::temp_dir().join(format!("impetus-shared-prefix-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&test_root).expect("create isolated test directory");
        let database = test_root.join("events.sqlite3");
        let store = SqliteEventStore::open(&database).expect("open sqlite event store");

        let source = store.create_session().expect("create source");
        for text in ["one", "two", "three"] {
            store
                .append_next(
                    source,
                    EventPayload::Intent(IntentEvent { text: text.into() }),
                )
                .expect("append source event");
        }
        let left = store
            .fork_session_named(source, 3, Some("left".into()))
            .expect("fork left");
        let right = store
            .fork_session_named(source, 3, Some("right".into()))
            .expect("fork right");

        store
            .append_next(
                left,
                EventPayload::Intent(IntentEvent {
                    text: "left-only".into(),
                }),
            )
            .expect("append left suffix");
        store
            .append_next(
                right,
                EventPayload::Intent(IntentEvent {
                    text: "right-only".into(),
                }),
            )
            .expect("append right suffix");

        let physical_events: u64 = store
            .connection
            .lock()
            .expect("lock connection")
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .expect("count physical events");
        assert_eq!(physical_events, 6, "forks store suffixes only");

        let left_events = store.list(left).expect("left logical history");
        let right_events = store.list(right).expect("right logical history");
        assert_eq!(left_events.len(), 4);
        assert_eq!(right_events.len(), 4);
        assert!(left_events.iter().all(|event| event.session_id == left));
        assert!(right_events.iter().all(|event| event.session_id == right));
        assert!(matches!(
            &left_events[3].payload,
            EventPayload::Intent(intent) if intent.text == "left-only"
        ));
        assert!(matches!(
            &right_events[3].payload,
            EventPayload::Intent(intent) if intent.text == "right-only"
        ));

        let left_info = store.session_info(left).expect("left metadata");
        assert_eq!(left_info.parent_session_id, Some(source));
        assert_eq!(left_info.fork_sequence, Some(3));
        assert_eq!(left_info.branch_name.as_deref(), Some("left"));
        assert_eq!(left_info.head_sequence, 4);

        std::fs::remove_dir_all(test_root).expect("remove isolated test directory");
    }

    #[test]
    fn checkpoint_restore_is_append_only_and_survives_restart() {
        let test_root = std::env::temp_dir().join(format!("impetus-checkpoint-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&test_root).expect("create isolated test directory");
        let database = test_root.join("events.sqlite3");
        let (source, restored, checkpoint_id);
        {
            let store = SqliteEventStore::open(&database).expect("open sqlite event store");
            source = store.create_session().expect("create source");
            store
                .append_next(
                    source,
                    EventPayload::Intent(IntentEvent {
                        text: "keep".into(),
                    }),
                )
                .expect("append checkpoint event");
            let checkpoint = store
                .create_checkpoint(source, "before experiment", Some(2))
                .expect("create checkpoint");
            checkpoint_id = checkpoint.id;
            store
                .append_next(
                    source,
                    EventPayload::Intent(IntentEvent {
                        text: "old future".into(),
                    }),
                )
                .expect("append old future");
            restored = store
                .restore_checkpoint(checkpoint.id, Some("retry".into()))
                .expect("restore as branch");
            store
                .append_next(
                    restored,
                    EventPayload::Intent(IntentEvent {
                        text: "new future".into(),
                    }),
                )
                .expect("append restored suffix");
        }

        let reopened = SqliteEventStore::open(&database).expect("reopen sqlite event store");
        let checkpoint = reopened
            .list_checkpoints(source)
            .expect("list checkpoints")
            .into_iter()
            .find(|checkpoint| checkpoint.id == checkpoint_id)
            .expect("durable checkpoint");
        assert_eq!(checkpoint.sequence, 2);
        assert!(matches!(
            &reopened.list(source).expect("old branch")[2].payload,
            EventPayload::Intent(intent) if intent.text == "old future"
        ));
        assert!(matches!(
            &reopened.list(restored).expect("restored branch")[2].payload,
            EventPayload::Intent(intent) if intent.text == "new future"
        ));
        assert_eq!(
            reopened
                .session_info(restored)
                .expect("restored metadata")
                .parent_session_id,
            Some(source)
        );

        std::fs::remove_dir_all(test_root).expect("remove isolated test directory");
    }

    #[test]
    fn bounded_history_reads_across_nested_ancestry() {
        let store = MemoryEventStore::default();
        let root = store.create_session().expect("create root");
        for text in ["one", "two", "three"] {
            store
                .append_next(
                    root,
                    EventPayload::Intent(IntentEvent { text: text.into() }),
                )
                .expect("append root");
        }
        let child = store.fork_session(root, 3).expect("fork child");
        store
            .append_next(
                child,
                EventPayload::Intent(IntentEvent {
                    text: "child".into(),
                }),
            )
            .expect("append child");
        let grandchild = store.fork_session(child, 4).expect("fork grandchild");
        store
            .append_next(
                grandchild,
                EventPayload::Intent(IntentEvent {
                    text: "grandchild".into(),
                }),
            )
            .expect("append grandchild");

        let page = store
            .list_range(grandchild, 2, 2)
            .expect("bounded logical page");
        assert_eq!(
            page.iter().map(|event| event.sequence).collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    fn assert_nested_fork_before_parent_fork_is_truncated(store: &dyn EventStore) {
        let root = store.create_session().expect("create root");
        for text in ["one", "two", "three", "four"] {
            store
                .append_next(
                    root,
                    EventPayload::Intent(IntentEvent { text: text.into() }),
                )
                .expect("append root");
        }
        let child = store.fork_session(root, 5).expect("fork child");
        let grandchild = store
            .fork_session(child, 3)
            .expect("fork grandchild before parent fork point");

        let history = store.list(grandchild).expect("list truncated history");
        assert_eq!(
            history
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(history.iter().all(|event| event.session_id == grandchild));
    }

    #[test]
    fn nested_fork_before_parent_fork_truncates_memory_prefix() {
        assert_nested_fork_before_parent_fork_is_truncated(&MemoryEventStore::default());
    }

    #[test]
    fn nested_fork_before_parent_fork_truncates_sqlite_prefix() {
        let test_root =
            std::env::temp_dir().join(format!("impetus-truncated-prefix-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&test_root).expect("create isolated test directory");
        let database = test_root.join("events.sqlite3");
        let store = SqliteEventStore::open(&database).expect("open sqlite event store");

        assert_nested_fork_before_parent_fork_is_truncated(store.as_ref());

        std::fs::remove_dir_all(test_root).expect("remove isolated test directory");
    }

    #[test]
    fn concurrent_forks_keep_one_shared_prefix() {
        let store = Arc::new(MemoryEventStore::default());
        let source = store.create_session().expect("create source");
        for index in 0..32 {
            store
                .append_next(
                    source,
                    EventPayload::Intent(IntentEvent {
                        text: format!("event-{index}"),
                    }),
                )
                .expect("append source");
        }

        let forks = std::thread::scope(|scope| {
            (0..8)
                .map(|index| {
                    let store = store.clone();
                    scope.spawn(move || {
                        store
                            .fork_session_named(source, 20, Some(format!("branch-{index}")))
                            .expect("concurrent fork")
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().expect("fork thread"))
                .collect::<Vec<_>>()
        });

        assert_eq!(forks.len(), 8);
        assert_eq!(
            store.state.lock().expect("lock memory events").events.len(),
            33,
            "fork creation must not copy events"
        );
        for fork in forks {
            let info = store.session_info(fork).expect("fork metadata");
            assert_eq!(info.head_sequence, 20);
            assert_eq!(store.list(fork).expect("fork history").len(), 20);
        }
    }

    #[test]
    #[ignore = "manual session DAG storage/query microbenchmark"]
    fn shared_prefix_storage_and_query_benchmark() {
        let store = MemoryEventStore::default();
        let source = store.create_session().expect("create source");
        for index in 0..10_000 {
            store
                .append_next(
                    source,
                    EventPayload::Intent(IntentEvent {
                        text: format!("event-{index}"),
                    }),
                )
                .expect("append source");
        }
        let shared_fork_started = std::time::Instant::now();
        let forks = (0..100)
            .map(|_| store.fork_session(source, 10_000).expect("fork"))
            .collect::<Vec<_>>();
        let shared_fork_elapsed = shared_fork_started.elapsed();
        let shared_query_started = std::time::Instant::now();
        for fork in &forks {
            assert_eq!(store.list_range(*fork, 9_900, 100).unwrap().len(), 100);
        }
        let shared_query_elapsed = shared_query_started.elapsed();
        let shared_physical_events = store.state.lock().expect("lock shared store").events.len();

        let source_history = store.list(source).expect("source history");
        let copied_fork_started = std::time::Instant::now();
        let copied_histories = (0..100)
            .map(|_| {
                let copied_session_id = Uuid::new_v4();
                source_history
                    .iter()
                    .filter(|event| event.sequence <= 10_000)
                    .map(|event| {
                        Event::with_metadata(
                            event.schema_version,
                            Uuid::new_v4(),
                            copied_session_id,
                            event.sequence,
                            event.at_unix_ms,
                            event.payload.clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let copied_fork_elapsed = copied_fork_started.elapsed();
        let copied_query_started = std::time::Instant::now();
        for history in &copied_histories {
            assert_eq!(
                history
                    .iter()
                    .filter(|event| event.sequence > 9_900)
                    .take(100)
                    .count(),
                100
            );
        }
        let copied_query_elapsed = copied_query_started.elapsed();
        let copied_physical_events =
            source_history.len() + copied_histories.iter().map(Vec::len).sum::<usize>();
        assert_eq!(shared_physical_events, source_history.len());
        assert!(shared_physical_events < copied_physical_events);
        eprintln!(
            "shared-prefix physical_events={shared_physical_events} fork={shared_fork_elapsed:?} query={shared_query_elapsed:?}; copied-prefix physical_events={copied_physical_events} fork={copied_fork_elapsed:?} query={copied_query_elapsed:?}"
        );
    }
}
