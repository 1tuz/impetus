use crate::types::{
    ApprovalRequest, DurableArtifactRef, ExecutionMode, SandboxDecision, SandboxDecisionState,
    UserPromptIntent,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const EVENT_SCHEMA_VERSION: u16 = 1;

/// Max UTF-8 chars kept in activity previews (Pty/Command/File/Search).
/// Session EventStore rows are durable; keep previews small.
pub const MAX_ACTIVITY_PREVIEW_CHARS: usize = 512;

/// Truncate activity preview for durable session-log rows.
pub fn bound_activity_preview(text: &str) -> String {
    let flat: String = text.chars().take(MAX_ACTIVITY_PREVIEW_CHARS).collect();
    if text.chars().count() > MAX_ACTIVITY_PREVIEW_CHARS {
        format!("{flat}…")
    } else {
        flat
    }
}

/// Durability contract for session activity:
/// - **Durable** — anything appended to the session EventStore
///   (SQLite WAL). Includes Pty*/FileRead/Search*/Command*/Tool*/Child*.
/// - **Ephemeral** — AttachmentStore only (RAM approvals/diffs).
///   Never treat AttachmentStore as EventStore, and never put secrets in either.
///
/// Large bodies spill to DurableArtifactStore; events keep ArtifactRef + preview.

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
    Retry(RetryEvent),
    /// Child / subagent lifecycle on the **parent** session log (live UI).
    Child(ChildEvent),
    /// OS sandbox prepare/deny evidence (Seatbelt et al.) on the session log.
    Sandbox(SandboxEvent),
    /// Daemon-owned PTY lifecycle on the session EventStore (durable).
    Pty(PtyEvent),
    /// Process/shell command lifecycle (durable; bounded previews).
    Command(CommandEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionEvent {
    Created,
    WorkspaceRoot { workspace_root: std::path::PathBuf },
    Attached,
    ExecutionModeChanged { mode: ExecutionMode },
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
    /// Large paste / attachment: durable ref only — raw body stays out of events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<DurableArtifactRef>,
    /// Prompt | Steer | FollowUp discriminant. Absent in legacy events → Prompt.
    #[serde(default)]
    pub intent: UserPromptIntent,
}

impl IntentEvent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            artifact: None,
            intent: UserPromptIntent::Prompt,
        }
    }

    pub fn with_artifact(text: impl Into<String>, artifact: DurableArtifactRef) -> Self {
        Self {
            text: text.into(),
            artifact: Some(artifact),
            intent: UserPromptIntent::Prompt,
        }
    }

    pub fn with_intent(
        text: impl Into<String>,
        intent: UserPromptIntent,
        artifact: Option<DurableArtifactRef>,
    ) -> Self {
        Self {
            text: text.into(),
            artifact,
            intent,
        }
    }
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
        /// Correlates with Observed/Deferred when present (legacy events omit).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
    },
    Finished {
        name: String,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
    },
    Observed {
        tool_call_id: String,
        tool_name: String,
        arguments_summary: String,
        outcome: ToolEventOutcome,
        preview: String,
        artifact: Option<DurableArtifactRef>,
        error: Option<String>,
    },
    Deferred {
        approval_id: Uuid,
        tool_call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    },
    /// Bounded mid-run output (durable EventStore row; keep preview small).
    Output {
        tool_call_id: String,
        tool_name: String,
        preview: String,
    },
    /// Read-file tool completed a path read (typed; not tool-name scrape).
    FileRead {
        tool_call_id: String,
        path: String,
        #[serde(default)]
        bytes: u64,
        preview: String,
    },
    /// Search tool began scanning.
    SearchStarted {
        tool_call_id: String,
        pattern: String,
        target: String,
    },
    /// Search tool finished with bounded match preview.
    SearchResult {
        tool_call_id: String,
        #[serde(default)]
        match_count: u32,
        preview: String,
    },
}

/// Daemon PTY lifecycle on the parent session log (durable EventStore).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PtyEvent {
    Started {
        pty_id: u64,
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
    },
    /// Bounded output sample (eof flush / terminate). Live bytes stay on IPC ring.
    Output {
        pty_id: u64,
        preview: String,
        #[serde(default)]
        dropped_total: u64,
        #[serde(default)]
        eof: bool,
    },
    /// Ring overflow: dropped oldest bytes spilled to DurableArtifactStore.
    Spill {
        pty_id: u64,
        artifact: DurableArtifactRef,
        dropped_bytes: u64,
    },
    Exited {
        pty_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
}

/// Process/shell command lifecycle (durable; correlates via `tool_call_id`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CommandEvent {
    Started {
        tool_call_id: String,
        command: String,
    },
    Output {
        tool_call_id: String,
        preview: String,
    },
    Finished {
        tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
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
        /// Inline text, or bounded preview when `artifact` is set.
        text: String,
        /// Large chunk body spilled out of the event log (SHA-256 content-addressed).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact: Option<DurableArtifactRef>,
    },
    Final {
        run_id: Uuid,
        text: String,
    },
    /// Provider-supplied reasoning **summary** only (never hidden CoT).
    ReasoningSummary {
        run_id: Uuid,
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ChildEvent {
    Started {
        child_id: String,
        parent_id: String,
        role: String,
    },
    StatusChanged {
        child_id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_action: Option<String>,
    },
    Finished {
        child_id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
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

/// Structural session state persisted with compaction commits.
///
/// Must never live only inside a model-generated text summary: policy, cwd,
/// budgets, and parent identity are recovered from this typed payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionStructuralState {
    pub workspace_root: std::path::PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<Uuid>,
    pub allow_network: bool,
    pub allow_web_outbound: bool,
    pub allow_private_network: bool,
    pub allowed_hosts: Vec<String>,
    pub turns_used: u32,
    pub tokens_used: u64,
    pub compaction_count: u32,
    /// Bound WorktreeManager identity; survives compaction as typed payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
}

/// Budget state events для live display в TUI/Zap.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BudgetEvent {
    Updated {
        turns_used: u32,
        tokens_used: u64,
        /// True if tokens_used is from provider-reported usage.
        measured: bool,
        compaction_count: u32,
        context_used_percent: u8,
    },
    CompactionRequired {
        threshold: u64,
        used: u64,
    },
    /// Durable start of a compaction transition (event-store range selected).
    CompactionStarted {
        from_sequence: u64,
        to_sequence: u64,
        threshold: u64,
        used: u64,
    },
    /// Compaction committed: summary/artifact refs + structural snapshot.
    /// (`CompactionCommitted` in roadmap language — same BudgetEvent family.)
    CompactionCompleted {
        compacted_to: u64,
        compaction_count: u32,
        #[serde(default)]
        from_sequence: u64,
        #[serde(default)]
        to_sequence: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary_artifact: Option<DurableArtifactRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        structural: Option<CompactionStructuralState>,
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

/// Durable OS-sandbox audit (backend name + prepare state). Secret-free.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SandboxEvent {
    /// Backend prepared (or denied) a confined spawn; mirrors [`SandboxDecision`].
    Decision {
        backend: String,
        prepare_state: SandboxPrepareState,
        network_allowed: bool,
        writable_root_count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason_code: Option<String>,
    },
}

/// Prepare outcome for [`SandboxEvent::Decision`] (stable wire name).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPrepareState {
    Prepared,
    Denied,
}

impl SandboxEvent {
    pub fn from_decision(decision: &SandboxDecision) -> Self {
        use SandboxDecisionState;
        Self::Decision {
            backend: decision.backend.clone(),
            prepare_state: match decision.state {
                SandboxDecisionState::Prepared => SandboxPrepareState::Prepared,
                SandboxDecisionState::Denied => SandboxPrepareState::Denied,
            },
            network_allowed: decision.network_allowed,
            writable_root_count: decision.writable_root_count,
            reason_code: decision.reason_code.clone(),
        }
    }
}

/// Retry event tracking for error recovery
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetryEvent {
    Attempting {
        attempt: u32,
        max_attempts: u32,
        reason: String,
        backoff_ms: u64,
    },
    Succeeded {
        attempt: u32,
    },
    Exhausted {
        attempts: u32,
        last_error: String,
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
mod sentinel_events {
    //! PR-safe durable events lib suite (TODO P2 CI / #315).
    //!
    //! Filter: `cargo test -p impetus-core --lib sentinel_events`
    //! All named sentinels: `cargo test -p impetus-core --lib -- sentinel`
    //!
    //! Payload/schema round-trips (incl. Child + SandboxDecision). No SQLite IPC.

    use super::*;

    #[test]
    fn payload_round_trip_is_tagged_and_typed() {
        let payload = EventPayload::Agent(AgentEvent::Chunk {
            run_id: Uuid::new_v4(),
            chunk_id: 1,
            text: "hello".into(),
            artifact: None,
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
            EventPayload::Intent(IntentEvent::new("explain"))
        );
    }

    #[test]
    fn intent_event_carries_prompt_intent_discriminant() {
        for intent in [
            UserPromptIntent::Prompt,
            UserPromptIntent::Steer,
            UserPromptIntent::FollowUp,
        ] {
            let event = IntentEvent::with_intent("nudge", intent, None);
            let encoded = serde_json::to_string(&event).expect("encode");
            let decoded: IntentEvent = serde_json::from_str(&encoded).expect("decode");
            assert_eq!(decoded.intent, intent);
            assert_eq!(decoded.text, "nudge");
        }
        let legacy: IntentEvent =
            serde_json::from_str(r#"{"text":"hi"}"#).expect("legacy intent event");
        assert_eq!(legacy.intent, UserPromptIntent::Prompt);
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
        let events = vec![
            SessionEvent::Created,
            SessionEvent::Attached,
            SessionEvent::ExecutionModeChanged {
                mode: ExecutionMode::Plan,
            },
        ];
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
            tool_call_id: None,
        };
        let finished = ToolEvent::Finished {
            name: "bash".into(),
            summary: "exit 0".into(),
            tool_call_id: None,
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
            artifact: None,
        };
        let final_ev = AgentEvent::Final {
            run_id,
            text: "done".into(),
        };
        let reasoning = AgentEvent::ReasoningSummary {
            run_id,
            text: "brief plan".into(),
        };
        for ev in [chunk, final_ev, reasoning] {
            let json = serde_json::to_string(&ev).unwrap();
            assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), ev);
        }
        // Legacy Chunk JSON without `artifact` deserializes as None.
        let legacy: AgentEvent = serde_json::from_str(&format!(
            r#"{{"state":"chunk","run_id":"{run_id}","chunk_id":1,"text":"hi"}}"#
        ))
        .expect("legacy chunk");
        assert_eq!(
            legacy,
            AgentEvent::Chunk {
                run_id,
                chunk_id: 1,
                text: "hi".into(),
                artifact: None,
            }
        );
    }

    #[test]
    fn approval_events_serialize() {
        let req = ApprovalRequest::pending(
            crate::types::Action {
                origin: crate::types::ActionOrigin::Agent,
                kind: crate::types::ActionKind::WriteFile,
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
                measured: true,
                compaction_count: 1,
                context_used_percent: 50,
            },
            BudgetEvent::CompactionRequired {
                threshold: 10000,
                used: 12000,
            },
            BudgetEvent::CompactionStarted {
                from_sequence: 1,
                to_sequence: 42,
                threshold: 10000,
                used: 12000,
            },
            BudgetEvent::CompactionCompleted {
                compacted_to: 5000,
                compaction_count: 2,
                from_sequence: 1,
                to_sequence: 42,
                summary_artifact: None,
                structural: Some(CompactionStructuralState {
                    workspace_root: std::path::PathBuf::from("/tmp/ws"),
                    parent_session_id: None,
                    allow_network: false,
                    allow_web_outbound: false,
                    allow_private_network: false,
                    allowed_hosts: vec![],
                    turns_used: 5,
                    tokens_used: 5000,
                    compaction_count: 2,
                    worktree_id: None,
                }),
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
    fn sandbox_decision_event_serializes() {
        let payload = EventPayload::Sandbox(SandboxEvent::Decision {
            backend: "macos_seatbelt".into(),
            prepare_state: SandboxPrepareState::Prepared,
            network_allowed: false,
            writable_root_count: 2,
            reason_code: None,
        });
        let json = serde_json::to_string(&payload).unwrap();
        assert_eq!(
            serde_json::from_str::<EventPayload>(&json).unwrap(),
            payload
        );
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
                measured: false,
                compaction_count: 0,
                context_used_percent: 25,
            }),
        );
        let json = serde_json::to_string(&event).unwrap();
        let decoded: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.session_id, session_id);
        assert_eq!(decoded.sequence, 10);
    }

    #[test]
    fn pty_command_and_file_search_events_serialize() {
        let pty = EventPayload::Pty(PtyEvent::Started {
            pty_id: 7,
            command: "/bin/zsh".into(),
            working_dir: Some("/tmp".into()),
        });
        let cmd = EventPayload::Command(CommandEvent::Finished {
            tool_call_id: "c1".into(),
            exit_code: Some(0),
            summary: Some("ok".into()),
        });
        let file = EventPayload::Tool(ToolEvent::FileRead {
            tool_call_id: "r1".into(),
            path: "src/lib.rs".into(),
            bytes: 42,
            preview: "fn main".into(),
        });
        let search = EventPayload::Tool(ToolEvent::SearchResult {
            tool_call_id: "s1".into(),
            match_count: 3,
            preview: "hit".into(),
        });
        for payload in [pty, cmd, file, search] {
            let json = serde_json::to_string(&payload).unwrap();
            assert_eq!(
                serde_json::from_str::<EventPayload>(&json).unwrap(),
                payload
            );
        }
    }

    #[test]
    fn bound_activity_preview_caps_chars() {
        let long = "x".repeat(MAX_ACTIVITY_PREVIEW_CHARS + 20);
        let preview = bound_activity_preview(&long);
        assert!(preview.ends_with('…'));
        assert_eq!(preview.chars().count(), MAX_ACTIVITY_PREVIEW_CHARS + 1);
    }
}
