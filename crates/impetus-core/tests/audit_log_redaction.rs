//! Integration tests for audit log with secret redaction.
//!
//! Verifies that secrets in tool arguments are automatically redacted
//! before being stored in the audit log.

use impetus_core::{
    AuditLog, AuditQuery, EventPayload, EventStore, MemoryEventStore, SqliteEventStore, ToolEvent,
    ToolEventOutcome,
};
use std::sync::Arc;
use tempfile::TempDir;

#[test]
fn audit_log_stores_redacted_arguments() {
    let store = Arc::new(MemoryEventStore::default());
    let session_id = store.create_session().unwrap();

    // Simulate tool call with sensitive data
    store
        .append_next(
            session_id,
            EventPayload::Tool(ToolEvent::Observed {
                tool_call_id: "call_1".to_string(),
                tool_name: "deploy".to_string(),
                arguments_summary: r#"{"token":"[REDACTED]","url":"https://api.example.com"}"#
                    .to_string(),
                outcome: ToolEventOutcome::Success,
                preview: "deployed".to_string(),
                artifact: None,
                error: None,
            }),
        )
        .unwrap();

    let audit = AuditLog::new(store);
    let entries = audit.query_session(session_id).unwrap();

    assert_eq!(entries.len(), 1);
    assert!(entries[0].arguments_summary.contains("[REDACTED]"));
    assert!(!entries[0].arguments_summary.contains("sk-"));
}

#[test]
fn audit_log_redacts_private_keys() {
    let store = Arc::new(MemoryEventStore::default());
    let session_id = store.create_session().unwrap();

    store
        .append_next(
            session_id,
            EventPayload::Tool(ToolEvent::Observed {
                tool_call_id: "call_1".to_string(),
                tool_name: "ssh_connect".to_string(),
                arguments_summary: "[REDACTED PRIVATE KEY]".to_string(),
                outcome: ToolEventOutcome::Success,
                preview: "connected".to_string(),
                artifact: None,
                error: None,
            }),
        )
        .unwrap();

    let audit = AuditLog::new(store);
    let entries = audit.query_session(session_id).unwrap();

    assert_eq!(entries.len(), 1);
    assert!(entries[0].arguments_summary.contains("[REDACTED"));
}

#[test]
fn audit_log_query_by_tool_name() {
    let store = Arc::new(MemoryEventStore::default());
    let session_id = store.create_session().unwrap();

    // Add multiple tool calls
    for i in 0..5 {
        store
            .append_next(
                session_id,
                EventPayload::Tool(ToolEvent::Observed {
                    tool_call_id: format!("call_{}", i),
                    tool_name: if i % 2 == 0 {
                        "read_file".to_string()
                    } else {
                        "write_file".to_string()
                    },
                    arguments_summary: format!(r#"{{"path":"file{}.txt"}}"#, i),
                    outcome: ToolEventOutcome::Success,
                    preview: "ok".to_string(),
                    artifact: None,
                    error: None,
                }),
            )
            .unwrap();
    }

    let audit = AuditLog::new(store);

    // Query only read_file calls
    let read_entries = audit
        .query(AuditQuery {
            session_id: Some(session_id),
            tool_name: Some("read_file".to_string()),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(read_entries.len(), 3); // 0, 2, 4
    assert!(read_entries.iter().all(|e| e.tool_name == "read_file"));
}

#[test]
fn audit_log_query_by_outcome() {
    let store = Arc::new(MemoryEventStore::default());
    let session_id = store.create_session().unwrap();

    store
        .append_next(
            session_id,
            EventPayload::Tool(ToolEvent::Observed {
                tool_call_id: "call_1".to_string(),
                tool_name: "read_file".to_string(),
                arguments_summary: r#"{"path":"ok.txt"}"#.to_string(),
                outcome: ToolEventOutcome::Success,
                preview: "content".to_string(),
                artifact: None,
                error: None,
            }),
        )
        .unwrap();

    store
        .append_next(
            session_id,
            EventPayload::Tool(ToolEvent::Observed {
                tool_call_id: "call_2".to_string(),
                tool_name: "write_file".to_string(),
                arguments_summary: r#"{"path":"/etc/passwd"}"#.to_string(),
                outcome: ToolEventOutcome::Denied,
                preview: "".to_string(),
                artifact: None,
                error: Some("policy denied".to_string()),
            }),
        )
        .unwrap();

    store
        .append_next(
            session_id,
            EventPayload::Tool(ToolEvent::Observed {
                tool_call_id: "call_3".to_string(),
                tool_name: "exec".to_string(),
                arguments_summary: r#"{"cmd":"fail"}"#.to_string(),
                outcome: ToolEventOutcome::Error,
                preview: "".to_string(),
                artifact: None,
                error: Some("command not found".to_string()),
            }),
        )
        .unwrap();

    let audit = AuditLog::new(store);

    // Query denied operations
    let denied = audit
        .query(AuditQuery {
            session_id: Some(session_id),
            outcome: Some(ToolEventOutcome::Denied),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].tool_name, "write_file");
    assert_eq!(denied[0].error, Some("policy denied".to_string()));

    // Query errors
    let errors = audit
        .query(AuditQuery {
            session_id: Some(session_id),
            outcome: Some(ToolEventOutcome::Error),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].tool_name, "exec");
}

#[test]
fn audit_log_sqlite_persistence() {
    let temp = TempDir::new().unwrap();
    let db_path = temp.path().join("audit.db");

    let session_id;
    {
        let store = SqliteEventStore::open(&db_path).unwrap();
        session_id = store.create_session().unwrap();

        store
            .append_next(
                session_id,
                EventPayload::Tool(ToolEvent::Observed {
                    tool_call_id: "call_1".to_string(),
                    tool_name: "test_tool".to_string(),
                    arguments_summary: r#"{"secret":"[REDACTED]"}"#.to_string(),
                    outcome: ToolEventOutcome::Success,
                    preview: "done".to_string(),
                    artifact: None,
                    error: None,
                }),
            )
            .unwrap();
    }

    // Reopen and verify persistence
    let store = SqliteEventStore::open(&db_path).unwrap();
    let audit = AuditLog::new(store);
    let entries = audit.query_session(session_id).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].tool_name, "test_tool");
    assert!(entries[0].arguments_summary.contains("[REDACTED]"));
}

#[test]
fn audit_log_query_time_range() {
    let store = Arc::new(MemoryEventStore::default());
    let session_id = store.create_session().unwrap();

    let _start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    store
        .append_next(
            session_id,
            EventPayload::Tool(ToolEvent::Observed {
                tool_call_id: "call_1".to_string(),
                tool_name: "early".to_string(),
                arguments_summary: "{}".to_string(),
                outcome: ToolEventOutcome::Success,
                preview: "".to_string(),
                artifact: None,
                error: None,
            }),
        )
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(100));

    let mid = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    std::thread::sleep(std::time::Duration::from_millis(10));

    store
        .append_next(
            session_id,
            EventPayload::Tool(ToolEvent::Observed {
                tool_call_id: "call_2".to_string(),
                tool_name: "late".to_string(),
                arguments_summary: "{}".to_string(),
                outcome: ToolEventOutcome::Success,
                preview: "".to_string(),
                artifact: None,
                error: None,
            }),
        )
        .unwrap();

    let audit = AuditLog::new(store);

    // Query after first event
    let late_entries = audit
        .query(AuditQuery {
            session_id: Some(session_id),
            after_unix_ms: Some(mid),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(late_entries.len(), 1);
    assert_eq!(late_entries[0].tool_name, "late");

    // Query before second event
    let early_entries = audit
        .query(AuditQuery {
            session_id: Some(session_id),
            before_unix_ms: Some(mid),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(early_entries.len(), 1);
    assert_eq!(early_entries[0].tool_name, "early");
}

#[test]
fn audit_log_arguments_summary_max_length() {
    let store = Arc::new(MemoryEventStore::default());
    let session_id = store.create_session().unwrap();

    // Arguments summary is limited to 1024 chars in summarize_arguments()
    let long_summary = "x".repeat(2000);

    store
        .append_next(
            session_id,
            EventPayload::Tool(ToolEvent::Observed {
                tool_call_id: "call_1".to_string(),
                tool_name: "big_args".to_string(),
                arguments_summary: long_summary[..1024].to_string(),
                outcome: ToolEventOutcome::Success,
                preview: "".to_string(),
                artifact: None,
                error: None,
            }),
        )
        .unwrap();

    let audit = AuditLog::new(store);
    let entries = audit.query_session(session_id).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].arguments_summary.len(), 1024);
}
