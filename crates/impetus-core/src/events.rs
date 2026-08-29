use crate::ApprovalRequest;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const EVENT_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub schema_version: u16,
    pub id: Uuid,
    pub session_id: Uuid,
    pub sequence: u64,
    pub at_unix_ms: u64,
    pub payload: EventPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EventPayload {
    Session(SessionEvent),
    Run(RunEvent),
    Intent(IntentEvent),
    Plan(PlanEvent),
    Tool(ToolEvent),
    Agent(AgentEvent),
    Approval(ApprovalEvent),
    Backend(BackendEvent),
    Budget(BudgetEvent),
    Notice(NoticeEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionEvent {
    Created,
    WorkspaceRoot { workspace_root: std::path::PathBuf },
    Attached,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunEvent {
    Started { run_id: Uuid },
    Completed { run_id: Uuid },
    Failed { run_id: Uuid, reason: String },
    Cancelled { run_id: Uuid },
    InterruptedUnknown { run_id: Uuid },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntentEvent {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanEvent {
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ToolEvent {
    Started {
        name: String,
    },
    Finished {
        name: String,
        summary: String,
    },
    Observed {
        tool_call_id: String,
        tool_name: String,
        arguments_summary: String,
        outcome: ToolEventOutcome,
        preview: String,
        artifact: Option<crate::DurableArtifactRef>,
        error: Option<String>,
    },
    Deferred {
        approval_id: Uuid,
        tool_call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEventOutcome {
    Success,
    Error,
    Denied,
    ApprovalRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentEvent {
    Chunk {
        run_id: Uuid,
        chunk_id: u64,
        text: String,
    },
    Final {
        run_id: Uuid,
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ApprovalEvent {
    Requested { request: ApprovalRequest },
    Resolved { request: ApprovalRequest },
}

/// Backend and auth state changes for structured client presentation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BackendEvent {
    ProviderHealthy {
        profile: String,
    },
    ProviderDegraded {
        profile: String,
        reason: String,
    },
    ProviderUnavailable {
        profile: String,
        reason: String,
    },
    KeychainAvailable,
    KeychainUnavailable {
        reason: String,
    },
    TokenExpiryWarning {
        profile: String,
        expires_in_seconds: u64,
    },
}

/// Budget state events для live display в TUI/Zap.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BudgetEvent {
    Updated {
        turns_used: u32,
        tokens_used: u64,
        compaction_count: u32,
        context_used_percent: u8,
    },
    CompactionRequired {
        threshold: u64,
        used: u64,
    },
    CompactionCompleted {
        compacted_to: u64,
        compaction_count: u32,
    },
    TurnLimitApproaching {
        limit: u32,
        used: u32,
    },
    TokenLimitApproaching {
        limit: u64,
        used: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NoticeEvent {
    PolicyAllowed,
    PolicyDenied {
        reason: String,
    },
    Runtime {
        message: String,
    },
    Legacy {
        event_kind: String,
        body: serde_json::Value,
    },
}

impl Event {
    pub fn new(session_id: Uuid, sequence: u64, payload: EventPayload) -> Self {
        let at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_millis() as u64;
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            id: Uuid::new_v4(),
            session_id,
            sequence,
            at_unix_ms,
            payload,
        }
    }

    pub fn with_metadata(
        schema_version: u16,
        id: Uuid,
        session_id: Uuid,
        sequence: u64,
        at_unix_ms: u64,
        payload: EventPayload,
    ) -> Self {
        Self {
            schema_version,
            id,
            session_id,
            sequence,
            at_unix_ms,
            payload,
        }
    }
}

pub fn legacy_payload(
    kind: &str,
    body: serde_json::Value,
) -> Result<EventPayload, serde_json::Error> {
    match kind {
        "user_intent" => Ok(EventPayload::Intent(serde_json::from_value(body)?)),
        "approval_requested" => Ok(EventPayload::Approval(ApprovalEvent::Requested {
            request: serde_json::from_value(body)?,
        })),
        "approval_resolved" => Ok(EventPayload::Approval(ApprovalEvent::Resolved {
            request: serde_json::from_value(body)?,
        })),
        "runtime_notice" => Ok(EventPayload::Notice(NoticeEvent::Runtime {
            message: body
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("legacy runtime notice")
                .to_owned(),
        })),
        "policy_evaluated" => match body.get("decision").and_then(serde_json::Value::as_str) {
            Some("allow") => Ok(EventPayload::Notice(NoticeEvent::PolicyAllowed)),
            Some("deny") => Ok(EventPayload::Notice(NoticeEvent::PolicyDenied {
                reason: body
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })),
            _ => Ok(EventPayload::Notice(NoticeEvent::Legacy {
                event_kind: kind.to_owned(),
                body,
            })),
        },
        _ => Ok(EventPayload::Notice(NoticeEvent::Legacy {
            event_kind: kind.to_owned(),
            body,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trip_is_tagged_and_typed() {
        let payload = EventPayload::Agent(AgentEvent::Chunk {
            run_id: Uuid::new_v4(),
            chunk_id: 1,
            text: "hello".into(),
        });
        let encoded = serde_json::to_string(&payload).expect("serialize payload");
        assert_eq!(
            serde_json::from_str::<EventPayload>(&encoded).expect("deserialize payload"),
            payload
        );
    }

    #[test]
    fn legacy_intent_becomes_typed_payload() {
        assert_eq!(
            legacy_payload("user_intent", serde_json::json!({ "text": "explain" }))
                .expect("convert legacy"),
            EventPayload::Intent(IntentEvent {
                text: "explain".into()
            })
        );
    }

    #[test]
    fn event_new_sets_schema_version_and_generates_ids() {
        let session_id = Uuid::new_v4();
        let event = Event::new(session_id, 42, EventPayload::Session(SessionEvent::Created));
        assert_eq!(event.schema_version, EVENT_SCHEMA_VERSION);
        assert_eq!(event.session_id, session_id);
        assert_eq!(event.sequence, 42);
        assert!(event.at_unix_ms > 0);
    }

    #[test]
    fn event_with_metadata_preserves_all_fields() {
        let id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let event = Event::with_metadata(
            1,
            id,
            session_id,
            100,
            1234567890,
            EventPayload::Session(SessionEvent::Attached),
        );
        assert_eq!(event.schema_version, 1);
        assert_eq!(event.id, id);
        assert_eq!(event.session_id, session_id);
        assert_eq!(event.sequence, 100);
        assert_eq!(event.at_unix_ms, 1234567890);
    }

    #[test]
    fn all_session_events_serialize() {
        let events = vec![SessionEvent::Created, SessionEvent::Attached];
        for ev in events {
            let json = serde_json::to_string(&ev).unwrap();
            assert_eq!(serde_json::from_str::<SessionEvent>(&json).unwrap(), ev);
        }
    }

    #[test]
    fn all_run_events_serialize() {
        let run_id = Uuid::new_v4();
        let events = vec![
            RunEvent::Started { run_id },
            RunEvent::Completed { run_id },
            RunEvent::Failed {
                run_id,
                reason: "test error".into(),
            },
            RunEvent::Cancelled { run_id },
            RunEvent::InterruptedUnknown { run_id },
        ];
        for ev in events {
            let json = serde_json::to_string(&ev).unwrap();
            assert_eq!(serde_json::from_str::<RunEvent>(&json).unwrap(), ev);
        }
    }

    #[test]
    fn tool_events_serialize() {
        let started = ToolEvent::Started {
            name: "bash".into(),
        };
        let finished = ToolEvent::Finished {
            name: "bash".into(),
            summary: "exit 0".into(),
        };
        for ev in [started, finished] {
            let json = serde_json::to_string(&ev).unwrap();
            assert_eq!(serde_json::from_str::<ToolEvent>(&json).unwrap(), ev);
        }
    }

    #[test]
    fn agent_events_serialize() {
        let run_id = Uuid::new_v4();
        let chunk = AgentEvent::Chunk {
            run_id,
            chunk_id: 1,
            text: "streaming".into(),
        };
        let final_ev = AgentEvent::Final {
            run_id,
            text: "done".into(),
        };
        for ev in [chunk, final_ev] {
            let json = serde_json::to_string(&ev).unwrap();
            assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), ev);
        }
    }

    #[test]
    fn approval_events_serialize() {
        let req = ApprovalRequest::pending(
            crate::policy::Action {
                origin: crate::policy::ActionOrigin::Agent,
                kind: crate::policy::ActionKind::WriteFile,
                summary: "test write".into(),
                target: Some("/tmp/test".into()),
            },
            "testing".into(),
            1,
        );
        let requested = ApprovalEvent::Requested {
            request: req.clone(),
        };
        let resolved = ApprovalEvent::Resolved { request: req };
        for ev in [requested, resolved] {
            let json = serde_json::to_string(&ev).unwrap();
            assert_eq!(serde_json::from_str::<ApprovalEvent>(&json).unwrap(), ev);
        }
    }

    #[test]
    fn backend_events_serialize() {
        let events = vec![
            BackendEvent::ProviderHealthy {
                profile: "default".into(),
            },
            BackendEvent::ProviderDegraded {
                profile: "default".into(),
                reason: "rate limit".into(),
            },
            BackendEvent::ProviderUnavailable {
                profile: "default".into(),
                reason: "offline".into(),
            },
            BackendEvent::KeychainAvailable,
            BackendEvent::KeychainUnavailable {
                reason: "locked".into(),
            },
            BackendEvent::TokenExpiryWarning {
                profile: "default".into(),
                expires_in_seconds: 300,
            },
        ];
        for ev in events {
            let json = serde_json::to_string(&ev).unwrap();
            assert_eq!(serde_json::from_str::<BackendEvent>(&json).unwrap(), ev);
        }
    }

    #[test]
    fn budget_events_serialize() {
        let events = vec![
            BudgetEvent::Updated {
                turns_used: 5,
                tokens_used: 1000,
                compaction_count: 1,
                context_used_percent: 50,
            },
            BudgetEvent::CompactionRequired {
                threshold: 10000,
                used: 12000,
            },
            BudgetEvent::CompactionCompleted {
                compacted_to: 5000,
                compaction_count: 2,
            },
            BudgetEvent::TurnLimitApproaching { limit: 10, used: 8 },
            BudgetEvent::TokenLimitApproaching {
                limit: 20000,
                used: 18000,
            },
        ];
        for ev in events {
            let json = serde_json::to_string(&ev).unwrap();
            assert_eq!(serde_json::from_str::<BudgetEvent>(&json).unwrap(), ev);
        }
    }

    #[test]
    fn notice_events_serialize() {
        let events = vec![
            NoticeEvent::PolicyAllowed,
            NoticeEvent::PolicyDenied {
                reason: "blocked".into(),
            },
            NoticeEvent::Runtime {
                message: "info".into(),
            },
            NoticeEvent::Legacy {
                event_kind: "old".into(),
                body: serde_json::json!({ "key": "value" }),
            },
        ];
        for ev in events {
            let json = serde_json::to_string(&ev).unwrap();
            assert_eq!(serde_json::from_str::<NoticeEvent>(&json).unwrap(), ev);
        }
    }

    #[test]
    fn legacy_approval_requested_converts() {
        let body = serde_json::json!({
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "action": {
                "origin": "agent",
                "kind": "write_file",
                "summary": "write",
                "target": "/tmp/test"
            },
            "action_fingerprint": "test_fp",
            "capability_version": null,
            "intent_revision": 1,
            "reason": "test",
            "state": "Pending"
        });
        let result = legacy_payload("approval_requested", body).unwrap();
        assert!(matches!(
            result,
            EventPayload::Approval(ApprovalEvent::Requested { .. })
        ));
    }

    #[test]
    fn legacy_approval_resolved_converts() {
        let body = serde_json::json!({
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "action": {
                "origin": "agent",
                "kind": "read_file",
                "summary": "read",
                "target": null
            },
            "action_fingerprint": "test_fp",
            "capability_version": null,
            "intent_revision": 2,
            "reason": "test read",
            "state": "Approved"
        });
        let result = legacy_payload("approval_resolved", body).unwrap();
        assert!(matches!(
            result,
            EventPayload::Approval(ApprovalEvent::Resolved { .. })
        ));
    }

    #[test]
    fn legacy_runtime_notice_extracts_status() {
        let body = serde_json::json!({ "status": "warming up" });
        assert_eq!(
            legacy_payload("runtime_notice", body).unwrap(),
            EventPayload::Notice(NoticeEvent::Runtime {
                message: "warming up".into()
            })
        );
    }

    #[test]
    fn legacy_policy_allow_converts() {
        let body = serde_json::json!({ "decision": "allow" });
        assert_eq!(
            legacy_payload("policy_evaluated", body).unwrap(),
            EventPayload::Notice(NoticeEvent::PolicyAllowed)
        );
    }

    #[test]
    fn legacy_policy_deny_converts() {
        let body = serde_json::json!({ "decision": "deny", "reason": "blocked" });
        assert_eq!(
            legacy_payload("policy_evaluated", body).unwrap(),
            EventPayload::Notice(NoticeEvent::PolicyDenied {
                reason: "blocked".into()
            })
        );
    }

    #[test]
    fn legacy_unknown_kind_becomes_legacy_notice() {
        let body = serde_json::json!({ "key": "val" });
        let result = legacy_payload("unknown_event", body.clone()).unwrap();
        assert!(matches!(
            result,
            EventPayload::Notice(NoticeEvent::Legacy { .. })
        ));
        if let EventPayload::Notice(NoticeEvent::Legacy {
            event_kind,
            body: b,
        }) = result
        {
            assert_eq!(event_kind, "unknown_event");
            assert_eq!(b, body);
        }
    }

    #[test]
    fn legacy_invalid_json_returns_error() {
        let result = legacy_payload("user_intent", serde_json::json!({ "wrong": "field" }));
        assert!(result.is_err());
    }

    #[test]
    fn full_event_round_trip() {
        let session_id = Uuid::new_v4();
        let event = Event::new(
            session_id,
            10,
            EventPayload::Budget(BudgetEvent::Updated {
                turns_used: 3,
                tokens_used: 500,
                compaction_count: 0,
                context_used_percent: 25,
            }),
        );
        let json = serde_json::to_string(&event).unwrap();
        let decoded: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.session_id, session_id);
        assert_eq!(decoded.sequence, 10);
    }
}
