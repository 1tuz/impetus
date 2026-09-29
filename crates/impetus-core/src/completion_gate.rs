//! Evidence-based completion gate.
//!
//! Agent turns that performed side effects (tool observations) must present
//! durable [`Evidence`] before the harness may record `RunEvent::Completed`.
//! Pure chat (no tool/effect activity) may complete without tool evidence.
//!
//! Not an authorization authority — Policy / Approval / Sandbox still gate
//! effects. Bounded retry on Insufficient → [`crate::gap_loop`].

use crate::{Event, EventPayload, ObligationLedger, RunEvent, ToolEvent, ToolEventOutcome};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Durable fact the gate can check. No secrets — summaries/labels only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// Stable kind token (e.g. `tool_observation`, `tool_observation_failed`).
    pub kind: String,
    /// Correlator (tool_call_id, artifact id, …).
    pub id: String,
    /// Short human/machine summary — never tokens or private keys.
    pub summary: String,
    /// Optional durable artifact id or workspace path label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_or_path: Option<String>,
    /// EventStore sequence when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_sequence: Option<u64>,
}

/// Outcome of evaluating a completion claim against collected evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum CompletionVerdict {
    Accepted,
    Rejected { reason: String },
    Insufficient { missing: Vec<String> },
}

/// What the agent claims is done for this turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionClaim {
    /// Free-form goal / claim label (audit only for this slice).
    pub goal: String,
    /// True when the turn performed tools/effects that need evidence.
    pub side_effects_occurred: bool,
}

/// In-memory evidence bag for a run; can later persist as EventStore rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvidenceBag {
    items: Vec<Evidence>,
    side_effects: bool,
}

impl EvidenceBag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, evidence: Evidence) {
        self.side_effects = true;
        self.items.push(evidence);
    }

    pub fn note_side_effect(&mut self) {
        self.side_effects = true;
    }

    pub fn as_slice(&self) -> &[Evidence] {
        &self.items
    }

    pub fn side_effects_occurred(&self) -> bool {
        self.side_effects || !self.items.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Collect tool observations (and side-effect markers) for `run_id`
    /// from durable EventStore rows. Cheap reuse of existing events.
    pub fn from_session_events(events: &[Event], run_id: Uuid) -> Self {
        let start = events.iter().rposition(|event| {
            matches!(
                &event.payload,
                EventPayload::Run(RunEvent::Started { run_id: id }) if *id == run_id
            )
        });
        let Some(start) = start else {
            return Self::default();
        };

        let mut bag = Self::default();
        for event in &events[start + 1..] {
            if matches!(&event.payload, EventPayload::Run(_)) {
                break;
            }
            match &event.payload {
                EventPayload::Tool(ToolEvent::Observed {
                    tool_call_id,
                    tool_name,
                    outcome,
                    preview,
                    artifact,
                    error,
                    ..
                }) => {
                    let kind = match outcome {
                        ToolEventOutcome::Success => "tool_observation",
                        ToolEventOutcome::Error | ToolEventOutcome::Denied => {
                            "tool_observation_failed"
                        }
                        ToolEventOutcome::ApprovalRequired => "tool_observation_pending",
                    };
                    let mut summary = format!("{tool_name}: {}", bound_summary(preview));
                    if let Some(err) = error.as_ref().filter(|s| !s.is_empty()) {
                        summary = format!("{summary} ({})", bound_summary(err));
                    }
                    bag.push(Evidence {
                        kind: kind.into(),
                        id: tool_call_id.clone(),
                        summary,
                        artifact_or_path: artifact.as_ref().map(|a| a.id.clone()),
                        created_sequence: Some(event.sequence),
                    });
                }
                EventPayload::Tool(
                    ToolEvent::Started { .. }
                    | ToolEvent::Finished { .. }
                    | ToolEvent::Deferred { .. }
                    | ToolEvent::Output { .. }
                    | ToolEvent::FileRead { .. }
                    | ToolEvent::SearchStarted { .. }
                    | ToolEvent::SearchResult { .. },
                )
                | EventPayload::Sandbox(_) => {
                    bag.note_side_effect();
                }
                _ => {}
            }
        }
        bag
    }
}

/// Fail-closed evaluator: side-effecting turns need successful tool evidence;
/// open required obligations also block Accepted.
#[derive(Debug, Default, Clone, Copy)]
pub struct CompletionGate;

impl CompletionGate {
    pub fn evaluate(claim: &CompletionClaim, evidence: &[Evidence]) -> CompletionVerdict {
        Self::evaluate_with_obligations(claim, evidence, &ObligationLedger::new())
    }

    /// Evidence check, then fail-closed on open required obligations.
    pub fn evaluate_with_obligations(
        claim: &CompletionClaim,
        evidence: &[Evidence],
        ledger: &ObligationLedger,
    ) -> CompletionVerdict {
        let evidence_verdict = Self::evaluate_evidence(claim, evidence);
        match evidence_verdict {
            CompletionVerdict::Accepted => Self::check_obligations(ledger),
            other => other,
        }
    }

    fn evaluate_evidence(claim: &CompletionClaim, evidence: &[Evidence]) -> CompletionVerdict {
        if !claim.side_effects_occurred {
            // Pure text / no tools — complete without tool evidence.
            return CompletionVerdict::Accepted;
        }
        if evidence.is_empty() {
            return CompletionVerdict::Insufficient {
                missing: vec!["tool_observation".into()],
            };
        }
        if evidence.iter().any(|e| e.kind == "tool_observation") {
            return CompletionVerdict::Accepted;
        }
        if evidence
            .iter()
            .all(|e| e.kind == "tool_observation_pending")
        {
            return CompletionVerdict::Insufficient {
                missing: vec!["tool_observation_success".into()],
            };
        }
        CompletionVerdict::Rejected {
            reason: "no successful tool observation for side-effecting turn".into(),
        }
    }

    /// Open required obligations → Insufficient (gap-fillable); never Accepted.
    pub fn check_obligations(ledger: &ObligationLedger) -> CompletionVerdict {
        if !ledger.has_open_required() {
            return CompletionVerdict::Accepted;
        }
        CompletionVerdict::Insufficient {
            missing: ledger.missing_labels(),
        }
    }

    /// Evaluate a run against EventStore-derived evidence (empty obligation ledger).
    pub fn evaluate_run(events: &[Event], run_id: Uuid) -> CompletionVerdict {
        Self::evaluate_run_with_obligations(events, run_id, &ObligationLedger::new())
    }

    /// Evaluate a run against evidence + obligation ledger (fail-closed).
    pub fn evaluate_run_with_obligations(
        events: &[Event],
        run_id: Uuid,
        ledger: &ObligationLedger,
    ) -> CompletionVerdict {
        let bag = EvidenceBag::from_session_events(events, run_id);
        let claim = CompletionClaim {
            goal: format!("run:{run_id}"),
            side_effects_occurred: bag.side_effects_occurred(),
        };
        Self::evaluate_with_obligations(&claim, bag.as_slice(), ledger)
    }
}

fn bound_summary(input: &str) -> String {
    const MAX: usize = 200;
    let trimmed = input.trim();
    if trimmed.len() <= MAX {
        return trimmed.to_owned();
    }
    let mut end = MAX;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::EventStore;
    use crate::{EventPayload, MemoryEventStore, ToolEvent, ToolEventOutcome};
    use std::sync::Arc;

    fn evidence(kind: &str, id: &str) -> Evidence {
        Evidence {
            kind: kind.into(),
            id: id.into(),
            summary: format!("{kind}:{id}"),
            artifact_or_path: None,
            created_sequence: Some(1),
        }
    }

    #[test]
    fn accept_pure_chat_without_evidence() {
        let claim = CompletionClaim {
            goal: "hello".into(),
            side_effects_occurred: false,
        };
        assert_eq!(
            CompletionGate::evaluate(&claim, &[]),
            CompletionVerdict::Accepted
        );
    }

    #[test]
    fn accept_with_enough_tool_evidence() {
        let claim = CompletionClaim {
            goal: "edit file".into(),
            side_effects_occurred: true,
        };
        let bag = vec![evidence("tool_observation", "call-1")];
        assert_eq!(
            CompletionGate::evaluate(&claim, &bag),
            CompletionVerdict::Accepted
        );
    }

    #[test]
    fn insufficient_when_side_effects_but_no_evidence() {
        let claim = CompletionClaim {
            goal: "edit file".into(),
            side_effects_occurred: true,
        };
        assert_eq!(
            CompletionGate::evaluate(&claim, &[]),
            CompletionVerdict::Insufficient {
                missing: vec!["tool_observation".into()],
            }
        );
    }

    #[test]
    fn reject_when_only_failed_observations() {
        let claim = CompletionClaim {
            goal: "edit file".into(),
            side_effects_occurred: true,
        };
        let bag = vec![evidence("tool_observation_failed", "call-1")];
        assert!(matches!(
            CompletionGate::evaluate(&claim, &bag),
            CompletionVerdict::Rejected { .. }
        ));
    }

    #[test]
    fn bag_from_events_collects_tool_observations() {
        let store = Arc::new(MemoryEventStore::default());
        let session = store.create_session().expect("session");
        let run_id = Uuid::new_v4();
        store
            .append_next(session, EventPayload::Run(RunEvent::Started { run_id }))
            .expect("started");
        store
            .append_next(
                session,
                EventPayload::Tool(ToolEvent::Observed {
                    tool_call_id: "c1".into(),
                    tool_name: "read_file".into(),
                    arguments_summary: r#"{"path":"a.txt"}"#.into(),
                    outcome: ToolEventOutcome::Success,
                    preview: "ok".into(),
                    artifact: None,
                    error: None,
                }),
            )
            .expect("observed");
        let events = store.list(session).expect("list");
        let bag = EvidenceBag::from_session_events(&events, run_id);
        assert!(bag.side_effects_occurred());
        assert_eq!(bag.as_slice().len(), 1);
        assert_eq!(bag.as_slice()[0].kind, "tool_observation");
        assert_eq!(
            CompletionGate::evaluate_run(&events, run_id),
            CompletionVerdict::Accepted
        );
    }

    #[test]
    fn side_effect_marker_without_observation_is_insufficient() {
        let store = Arc::new(MemoryEventStore::default());
        let session = store.create_session().expect("session");
        let run_id = Uuid::new_v4();
        store
            .append_next(session, EventPayload::Run(RunEvent::Started { run_id }))
            .expect("started");
        store
            .append_next(
                session,
                EventPayload::Tool(ToolEvent::Started {
                    name: "write_file".into(),
                    tool_call_id: Some("c1".into()),
                }),
            )
            .expect("started tool");
        let events = store.list(session).expect("list");
        assert_eq!(
            CompletionGate::evaluate_run(&events, run_id),
            CompletionVerdict::Insufficient {
                missing: vec!["tool_observation".into()],
            }
        );
    }

    #[test]
    fn open_obligation_blocks_accept_even_with_evidence() {
        let claim = CompletionClaim {
            goal: "ship feature".into(),
            side_effects_occurred: true,
        };
        let bag = vec![evidence("tool_observation", "call-1")];
        let mut ledger = ObligationLedger::new();
        ledger
            .register("todo-1", "promised_todo", "wire unit tests")
            .expect("register");
        assert_eq!(
            CompletionGate::evaluate_with_obligations(&claim, &bag, &ledger),
            CompletionVerdict::Insufficient {
                missing: vec!["obligation:todo-1".into()],
            }
        );
    }

    #[test]
    fn fulfilled_obligation_allows_accept() {
        let claim = CompletionClaim {
            goal: "ship feature".into(),
            side_effects_occurred: true,
        };
        let bag = vec![evidence("tool_observation", "call-1")];
        let mut ledger = ObligationLedger::new();
        ledger
            .register("todo-1", "promised_todo", "wire unit tests")
            .expect("register");
        ledger.fulfill("todo-1").expect("fulfill");
        assert_eq!(
            CompletionGate::evaluate_with_obligations(&claim, &bag, &ledger),
            CompletionVerdict::Accepted
        );
    }

    #[test]
    fn open_obligation_blocks_pure_chat_accept() {
        let claim = CompletionClaim {
            goal: "hello".into(),
            side_effects_occurred: false,
        };
        let mut ledger = ObligationLedger::new();
        ledger
            .register("docs-1", "docs_update", "ARCHITECTURE.md")
            .expect("register");
        assert_eq!(
            CompletionGate::evaluate_with_obligations(&claim, &[], &ledger),
            CompletionVerdict::Insufficient {
                missing: vec!["obligation:docs-1".into()],
            }
        );
    }
}
