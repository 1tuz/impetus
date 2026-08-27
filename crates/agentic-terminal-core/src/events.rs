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
    Notice(NoticeEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionEvent {
    Created,
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
    Started { name: String },
    Finished { name: String, summary: String },
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
}
