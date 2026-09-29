//! ObservationPack + Evidence Anchors — SoL-Pi-style context efficiency.
//!
//! Large tool/process outputs stay durable in ArtifactStore / EventStore.
//! HOT context carries a bounded preview plus immutable [`EvidenceAnchor`]s.
//! Compaction and output reduction must never drop recoverable raw evidence.

use crate::durable_artifacts::DurableArtifactStore;
use crate::output_reducer::{OutputReducer, TokenBudget};
use crate::storage::EventStore;
use crate::tool_orchestrator::ToolObservation;
use crate::{DurableArtifactRef, Event, EventPayload, ToolEvent};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Immutable pointer to durable raw evidence. Survives reduce/compact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceAnchor {
    /// Content-addressed body in DurableArtifactStore.
    Artifact {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        byte_count: Option<usize>,
    },
    /// Ordered EventStore row (typically `Tool::Observed`).
    Event {
        sequence: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
    },
}

impl EvidenceAnchor {
    pub fn artifact(id: impl Into<String>, byte_count: Option<usize>) -> Self {
        Self::Artifact {
            id: id.into(),
            byte_count,
        }
    }

    pub fn event(sequence: u64, tool_call_id: Option<String>) -> Self {
        Self::Event {
            sequence,
            tool_call_id,
        }
    }

    /// Stable label for summaries / compaction receipts (no secrets).
    pub fn label(&self) -> String {
        match self {
            Self::Artifact { id, byte_count } => match byte_count {
                Some(n) => format!("evidence_anchor:artifact:{id}:{n}"),
                None => format!("evidence_anchor:artifact:{id}"),
            },
            Self::Event {
                sequence,
                tool_call_id,
            } => match tool_call_id {
                Some(id) => format!("evidence_anchor:event:{sequence}:{id}"),
                None => format!("evidence_anchor:event:{sequence}"),
            },
        }
    }

    pub fn artifact_id(&self) -> Option<&str> {
        match self {
            Self::Artifact { id, .. } => Some(id.as_str()),
            Self::Event { .. } => None,
        }
    }

    pub fn event_sequence(&self) -> Option<u64> {
        match self {
            Self::Event { sequence, .. } => Some(*sequence),
            Self::Artifact { .. } => None,
        }
    }
}

/// Bounded HOT-context receipt for a tool/process observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationPack {
    pub tool_call_id: String,
    pub tool_name: String,
    pub outcome: String,
    /// Token-bounded preview for the model. Never the sole SoT when anchors exist.
    pub preview: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<EvidenceAnchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments_summary: Option<String>,
}

impl ObservationPack {
    /// Pack an existing orchestrator observation into HOT receipt + anchors.
    pub fn from_tool_observation(
        observation: &ToolObservation,
        event_sequence: Option<u64>,
    ) -> Self {
        let mut anchors = Vec::new();
        if let Some(artifact) = observation.artifact.as_ref() {
            anchors.push(EvidenceAnchor::artifact(
                artifact.id.clone(),
                Some(artifact.byte_count),
            ));
        }
        if let Some(seq) = event_sequence {
            anchors.push(EvidenceAnchor::event(
                seq,
                Some(observation.tool_call_id.clone()),
            ));
        }
        let truncated = observation.artifact.is_some();
        Self {
            tool_call_id: observation.tool_call_id.clone(),
            tool_name: observation.tool_name.clone(),
            outcome: format!("{:?}", observation.outcome),
            preview: observation.preview.clone(),
            truncated,
            anchors,
            error: observation.error.clone(),
            arguments_summary: Some(observation.arguments_summary.clone()),
        }
    }

    /// Persist raw body when it exceeds budget; return bounded preview + anchors.
    pub fn pack_raw(
        store: Option<&DurableArtifactStore>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        outcome: impl Into<String>,
        raw: &str,
        budget: TokenBudget,
        event_sequence: Option<u64>,
    ) -> Result<Self, ObservationPackError> {
        let tool_call_id = tool_call_id.into();
        let tool_name = tool_name.into();
        let outcome = outcome.into();
        let reducer = OutputReducer::new_without_rtk(budget);
        let reduced = reducer.reduce(raw);

        let mut anchors = Vec::new();
        let truncated = reduced.truncated;
        if truncated {
            let Some(store) = store else {
                return Err(ObservationPackError::ArtifactStoreRequired);
            };
            let artifact = store
                .store(raw.as_bytes())
                .map_err(|e| ObservationPackError::Store(e.to_string()))?;
            anchors.push(EvidenceAnchor::artifact(
                artifact.id,
                Some(artifact.byte_count),
            ));
        }
        if let Some(seq) = event_sequence {
            anchors.push(EvidenceAnchor::event(seq, Some(tool_call_id.clone())));
        }

        Ok(Self {
            tool_call_id,
            tool_name,
            outcome,
            preview: reduced.content.into_owned(),
            truncated,
            anchors,
            error: None,
            arguments_summary: None,
        })
    }

    /// Evidence-preserving reduce: shrink preview, never drop anchors.
    pub fn reduce_preserving_anchors(&self, budget: TokenBudget) -> Self {
        let reducer = OutputReducer::new_without_rtk(budget);
        let reduced = reducer.reduce(&self.preview);
        let truncated = self.truncated || reduced.truncated;
        Self {
            tool_call_id: self.tool_call_id.clone(),
            tool_name: self.tool_name.clone(),
            outcome: self.outcome.clone(),
            preview: reduced.content.into_owned(),
            truncated,
            anchors: self.anchors.clone(),
            error: self.error.clone(),
            arguments_summary: self.arguments_summary.clone(),
        }
    }

    /// Compact receipt text for compaction summaries (anchors preserved as labels).
    pub fn receipt_line(&self) -> String {
        let mut line = format!(
            "[{} {}] {}",
            self.tool_name,
            self.outcome,
            truncate_chars(&self.preview, 160)
        );
        for anchor in &self.anchors {
            line.push(' ');
            line.push_str(&anchor.label());
        }
        line
    }

    pub fn context_json(&self) -> Result<String, ObservationPackError> {
        serde_json::to_string(self).map_err(|e| ObservationPackError::Serialize(e.to_string()))
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ObservationPackError {
    #[error("artifact store required to pack truncated observation")]
    ArtifactStoreRequired,
    #[error("artifact store error: {0}")]
    Store(String),
    #[error("serialize observation pack: {0}")]
    Serialize(String),
    #[error("evidence not recoverable: {0}")]
    NotRecoverable(String),
}

/// Recover raw artifact bytes via an Evidence Anchor.
pub fn recover_artifact_bytes(
    store: &DurableArtifactStore,
    anchor: &EvidenceAnchor,
) -> Result<Vec<u8>, ObservationPackError> {
    let Some(id) = anchor.artifact_id() else {
        return Err(ObservationPackError::NotRecoverable(
            "anchor is not an artifact pointer".into(),
        ));
    };
    store
        .read(id)
        .map_err(|e| ObservationPackError::NotRecoverable(e.to_string()))
}

/// Recover a durable EventStore row via an event Evidence Anchor.
pub fn recover_event(
    store: &dyn EventStore,
    session_id: uuid::Uuid,
    anchor: &EvidenceAnchor,
) -> Result<Event, ObservationPackError> {
    let Some(sequence) = anchor.event_sequence() else {
        return Err(ObservationPackError::NotRecoverable(
            "anchor is not an event pointer".into(),
        ));
    };
    let events = store
        .list(session_id)
        .map_err(|e| ObservationPackError::NotRecoverable(e.to_string()))?;
    events
        .into_iter()
        .find(|e| e.sequence == sequence)
        .ok_or_else(|| {
            ObservationPackError::NotRecoverable(format!("event sequence {sequence} missing"))
        })
}

/// Collect Evidence Anchors from Tool::Observed rows in `[from_sequence, to_sequence]`.
pub fn anchors_from_events(
    events: &[Event],
    from_sequence: u64,
    to_sequence: u64,
) -> Vec<EvidenceAnchor> {
    let mut anchors = Vec::new();
    for event in events {
        if event.sequence < from_sequence || event.sequence > to_sequence {
            continue;
        }
        if let EventPayload::Tool(ToolEvent::Observed {
            tool_call_id,
            artifact,
            ..
        }) = &event.payload
        {
            if let Some(art) = artifact {
                anchors.push(EvidenceAnchor::artifact(
                    art.id.clone(),
                    Some(art.byte_count),
                ));
            }
            anchors.push(EvidenceAnchor::event(
                event.sequence,
                Some(tool_call_id.clone()),
            ));
        }
    }
    anchors
}

/// Append evidence-anchor receipt lines to a compaction summary (never drop anchors).
pub fn append_anchors_to_summary(summary: &str, anchors: &[EvidenceAnchor]) -> String {
    if anchors.is_empty() {
        return summary.to_string();
    }
    let mut out = String::with_capacity(summary.len() + anchors.len() * 64);
    out.push_str(summary);
    if !summary.is_empty() && !summary.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("[evidence anchors]\n");
    for anchor in anchors {
        out.push_str(&anchor.label());
        out.push('\n');
    }
    out
}

/// Parse artifact ids from compaction summary / receipt text.
pub fn parse_artifact_ids_from_summary(summary: &str) -> Vec<String> {
    summary
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("evidence_anchor:artifact:")?;
            let id = rest.split(':').next()?.to_string();
            if id.is_empty() { None } else { Some(id) }
        })
        .collect()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut truncated = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        truncated.push('…');
    }
    truncated
}

/// Helper: artifact ref → anchor (for callers that already spilled).
pub fn anchor_from_artifact(artifact: &DurableArtifactRef) -> EvidenceAnchor {
    EvidenceAnchor::artifact(artifact.id.clone(), Some(artifact.byte_count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::EventStore;
    use crate::{
        DurableArtifactStore, EventPayload, MemoryEventStore, ToolEvent, ToolEventOutcome,
        ToolObservation,
    };
    use std::sync::Arc;

    #[test]
    fn pack_raw_stores_artifact_and_keeps_anchor() {
        let root = tempfile::tempdir().expect("tmp");
        let store = DurableArtifactStore::open(root.path()).expect("open");
        let raw = (0..200)
            .map(|i| format!("line-{i}-evidence-marker-UNIQUE\n"))
            .collect::<String>();
        let pack = ObservationPack::pack_raw(
            Some(&store),
            "call-1",
            "shell",
            "Success",
            &raw,
            TokenBudget { max_tokens: 20 },
            Some(42),
        )
        .expect("pack");

        assert!(pack.truncated);
        assert_eq!(pack.anchors.len(), 2);
        assert!(pack.anchors.iter().any(|a| a.artifact_id().is_some()));
        assert!(pack.anchors.iter().any(|a| a.event_sequence() == Some(42)));

        let artifact_anchor = pack
            .anchors
            .iter()
            .find(|a| a.artifact_id().is_some())
            .expect("artifact anchor");
        let recovered = recover_artifact_bytes(&store, artifact_anchor).expect("recover");
        let recovered_text = String::from_utf8(recovered).expect("utf8");
        assert!(recovered_text.contains("evidence-marker-UNIQUE"));
        assert_eq!(recovered_text, raw);
    }

    #[test]
    fn reduce_preserving_anchors_never_drops_anchors() {
        let pack = ObservationPack {
            tool_call_id: "c1".into(),
            tool_name: "read_file".into(),
            outcome: "Success".into(),
            preview: (0..100)
                .map(|i| format!("error line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
            truncated: true,
            anchors: vec![
                EvidenceAnchor::artifact("abc123", Some(99)),
                EvidenceAnchor::event(7, Some("c1".into())),
            ],
            error: None,
            arguments_summary: None,
        };
        let reduced = pack.reduce_preserving_anchors(TokenBudget { max_tokens: 10 });
        assert!(reduced.truncated);
        assert_eq!(reduced.anchors, pack.anchors);
        assert!(reduced.preview.len() < pack.preview.len());
    }

    #[test]
    fn from_tool_observation_carries_artifact_and_event_anchors() {
        let obs = ToolObservation {
            tool_call_id: "tc".into(),
            tool_name: "search".into(),
            arguments_summary: "q=todo".into(),
            outcome: ToolEventOutcome::Success,
            preview: "hit".into(),
            artifact: Some(DurableArtifactRef {
                id: "deadbeef".into(),
                byte_count: 12,
            }),
            error: None,
        };
        let pack = ObservationPack::from_tool_observation(&obs, Some(9));
        assert_eq!(pack.anchors.len(), 2);
        assert_eq!(pack.anchors[0].artifact_id(), Some("deadbeef"));
        assert_eq!(pack.anchors[1].event_sequence(), Some(9));
    }

    #[test]
    fn compaction_summary_anchors_recover_artifact() {
        let root = tempfile::tempdir().expect("tmp");
        let store = DurableArtifactStore::open(root.path()).expect("open");
        let raw = b"compiler error: UNIQUE_FAIL_MARKER at src/main.rs:1";
        let artifact = store.store(raw).expect("store");
        let anchors = vec![
            EvidenceAnchor::artifact(artifact.id.clone(), Some(artifact.byte_count)),
            EvidenceAnchor::event(3, Some("tool-1".into())),
        ];
        let summary = append_anchors_to_summary("[user] early turn", &anchors);
        assert!(summary.contains("evidence_anchor:artifact:"));
        let ids = parse_artifact_ids_from_summary(&summary);
        assert_eq!(ids, vec![artifact.id.clone()]);
        let recovered = store.read(&ids[0]).expect("read");
        assert_eq!(recovered, raw);
    }

    #[test]
    fn anchors_from_events_and_event_recover() {
        let events: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let session = events.create_session().expect("session");
        let artifact = DurableArtifactRef {
            id: "art-1".into(),
            byte_count: 4,
        };
        events
            .append_next(
                session,
                EventPayload::Tool(ToolEvent::Observed {
                    tool_call_id: "t1".into(),
                    tool_name: "shell".into(),
                    arguments_summary: "echo".into(),
                    outcome: ToolEventOutcome::Success,
                    preview: "ok".into(),
                    artifact: Some(artifact),
                    error: None,
                }),
            )
            .expect("append");
        let listed = events.list(session).expect("list");
        let observed = listed
            .iter()
            .find(|e| matches!(&e.payload, EventPayload::Tool(ToolEvent::Observed { .. })))
            .expect("observed event");
        let seq = observed.sequence;
        let anchors = anchors_from_events(&listed, seq, seq);
        assert!(anchors.iter().any(|a| a.artifact_id() == Some("art-1")));
        let event_anchor = anchors
            .iter()
            .find(|a| a.event_sequence() == Some(seq))
            .expect("event anchor");
        let recovered = recover_event(events.as_ref(), session, event_anchor).expect("recover");
        assert_eq!(recovered.sequence, seq);
        assert!(matches!(
            recovered.payload,
            EventPayload::Tool(ToolEvent::Observed { .. })
        ));
    }
}
