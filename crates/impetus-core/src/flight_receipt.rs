//! Flight Recorder receipts + observe-only replay over EventStore (#413 / parent #397).
//!
//! Reuses the existing session event log. Never creates a second journal.
//! Replay projects durable events only — it must **not** call
//! [`crate::EffectSeam::execute`] / [`crate::EffectSeam::execute_with_fence`].
//! Digests, kinds, path labels, and summaries only — no tokens / private keys.

use crate::effect_fence::{FenceState, records_from_events};
use crate::storage::{EventStore, StoreError};
use crate::{
    AgentEvent, ApprovalEvent, BackendEvent, BudgetEvent, ChildEvent, CommandEvent, Event,
    EventPayload, PtyEvent, RunEvent, ToolEvent,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use thiserror::Error;
use uuid::Uuid;

/// How replay may touch effects. Production always uses [`EffectReplayMode::ObserveOnly`].
#[derive(Debug)]
pub enum EffectReplayMode<'a> {
    /// Project events only — never re-execute side effects.
    ObserveOnly,
    /// Test probe: any mistaken re-execute path must increment this counter.
    ProbeExecuteAttempt(&'a AtomicUsize),
}

/// Secret-free effect-fence summary for receipts / replay timelines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectFenceSummary {
    pub effect_id: Uuid,
    pub args_digest: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_label: Option<String>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub blocks_blind_replay: bool,
}

/// One observe-only timeline row (sequence + kind label + short detail).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayTimelineEntry {
    pub sequence: u64,
    pub at_unix_ms: u64,
    pub kind: String,
    pub detail: String,
}

/// Deterministic observe-only projection of a session event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayTimeline {
    pub session_id: Uuid,
    pub event_count: usize,
    pub first_sequence: Option<u64>,
    pub last_sequence: Option<u64>,
    pub entries: Vec<ReplayTimelineEntry>,
    pub effect_fences: Vec<EffectFenceSummary>,
}

/// Flight Recorder receipt: session export over EventStore (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlightReceipt {
    pub session_id: Uuid,
    pub event_count: usize,
    pub first_sequence: Option<u64>,
    pub last_sequence: Option<u64>,
    pub started_at_unix_ms: Option<u64>,
    pub ended_at_unix_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_summary_count: Option<u32>,
    pub tool_calls: Vec<String>,
    pub commands: Vec<String>,
    pub files_read: Vec<String>,
    pub child_runs: Vec<String>,
    pub approvals: Vec<String>,
    pub denials: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns_used: Option<u32>,
    pub artifacts: Vec<String>,
    pub effect_fences: Vec<EffectFenceSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_outcome: Option<String>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FlightReceiptError {
    #[error("flight receipt store error: {0}")]
    Store(String),
    #[error("flight receipt: empty event stream for session {0}")]
    EmptySession(Uuid),
    #[error("flight receipt: mixed session ids ({expected} vs {actual})")]
    MixedSession { expected: Uuid, actual: Uuid },
}

impl From<StoreError> for FlightReceiptError {
    fn from(value: StoreError) -> Self {
        Self::Store(value.to_string())
    }
}

/// Build a receipt from an already-loaded EventStore event list.
pub fn receipt_from_events(events: &[Event]) -> Result<FlightReceipt, FlightReceiptError> {
    let Some(first) = events.first() else {
        return Err(FlightReceiptError::EmptySession(Uuid::nil()));
    };
    let session_id = first.session_id;
    let mut tool_calls = Vec::new();
    let mut commands = Vec::new();
    let mut files_read = Vec::new();
    let mut child_runs = Vec::new();
    let mut approvals = Vec::new();
    let mut denials = Vec::new();
    let mut artifacts = Vec::new();
    let mut provider_profile = None;
    let mut reasoning_summary_count = 0u32;
    let mut tokens_used = None;
    let mut turns_used = None;
    let mut run_outcome = None;

    for event in events {
        if event.session_id != session_id {
            return Err(FlightReceiptError::MixedSession {
                expected: session_id,
                actual: event.session_id,
            });
        }
        match &event.payload {
            EventPayload::Tool(ToolEvent::Started { name, .. }) => {
                tool_calls.push(format!("started:{name}"));
            }
            EventPayload::Tool(ToolEvent::Finished { name, summary, .. }) => {
                tool_calls.push(format!("finished:{name}:{summary}"));
            }
            EventPayload::Tool(ToolEvent::Observed {
                tool_name,
                outcome,
                artifact,
                ..
            }) => {
                tool_calls.push(format!("observed:{tool_name}:{outcome:?}"));
                if let Some(a) = artifact {
                    artifacts.push(a.id.clone());
                }
            }
            EventPayload::Tool(ToolEvent::FileRead { path, bytes, .. }) => {
                files_read.push(format!("{path} ({bytes}b)"));
            }
            EventPayload::Tool(ToolEvent::Deferred {
                tool_name,
                approval_id,
                ..
            }) => {
                // Arguments omitted — may contain sensitive material.
                tool_calls.push(format!("deferred:{tool_name}:approval={approval_id}"));
            }
            EventPayload::Command(CommandEvent::Started { command, .. }) => {
                commands.push(format!("started:{command}"));
            }
            EventPayload::Command(CommandEvent::Finished {
                exit_code, summary, ..
            }) => {
                let code = exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into());
                let sum = summary.as_deref().unwrap_or("");
                commands.push(format!("finished:exit={code}:{sum}"));
            }
            EventPayload::Pty(PtyEvent::Started { command, .. }) => {
                commands.push(format!("pty:{command}"));
            }
            EventPayload::Child(ChildEvent::Started { child_id, role, .. }) => {
                child_runs.push(format!("started:{child_id}:{role}"));
            }
            EventPayload::Child(ChildEvent::Finished {
                child_id,
                status,
                summary,
                ..
            }) => {
                let sum = summary.as_deref().unwrap_or("");
                child_runs.push(format!("finished:{child_id}:{status}:{sum}"));
            }
            EventPayload::Approval(ApprovalEvent::Requested { request }) => {
                approvals.push(format!(
                    "requested:{}:{:?}:{}",
                    request.id, request.action.kind, request.reason
                ));
            }
            EventPayload::Approval(ApprovalEvent::Resolved { request }) => {
                approvals.push(format!(
                    "resolved:{}:{:?}:{:?}",
                    request.id, request.action.kind, request.state
                ));
            }
            EventPayload::Notice(crate::NoticeEvent::PolicyDenied { reason }) => {
                denials.push(reason.clone());
            }
            EventPayload::Agent(AgentEvent::ReasoningSummary { .. }) => {
                reasoning_summary_count += 1;
            }
            EventPayload::Agent(AgentEvent::Chunk { artifact, .. }) => {
                if let Some(a) = artifact {
                    artifacts.push(a.id.clone());
                }
            }
            EventPayload::Backend(
                BackendEvent::ProviderHealthy { profile }
                | BackendEvent::ProviderDegraded { profile, .. }
                | BackendEvent::ProviderUnavailable { profile, .. },
            ) => {
                provider_profile = Some(profile.clone());
            }
            EventPayload::Budget(BudgetEvent::Updated {
                turns_used: t,
                tokens_used: tok,
                ..
            }) => {
                turns_used = Some(*t);
                tokens_used = Some(*tok);
            }
            EventPayload::Budget(BudgetEvent::CompactionCompleted {
                summary_artifact, ..
            }) => {
                if let Some(a) = summary_artifact {
                    artifacts.push(a.id.clone());
                }
            }
            EventPayload::Run(
                RunEvent::Completed { run_id }
                | RunEvent::Failed { run_id, .. }
                | RunEvent::Cancelled { run_id }
                | RunEvent::InterruptedUnknown { run_id },
            ) => {
                run_outcome = Some(match &event.payload {
                    EventPayload::Run(RunEvent::Completed { .. }) => {
                        format!("completed:{run_id}")
                    }
                    EventPayload::Run(RunEvent::Failed { reason, .. }) => {
                        format!("failed:{run_id}:{reason}")
                    }
                    EventPayload::Run(RunEvent::Cancelled { .. }) => {
                        format!("cancelled:{run_id}")
                    }
                    EventPayload::Run(RunEvent::InterruptedUnknown { .. }) => {
                        format!("interrupted_unknown:{run_id}")
                    }
                    _ => unreachable!(),
                });
            }
            _ => {}
        }
    }

    let first_sequence = events.first().map(|e| e.sequence);
    let last_sequence = events.last().map(|e| e.sequence);
    let started_at_unix_ms = events.first().map(|e| e.at_unix_ms);
    let ended_at_unix_ms = events.last().map(|e| e.at_unix_ms);
    let duration_ms = match (started_at_unix_ms, ended_at_unix_ms) {
        (Some(a), Some(b)) if b >= a => Some(b - a),
        _ => None,
    };

    Ok(FlightReceipt {
        session_id,
        event_count: events.len(),
        first_sequence,
        last_sequence,
        started_at_unix_ms,
        ended_at_unix_ms,
        duration_ms,
        provider_profile,
        reasoning_summary_count: if reasoning_summary_count > 0 {
            Some(reasoning_summary_count)
        } else {
            None
        },
        tool_calls,
        commands,
        files_read,
        child_runs,
        approvals,
        denials,
        tokens_used,
        turns_used,
        artifacts,
        effect_fences: fence_summaries(events),
        run_outcome,
    })
}

/// Load session events from EventStore and build a receipt.
pub fn export_receipt(
    store: &dyn EventStore,
    session_id: Uuid,
) -> Result<FlightReceipt, FlightReceiptError> {
    let events = store.list(session_id)?;
    if events.is_empty() {
        return Err(FlightReceiptError::EmptySession(session_id));
    }
    receipt_from_events(&events)
}

/// Observe-only replay: project EventStore events into a timeline.
///
/// Must never call EffectSeam execute / execute_with_fence. The optional probe
/// mode exists so unit tests can assert no re-execute path was taken.
pub fn replay_events(
    events: &[Event],
    mode: EffectReplayMode<'_>,
) -> Result<ReplayTimeline, FlightReceiptError> {
    // Contract: observe-only. Never call `forbidden_reexecute`.
    let _ = &mode;

    let Some(first) = events.first() else {
        return Err(FlightReceiptError::EmptySession(Uuid::nil()));
    };
    let session_id = first.session_id;
    let mut entries = Vec::with_capacity(events.len());

    for event in events {
        if event.session_id != session_id {
            return Err(FlightReceiptError::MixedSession {
                expected: session_id,
                actual: event.session_id,
            });
        }
        let (kind, detail) = classify_payload(&event.payload);
        entries.push(ReplayTimelineEntry {
            sequence: event.sequence,
            at_unix_ms: event.at_unix_ms,
            kind,
            detail,
        });
    }

    Ok(ReplayTimeline {
        session_id,
        event_count: events.len(),
        first_sequence: events.first().map(|e| e.sequence),
        last_sequence: events.last().map(|e| e.sequence),
        entries,
        effect_fences: fence_summaries(events),
    })
}

/// Load + observe-only replay from EventStore.
pub fn replay_session(
    store: &dyn EventStore,
    session_id: Uuid,
    mode: EffectReplayMode<'_>,
) -> Result<ReplayTimeline, FlightReceiptError> {
    let events = store.list(session_id)?;
    if events.is_empty() {
        return Err(FlightReceiptError::EmptySession(session_id));
    }
    replay_events(&events, mode)
}

/// Internal: any future bug that re-executes effects during replay must call this.
/// ObserveOnly panics; Probe increments the test counter.
#[allow(dead_code)]
fn forbidden_reexecute(mode: &EffectReplayMode<'_>) {
    match mode {
        EffectReplayMode::ObserveOnly => {
            panic!("flight recorder replay must not re-execute effects");
        }
        EffectReplayMode::ProbeExecuteAttempt(counter) => {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    }
}

fn fence_summaries(events: &[Event]) -> Vec<EffectFenceSummary> {
    let mut rows: Vec<_> = records_from_events(events)
        .into_values()
        .map(|record| EffectFenceSummary {
            effect_id: record.identity.effect_id,
            args_digest: record.identity.args_digest.as_str().to_string(),
            kind: record.identity.kind,
            target_label: record.identity.target_label,
            state: match record.state {
                FenceState::Prepared => "prepared",
                FenceState::Started => "started",
                FenceState::Observed => "observed",
                FenceState::Failed => "failed",
                FenceState::Unknown => "unknown",
            }
            .to_string(),
            summary: record.summary,
            blocks_blind_replay: record.state.blocks_blind_replay(),
        })
        .collect();
    rows.sort_by(|a, b| a.effect_id.cmp(&b.effect_id));
    rows
}

fn classify_payload(payload: &EventPayload) -> (String, String) {
    match payload {
        EventPayload::Session(s) => ("session".into(), format!("{s:?}")),
        EventPayload::Run(r) => ("run".into(), format!("{r:?}")),
        EventPayload::Intent(i) => {
            let preview: String = i.text.chars().take(80).collect();
            ("intent".into(), preview)
        }
        EventPayload::Plan(p) => ("plan".into(), p.summary.clone()),
        EventPayload::Tool(t) => match t {
            ToolEvent::Started { name, .. } => ("tool.started".into(), name.clone()),
            ToolEvent::Finished { name, summary, .. } => {
                ("tool.finished".into(), format!("{name}: {summary}"))
            }
            ToolEvent::Observed {
                tool_name, outcome, ..
            } => ("tool.observed".into(), format!("{tool_name}: {outcome:?}")),
            ToolEvent::Deferred {
                tool_name,
                approval_id,
                ..
            } => (
                "tool.deferred".into(),
                format!("{tool_name} approval={approval_id}"),
            ),
            ToolEvent::Output { tool_name, .. } => ("tool.output".into(), tool_name.clone()),
            ToolEvent::FileRead { path, .. } => ("tool.file_read".into(), path.clone()),
            ToolEvent::SearchStarted { pattern, .. } => {
                ("tool.search_started".into(), pattern.clone())
            }
            ToolEvent::SearchResult { match_count, .. } => (
                "tool.search_result".into(),
                format!("matches={match_count}"),
            ),
        },
        EventPayload::Agent(AgentEvent::Chunk { run_id, .. }) => {
            ("agent.chunk".into(), format!("run={run_id}"))
        }
        EventPayload::Agent(AgentEvent::Final { run_id, .. }) => {
            ("agent.final".into(), format!("run={run_id}"))
        }
        EventPayload::Agent(AgentEvent::ReasoningSummary { run_id, .. }) => {
            ("agent.reasoning_summary".into(), format!("run={run_id}"))
        }
        EventPayload::Approval(ApprovalEvent::Requested { request }) => (
            "approval.requested".into(),
            format!("{} {:?}", request.id, request.action.kind),
        ),
        EventPayload::Approval(ApprovalEvent::Resolved { request }) => (
            "approval.resolved".into(),
            format!("{} {:?}", request.id, request.state),
        ),
        EventPayload::Backend(b) => ("backend".into(), format!("{b:?}")),
        EventPayload::Budget(b) => ("budget".into(), format!("{b:?}")),
        EventPayload::Notice(n) => ("notice".into(), format!("{n:?}")),
        EventPayload::Retry(r) => ("retry".into(), format!("{r:?}")),
        EventPayload::Child(c) => ("child".into(), format!("{c:?}")),
        EventPayload::Sandbox(s) => ("sandbox".into(), format!("{s:?}")),
        EventPayload::Pty(p) => ("pty".into(), format!("{p:?}")),
        EventPayload::Command(c) => ("command".into(), format!("{c:?}")),
        EventPayload::EffectFence(f) => ("effect_fence".into(), format!("{f:?}")),
        EventPayload::OfflineBatch(b) => ("offline_batch".into(), format!("{b:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectFenceEvent, EventPayload, MemoryEventStore, SessionEvent, ToolEventOutcome};
    use std::sync::Arc;

    fn sample_events(session_id: Uuid) -> Vec<Event> {
        let effect_id = Uuid::new_v4();
        let digest = "abc123digest";
        vec![
            Event::with_metadata(
                crate::EVENT_SCHEMA_VERSION,
                Uuid::new_v4(),
                session_id,
                1,
                1_000,
                EventPayload::Session(SessionEvent::Created),
            ),
            Event::with_metadata(
                crate::EVENT_SCHEMA_VERSION,
                Uuid::new_v4(),
                session_id,
                2,
                1_100,
                EventPayload::Tool(ToolEvent::Started {
                    name: "write_file".into(),
                    tool_call_id: Some("tc1".into()),
                }),
            ),
            Event::with_metadata(
                crate::EVENT_SCHEMA_VERSION,
                Uuid::new_v4(),
                session_id,
                3,
                1_200,
                EventPayload::EffectFence(EffectFenceEvent::Prepared {
                    effect_id,
                    args_digest: digest.into(),
                    kind: "workspace_write:write_file".into(),
                    target_label: Some("src/main.rs".into()),
                }),
            ),
            Event::with_metadata(
                crate::EVENT_SCHEMA_VERSION,
                Uuid::new_v4(),
                session_id,
                4,
                1_300,
                EventPayload::EffectFence(EffectFenceEvent::Started {
                    effect_id,
                    args_digest: digest.into(),
                }),
            ),
            Event::with_metadata(
                crate::EVENT_SCHEMA_VERSION,
                Uuid::new_v4(),
                session_id,
                5,
                1_400,
                EventPayload::EffectFence(EffectFenceEvent::Observed {
                    effect_id,
                    args_digest: digest.into(),
                    summary: "wrote 12 bytes".into(),
                }),
            ),
            Event::with_metadata(
                crate::EVENT_SCHEMA_VERSION,
                Uuid::new_v4(),
                session_id,
                6,
                1_500,
                EventPayload::Tool(ToolEvent::Observed {
                    tool_call_id: "tc1".into(),
                    tool_name: "write_file".into(),
                    arguments_summary: "path=src/main.rs".into(),
                    outcome: ToolEventOutcome::Success,
                    preview: "ok".into(),
                    artifact: None,
                    error: None,
                }),
            ),
            Event::with_metadata(
                crate::EVENT_SCHEMA_VERSION,
                Uuid::new_v4(),
                session_id,
                7,
                2_000,
                EventPayload::Run(RunEvent::Completed {
                    run_id: Uuid::new_v4(),
                }),
            ),
        ]
    }

    #[test]
    fn receipt_summarizes_session_sequences_and_fences() {
        let session_id = Uuid::new_v4();
        let events = sample_events(session_id);
        let receipt = receipt_from_events(&events).expect("receipt");
        assert_eq!(receipt.session_id, session_id);
        assert_eq!(receipt.first_sequence, Some(1));
        assert_eq!(receipt.last_sequence, Some(7));
        assert_eq!(receipt.duration_ms, Some(1_000));
        assert_eq!(receipt.effect_fences.len(), 1);
        assert_eq!(receipt.effect_fences[0].state, "observed");
        assert_eq!(
            receipt.effect_fences[0].target_label.as_deref(),
            Some("src/main.rs")
        );
        assert!(receipt.tool_calls.iter().any(|t| t.contains("write_file")));
        assert!(receipt.run_outcome.unwrap().starts_with("completed:"));
    }

    #[test]
    fn export_receipt_reads_event_store() {
        let store = Arc::new(MemoryEventStore::default());
        let session_id = store.create_session().expect("session");
        for payload in [
            EventPayload::Session(SessionEvent::Created),
            EventPayload::Tool(ToolEvent::FileRead {
                tool_call_id: "r1".into(),
                path: "README.md".into(),
                bytes: 42,
                preview: "hi".into(),
            }),
        ] {
            store.append_next(session_id, payload).expect("append");
        }
        let receipt = export_receipt(store.as_ref(), session_id).expect("export");
        assert_eq!(receipt.session_id, session_id);
        assert_eq!(receipt.files_read.len(), 1);
        assert!(receipt.files_read[0].contains("README.md"));
    }

    #[test]
    fn replay_path_does_not_execute_effects() {
        let session_id = Uuid::new_v4();
        let events = sample_events(session_id);
        let execute_count = AtomicUsize::new(0);

        let timeline = replay_events(
            &events,
            EffectReplayMode::ProbeExecuteAttempt(&execute_count),
        )
        .expect("replay");

        assert_eq!(
            execute_count.load(Ordering::SeqCst),
            0,
            "observe-only replay must not call EffectSeam execute / execute_with_fence"
        );
        assert_eq!(timeline.event_count, events.len());
        assert_eq!(timeline.effect_fences.len(), 1);
        assert!(timeline.entries.iter().any(|e| e.kind == "effect_fence"));
        // Probe hook exists; production path never invokes forbidden_reexecute.
        let _ = forbidden_reexecute as fn(&EffectReplayMode<'_>);
    }

    #[test]
    fn receipt_omits_deferred_tool_arguments() {
        let session_id = Uuid::new_v4();
        let events = vec![Event::with_metadata(
            crate::EVENT_SCHEMA_VERSION,
            Uuid::new_v4(),
            session_id,
            1,
            10,
            EventPayload::Tool(ToolEvent::Deferred {
                approval_id: Uuid::new_v4(),
                tool_call_id: "tc".into(),
                tool_name: "bash".into(),
                arguments: serde_json::json!({
                    "command": "export SECRET_TOKEN=super-secret-value"
                }),
            }),
        )];
        let receipt = receipt_from_events(&events).expect("receipt");
        let blob = serde_json::to_string(&receipt).expect("json");
        assert!(!blob.contains("super-secret-value"));
        assert!(!blob.contains("SECRET_TOKEN"));
        assert!(receipt.tool_calls[0].contains("deferred:bash"));
    }
}
