//! Audit log query interface with automatic secret redaction.
//!
//! Provides structured queries over the event store for audit and compliance purposes.
//! All tool arguments are automatically redacted before storage via `summarize_arguments()`.

use crate::{Event, EventPayload, EventStore, StoreError, ToolEvent, ToolEventOutcome};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// Audit log entry for a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEntry {
    pub event_id: Uuid,
    pub session_id: Uuid,
    pub sequence: u64,
    pub timestamp_unix_ms: u64,
    pub tool_call_id: String,
    pub tool_name: String,
    /// Redacted arguments summary (max 1024 chars, secrets removed)
    pub arguments_summary: String,
    pub outcome: ToolEventOutcome,
    pub preview: String,
    pub error: Option<String>,
}

/// Query filter for audit log entries.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    pub session_id: Option<Uuid>,
    pub tool_name: Option<String>,
    pub outcome: Option<ToolEventOutcome>,
    pub after_unix_ms: Option<u64>,
    pub before_unix_ms: Option<u64>,
}

/// Audit log query interface.
pub struct AuditLog {
    store: Arc<dyn EventStore>,
}

impl AuditLog {
    pub fn new(store: Arc<dyn EventStore>) -> Self {
        Self { store }
    }

    /// Query audit entries for a specific session.
    pub fn query_session(&self, session_id: Uuid) -> Result<Vec<AuditEntry>, StoreError> {
        self.query(AuditQuery {
            session_id: Some(session_id),
            ..Default::default()
        })
    }

    /// Query audit entries with filters.
    pub fn query(&self, filter: AuditQuery) -> Result<Vec<AuditEntry>, StoreError> {
        let sessions = if let Some(session_id) = filter.session_id {
            vec![session_id]
        } else {
            self.store
                .list_sessions()?
                .into_iter()
                .map(|s| s.id)
                .collect()
        };

        let mut entries = Vec::new();

        for session_id in sessions {
            let events = self.store.list(session_id)?;
            for event in events {
                if let Some(entry) = self.extract_audit_entry(&event)
                    && self.matches_filter(&entry, &filter)
                {
                    entries.push(entry);
                }
            }
        }

        // Sort by timestamp
        entries.sort_by_key(|e| e.timestamp_unix_ms);
        Ok(entries)
    }

    fn extract_audit_entry(&self, event: &Event) -> Option<AuditEntry> {
        if let EventPayload::Tool(ToolEvent::Observed {
            tool_call_id,
            tool_name,
            arguments_summary,
            outcome,
            preview,
            error,
            ..
        }) = &event.payload
        {
            Some(AuditEntry {
                event_id: event.id,
                session_id: event.session_id,
                sequence: event.sequence,
                timestamp_unix_ms: event.at_unix_ms,
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                arguments_summary: arguments_summary.clone(),
                outcome: outcome.clone(),
                preview: preview.clone(),
                error: error.clone(),
            })
        } else {
            None
        }
    }

    fn matches_filter(&self, entry: &AuditEntry, filter: &AuditQuery) -> bool {
        if let Some(ref tool_name) = filter.tool_name
            && &entry.tool_name != tool_name
        {
            return false;
        }

        if let Some(ref outcome) = filter.outcome
            && &entry.outcome != outcome
        {
            return false;
        }

        if let Some(after) = filter.after_unix_ms
            && entry.timestamp_unix_ms < after
        {
            return false;
        }

        if let Some(before) = filter.before_unix_ms
            && entry.timestamp_unix_ms > before
        {
            return false;
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventPayload, MemoryEventStore, ToolEvent, ToolEventOutcome};

    #[test]
    fn query_empty_store() {
        let store = Arc::new(MemoryEventStore::default());
        let audit = AuditLog::new(store);

        let entries = audit.query(AuditQuery::default()).unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn query_session_with_tool_events() {
        let store = Arc::new(MemoryEventStore::default());
        let session_id = store.create_session().unwrap();

        store
            .append_next(
                session_id,
                EventPayload::Tool(ToolEvent::Observed {
                    tool_call_id: "call_1".to_string(),
                    tool_name: "read_file".to_string(),
                    arguments_summary: r#"{"path":"test.txt"}"#.to_string(),
                    outcome: ToolEventOutcome::Success,
                    preview: "file content".to_string(),
                    artifact: None,
                    error: None,
                }),
            )
            .unwrap();

        let audit = AuditLog::new(store);
        let entries = audit.query_session(session_id).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tool_name, "read_file");
        assert_eq!(entries[0].outcome, ToolEventOutcome::Success);
    }

    #[test]
    fn query_filters_by_tool_name() {
        let store = Arc::new(MemoryEventStore::default());
        let session_id = store.create_session().unwrap();

        store
            .append_next(
                session_id,
                EventPayload::Tool(ToolEvent::Observed {
                    tool_call_id: "call_1".to_string(),
                    tool_name: "read_file".to_string(),
                    arguments_summary: r#"{"path":"a.txt"}"#.to_string(),
                    outcome: ToolEventOutcome::Success,
                    preview: "a".to_string(),
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
                    arguments_summary: r#"{"path":"b.txt"}"#.to_string(),
                    outcome: ToolEventOutcome::Success,
                    preview: "written".to_string(),
                    artifact: None,
                    error: None,
                }),
            )
            .unwrap();

        let audit = AuditLog::new(store);
        let entries = audit
            .query(AuditQuery {
                session_id: Some(session_id),
                tool_name: Some("read_file".to_string()),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tool_name, "read_file");
    }

    #[test]
    fn query_filters_by_outcome() {
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
                    preview: "ok".to_string(),
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
                    arguments_summary: r#"{"path":"denied.txt"}"#.to_string(),
                    outcome: ToolEventOutcome::Denied,
                    preview: "".to_string(),
                    artifact: None,
                    error: Some("policy denied".to_string()),
                }),
            )
            .unwrap();

        let audit = AuditLog::new(store);
        let denied = audit
            .query(AuditQuery {
                session_id: Some(session_id),
                outcome: Some(ToolEventOutcome::Denied),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(denied.len(), 1);
        assert_eq!(denied[0].outcome, ToolEventOutcome::Denied);
        assert_eq!(denied[0].error, Some("policy denied".to_string()));
    }

    #[test]
    fn query_filters_by_time_range() {
        let store = Arc::new(MemoryEventStore::default());
        let session_id = store.create_session().unwrap();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        store
            .append_next(
                session_id,
                EventPayload::Tool(ToolEvent::Observed {
                    tool_call_id: "call_1".to_string(),
                    tool_name: "tool_1".to_string(),
                    arguments_summary: "{}".to_string(),
                    outcome: ToolEventOutcome::Success,
                    preview: "".to_string(),
                    artifact: None,
                    error: None,
                }),
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        store
            .append_next(
                session_id,
                EventPayload::Tool(ToolEvent::Observed {
                    tool_call_id: "call_2".to_string(),
                    tool_name: "tool_2".to_string(),
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
        let entries = audit
            .query(AuditQuery {
                session_id: Some(session_id),
                after_unix_ms: Some(now + 5),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tool_name, "tool_2");
    }

    #[test]
    fn query_across_multiple_sessions() {
        let store = Arc::new(MemoryEventStore::default());

        let session_a = store.create_session().unwrap();
        let session_b = store.create_session().unwrap();

        store
            .append_next(
                session_a,
                EventPayload::Tool(ToolEvent::Observed {
                    tool_call_id: "call_a".to_string(),
                    tool_name: "read_file".to_string(),
                    arguments_summary: r#"{"path":"a.txt"}"#.to_string(),
                    outcome: ToolEventOutcome::Success,
                    preview: "a".to_string(),
                    artifact: None,
                    error: None,
                }),
            )
            .unwrap();

        store
            .append_next(
                session_b,
                EventPayload::Tool(ToolEvent::Observed {
                    tool_call_id: "call_b".to_string(),
                    tool_name: "read_file".to_string(),
                    arguments_summary: r#"{"path":"b.txt"}"#.to_string(),
                    outcome: ToolEventOutcome::Success,
                    preview: "b".to_string(),
                    artifact: None,
                    error: None,
                }),
            )
            .unwrap();

        let audit = AuditLog::new(store);

        // Query all sessions
        let all = audit.query(AuditQuery::default()).unwrap();
        assert_eq!(all.len(), 2);

        // Query specific session
        let session_a_entries = audit.query_session(session_a).unwrap();
        assert_eq!(session_a_entries.len(), 1);
        assert_eq!(session_a_entries[0].tool_call_id, "call_a");
    }
}
