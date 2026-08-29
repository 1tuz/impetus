use crate::{
    AgentEvent, ApprovalEvent, ApprovalRequest, EVENT_SCHEMA_VERSION, Event, EventPayload,
    IntentEvent, PlanEvent, RunEvent, ToolEvent,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionProjection {
    pub session_id: Uuid,
    pub last_sequence: u64,
    pub latest_intent: Option<String>,
    pub latest_intent_revision: Option<u64>,
    pub latest_plan: Option<String>,
    pub workspace_root: Option<PathBuf>,
    pub tool_summaries: BTreeMap<String, String>,
    pub agent_output: String,
    pub agent_chunk_ids: BTreeMap<Uuid, u64>,
    pub pending_approvals: BTreeMap<Uuid, ApprovalRequest>,
    pub deferred_tools: BTreeMap<Uuid, (String, String, serde_json::Value)>,
    pub active_run_id: Option<Uuid>,
    pub last_run_id: Option<Uuid>,
    pub outcome: Option<RunEvent>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProjectionError {
    #[error("event `{event_id}` has unsupported schema version {schema_version}")]
    UnsupportedSchema { event_id: Uuid, schema_version: u16 },
    #[error("event stream mixes session `{expected}` with `{actual}")]
    MixedSession { expected: Uuid, actual: Uuid },
    #[error("event sequence is not contiguous: expected {expected}, got {actual}")]
    NonContiguousSequence { expected: u64, actual: u64 },
}

pub fn reduce(events: &[Event]) -> Result<Option<SessionProjection>, ProjectionError> {
    let Some(first) = events.first() else {
        return Ok(None);
    };
    let mut projection = SessionProjection {
        session_id: first.session_id,
        last_sequence: 0,
        latest_intent: None,
        latest_intent_revision: None,
        latest_plan: None,
        workspace_root: None,
        tool_summaries: BTreeMap::new(),
        agent_output: String::new(),
        agent_chunk_ids: BTreeMap::new(),
        pending_approvals: BTreeMap::new(),
        deferred_tools: BTreeMap::new(),
        active_run_id: None,
        last_run_id: None,
        outcome: None,
    };
    for (index, event) in events.iter().enumerate() {
        if event.schema_version != EVENT_SCHEMA_VERSION {
            return Err(ProjectionError::UnsupportedSchema {
                event_id: event.id,
                schema_version: event.schema_version,
            });
        }
        if event.session_id != projection.session_id {
            return Err(ProjectionError::MixedSession {
                expected: projection.session_id,
                actual: event.session_id,
            });
        }
        let expected = index as u64 + 1;
        if event.sequence != expected {
            return Err(ProjectionError::NonContiguousSequence {
                expected,
                actual: event.sequence,
            });
        }
        projection.last_sequence = event.sequence;
        match &event.payload {
            EventPayload::Session(crate::SessionEvent::WorkspaceRoot { workspace_root }) => {
                projection.workspace_root = Some(workspace_root.clone());
            }
            EventPayload::Intent(IntentEvent { text }) => {
                projection.latest_intent = Some(text.clone());
                projection.latest_intent_revision = Some(event.sequence);
            }
            EventPayload::Plan(PlanEvent { summary }) => {
                projection.latest_plan = Some(summary.clone())
            }
            EventPayload::Tool(ToolEvent::Started { name }) => {
                projection
                    .tool_summaries
                    .insert(name.clone(), "running".into());
            }
            EventPayload::Tool(ToolEvent::Finished { name, summary }) => {
                projection
                    .tool_summaries
                    .insert(name.clone(), summary.clone());
            }
            EventPayload::Tool(ToolEvent::Observed {
                tool_name, outcome, ..
            }) => {
                projection
                    .tool_summaries
                    .insert(tool_name.clone(), format!("{outcome:?}"));
            }
            EventPayload::Tool(ToolEvent::Deferred {
                approval_id,
                tool_call_id,
                tool_name,
                arguments,
            }) => {
                projection.deferred_tools.insert(
                    *approval_id,
                    (tool_call_id.clone(), tool_name.clone(), arguments.clone()),
                );
            }
            EventPayload::Agent(AgentEvent::Chunk {
                run_id,
                chunk_id,
                text,
            }) => {
                let last_chunk_id = projection.agent_chunk_ids.entry(*run_id).or_insert(0);
                if *chunk_id > *last_chunk_id {
                    *last_chunk_id = *chunk_id;
                    projection.agent_output.push_str(text);
                }
            }
            EventPayload::Agent(AgentEvent::Final { text, .. }) => {
                projection.agent_output.push_str(text)
            }
            EventPayload::Approval(ApprovalEvent::Requested { request }) => {
                projection
                    .pending_approvals
                    .insert(request.id, request.clone());
            }
            EventPayload::Approval(ApprovalEvent::Resolved { request }) => {
                projection.pending_approvals.remove(&request.id);
                projection.deferred_tools.remove(&request.id);
            }
            EventPayload::Run(RunEvent::Started { run_id }) => {
                projection.active_run_id = Some(*run_id);
                projection.last_run_id = Some(*run_id);
                projection.outcome = None;
            }
            EventPayload::Run(
                outcome @ (RunEvent::Completed { run_id }
                | RunEvent::Failed { run_id, .. }
                | RunEvent::Cancelled { run_id }
                | RunEvent::InterruptedUnknown { run_id }),
            ) if projection.active_run_id == Some(*run_id) => {
                projection.active_run_id = None;
                projection.outcome = Some(outcome.clone());
            }
            _ => {}
        }
    }
    Ok(Some(projection))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventPayload, IntentEvent};

    #[test]
    fn replay_is_deterministic() {
        let session_id = Uuid::new_v4();
        let events = vec![Event::new(
            session_id,
            1,
            EventPayload::Intent(IntentEvent {
                text: "explain repo".into(),
            }),
        )];
        assert_eq!(
            reduce(&events).expect("first replay"),
            reduce(&events).expect("second replay")
        );
    }

    #[test]
    fn unknown_version_never_projects_as_success() {
        let session_id = Uuid::new_v4();
        let event = Event::with_metadata(
            99,
            Uuid::new_v4(),
            session_id,
            1,
            0,
            EventPayload::Run(RunEvent::Completed {
                run_id: Uuid::new_v4(),
            }),
        );
        assert!(matches!(
            reduce(&[event]),
            Err(ProjectionError::UnsupportedSchema {
                schema_version: 99,
                ..
            })
        ));
    }
}
