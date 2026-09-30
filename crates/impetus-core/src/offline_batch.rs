//! Durable offline batch execution + generic [`BatchProvider`] boundary (#416 / parent #397).
//!
//! Mock provider only in this slice — no live network. Submit intent is journaled
//! to EventStore **before** cost-bearing provider submit. `Unknown` blocks blind
//! duplicate submission. Collection is workspace-root confined and idempotent.

use crate::storage::{EventStore, StoreError};
use crate::workspace_files::{WorkspaceFilesError, resolve_workspace_path};
use crate::{Event, EventPayload, OfflineBatchEvent};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

/// How often `impetusd` re-runs [`OfflineBatchRegistry::poll_all`] after startup.
/// Shorter than artifact GC — offline batch Ready collect should not wait hours.
pub const OFFLINE_BATCH_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Terminal batch lifecycle states exposed to callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchLifecycleState {
    Submitted,
    Collecting,
    Delivered,
    Failed,
    Unknown,
}

impl BatchLifecycleState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Delivered | Self::Failed | Self::Unknown)
    }

    pub fn blocks_blind_resubmit(self) -> bool {
        matches!(self, Self::Submitted | Self::Collecting | Self::Unknown)
    }
}

/// Immutable execution configuration frozen at submission (digest labels only).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FrozenBatchConfig {
    pub provider_label: String,
    pub model_label: String,
    /// Stable option tokens affecting cost/output (temperature caps, etc.).
    pub option_labels: Vec<String>,
    pub digest: String,
}

impl FrozenBatchConfig {
    pub fn freeze(
        provider_label: impl Into<String>,
        model_label: impl Into<String>,
        mut option_labels: Vec<String>,
        plan: &BatchPlan,
    ) -> Self {
        let provider_label = provider_label.into();
        let model_label = model_label.into();
        option_labels.sort();
        option_labels.dedup();
        let mut item_ids: Vec<String> = plan.items.iter().map(|i| i.item_id.clone()).collect();
        item_ids.sort();
        let mut input_hashes: Vec<String> =
            plan.items.iter().map(|i| i.input_hash.clone()).collect();
        input_hashes.sort();
        let digest = compute_config_digest(
            &provider_label,
            &model_label,
            &option_labels,
            &item_ids,
            &input_hashes,
        );
        Self {
            provider_label,
            model_label,
            option_labels,
            digest,
        }
    }
}

fn compute_config_digest(
    provider: &str,
    model: &str,
    options: &[String],
    item_ids: &[String],
    input_hashes: &[String],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"impetus.offline_batch_config.v1\n");
    hasher.update(b"provider:");
    hasher.update(provider.as_bytes());
    hasher.update(b"\nmodel:");
    hasher.update(model.as_bytes());
    hasher.update(b"\noptions:");
    hasher.update(options.join(",").as_bytes());
    hasher.update(b"\nitems:");
    hasher.update(item_ids.join(",").as_bytes());
    hasher.update(b"\ninputs:");
    hasher.update(input_hashes.join(",").as_bytes());
    hasher.update(b"\n");
    format!("sha256:{:x}", hasher.finalize())
}

/// One decomposable batch item (paths/hashes only).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BatchItemSpec {
    pub item_id: String,
    /// Hash of workspace target file at prepare/submit time.
    pub input_hash: String,
    pub output_relpath: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BatchPlan {
    pub items: Vec<BatchItemSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchOperationRecord {
    pub batch_id: Uuid,
    pub state: BatchLifecycleState,
    pub config_digest: String,
    pub provider_job_id: Option<String>,
    pub delivered_items: HashSet<String>,
}

/// Whether a new submit is safe for the same frozen config digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchResubmitDecision {
    AllowFresh,
    ContinueExisting {
        batch_id: Uuid,
        state: BatchLifecycleState,
    },
    RefuseDuplicateCost {
        batch_id: Uuid,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchPrepareOutcome {
    pub normalized_item_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchSubmitOutcome {
    pub provider_job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchProviderStatus {
    Pending,
    Running,
    Ready,
    Failed { reason: String },
    Unknown { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchItemResult {
    pub item_id: String,
    pub result_bytes: Vec<u8>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum BatchProviderError {
    #[error("batch provider prepare: {0}")]
    Prepare(String),
    #[error("batch provider submit: {0}")]
    Submit(String),
    #[error("batch provider status: {0}")]
    Status(String),
    #[error("batch provider collect: {0}")]
    Collect(String),
    #[error("batch provider cancel: {0}")]
    Cancel(String),
}

/// Replaceable batch execution boundary — concrete APIs live in providers/extensions.
pub trait BatchProvider: Send + Sync {
    fn prepare(&self, plan: &BatchPlan) -> Result<BatchPrepareOutcome, BatchProviderError>;
    fn submit(
        &self,
        batch_id: Uuid,
        config_digest: &str,
        plan: &BatchPlan,
    ) -> Result<BatchSubmitOutcome, BatchProviderError>;
    fn status(&self, provider_job_id: &str) -> Result<BatchProviderStatus, BatchProviderError>;
    fn collect_results(
        &self,
        provider_job_id: &str,
    ) -> Result<Vec<BatchItemResult>, BatchProviderError>;
    fn cancel(&self, provider_job_id: &str) -> Result<(), BatchProviderError>;
}

#[derive(Debug)]
struct MockJob {
    status: BatchProviderStatus,
    results: Vec<BatchItemResult>,
}

/// In-memory mock batch provider for unit tests (no network).
#[derive(Debug, Default)]
pub struct MockBatchProvider {
    inner: Mutex<HashMap<String, MockJob>>,
}

impl MockBatchProvider {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BatchProvider for MockBatchProvider {
    fn prepare(&self, plan: &BatchPlan) -> Result<BatchPrepareOutcome, BatchProviderError> {
        if plan.items.is_empty() {
            return Err(BatchProviderError::Prepare("empty plan".into()));
        }
        Ok(BatchPrepareOutcome {
            normalized_item_count: plan.items.len() as u32,
        })
    }

    fn submit(
        &self,
        batch_id: Uuid,
        config_digest: &str,
        plan: &BatchPlan,
    ) -> Result<BatchSubmitOutcome, BatchProviderError> {
        let provider_job_id = format!("mock-job-{batch_id}");
        let results: Vec<BatchItemResult> = plan
            .items
            .iter()
            .map(|item| BatchItemResult {
                item_id: item.item_id.clone(),
                result_bytes: format!("result-for-{}", item.item_id).into_bytes(),
            })
            .collect();
        let mut guard = self.inner.lock().expect("mock batch lock");
        guard.insert(
            provider_job_id.clone(),
            MockJob {
                status: BatchProviderStatus::Ready,
                results,
            },
        );
        let _ = config_digest;
        Ok(BatchSubmitOutcome { provider_job_id })
    }

    fn status(&self, provider_job_id: &str) -> Result<BatchProviderStatus, BatchProviderError> {
        let guard = self.inner.lock().expect("mock batch lock");
        guard
            .get(provider_job_id)
            .map(|job| job.status.clone())
            .ok_or_else(|| BatchProviderError::Status(format!("unknown job {provider_job_id}")))
    }

    fn collect_results(
        &self,
        provider_job_id: &str,
    ) -> Result<Vec<BatchItemResult>, BatchProviderError> {
        let guard = self.inner.lock().expect("mock batch lock");
        guard
            .get(provider_job_id)
            .map(|job| job.results.clone())
            .ok_or_else(|| BatchProviderError::Collect(format!("unknown job {provider_job_id}")))
    }

    fn cancel(&self, provider_job_id: &str) -> Result<(), BatchProviderError> {
        let mut guard = self.inner.lock().expect("mock batch lock");
        guard.remove(provider_job_id);
        Ok(())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OfflineBatchError {
    #[error("offline batch store error: {0}")]
    Store(String),
    #[error("offline batch provider error: {0}")]
    Provider(String),
    #[error("offline batch refuse resubmit: {0}")]
    RefuseResubmit(String),
    #[error("offline batch not found: {0}")]
    NotFound(Uuid),
    #[error("offline batch workspace: {0}")]
    Workspace(String),
    #[error("offline batch collect conflict: {0}")]
    CollectConflict(String),
}

impl From<StoreError> for OfflineBatchError {
    fn from(value: StoreError) -> Self {
        Self::Store(value.to_string())
    }
}

impl From<BatchProviderError> for OfflineBatchError {
    fn from(value: BatchProviderError) -> Self {
        Self::Provider(value.to_string())
    }
}

impl From<WorkspaceFilesError> for OfflineBatchError {
    fn from(value: WorkspaceFilesError) -> Self {
        Self::Workspace(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectItemOutcome {
    Written {
        result_hash: String,
    },
    AlreadyDelivered {
        result_hash: String,
    },
    SourceHashConflict {
        expected_source_hash: String,
        current_hash: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectBatchOutcome {
    pub batch_id: Uuid,
    pub state: BatchLifecycleState,
    pub item_outcomes: Vec<(String, CollectItemOutcome)>,
}

/// One batch touched by [`DurableOfflineBatchExecutor::poll_collect_due`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollCollectDueResult {
    pub batch_id: Uuid,
    pub action: PollCollectAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollCollectAction {
    /// In-flight batch with no entry in the caller's plan map.
    SkippedNoPlan,
    /// Provider still Pending/Running — no journal transition.
    AwaitingProvider,
    /// Terminal failure journaled from provider status.
    MarkedFailed { reason: String },
    /// Ambiguous provider outcome journaled as Unknown.
    MarkedUnknown { reason: String },
    /// Ready path ran [`DurableOfflineBatchExecutor::collect_durable_with_plan`].
    Collected(CollectBatchOutcome),
}

/// Durable submit journal backed by EventStore.
#[derive(Clone)]
pub struct OfflineBatchJournal {
    store: Arc<dyn EventStore>,
    session_id: Uuid,
}

impl OfflineBatchJournal {
    pub fn new(store: Arc<dyn EventStore>, session_id: Uuid) -> Self {
        Self { store, session_id }
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    fn append(&self, event: OfflineBatchEvent) -> Result<(), OfflineBatchError> {
        self.store
            .append_next(self.session_id, EventPayload::OfflineBatch(event))?;
        Ok(())
    }

    pub fn load_records(&self) -> Result<HashMap<Uuid, BatchOperationRecord>, OfflineBatchError> {
        let events = self.store.list(self.session_id)?;
        Ok(batch_records_from_events(&events))
    }

    pub fn record_for(
        &self,
        batch_id: Uuid,
    ) -> Result<Option<BatchOperationRecord>, OfflineBatchError> {
        Ok(self.load_records()?.get(&batch_id).cloned())
    }
}

pub fn batch_records_from_events(events: &[Event]) -> HashMap<Uuid, BatchOperationRecord> {
    let mut map = HashMap::new();
    for event in events {
        let EventPayload::OfflineBatch(batch) = &event.payload else {
            continue;
        };
        apply_batch_event(&mut map, batch);
    }
    map
}

fn apply_batch_event(map: &mut HashMap<Uuid, BatchOperationRecord>, batch: &OfflineBatchEvent) {
    match batch {
        OfflineBatchEvent::Submitted {
            batch_id,
            config_digest,
            ..
        } => {
            map.entry(*batch_id)
                .or_insert_with(|| BatchOperationRecord {
                    batch_id: *batch_id,
                    state: BatchLifecycleState::Submitted,
                    config_digest: config_digest.clone(),
                    provider_job_id: None,
                    delivered_items: HashSet::new(),
                });
        }
        OfflineBatchEvent::ProviderLinked {
            batch_id,
            config_digest,
            provider_job_id,
        } => {
            let entry = map
                .entry(*batch_id)
                .or_insert_with(|| BatchOperationRecord {
                    batch_id: *batch_id,
                    state: BatchLifecycleState::Submitted,
                    config_digest: config_digest.clone(),
                    provider_job_id: None,
                    delivered_items: HashSet::new(),
                });
            entry.config_digest = config_digest.clone();
            entry.provider_job_id = Some(provider_job_id.clone());
        }
        OfflineBatchEvent::Collecting {
            batch_id,
            config_digest,
        } => {
            let entry = map
                .entry(*batch_id)
                .or_insert_with(|| BatchOperationRecord {
                    batch_id: *batch_id,
                    state: BatchLifecycleState::Collecting,
                    config_digest: config_digest.clone(),
                    provider_job_id: None,
                    delivered_items: HashSet::new(),
                });
            entry.state = BatchLifecycleState::Collecting;
            entry.config_digest = config_digest.clone();
        }
        OfflineBatchEvent::ItemDelivered {
            batch_id, item_id, ..
        } => {
            if let Some(entry) = map.get_mut(batch_id) {
                entry.delivered_items.insert(item_id.clone());
            }
        }
        OfflineBatchEvent::Delivered {
            batch_id,
            config_digest,
        } => {
            let entry = map
                .entry(*batch_id)
                .or_insert_with(|| BatchOperationRecord {
                    batch_id: *batch_id,
                    state: BatchLifecycleState::Delivered,
                    config_digest: config_digest.clone(),
                    provider_job_id: None,
                    delivered_items: HashSet::new(),
                });
            entry.state = BatchLifecycleState::Delivered;
            entry.config_digest = config_digest.clone();
        }
        OfflineBatchEvent::Failed {
            batch_id,
            config_digest,
            ..
        } => {
            let entry = map
                .entry(*batch_id)
                .or_insert_with(|| BatchOperationRecord {
                    batch_id: *batch_id,
                    state: BatchLifecycleState::Failed,
                    config_digest: config_digest.clone(),
                    provider_job_id: None,
                    delivered_items: HashSet::new(),
                });
            entry.state = BatchLifecycleState::Failed;
            entry.config_digest = config_digest.clone();
        }
        OfflineBatchEvent::Unknown {
            batch_id,
            config_digest,
            ..
        } => {
            let entry = map
                .entry(*batch_id)
                .or_insert_with(|| BatchOperationRecord {
                    batch_id: *batch_id,
                    state: BatchLifecycleState::Unknown,
                    config_digest: config_digest.clone(),
                    provider_job_id: None,
                    delivered_items: HashSet::new(),
                });
            entry.state = BatchLifecycleState::Unknown;
            entry.config_digest = config_digest.clone();
        }
        OfflineBatchEvent::ResubmitRefused { .. } => {}
    }
}

pub fn reconcile_resubmit(
    records: &HashMap<Uuid, BatchOperationRecord>,
    config_digest: &str,
) -> BatchResubmitDecision {
    let mut matches: Vec<&BatchOperationRecord> = records
        .values()
        .filter(|r| r.config_digest == config_digest)
        .collect();
    matches.sort_by_key(|r| r.batch_id);
    let Some(latest) = matches.last() else {
        return BatchResubmitDecision::AllowFresh;
    };
    if latest.state == BatchLifecycleState::Delivered {
        return BatchResubmitDecision::ContinueExisting {
            batch_id: latest.batch_id,
            state: latest.state,
        };
    }
    if latest.state.blocks_blind_resubmit() {
        let reason = match latest.state {
            BatchLifecycleState::Unknown => {
                "batch outcome unknown — reconcile before cost-bearing resubmit".to_string()
            }
            BatchLifecycleState::Submitted if latest.provider_job_id.is_none() => {
                "submit journal present without provider link — reconcile first".to_string()
            }
            BatchLifecycleState::Submitted | BatchLifecycleState::Collecting => {
                "batch still in flight — wait or reconcile".to_string()
            }
            _ => "batch blocks blind resubmit".to_string(),
        };
        return BatchResubmitDecision::RefuseDuplicateCost {
            batch_id: latest.batch_id,
            reason,
        };
    }
    if latest.state == BatchLifecycleState::Failed {
        return BatchResubmitDecision::AllowFresh;
    }
    BatchResubmitDecision::ContinueExisting {
        batch_id: latest.batch_id,
        state: latest.state,
    }
}

/// Deterministic submit + collect orchestration.
///
/// Daemon/harness should call [`Self::poll_collect_due`] on a tick (no sleep here).
pub struct DurableOfflineBatchExecutor {
    pub journal: OfflineBatchJournal,
    provider: Arc<dyn BatchProvider>,
    workspace_root: PathBuf,
}

impl DurableOfflineBatchExecutor {
    pub fn new(
        journal: OfflineBatchJournal,
        provider: Arc<dyn BatchProvider>,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            journal,
            provider,
            workspace_root,
        }
    }

    pub fn decide_resubmit(
        &self,
        config_digest: &str,
    ) -> Result<BatchResubmitDecision, OfflineBatchError> {
        let records = self.journal.load_records()?;
        Ok(reconcile_resubmit(&records, config_digest))
    }

    /// Journal submit intent, then call provider submit. Marks `Unknown` on ambiguous failure.
    pub fn submit_durable(
        &self,
        plan: &BatchPlan,
        config: &FrozenBatchConfig,
    ) -> Result<Uuid, OfflineBatchError> {
        match self.decide_resubmit(&config.digest)? {
            BatchResubmitDecision::RefuseDuplicateCost { reason, batch_id } => {
                let _ = self.journal.append(OfflineBatchEvent::ResubmitRefused {
                    batch_id,
                    config_digest: config.digest.clone(),
                    reason: reason.clone(),
                });
                return Err(OfflineBatchError::RefuseResubmit(reason));
            }
            BatchResubmitDecision::ContinueExisting { batch_id, state }
                if state == BatchLifecycleState::Delivered =>
            {
                return Ok(batch_id);
            }
            BatchResubmitDecision::AllowFresh | BatchResubmitDecision::ContinueExisting { .. } => {}
        }

        self.provider.prepare(plan)?;
        let batch_id = Uuid::new_v4();
        self.journal.append(OfflineBatchEvent::Submitted {
            batch_id,
            config_digest: config.digest.clone(),
            item_count: plan.items.len() as u32,
        })?;

        match self.provider.submit(batch_id, &config.digest, plan) {
            Ok(outcome) => {
                self.journal.append(OfflineBatchEvent::ProviderLinked {
                    batch_id,
                    config_digest: config.digest.clone(),
                    provider_job_id: outcome.provider_job_id,
                })?;
                Ok(batch_id)
            }
            Err(err) => {
                let reason = err.to_string();
                self.journal.append(OfflineBatchEvent::Unknown {
                    batch_id,
                    config_digest: config.digest.clone(),
                    reason: reason.clone(),
                })?;
                Err(OfflineBatchError::Provider(reason))
            }
        }
    }

    /// Idempotent collect: workspace confined, no blind overwrite on source hash drift.
    pub fn collect_durable_with_plan(
        &self,
        batch_id: Uuid,
        plan: &BatchPlan,
    ) -> Result<CollectBatchOutcome, OfflineBatchError> {
        let record = self
            .journal
            .record_for(batch_id)?
            .ok_or(OfflineBatchError::NotFound(batch_id))?;
        if record.state == BatchLifecycleState::Delivered {
            return Ok(CollectBatchOutcome {
                batch_id,
                state: BatchLifecycleState::Delivered,
                item_outcomes: Vec::new(),
            });
        }
        let provider_job_id = record.provider_job_id.clone().ok_or_else(|| {
            OfflineBatchError::RefuseResubmit(
                "collect refused: provider job not linked — reconcile submit first".into(),
            )
        })?;

        self.journal.append(OfflineBatchEvent::Collecting {
            batch_id,
            config_digest: record.config_digest.clone(),
        })?;

        let status = self.provider.status(&provider_job_id)?;
        if matches!(status, BatchProviderStatus::Failed { .. }) {
            if let BatchProviderStatus::Failed { reason } = status {
                self.journal.append(OfflineBatchEvent::Failed {
                    batch_id,
                    config_digest: record.config_digest.clone(),
                    reason: reason.clone(),
                })?;
                return Ok(CollectBatchOutcome {
                    batch_id,
                    state: BatchLifecycleState::Failed,
                    item_outcomes: Vec::new(),
                });
            }
        }
        if matches!(status, BatchProviderStatus::Unknown { .. }) {
            if let BatchProviderStatus::Unknown { reason } = status {
                self.journal.append(OfflineBatchEvent::Unknown {
                    batch_id,
                    config_digest: record.config_digest.clone(),
                    reason: reason.clone(),
                })?;
                return Ok(CollectBatchOutcome {
                    batch_id,
                    state: BatchLifecycleState::Unknown,
                    item_outcomes: Vec::new(),
                });
            }
        }
        if !matches!(status, BatchProviderStatus::Ready) {
            return Ok(CollectBatchOutcome {
                batch_id,
                state: BatchLifecycleState::Collecting,
                item_outcomes: Vec::new(),
            });
        }

        let results = self.provider.collect_results(&provider_job_id)?;
        let spec_by_id: HashMap<&str, &BatchItemSpec> =
            plan.items.iter().map(|s| (s.item_id.as_str(), s)).collect();

        let mut item_outcomes = Vec::with_capacity(results.len());
        let mut all_delivered = true;
        for result in results {
            let spec = spec_by_id.get(result.item_id.as_str()).ok_or_else(|| {
                OfflineBatchError::CollectConflict(format!("unexpected item {}", result.item_id))
            })?;
            let fresh_record = self.journal.record_for(batch_id)?.unwrap_or(record.clone());
            let outcome = deliver_item(
                &self.workspace_root,
                &spec.output_relpath,
                &spec.input_hash,
                &result.result_bytes,
                &fresh_record,
                &result,
            )?;
            if matches!(
                outcome,
                CollectItemOutcome::Written { .. } | CollectItemOutcome::AlreadyDelivered { .. }
            ) {
                let result_hash = hash_bytes(&result.result_bytes);
                self.journal.append(OfflineBatchEvent::ItemDelivered {
                    batch_id,
                    item_id: result.item_id.clone(),
                    result_hash,
                    workspace_relpath: spec.output_relpath.clone(),
                })?;
            } else {
                all_delivered = false;
            }
            item_outcomes.push((result.item_id.clone(), outcome));
        }

        if all_delivered {
            self.journal.append(OfflineBatchEvent::Delivered {
                batch_id,
                config_digest: record.config_digest.clone(),
            })?;
            Ok(CollectBatchOutcome {
                batch_id,
                state: BatchLifecycleState::Delivered,
                item_outcomes,
            })
        } else {
            Ok(CollectBatchOutcome {
                batch_id,
                state: BatchLifecycleState::Collecting,
                item_outcomes,
            })
        }
    }

    /// Batch ids in Submitted (provider-linked) or Collecting — candidates for poll.
    pub fn in_flight_batch_ids(&self) -> Result<Vec<Uuid>, OfflineBatchError> {
        let records = self.journal.load_records()?;
        let mut ids: Vec<Uuid> = records
            .values()
            .filter(|r| batch_record_poll_eligible(r))
            .map(|r| r.batch_id)
            .collect();
        ids.sort();
        Ok(ids)
    }

    /// Poll provider status for in-flight batches; collect when Ready.
    ///
    /// Caller supplies frozen [`BatchPlan`] per batch id (not stored in the journal).
    /// Safe to call every daemon tick — Pending/Running does not append journal noise.
    pub fn poll_collect_due(
        &self,
        plans: &HashMap<Uuid, BatchPlan>,
    ) -> Result<Vec<PollCollectDueResult>, OfflineBatchError> {
        let records = self.journal.load_records()?;
        let mut in_flight: Vec<&BatchOperationRecord> = records
            .values()
            .filter(|r| batch_record_poll_eligible(r))
            .collect();
        in_flight.sort_by_key(|r| r.batch_id);

        let mut results = Vec::with_capacity(in_flight.len());
        for record in in_flight {
            let batch_id = record.batch_id;
            let Some(plan) = plans.get(&batch_id) else {
                results.push(PollCollectDueResult {
                    batch_id,
                    action: PollCollectAction::SkippedNoPlan,
                });
                continue;
            };
            let Some(provider_job_id) = record.provider_job_id.as_deref() else {
                continue;
            };

            let status = self.provider.status(provider_job_id)?;
            match status {
                BatchProviderStatus::Pending | BatchProviderStatus::Running => {
                    results.push(PollCollectDueResult {
                        batch_id,
                        action: PollCollectAction::AwaitingProvider,
                    });
                }
                BatchProviderStatus::Failed { reason } => {
                    self.journal.append(OfflineBatchEvent::Failed {
                        batch_id,
                        config_digest: record.config_digest.clone(),
                        reason: reason.clone(),
                    })?;
                    results.push(PollCollectDueResult {
                        batch_id,
                        action: PollCollectAction::MarkedFailed { reason },
                    });
                }
                BatchProviderStatus::Unknown { reason } => {
                    self.journal.append(OfflineBatchEvent::Unknown {
                        batch_id,
                        config_digest: record.config_digest.clone(),
                        reason: reason.clone(),
                    })?;
                    results.push(PollCollectDueResult {
                        batch_id,
                        action: PollCollectAction::MarkedUnknown { reason },
                    });
                }
                BatchProviderStatus::Ready => {
                    let collected = self.collect_durable_with_plan(batch_id, plan)?;
                    results.push(PollCollectDueResult {
                        batch_id,
                        action: PollCollectAction::Collected(collected),
                    });
                }
            }
        }
        Ok(results)
    }
}

/// Daemon/harness holder for offline-batch executors + frozen plans (not in journal).
///
/// Tick calls [`Self::poll_all`]; empty registry is a no-op.
pub struct OfflineBatchRegistry {
    state: Mutex<OfflineBatchRegistryState>,
}

struct OfflineBatchRegistryState {
    executors: Vec<DurableOfflineBatchExecutor>,
    plans: HashMap<Uuid, BatchPlan>,
}

impl OfflineBatchRegistry {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(OfflineBatchRegistryState {
                executors: Vec::new(),
                plans: HashMap::new(),
            }),
        }
    }

    pub fn register_executor(&self, executor: DurableOfflineBatchExecutor) {
        self.state
            .lock()
            .expect("offline batch registry poisoned")
            .executors
            .push(executor);
    }

    pub fn register_plan(&self, batch_id: Uuid, plan: BatchPlan) {
        self.state
            .lock()
            .expect("offline batch registry poisoned")
            .plans
            .insert(batch_id, plan);
    }

    /// Poll every registered executor with the shared plan map.
    /// Safe for daemon tick — errors returned to caller (log, never abort).
    pub fn poll_all(&self) -> Result<Vec<PollCollectDueResult>, OfflineBatchError> {
        let state = self.state.lock().expect("offline batch registry poisoned");
        let mut out = Vec::new();
        for executor in &state.executors {
            out.extend(executor.poll_collect_due(&state.plans)?);
        }
        Ok(out)
    }
}

impl Default for OfflineBatchRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn batch_record_poll_eligible(record: &BatchOperationRecord) -> bool {
    match record.state {
        BatchLifecycleState::Collecting => true,
        BatchLifecycleState::Submitted => record.provider_job_id.is_some(),
        _ => false,
    }
}

fn deliver_item(
    workspace_root: &Path,
    relpath: &str,
    expected_source_hash: &str,
    result_bytes: &[u8],
    record: &BatchOperationRecord,
    result: &BatchItemResult,
) -> Result<CollectItemOutcome, OfflineBatchError> {
    // New output paths may not exist yet; parent must exist before containment check.
    let joined = workspace_root.join(relpath);
    if let Some(parent) = joined.parent() {
        if parent != workspace_root {
            std::fs::create_dir_all(parent)
                .map_err(|e| OfflineBatchError::Workspace(e.to_string()))?;
        }
    }
    let path = resolve_workspace_path(workspace_root, Path::new(relpath))?;
    let _ = result;
    let result_hash = hash_bytes(result_bytes);

    if record.delivered_items.contains(&result.item_id) {
        if path.is_file() {
            let current = hash_file(&path)?;
            if current == result_hash {
                return Ok(CollectItemOutcome::AlreadyDelivered { result_hash });
            }
        }
        return Ok(CollectItemOutcome::AlreadyDelivered { result_hash });
    }

    if path.is_file() {
        let current_hash = hash_file(&path)?;
        if !expected_source_hash.is_empty() && current_hash != expected_source_hash {
            return Ok(CollectItemOutcome::SourceHashConflict {
                expected_source_hash: expected_source_hash.to_string(),
                current_hash,
            });
        }
        if current_hash == result_hash {
            return Ok(CollectItemOutcome::AlreadyDelivered { result_hash });
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| OfflineBatchError::Workspace(e.to_string()))?;
    }
    std::fs::write(&path, result_bytes).map_err(|e| OfflineBatchError::Workspace(e.to_string()))?;
    Ok(CollectItemOutcome::Written { result_hash })
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn hash_file(path: &Path) -> Result<String, OfflineBatchError> {
    let bytes = std::fs::read(path).map_err(|e| OfflineBatchError::Workspace(e.to_string()))?;
    Ok(hash_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryEventStore;

    fn executor_fixture() -> (TempDirGuard, DurableOfflineBatchExecutor) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(MemoryEventStore::default());
        let session = Uuid::new_v4();
        let journal = OfflineBatchJournal::new(store, session);
        let exec = DurableOfflineBatchExecutor::new(
            journal,
            Arc::new(MockBatchProvider::new()),
            dir.path().to_path_buf(),
        );
        (TempDirGuard(dir), exec)
    }

    struct TempDirGuard(tempfile::TempDir);

    fn sample_plan() -> BatchPlan {
        BatchPlan {
            items: vec![BatchItemSpec {
                item_id: "item-a".into(),
                input_hash: hash_bytes(b"seed-a"),
                output_relpath: "out/a.txt".into(),
            }],
        }
    }

    fn sample_config(plan: &BatchPlan) -> FrozenBatchConfig {
        FrozenBatchConfig::freeze("mock", "mock-model", vec!["temp=0".into()], plan)
    }

    #[test]
    fn frozen_config_digest_stable() {
        let plan = sample_plan();
        let a = sample_config(&plan);
        let b = sample_config(&plan);
        assert_eq!(a.digest, b.digest);
        assert!(a.digest.starts_with("sha256:"));
    }

    #[test]
    fn submit_journals_before_provider_link() {
        let (_dir, exec) = executor_fixture();
        let plan = sample_plan();
        let config = sample_config(&plan);
        let batch_id = exec.submit_durable(&plan, &config).expect("submit");
        let record = exec.journal.record_for(batch_id).unwrap().unwrap();
        assert_eq!(record.state, BatchLifecycleState::Submitted);
        assert!(record.provider_job_id.is_some());
    }

    #[test]
    fn unknown_blocks_duplicate_cost_submit() {
        let (_dir, exec) = executor_fixture();
        let plan = sample_plan();
        let config = sample_config(&plan);
        let batch_id = Uuid::new_v4();
        exec.journal
            .append(OfflineBatchEvent::Submitted {
                batch_id,
                config_digest: config.digest.clone(),
                item_count: 1,
            })
            .unwrap();
        exec.journal
            .append(OfflineBatchEvent::Unknown {
                batch_id,
                config_digest: config.digest.clone(),
                reason: "crash mid submit".into(),
            })
            .unwrap();
        let decision = exec.decide_resubmit(&config.digest).unwrap();
        assert!(matches!(
            decision,
            BatchResubmitDecision::RefuseDuplicateCost { .. }
        ));
        let err = exec.submit_durable(&plan, &config).unwrap_err();
        assert!(matches!(err, OfflineBatchError::RefuseResubmit(_)));
    }

    #[test]
    fn collect_delivers_and_is_idempotent() {
        let (_dir, exec) = executor_fixture();
        let plan = sample_plan();
        let config = sample_config(&plan);
        let batch_id = exec.submit_durable(&plan, &config).expect("submit");
        let first = exec
            .collect_durable_with_plan(batch_id, &plan)
            .expect("collect");
        assert_eq!(first.state, BatchLifecycleState::Delivered);
        assert!(matches!(
            first.item_outcomes[0].1,
            CollectItemOutcome::Written { .. }
        ));
        let second = exec
            .collect_durable_with_plan(batch_id, &plan)
            .expect("collect again");
        assert_eq!(second.state, BatchLifecycleState::Delivered);
        assert!(second.item_outcomes.is_empty());
    }

    #[test]
    fn collect_refuses_source_hash_conflict() {
        let (dir, exec) = executor_fixture();
        let target = dir.0.path().join("out/a.txt");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"changed-on-disk").unwrap();

        let plan = sample_plan();
        let config = sample_config(&plan);
        let batch_id = exec.submit_durable(&plan, &config).expect("submit");
        let outcome = exec
            .collect_durable_with_plan(batch_id, &plan)
            .expect("collect");
        assert!(matches!(
            outcome.item_outcomes[0].1,
            CollectItemOutcome::SourceHashConflict { .. }
        ));
    }

    #[test]
    fn collect_rejects_unsafe_relative_path() {
        let (_dir, exec) = executor_fixture();
        let plan = BatchPlan {
            items: vec![BatchItemSpec {
                item_id: "escape".into(),
                input_hash: hash_bytes(b"x"),
                output_relpath: "../escape.txt".into(),
            }],
        };
        let config = sample_config(&plan);
        let batch_id = exec.submit_durable(&plan, &config).expect("submit");
        let err = exec.collect_durable_with_plan(batch_id, &plan).unwrap_err();
        assert!(matches!(err, OfflineBatchError::Workspace(_)));
    }

    #[test]
    fn poll_collect_due_delivers_when_provider_ready() {
        let (_dir, exec) = executor_fixture();
        let plan = sample_plan();
        let config = sample_config(&plan);
        let batch_id = exec.submit_durable(&plan, &config).expect("submit");
        assert_eq!(
            exec.journal.record_for(batch_id).unwrap().unwrap().state,
            BatchLifecycleState::Submitted
        );

        let mut plans = HashMap::new();
        plans.insert(batch_id, plan.clone());
        let polled = exec.poll_collect_due(&plans).expect("poll");
        assert_eq!(polled.len(), 1);
        assert!(matches!(
            polled[0].action,
            PollCollectAction::Collected(ref c) if c.state == BatchLifecycleState::Delivered
        ));
        assert_eq!(
            exec.journal.record_for(batch_id).unwrap().unwrap().state,
            BatchLifecycleState::Delivered
        );
    }

    #[test]
    fn registry_poll_all_delivers_when_provider_ready() {
        let (_dir, exec) = executor_fixture();
        let plan = sample_plan();
        let config = sample_config(&plan);
        let batch_id = exec.submit_durable(&plan, &config).expect("submit");

        let registry = OfflineBatchRegistry::new();
        registry.register_plan(batch_id, plan);
        registry.register_executor(exec);
        let polled = registry.poll_all().expect("registry poll");
        assert_eq!(polled.len(), 1);
        assert!(matches!(
            polled[0].action,
            PollCollectAction::Collected(ref c) if c.state == BatchLifecycleState::Delivered
        ));
    }

    #[test]
    fn mock_provider_lifecycle_no_network() {
        let provider = MockBatchProvider::new();
        let plan = sample_plan();
        provider.prepare(&plan).unwrap();
        let batch_id = Uuid::new_v4();
        let config = sample_config(&plan);
        let submit = provider.submit(batch_id, &config.digest, &plan).unwrap();
        assert!(matches!(
            provider.status(&submit.provider_job_id).unwrap(),
            BatchProviderStatus::Ready
        ));
        let results = provider.collect_results(&submit.provider_job_id).unwrap();
        assert_eq!(results.len(), 1);
    }
}
