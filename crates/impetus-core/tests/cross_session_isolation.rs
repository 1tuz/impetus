//! Cross-session state isolation and cleanup tests.
//!
//! Verifies:
//! - Sessions are fully isolated from each other
//! - Resources are cleaned up on session termination
//! - No memory leaks or state pollution between sessions

use impetus_core::{EventPayload, EventStore, MemoryEventStore, SessionEvent, SqliteEventStore};
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn memory_store_sessions_are_isolated() {
    let store = Arc::new(MemoryEventStore::default());

    // Create two sessions
    let session_a = store.create_session().unwrap();
    let session_b = store.create_session().unwrap();

    // Append events to each session
    store
        .append_next(session_a, EventPayload::Session(SessionEvent::Created))
        .unwrap();
    store
        .append_next(session_b, EventPayload::Session(SessionEvent::Created))
        .unwrap();

    // Verify isolation: each session sees only its own events
    let events_a = store.list(session_a).unwrap();
    let events_b = store.list(session_b).unwrap();

    assert_eq!(events_a.len(), 2); // Created + one more
    assert_eq!(events_b.len(), 2);
    assert!(events_a.iter().all(|e| e.session_id == session_a));
    assert!(events_b.iter().all(|e| e.session_id == session_b));
}

#[test]
fn memory_store_delete_session_removes_all_events() {
    let store = Arc::new(MemoryEventStore::default());

    let session_a = store.create_session().unwrap();
    let session_b = store.create_session().unwrap();

    store
        .append_next(session_a, EventPayload::Session(SessionEvent::Created))
        .unwrap();
    store
        .append_next(session_b, EventPayload::Session(SessionEvent::Created))
        .unwrap();

    // Delete session A
    store.delete_session(session_a).unwrap();

    // Session A events are gone
    let events_a = store.list(session_a).unwrap();
    assert_eq!(events_a.len(), 0);

    // Session B events remain intact
    let events_b = store.list(session_b).unwrap();
    assert_eq!(events_b.len(), 2);

    // Session A not in session list
    let sessions = store.list_sessions().unwrap();
    assert!(!sessions.iter().any(|s| s.id == session_a));
    assert!(sessions.iter().any(|s| s.id == session_b));
}

#[test]
fn memory_store_delete_nonexistent_session_is_idempotent() {
    let store = Arc::new(MemoryEventStore::default());
    let nonexistent = Uuid::new_v4();

    // Should not error
    store.delete_session(nonexistent).unwrap();
    store.delete_session(nonexistent).unwrap();
}

#[test]
fn sqlite_store_sessions_are_isolated() {
    let temp = TempDir::new().unwrap();
    let db_path = temp.path().join("test.db");
    let store = SqliteEventStore::open(&db_path).unwrap();

    let session_a = store.create_session().unwrap();
    let session_b = store.create_session().unwrap();

    store
        .append_next(session_a, EventPayload::Session(SessionEvent::Created))
        .unwrap();
    store
        .append_next(session_b, EventPayload::Session(SessionEvent::Created))
        .unwrap();

    let events_a = store.list(session_a).unwrap();
    let events_b = store.list(session_b).unwrap();

    assert_eq!(events_a.len(), 2);
    assert_eq!(events_b.len(), 2);
    assert!(events_a.iter().all(|e| e.session_id == session_a));
    assert!(events_b.iter().all(|e| e.session_id == session_b));
}

#[test]
fn sqlite_store_delete_session_removes_all_events() {
    let temp = TempDir::new().unwrap();
    let db_path = temp.path().join("test.db");
    let store = SqliteEventStore::open(&db_path).unwrap();

    let session_a = store.create_session().unwrap();
    let session_b = store.create_session().unwrap();

    store
        .append_next(session_a, EventPayload::Session(SessionEvent::Created))
        .unwrap();
    store
        .append_next(session_b, EventPayload::Session(SessionEvent::Created))
        .unwrap();

    store.delete_session(session_a).unwrap();

    let events_a = store.list(session_a).unwrap();
    assert_eq!(events_a.len(), 0);

    let events_b = store.list(session_b).unwrap();
    assert_eq!(events_b.len(), 2);

    let sessions = store.list_sessions().unwrap();
    assert!(!sessions.iter().any(|s| s.id == session_a));
    assert!(sessions.iter().any(|s| s.id == session_b));
}

#[test]
fn sqlite_store_delete_nonexistent_session_is_idempotent() {
    let temp = TempDir::new().unwrap();
    let db_path = temp.path().join("test.db");
    let store = SqliteEventStore::open(&db_path).unwrap();
    let nonexistent = Uuid::new_v4();

    store.delete_session(nonexistent).unwrap();
    store.delete_session(nonexistent).unwrap();
}

#[test]
fn budget_state_isolated_per_session() {
    let temp = TempDir::new().unwrap();
    let db_path = temp.path().join("test.db");
    let store = SqliteEventStore::open(&db_path).unwrap();

    let session_a = store.create_session().unwrap();
    let session_b = store.create_session().unwrap();

    let mut state_a = store.get_budget_state(session_a).unwrap();
    state_a.turns_used = 5;
    state_a.tokens_used = 1000;
    store.update_budget_state(session_a, &state_a).unwrap();

    let mut state_b = store.get_budget_state(session_b).unwrap();
    state_b.turns_used = 10;
    state_b.tokens_used = 2000;
    store.update_budget_state(session_b, &state_b).unwrap();

    // Verify isolation
    let retrieved_a = store.get_budget_state(session_a).unwrap();
    let retrieved_b = store.get_budget_state(session_b).unwrap();

    assert_eq!(retrieved_a.turns_used, 5);
    assert_eq!(retrieved_a.tokens_used, 1000);
    assert_eq!(retrieved_b.turns_used, 10);
    assert_eq!(retrieved_b.tokens_used, 2000);
}

#[test]
fn delete_session_cleans_up_budget_state() {
    let temp = TempDir::new().unwrap();
    let db_path = temp.path().join("test.db");
    let store = SqliteEventStore::open(&db_path).unwrap();

    let session = store.create_session().unwrap();

    let mut state = store.get_budget_state(session).unwrap();
    state.turns_used = 5;
    store.update_budget_state(session, &state).unwrap();

    store.delete_session(session).unwrap();

    // After delete, budget state is fresh (not persisted)
    let retrieved = store.get_budget_state(session).unwrap();
    assert_eq!(retrieved.turns_used, 0);
    assert_eq!(retrieved.tokens_used, 0);
}

#[test]
fn forked_session_does_not_affect_source() {
    let temp = TempDir::new().unwrap();
    let db_path = temp.path().join("test.db");
    let store = SqliteEventStore::open(&db_path).unwrap();

    let source = store.create_session().unwrap();
    store
        .append_next(source, EventPayload::Session(SessionEvent::Created))
        .unwrap();
    store
        .append_next(source, EventPayload::Session(SessionEvent::Created))
        .unwrap();

    let forked = store.fork_session(source, 2).unwrap();

    // Append to source after fork
    store
        .append_next(source, EventPayload::Session(SessionEvent::Created))
        .unwrap();

    let source_events = store.list(source).unwrap();
    let forked_events = store.list(forked).unwrap();

    assert_eq!(source_events.len(), 4); // 1 create + 3 appends
    assert_eq!(forked_events.len(), 2); // Only up to checkpoint

    // Delete forked session doesn't affect source
    store.delete_session(forked).unwrap();
    let source_after_delete = store.list(source).unwrap();
    assert_eq!(source_after_delete.len(), 4);
}
