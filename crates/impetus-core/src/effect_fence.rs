//! Write-ahead Effect Fence: durable invocation identity + state machine.
//!
//! Prepared → Started → Observed | Failed | Unknown.
//! `Unknown` / unfinished `Started` must never be treated as safe Completed
//! or as safe idempotent replay. Digest inputs are labels/kinds/paths only —
//! never tokens, private keys, or raw secret argument bodies.

use crate::storage::{EventStore, StoreError};
use crate::{EffectCapability, Event, EventPayload, NormalizedEffect};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

/// Stable SHA-256 hex digest over typed, non-secret effect identity fields.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ArgsDigest(String);

impl ArgsDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Display for ArgsDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Durable effect invocation identity (survives crash / session attach).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffectInvocationIdentity {
    pub effect_id: Uuid,
    pub args_digest: ArgsDigest,
    /// Stable kind label (capability + action kind).
    pub kind: String,
    /// Path/resource label only — never secret material.
    pub target_label: Option<String>,
}

/// Fence state machine. Terminal: Observed | Failed | Unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FenceState {
    Prepared,
    Started,
    Observed,
    Failed,
    Unknown,
}

impl FenceState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Observed | Self::Failed | Self::Unknown)
    }

    /// Crash mid-flight (`Started`) is treated like Unknown for replay safety.
    pub fn blocks_blind_replay(self) -> bool {
        matches!(self, Self::Started | Self::Unknown)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffectFenceRecord {
    pub identity: EffectInvocationIdentity,
    pub state: FenceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Reconciliation outcome for a proposed replay of the same args_digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceReplayDecision {
    /// Durable evidence proves execution never began (Prepared only).
    AllowFresh,
    /// Already Observed — not a blind replay; caller may short-circuit.
    AlreadyObserved { summary: Option<String> },
    /// Known Failed — explicit retry policy is out of scope for this slice.
    AlreadyFailed { reason: Option<String> },
    /// Unknown / Started / digest mismatch — refuse blind idempotent replay.
    Refuse { reason: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EffectFenceError {
    #[error("effect fence store error: {0}")]
    Store(String),
    #[error("effect fence refuse replay: {0}")]
    RefuseReplay(String),
}

impl From<StoreError> for EffectFenceError {
    fn from(value: StoreError) -> Self {
        Self::Store(value.to_string())
    }
}

/// Compute a stable args_digest from kind/path/origin labels only.
pub fn digest_args(parts: &[&str]) -> ArgsDigest {
    let mut hasher = Sha256::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update([0u8]);
        }
        hasher.update(part.as_bytes());
    }
    ArgsDigest(format!("{:x}", hasher.finalize()))
}

fn capability_label(capability: EffectCapability) -> &'static str {
    match capability {
        EffectCapability::WorkspaceRead => "workspace_read",
        EffectCapability::WorkspaceWrite => "workspace_write",
        EffectCapability::ProcessSpawn => "process_spawn",
        EffectCapability::NetworkConnect => "network_connect",
    }
}

fn origin_label(origin: crate::ActionOrigin) -> &'static str {
    match origin {
        crate::ActionOrigin::User => "user",
        crate::ActionOrigin::Agent => "agent",
    }
}

fn action_kind_label(kind: crate::ActionKind) -> String {
    // serde snake_case names stay stable across Debug churn.
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{kind:?}").to_ascii_lowercase())
}

/// Digest for a normalized effect: capability, action kind, origin, target path, version.
/// Excludes summary and any secret-bearing argument bodies.
pub fn args_digest_for_effect(effect: &NormalizedEffect) -> ArgsDigest {
    let kind = action_kind_label(effect.action.kind);
    let target = effect.action.target.as_deref().unwrap_or("");
    let version = effect.version.0.to_string();
    digest_args(&[
        capability_label(effect.capability),
        kind.as_str(),
        origin_label(effect.origin),
        target,
        version.as_str(),
    ])
}

pub fn kind_label_for_effect(effect: &NormalizedEffect) -> String {
    format!(
        "{}:{}",
        capability_label(effect.capability),
        action_kind_label(effect.action.kind)
    )
}

impl EffectInvocationIdentity {
    pub fn from_effect(effect: &NormalizedEffect) -> Self {
        Self {
            effect_id: Uuid::new_v4(),
            args_digest: args_digest_for_effect(effect),
            kind: kind_label_for_effect(effect),
            target_label: effect.action.target.clone(),
        }
    }

    pub fn with_id(effect: &NormalizedEffect, effect_id: Uuid) -> Self {
        Self {
            effect_id,
            args_digest: args_digest_for_effect(effect),
            kind: kind_label_for_effect(effect),
            target_label: effect.action.target.clone(),
        }
    }
}

/// Pure reconcile: Unknown / unfinished Started ≠ safe Completed / blind replay.
pub fn reconcile_for_replay(
    record: &EffectFenceRecord,
    expected_digest: &ArgsDigest,
) -> FenceReplayDecision {
    if record.identity.args_digest != *expected_digest {
        return FenceReplayDecision::Refuse {
            reason: "args_digest mismatch with durable fence record".into(),
        };
    }
    match record.state {
        FenceState::Prepared => FenceReplayDecision::AllowFresh,
        FenceState::Observed => FenceReplayDecision::AlreadyObserved {
            summary: record.summary.clone(),
        },
        FenceState::Failed => FenceReplayDecision::AlreadyFailed {
            reason: record.summary.clone(),
        },
        FenceState::Started => FenceReplayDecision::Refuse {
            reason: "effect Started without Observed — outcome Unknown; refuse blind replay".into(),
        },
        FenceState::Unknown => FenceReplayDecision::Refuse {
            reason: "effect fence state Unknown — refuse blind idempotent replay".into(),
        },
    }
}

/// Fold EventStore fence events into latest record per effect_id.
pub fn records_from_events(events: &[Event]) -> HashMap<Uuid, EffectFenceRecord> {
    let mut map = HashMap::new();
    for event in events {
        let EventPayload::EffectFence(fence) = &event.payload else {
            continue;
        };
        apply_fence_event(&mut map, fence);
    }
    map
}

fn apply_fence_event(map: &mut HashMap<Uuid, EffectFenceRecord>, fence: &crate::EffectFenceEvent) {
    use crate::EffectFenceEvent;
    match fence {
        EffectFenceEvent::Prepared {
            effect_id,
            args_digest,
            kind,
            target_label,
        } => {
            map.insert(
                *effect_id,
                EffectFenceRecord {
                    identity: EffectInvocationIdentity {
                        effect_id: *effect_id,
                        args_digest: ArgsDigest(args_digest.clone()),
                        kind: kind.clone(),
                        target_label: target_label.clone(),
                    },
                    state: FenceState::Prepared,
                    summary: None,
                },
            );
        }
        EffectFenceEvent::Started {
            effect_id,
            args_digest,
        } => {
            let entry = map.entry(*effect_id).or_insert_with(|| EffectFenceRecord {
                identity: EffectInvocationIdentity {
                    effect_id: *effect_id,
                    args_digest: ArgsDigest(args_digest.clone()),
                    kind: "unknown".into(),
                    target_label: None,
                },
                state: FenceState::Prepared,
                summary: None,
            });
            entry.identity.args_digest = ArgsDigest(args_digest.clone());
            entry.state = FenceState::Started;
        }
        EffectFenceEvent::Observed {
            effect_id,
            args_digest,
            summary,
        } => {
            let entry = map.entry(*effect_id).or_insert_with(|| EffectFenceRecord {
                identity: EffectInvocationIdentity {
                    effect_id: *effect_id,
                    args_digest: ArgsDigest(args_digest.clone()),
                    kind: "unknown".into(),
                    target_label: None,
                },
                state: FenceState::Prepared,
                summary: None,
            });
            entry.identity.args_digest = ArgsDigest(args_digest.clone());
            entry.state = FenceState::Observed;
            entry.summary = Some(summary.clone());
        }
        EffectFenceEvent::Failed {
            effect_id,
            args_digest,
            reason,
        } => {
            let entry = map.entry(*effect_id).or_insert_with(|| EffectFenceRecord {
                identity: EffectInvocationIdentity {
                    effect_id: *effect_id,
                    args_digest: ArgsDigest(args_digest.clone()),
                    kind: "unknown".into(),
                    target_label: None,
                },
                state: FenceState::Prepared,
                summary: None,
            });
            entry.identity.args_digest = ArgsDigest(args_digest.clone());
            entry.state = FenceState::Failed;
            entry.summary = Some(reason.clone());
        }
        EffectFenceEvent::Unknown {
            effect_id,
            args_digest,
            reason,
        } => {
            let entry = map.entry(*effect_id).or_insert_with(|| EffectFenceRecord {
                identity: EffectInvocationIdentity {
                    effect_id: *effect_id,
                    args_digest: ArgsDigest(args_digest.clone()),
                    kind: "unknown".into(),
                    target_label: None,
                },
                state: FenceState::Prepared,
                summary: None,
            });
            entry.identity.args_digest = ArgsDigest(args_digest.clone());
            entry.state = FenceState::Unknown;
            entry.summary = Some(reason.clone());
        }
    }
}

/// Persist fence transitions via EventStore (survives session attach).
pub struct EffectFenceLedger {
    store: Arc<dyn EventStore>,
    session_id: Uuid,
}

impl EffectFenceLedger {
    pub fn new(store: Arc<dyn EventStore>, session_id: Uuid) -> Self {
        Self { store, session_id }
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    fn append(&self, event: crate::EffectFenceEvent) -> Result<(), EffectFenceError> {
        self.store
            .append_next(self.session_id, EventPayload::EffectFence(event))?;
        Ok(())
    }

    pub fn prepare(
        &self,
        identity: &EffectInvocationIdentity,
    ) -> Result<EffectFenceRecord, EffectFenceError> {
        self.append(crate::EffectFenceEvent::Prepared {
            effect_id: identity.effect_id,
            args_digest: identity.args_digest.as_str().to_owned(),
            kind: identity.kind.clone(),
            target_label: identity.target_label.clone(),
        })?;
        Ok(EffectFenceRecord {
            identity: identity.clone(),
            state: FenceState::Prepared,
            summary: None,
        })
    }

    pub fn mark_started(
        &self,
        identity: &EffectInvocationIdentity,
    ) -> Result<EffectFenceRecord, EffectFenceError> {
        self.append(crate::EffectFenceEvent::Started {
            effect_id: identity.effect_id,
            args_digest: identity.args_digest.as_str().to_owned(),
        })?;
        Ok(EffectFenceRecord {
            identity: identity.clone(),
            state: FenceState::Started,
            summary: None,
        })
    }

    /// Write-ahead: Prepared then Started before the side effect begins.
    pub fn prepare_and_start(
        &self,
        identity: &EffectInvocationIdentity,
    ) -> Result<EffectFenceRecord, EffectFenceError> {
        self.prepare(identity)?;
        self.mark_started(identity)
    }

    pub fn observe(
        &self,
        identity: &EffectInvocationIdentity,
        summary: impl Into<String>,
    ) -> Result<EffectFenceRecord, EffectFenceError> {
        let summary = bound_fence_summary(summary.into());
        self.append(crate::EffectFenceEvent::Observed {
            effect_id: identity.effect_id,
            args_digest: identity.args_digest.as_str().to_owned(),
            summary: summary.clone(),
        })?;
        Ok(EffectFenceRecord {
            identity: identity.clone(),
            state: FenceState::Observed,
            summary: Some(summary),
        })
    }

    pub fn mark_failed(
        &self,
        identity: &EffectInvocationIdentity,
        reason: impl Into<String>,
    ) -> Result<EffectFenceRecord, EffectFenceError> {
        let reason = bound_fence_summary(reason.into());
        self.append(crate::EffectFenceEvent::Failed {
            effect_id: identity.effect_id,
            args_digest: identity.args_digest.as_str().to_owned(),
            reason: reason.clone(),
        })?;
        Ok(EffectFenceRecord {
            identity: identity.clone(),
            state: FenceState::Failed,
            summary: Some(reason),
        })
    }

    pub fn mark_unknown(
        &self,
        identity: &EffectInvocationIdentity,
        reason: impl Into<String>,
    ) -> Result<EffectFenceRecord, EffectFenceError> {
        let reason = bound_fence_summary(reason.into());
        self.append(crate::EffectFenceEvent::Unknown {
            effect_id: identity.effect_id,
            args_digest: identity.args_digest.as_str().to_owned(),
            reason: reason.clone(),
        })?;
        Ok(EffectFenceRecord {
            identity: identity.clone(),
            state: FenceState::Unknown,
            summary: Some(reason),
        })
    }

    pub fn load_all(&self) -> Result<HashMap<Uuid, EffectFenceRecord>, EffectFenceError> {
        let events = self.store.list(self.session_id)?;
        Ok(records_from_events(&events))
    }

    pub fn latest(&self, effect_id: Uuid) -> Result<Option<EffectFenceRecord>, EffectFenceError> {
        Ok(self.load_all()?.remove(&effect_id))
    }

    /// Find latest fence for an args_digest (any effect_id).
    pub fn latest_for_digest(
        &self,
        digest: &ArgsDigest,
    ) -> Result<Option<EffectFenceRecord>, EffectFenceError> {
        let mut best: Option<EffectFenceRecord> = None;
        for record in self.load_all()?.into_values() {
            if &record.identity.args_digest == digest {
                best = Some(record);
            }
        }
        Ok(best)
    }

    /// Refuse blind replay when durable state is Unknown / Started / digest mismatch.
    pub fn check_replay_safe(
        &self,
        expected_digest: &ArgsDigest,
    ) -> Result<FenceReplayDecision, EffectFenceError> {
        match self.latest_for_digest(expected_digest)? {
            None => Ok(FenceReplayDecision::AllowFresh),
            Some(record) => Ok(reconcile_for_replay(&record, expected_digest)),
        }
    }
}

const MAX_FENCE_SUMMARY_CHARS: usize = 256;

fn bound_fence_summary(input: String) -> String {
    if input.chars().count() <= MAX_FENCE_SUMMARY_CHARS {
        return input;
    }
    input.chars().take(MAX_FENCE_SUMMARY_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActionOrigin, EffectSeam, MemoryEventStore, NormalizedEffect, SandboxScope,
        storage::EventStore,
    };

    fn session_ledger() -> (Uuid, Arc<MemoryEventStore>, EffectFenceLedger) {
        let store = Arc::new(MemoryEventStore::default());
        let session = store.create_session().expect("session");
        let ledger = EffectFenceLedger::new(store.clone(), session);
        (session, store, ledger)
    }

    #[test]
    fn args_digest_stable_for_same_args() {
        let effect_a =
            NormalizedEffect::workspace_write(ActionOrigin::Agent, "write note", "out.txt");
        let effect_b =
            NormalizedEffect::workspace_write(ActionOrigin::Agent, "different summary", "out.txt");
        let d1 = args_digest_for_effect(&effect_a);
        let d2 = args_digest_for_effect(&effect_b);
        assert_eq!(d1, d2, "summary must not enter digest");
        let effect_c =
            NormalizedEffect::workspace_write(ActionOrigin::Agent, "write note", "other.txt");
        assert_ne!(d1, args_digest_for_effect(&effect_c));
    }

    #[test]
    fn happy_path_observed_survives_session_events() {
        let (_session, store, ledger) = session_ledger();
        let effect =
            NormalizedEffect::workspace_write(ActionOrigin::Agent, "write note", "out.txt");
        let identity = EffectInvocationIdentity::from_effect(&effect);
        ledger.prepare_and_start(&identity).expect("prepare");
        ledger.observe(&identity, "ok").expect("observe");

        let events = store.list(ledger.session_id()).expect("list");
        let records = records_from_events(&events);
        let record = records.get(&identity.effect_id).expect("record");
        assert_eq!(record.state, FenceState::Observed);
        assert_eq!(record.identity.args_digest, identity.args_digest);

        // Attach-style reload from same EventStore.
        let reloaded = EffectFenceLedger::new(store, ledger.session_id());
        let again = reloaded.latest(identity.effect_id).expect("load");
        assert_eq!(again.map(|r| r.state), Some(FenceState::Observed));
    }

    #[test]
    fn unknown_blocks_safe_replay() {
        let (_session, _store, ledger) = session_ledger();
        let effect =
            NormalizedEffect::workspace_write(ActionOrigin::Agent, "write note", "out.txt");
        let identity = EffectInvocationIdentity::from_effect(&effect);
        ledger.prepare_and_start(&identity).expect("prepare");
        ledger
            .mark_unknown(&identity, "crash before observe")
            .expect("unknown");

        let decision = ledger
            .check_replay_safe(&identity.args_digest)
            .expect("reconcile");
        assert!(
            matches!(decision, FenceReplayDecision::Refuse { .. }),
            "Unknown must refuse blind replay: {decision:?}"
        );

        let record = ledger.latest(identity.effect_id).expect("load").unwrap();
        assert!(record.state.blocks_blind_replay());
        assert_eq!(
            reconcile_for_replay(&record, &identity.args_digest),
            FenceReplayDecision::Refuse {
                reason: "effect fence state Unknown — refuse blind idempotent replay".into(),
            }
        );
    }

    #[test]
    fn started_without_observe_blocks_replay_like_unknown() {
        let (_session, _store, ledger) = session_ledger();
        let effect =
            NormalizedEffect::workspace_write(ActionOrigin::Agent, "write note", "out.txt");
        let identity = EffectInvocationIdentity::from_effect(&effect);
        ledger.prepare_and_start(&identity).expect("prepare");

        let decision = ledger
            .check_replay_safe(&identity.args_digest)
            .expect("reconcile");
        assert!(
            matches!(decision, FenceReplayDecision::Refuse { reason } if reason.contains("Unknown") || reason.contains("Started"))
        );
    }

    #[test]
    fn execute_with_fence_observes_on_success() {
        let root = std::env::temp_dir().join(format!("fence-exec-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        let (_session, store, ledger) = session_ledger();
        let seam = EffectSeam::workspace_full(&root)
            .with_execution_mode(crate::ExecutionMode::AcceptEdits);
        let effect =
            NormalizedEffect::workspace_write(ActionOrigin::User, "create file", "new.txt");

        let outcome = seam
            .execute_with_fence(&effect, &ledger, || Ok::<_, ()>("done"))
            .expect("exec");
        assert_eq!(outcome, crate::EffectExecution::Executed("done"));

        let records = records_from_events(&store.list(ledger.session_id()).unwrap());
        assert!(records.values().any(|r| r.state == FenceState::Observed
            && r.identity.args_digest == args_digest_for_effect(&effect)));
    }

    #[test]
    fn execute_with_fence_refuses_when_prior_unknown() {
        let root = std::env::temp_dir().join(format!("fence-refuse-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        let (_session, _store, ledger) = session_ledger();
        let seam = EffectSeam::workspace_full(&root)
            .with_execution_mode(crate::ExecutionMode::AcceptEdits);
        let effect =
            NormalizedEffect::workspace_write(ActionOrigin::User, "create file", "new.txt");
        let identity = EffectInvocationIdentity::from_effect(&effect);
        ledger.prepare_and_start(&identity).unwrap();
        ledger.mark_unknown(&identity, "lost").unwrap();

        let outcome = seam
            .execute_with_fence(&effect, &ledger, || -> Result<(), ()> {
                panic!("must not execute under Unknown fence")
            })
            .expect("denial is ok");
        assert!(matches!(
            outcome,
            crate::EffectExecution::Denied { reason } if reason.contains("refuse") || reason.contains("Unknown")
        ));
        let _ = SandboxScope::local_workspace(&root);
    }
}
