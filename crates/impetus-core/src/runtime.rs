use crate::{
    Action, ApprovalEvent, ApprovalRequest, ApprovalResolution, ApprovalResolver, ApprovalState,
    BudgetChecker, BudgetConfig, ChildEvent, DeferredEffect, EffectSeam, Event, EventPayload,
    EventStore, ExecutionMode, IntentEvent, NoticeEvent, PolicyEngine, ProjectionError, RunEvent,
    Sandbox, ToolEvent, default_risk_gate, normalized_effect_from_action, reduce,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

pub use impetus_protocol::RuntimeStatus;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Store(#[from] crate::storage::StoreError),
    #[error(transparent)]
    Projection(#[from] ProjectionError),
    #[error("session `{0}` does not exist")]
    MissingSession(Uuid),
    #[error("approval `{0}` does not exist")]
    MissingApproval(Uuid),
    #[error("approval `{0}` is not pending")]
    ApprovalNotPending(Uuid),
    #[error("action needs a durable user intent revision before it can be approved")]
    MissingIntentRevision,
    #[error("approval `{0}` must be resolved by a user")]
    ApprovalResolverNotUser(Uuid),
    #[error("approval `{0}` is stale because its action or user intent changed")]
    StaleApproval(Uuid),
    #[error("action denied by policy: {0}")]
    Denied(String),
    #[error("run `{0}` is not active")]
    InactiveRun(Uuid),
    #[error("run `{0}` is already active")]
    ActiveRun(Uuid),
    #[error("workspace root `{0}` is not a directory")]
    InvalidWorkspace(PathBuf),
}

pub struct AgentRuntime {
    session_id: Uuid,
    store: Arc<dyn EventStore>,
    policy: PolicyEngine,
    workspace_root: PathBuf,
    budget: Option<Arc<Mutex<BudgetChecker>>>,
    /// Bound WorktreeManager identity; snapped into CompactionStructuralState.
    worktree_id: Option<String>,
    // A2 Phase 2: Store deferred effects for approval continuation.
    // Maps approval_id -> DeferredEffect so approved work can resume.
    deferred_effects: Arc<Mutex<HashMap<Uuid, DeferredEffect>>>,
}

fn policy_for_workspace(policy: &PolicyEngine, workspace_root: PathBuf) -> PolicyEngine {
    let mut scope = policy.scope().clone();
    scope.workspace_root = workspace_root;
    PolicyEngine::with_config(scope, policy.config())
}

/// Max inline bytes stored in a durable `AgentEvent::Chunk` row. Larger bodies
/// spill to [`crate::DurableArtifactStore`] with a bounded preview + ArtifactRef.
pub const MAX_AGENT_CHUNK_EVENT_BYTES: usize = 16 * 1024;

/// Accumulate tiny stream deltas until this many bytes before flushing one
/// durable chunk (keeps chunk_id ordering; fewer event rows).
pub const AGENT_CHUNK_COALESCE_BYTES: usize = 512;

/// Preview kept in the event when a chunk spills to an artifact.
pub const AGENT_CHUNK_PREVIEW_BYTES: usize = 256;

fn truncate_agent_chunk_preview(input: &str) -> String {
    if input.len() <= AGENT_CHUNK_PREVIEW_BYTES {
        return input.to_owned();
    }
    let mut end = AGENT_CHUNK_PREVIEW_BYTES;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    let mut preview = input[..end].to_owned();
    preview.push('…');
    preview
}

/// Bound chunk text for durable persistence. Returns `(preview_or_full, artifact)`.
pub fn bound_agent_chunk_text(
    text: String,
    artifact_store: Option<&crate::DurableArtifactStore>,
) -> (String, Option<crate::DurableArtifactRef>) {
    if text.len() <= MAX_AGENT_CHUNK_EVENT_BYTES {
        return (text, None);
    }
    match artifact_store.and_then(|store| store.store(text.as_bytes()).ok()) {
        Some(artifact) => (truncate_agent_chunk_preview(&text), Some(artifact)),
        // Prefer an oversized event row over silently dropping the stream body.
        None => (text, None),
    }
}

/// Latest `worktree_id` snapped into a durable CompactionCompleted structural payload.
fn worktree_id_from_compaction_events(events: &[Event]) -> Option<String> {
    events.iter().rev().find_map(|event| match &event.payload {
        EventPayload::Budget(crate::BudgetEvent::CompactionCompleted {
            structural: Some(structural),
            ..
        }) => structural.worktree_id.clone(),
        _ => None,
    })
}

impl AgentRuntime {
    pub fn new(store: Arc<dyn EventStore>, policy: PolicyEngine) -> Self {
        Self::create(store, policy).expect("create durable session")
    }

    pub fn create(store: Arc<dyn EventStore>, policy: PolicyEngine) -> Result<Self, RuntimeError> {
        let session_id = store.create_session()?;
        Ok(Self {
            session_id,
            store,
            workspace_root: policy.scope().workspace_root.clone(),
            policy,
            budget: None,
            worktree_id: None,
            deferred_effects: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn create_with_workspace(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        workspace_root: PathBuf,
    ) -> Result<Self, RuntimeError> {
        let workspace_root = workspace_root
            .canonicalize()
            .map_err(|_| RuntimeError::InvalidWorkspace(workspace_root.clone()))?;
        if !workspace_root.is_dir() {
            return Err(RuntimeError::InvalidWorkspace(workspace_root));
        }
        let session_id = store.create_session()?;
        let scoped_policy = policy_for_workspace(&policy, workspace_root.clone());
        store.append_next(
            session_id,
            EventPayload::Session(crate::SessionEvent::WorkspaceRoot {
                workspace_root: workspace_root.clone(),
            }),
        )?;
        Ok(Self {
            session_id,
            store,
            policy: scoped_policy,
            workspace_root,
            budget: None,
            worktree_id: None,
            deferred_effects: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn attach(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        session_id: Uuid,
    ) -> Result<Self, RuntimeError> {
        let events = store.list(session_id)?;
        if events.is_empty() {
            return Err(RuntimeError::MissingSession(session_id));
        }
        let projection = reduce(&events)?.ok_or(RuntimeError::MissingSession(session_id))?;
        let workspace_root = projection
            .workspace_root
            .unwrap_or_else(|| policy.scope().workspace_root.clone());
        let worktree_id = worktree_id_from_compaction_events(&events);
        Ok(Self {
            session_id,
            store,
            policy: policy_for_workspace(&policy, workspace_root.clone()),
            workspace_root,
            budget: None,
            worktree_id,
            deferred_effects: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn fork(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        source_session_id: Uuid,
        up_to_sequence: u64,
    ) -> Result<Self, RuntimeError> {
        let new_session_id = store.fork_session(source_session_id, up_to_sequence)?;
        Ok(Self {
            session_id: new_session_id,
            store,
            workspace_root: policy.scope().workspace_root.clone(),
            policy,
            budget: None,
            worktree_id: None,
            deferred_effects: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Restore named checkpoint as a new shared-prefix branch.
    /// Source session history stays immutable.
    pub fn restore_checkpoint(
        store: Arc<dyn EventStore>,
        policy: PolicyEngine,
        checkpoint_id: Uuid,
    ) -> Result<Self, RuntimeError> {
        let checkpoint = store.get_checkpoint(checkpoint_id)?;
        Self::fork(store, policy, checkpoint.session_id, checkpoint.sequence)
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// Bound WorktreeManager identity (survives compaction via structural snapshot).
    pub fn worktree_id(&self) -> Option<&str> {
        self.worktree_id.as_deref()
    }

    /// Bind a managed worktree identity to this session runtime.
    pub fn set_worktree_id(&mut self, worktree_id: impl Into<String>) {
        self.worktree_id = Some(worktree_id.into());
    }

    /// Clear the bound worktree identity (e.g. after close).
    pub fn clear_worktree_id(&mut self) {
        self.worktree_id = None;
    }

    /// Set budget configuration for this runtime
    pub fn set_budget(&mut self, config: BudgetConfig) -> Result<(), RuntimeError> {
        // Load existing budget state from store
        let state = self.store.as_ref().get_budget_state(self.session_id)?;
        let mut checker = BudgetChecker::new(config);
        // Restore state
        *checker.state_mut() = state;
        self.budget = Some(Arc::new(Mutex::new(checker)));
        Ok(())
    }

    /// Get current budget state (if budget enabled)
    pub fn budget_state(&self) -> Option<crate::budget::BudgetState> {
        self.budget
            .as_ref()
            .map(|b| b.lock().unwrap().state().clone())
    }

    /// Get budget checker reference (if budget enabled)
    pub fn budget(&self) -> Option<Arc<Mutex<BudgetChecker>>> {
        self.budget.clone()
    }

    /// Get the current budget configuration if budget is set
    pub fn budget_config(&self) -> Option<BudgetConfig> {
        self.budget
            .as_ref()
            .map(|checker| checker.lock().unwrap().config().clone())
    }

    /// Check if budget allows this request
    pub fn check_budget(&self, estimated_tokens: u64) -> Result<(), crate::budget::BudgetError> {
        if let Some(ref checker) = self.budget {
            checker.lock().unwrap().check_all(estimated_tokens)?;
        }
        Ok(())
    }

    /// Returns `(threshold, used)` when auto-compaction should run.
    pub fn compaction_needed(&self) -> Option<(u64, u64)> {
        let checker = self.budget.as_ref()?;
        let guard = checker.lock().unwrap();
        match guard.check_compaction() {
            Err(crate::budget::BudgetError::CompactionRequired { threshold, used }) => {
                Some((threshold, used))
            }
            _ => None,
        }
    }

    /// Record turn completion and persist budget state (tokens treated as estimated).
    pub fn record_turn(&self, tokens_used: u64) -> Result<(), RuntimeError> {
        self.record_turn_with_usage(tokens_used, false)
    }

    /// Record turn completion with measured vs estimated token accounting.
    pub fn record_turn_with_usage(
        &self,
        tokens_used: u64,
        measured: bool,
    ) -> Result<(), RuntimeError> {
        if let Some(ref checker) = self.budget {
            let mut guard = checker.lock().unwrap();
            guard.record_turn_with_usage(tokens_used, measured);
            self.store
                .as_ref()
                .update_budget_state(self.session_id, guard.state())?;
        }
        Ok(())
    }

    /// Persist compaction token reset after a durable compaction commit.
    pub fn record_compaction(&self, compacted_to: u64) -> Result<(), RuntimeError> {
        if let Some(ref checker) = self.budget {
            let mut guard = checker.lock().unwrap();
            guard.record_compaction(compacted_to);
            self.store
                .as_ref()
                .update_budget_state(self.session_id, guard.state())?;
        }
        Ok(())
    }

    /// Run durable compaction: emit Started → summary/artifact → Completed.
    ///
    /// Prompt `messages` are folded for the next model turn. Event history is
    /// append-only — never rewritten. Structural state is stored as typed
    /// payload fields, not only inside the summary text.
    pub fn run_durable_compaction(
        &self,
        messages: Vec<crate::ProviderMessage>,
    ) -> Result<Vec<crate::ProviderMessage>, RuntimeError> {
        let owned_store = crate::DurableArtifactStore::open(crate::default_artifact_root()).ok();
        self.run_durable_compaction_with_store(messages, owned_store.as_ref())
    }

    /// Same as [`Self::run_durable_compaction`] with an explicit artifact store
    /// (tests inject a temp root; production may pass `None` to skip artifacts).
    pub fn run_durable_compaction_with_store(
        &self,
        messages: Vec<crate::ProviderMessage>,
        artifact_store: Option<&crate::DurableArtifactStore>,
    ) -> Result<Vec<crate::ProviderMessage>, RuntimeError> {
        let Some((threshold, used)) = self.compaction_needed() else {
            return Ok(messages);
        };

        let events = self.events()?;
        let to_sequence = events.last().map(|e| e.sequence).unwrap_or(0);
        let from_sequence = events
            .iter()
            .rev()
            .find_map(|event| match &event.payload {
                EventPayload::Budget(crate::BudgetEvent::CompactionCompleted {
                    to_sequence,
                    ..
                }) if *to_sequence > 0 => Some(*to_sequence + 1),
                _ => None,
            })
            .unwrap_or(1);

        self.record(EventPayload::Budget(
            crate::BudgetEvent::CompactionRequired { threshold, used },
        ))?;
        self.record(EventPayload::Budget(
            crate::BudgetEvent::CompactionStarted {
                from_sequence,
                to_sequence,
                threshold,
                used,
            },
        ))?;

        let (compacted_messages, summary) = crate::compaction::compact_provider_messages(messages);
        let compacted_to = crate::compaction::estimate_tokens(&summary)
            + compacted_messages
                .iter()
                .map(|m| crate::compaction::estimate_tokens(m.content()))
                .sum::<u64>();

        let summary_artifact =
            artifact_store.and_then(|store| store.store(summary.as_bytes()).ok());

        self.record_compaction(compacted_to)?;
        let budget_state = self.budget_state().unwrap_or_default();
        let scope = self.policy.scope();
        let structural = crate::CompactionStructuralState {
            workspace_root: self.workspace_root.clone(),
            parent_session_id: self.parent_session_id(),
            allow_network: scope.allow_network,
            allow_web_outbound: scope.allow_web_outbound,
            allow_private_network: scope.allow_private_network,
            allowed_hosts: scope.allowed_hosts.clone(),
            turns_used: budget_state.turns_used,
            tokens_used: budget_state.tokens_used,
            compaction_count: budget_state.compaction_count,
            worktree_id: self.worktree_id.clone(),
        };

        self.record(EventPayload::Budget(
            crate::BudgetEvent::CompactionCompleted {
                compacted_to,
                compaction_count: budget_state.compaction_count,
                from_sequence,
                to_sequence,
                summary_artifact,
                structural: Some(structural),
            },
        ))?;

        Ok(compacted_messages)
    }

    fn parent_session_id(&self) -> Option<Uuid> {
        self.store
            .list_sessions()
            .ok()?
            .into_iter()
            .find(|session| session.id == self.session_id)
            .and_then(|session| session.parent_session_id)
    }

    pub fn workspace_root(&self) -> Result<PathBuf, RuntimeError> {
        Ok(self.workspace_root.clone())
    }

    pub fn policy(&self) -> PolicyEngine {
        self.policy.clone()
    }

    pub fn execution_mode(&self) -> Result<ExecutionMode, RuntimeError> {
        Ok(self.projection()?.execution_mode)
    }

    pub fn session_sandbox(&self) -> Result<Sandbox, RuntimeError> {
        let workspace = self.workspace_root()?;
        let mut scope = self.policy.scope().clone();
        scope.workspace_root = workspace;
        Ok(Sandbox::Provisioned { scope })
    }

    pub fn effect_seam(&self) -> Result<EffectSeam, RuntimeError> {
        Ok(EffectSeam::with_admission(
            self.policy(),
            self.session_sandbox()?,
            self.execution_mode()?,
            default_risk_gate(),
        ))
    }

    pub fn effect_seam_with_sandbox(&self, sandbox: Sandbox) -> Result<EffectSeam, RuntimeError> {
        Ok(EffectSeam::with_admission(
            self.policy(),
            sandbox,
            self.execution_mode()?,
            default_risk_gate(),
        ))
    }

    fn action_admission(
        &self,
        action: &Action,
        capability_version: Option<u32>,
    ) -> Result<crate::EffectDecision, RuntimeError> {
        // Every ActionKind maps to a NormalizedEffect — always admit via EffectSeam
        // (sandbox → hard policy → execution mode → RiskGate). Never policy-only.
        let effect =
            normalized_effect_from_action(action, capability_version).ok_or_else(|| {
                RuntimeError::Denied(format!(
                    "action kind {:?} has no normalized effect for admission",
                    action.kind
                ))
            })?;
        let seam = self.effect_seam()?;
        Ok(seam.decide(&effect))
    }

    /// Replace live policy overrides without restarting the runtime.
    pub fn reload_policy_config(&mut self, config: crate::PolicyConfig) {
        self.policy.reload_config(config);
    }

    /// Reload policy overrides from a JSON file path. On error, prior overrides stay.
    pub fn reload_policy_config_from_path(
        &mut self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<(), crate::PolicyConfigError> {
        self.policy.reload_config_from_path(path)
    }

    pub fn submit_intent(&self, text: impl Into<String>) -> Result<(), RuntimeError> {
        self.submit_intent_with_artifact(text, None)
    }

    pub fn submit_intent_with_artifact(
        &self,
        text: impl Into<String>,
        artifact: Option<crate::DurableArtifactRef>,
    ) -> Result<(), RuntimeError> {
        self.submit_intent_with_artifact_and_kind(text, artifact, crate::UserPromptIntent::Prompt)
    }

    pub fn submit_intent_with_artifact_and_kind(
        &self,
        text: impl Into<String>,
        artifact: Option<crate::DurableArtifactRef>,
        intent: crate::UserPromptIntent,
    ) -> Result<(), RuntimeError> {
        self.record(EventPayload::Intent(IntentEvent::with_intent(
            text, intent, artifact,
        )))
    }

    pub fn submit_intent_and_start_run(
        &self,
        text: impl Into<String>,
    ) -> Result<Uuid, RuntimeError> {
        self.submit_intent_and_start_run_with_artifact(text, None)
    }

    pub fn submit_intent_and_start_run_with_artifact(
        &self,
        text: impl Into<String>,
        artifact: Option<crate::DurableArtifactRef>,
    ) -> Result<Uuid, RuntimeError> {
        if let Some(run_id) = self.projection()?.active_run_id {
            return Err(RuntimeError::ActiveRun(run_id));
        }
        self.submit_intent_with_artifact(text, artifact)?;
        self.start_run()
    }

    pub fn start_run(&self) -> Result<Uuid, RuntimeError> {
        if let Some(run_id) = self.projection()?.active_run_id {
            return Err(RuntimeError::ActiveRun(run_id));
        }
        let run_id = Uuid::new_v4();
        self.record(EventPayload::Run(RunEvent::Started { run_id }))?;
        Ok(run_id)
    }

    pub fn finish_run(&self, outcome: RunEvent) -> Result<(), RuntimeError> {
        let run_id = match &outcome {
            RunEvent::Started { run_id }
            | RunEvent::Completed { run_id }
            | RunEvent::Failed { run_id, .. }
            | RunEvent::Cancelled { run_id }
            | RunEvent::InterruptedUnknown { run_id } => *run_id,
        };
        if self.projection()?.active_run_id != Some(run_id)
            || matches!(&outcome, RunEvent::Started { .. })
        {
            return Err(RuntimeError::InactiveRun(run_id));
        }
        self.record(EventPayload::Run(outcome))
    }

    pub fn record_agent_chunk(
        &self,
        run_id: Uuid,
        chunk_id: u64,
        text: impl Into<String>,
    ) -> Result<bool, RuntimeError> {
        self.record_agent_chunk_with_store(run_id, chunk_id, text, None)
    }

    /// Record an agent text chunk, optionally spilling oversized bodies to
    /// [`DurableArtifactStore`]. When `artifact_store` is `None`, opens the
    /// default root on demand (same pattern as durable compaction).
    pub fn record_agent_chunk_with_store(
        &self,
        run_id: Uuid,
        chunk_id: u64,
        text: impl Into<String>,
        artifact_store: Option<&crate::DurableArtifactStore>,
    ) -> Result<bool, RuntimeError> {
        let projection = self.projection()?;
        if projection.active_run_id != Some(run_id) {
            return Err(RuntimeError::InactiveRun(run_id));
        }
        if projection
            .agent_chunk_ids
            .get(&run_id)
            .is_some_and(|last| *last >= chunk_id)
        {
            return Ok(false);
        }
        let text = text.into();
        let owned_store = if artifact_store.is_none() && text.len() > MAX_AGENT_CHUNK_EVENT_BYTES {
            crate::DurableArtifactStore::open(crate::default_artifact_root()).ok()
        } else {
            None
        };
        let store = artifact_store.or(owned_store.as_ref());
        let (text, artifact) = bound_agent_chunk_text(text, store);
        self.record(EventPayload::Agent(crate::AgentEvent::Chunk {
            run_id,
            chunk_id,
            text,
            artifact,
        }))?;
        Ok(true)
    }

    pub fn record_agent_final(
        &self,
        run_id: Uuid,
        text: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        let projection = self.projection()?;
        if projection.active_run_id != Some(run_id) {
            return Err(RuntimeError::InactiveRun(run_id));
        }
        // When chunks already recorded the stream body, keep Final compact so a
        // large answer is not duplicated as a second megabyte event row.
        let text = text.into();
        let text = if projection.agent_chunk_ids.contains_key(&run_id)
            && text.len() > MAX_AGENT_CHUNK_EVENT_BYTES
        {
            truncate_agent_chunk_preview(&text)
        } else {
            text
        };
        self.record(EventPayload::Agent(crate::AgentEvent::Final {
            run_id,
            text,
        }))
    }

    /// Record a provider reasoning **summary** (never hidden chain-of-thought).
    ///
    /// Empty / whitespace-only content is skipped. Long input is truncated so a
    /// misbehaving provider cannot dump unbounded CoT into the event log.
    pub fn record_agent_reasoning_summary(
        &self,
        run_id: Uuid,
        text: impl Into<String>,
    ) -> Result<bool, RuntimeError> {
        if self.projection()?.active_run_id != Some(run_id) {
            return Err(RuntimeError::InactiveRun(run_id));
        }
        let trimmed = text.into();
        let trimmed = trimmed.trim();
        if trimmed.is_empty() {
            return Ok(false);
        }
        const MAX_SUMMARY_CHARS: usize = 2_048;
        let bounded = if trimmed.chars().count() > MAX_SUMMARY_CHARS {
            let end = trimmed
                .char_indices()
                .nth(MAX_SUMMARY_CHARS)
                .map(|(i, _)| i)
                .unwrap_or(trimmed.len());
            format!("{}…", &trimmed[..end])
        } else {
            trimmed.to_owned()
        };
        self.record(EventPayload::Agent(crate::AgentEvent::ReasoningSummary {
            run_id,
            text: bounded,
        }))?;
        Ok(true)
    }

    pub fn request_action(&self, action: Action) -> Result<RuntimeStatus, RuntimeError> {
        self.request_action_with_capability_version(action, None)
    }

    pub fn request_action_with_capability_version(
        &self,
        action: Action,
        capability_version: Option<u32>,
    ) -> Result<RuntimeStatus, RuntimeError> {
        match self.action_admission(&action, capability_version)? {
            crate::EffectDecision::Allow => {
                self.record(EventPayload::Notice(NoticeEvent::PolicyAllowed))?;
                Ok(RuntimeStatus::Idle)
            }
            crate::EffectDecision::Deny { reason } => {
                self.record(EventPayload::Notice(NoticeEvent::PolicyDenied {
                    reason: reason.clone(),
                }))?;
                Err(RuntimeError::Denied(reason))
            }
            crate::EffectDecision::NeedsApproval { reason } => {
                let intent_revision = self
                    .projection()?
                    .latest_intent_revision
                    .ok_or(RuntimeError::MissingIntentRevision)?;
                let approval = crate::ApprovalRequest::pending_with_version(
                    action,
                    reason,
                    intent_revision,
                    capability_version,
                );
                self.record(EventPayload::Approval(ApprovalEvent::Requested {
                    request: approval,
                }))?;
                Ok(RuntimeStatus::AwaitingApproval)
            }
        }
    }

    pub fn record_tool_started(
        &self,
        name: &str,
        tool_call_id: Option<&str>,
    ) -> Result<(), RuntimeError> {
        self.record(EventPayload::Tool(ToolEvent::Started {
            name: name.to_owned(),
            tool_call_id: tool_call_id.map(str::to_owned),
        }))
    }

    pub fn record_tool_finished(
        &self,
        name: &str,
        summary: &str,
        tool_call_id: Option<&str>,
    ) -> Result<(), RuntimeError> {
        self.record(EventPayload::Tool(ToolEvent::Finished {
            name: name.to_owned(),
            summary: summary.to_owned(),
            tool_call_id: tool_call_id.map(str::to_owned),
        }))
    }

    /// Bounded mid-tool output preview (chars, not bytes).
    pub const TOOL_OUTPUT_PREVIEW_CHARS: usize = 256;

    pub fn record_tool_output(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        preview: &str,
    ) -> Result<(), RuntimeError> {
        let bounded: String = preview
            .chars()
            .take(Self::TOOL_OUTPUT_PREVIEW_CHARS)
            .collect();
        let preview = if preview.chars().count() > Self::TOOL_OUTPUT_PREVIEW_CHARS {
            format!("{bounded}…")
        } else {
            bounded
        };
        self.record(EventPayload::Tool(ToolEvent::Output {
            tool_call_id: tool_call_id.to_owned(),
            tool_name: tool_name.to_owned(),
            preview,
        }))
    }

    /// Append `Child*` to parent session log when `parent_session_id` is a Uuid.
    /// Non-uuid labels (legacy tests) and missing sessions are no-ops.
    pub fn emit_parent_child_event(
        store: &dyn EventStore,
        parent_session_id: &str,
        event: ChildEvent,
    ) -> Result<(), RuntimeError> {
        let Ok(parent_id) = Uuid::parse_str(parent_session_id.trim()) else {
            return Ok(());
        };
        match store.append_next(parent_id, EventPayload::Child(event)) {
            Ok(_) => Ok(()),
            Err(crate::storage::StoreError::MissingSession(_)) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn record_child_started(&self, child_id: &str, role: &str) -> Result<(), RuntimeError> {
        self.record(EventPayload::Child(ChildEvent::Started {
            child_id: child_id.to_owned(),
            parent_id: self.session_id.to_string(),
            role: role.to_owned(),
        }))
    }

    pub fn record_child_status_changed(
        &self,
        child_id: &str,
        status: &str,
        current_action: Option<&str>,
    ) -> Result<(), RuntimeError> {
        self.record(EventPayload::Child(ChildEvent::StatusChanged {
            child_id: child_id.to_owned(),
            status: status.to_owned(),
            current_action: current_action.map(str::to_owned),
        }))
    }

    pub fn record_child_finished(
        &self,
        child_id: &str,
        status: &str,
        summary: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), RuntimeError> {
        self.record(EventPayload::Child(ChildEvent::Finished {
            child_id: child_id.to_owned(),
            status: status.to_owned(),
            summary: summary.map(str::to_owned),
            error: error.map(str::to_owned),
        }))
    }

    pub fn resolve_approval(&self, resolution: ApprovalResolution) -> Result<(), RuntimeError> {
        let mut approval = self
            .projection()?
            .pending_approvals
            .remove(&resolution.id)
            .ok_or(RuntimeError::MissingApproval(resolution.id))?;
        if approval.state != ApprovalState::Pending {
            return Err(RuntimeError::ApprovalNotPending(resolution.id));
        }
        if resolution.resolver != ApprovalResolver::User {
            return Err(RuntimeError::ApprovalResolverNotUser(resolution.id));
        }
        let latest_intent_revision = self.projection()?.latest_intent_revision;
        if resolution.action_fingerprint != approval.action_fingerprint
            || resolution.intent_revision != approval.intent_revision
            || latest_intent_revision != Some(approval.intent_revision)
        {
            approval.state = ApprovalState::Rejected;
            self.record(EventPayload::Approval(ApprovalEvent::Resolved {
                request: approval,
            }))?;
            return Err(RuntimeError::StaleApproval(resolution.id));
        }
        approval.state = if resolution.accepted {
            ApprovalState::Approved
        } else {
            ApprovalState::Rejected
        };
        self.record(EventPayload::Approval(ApprovalEvent::Resolved {
            request: approval,
        }))
    }

    /// A2 Phase 2: Store a deferred effect for later approval continuation.
    /// The effect must match the approval request exactly.
    pub fn store_deferred_effect(&self, deferred: DeferredEffect) -> Result<(), RuntimeError> {
        let approval_id = deferred.approval().id;
        if let Ok(mut effects) = self.deferred_effects.lock() {
            effects.insert(approval_id, deferred);
            Ok(())
        } else {
            Err(RuntimeError::Denied(
                "deferred effect storage lock poisoned".into(),
            ))
        }
    }

    /// A2 Phase 2: Retrieve a deferred effect for approval continuation.
    /// Returns None if no effect was stored for this approval.
    pub fn take_deferred_effect(
        &self,
        approval_id: Uuid,
    ) -> Result<Option<DeferredEffect>, RuntimeError> {
        if let Ok(mut effects) = self.deferred_effects.lock() {
            Ok(effects.remove(&approval_id))
        } else {
            Err(RuntimeError::Denied(
                "deferred effect storage lock poisoned".into(),
            ))
        }
    }

    pub fn events(&self) -> Result<Vec<Event>, RuntimeError> {
        Ok(self.store.list(self.session_id)?)
    }

    /// Append an event to the session event log
    pub fn append_event(&self, payload: EventPayload) -> Result<(), RuntimeError> {
        self.store.append_next(self.session_id, payload)?;
        Ok(())
    }

    pub fn status(&self) -> Result<RuntimeStatus, RuntimeError> {
        let projection = self.projection()?;
        if !projection.pending_approvals.is_empty() {
            return Ok(RuntimeStatus::AwaitingApproval);
        }
        if projection.active_run_id.is_some() {
            return Ok(RuntimeStatus::Running);
        }
        Ok(match projection.outcome {
            Some(RunEvent::Completed { .. }) => RuntimeStatus::Completed,
            Some(RunEvent::Failed { .. }) => RuntimeStatus::Failed,
            Some(RunEvent::Cancelled { .. }) => RuntimeStatus::Cancelled,
            Some(RunEvent::InterruptedUnknown { .. }) => RuntimeStatus::InterruptedUnknown,
            Some(RunEvent::Started { .. }) => RuntimeStatus::Running,
            None => RuntimeStatus::Idle,
        })
    }

    pub fn pending_approval(&self, id: Uuid) -> Result<Option<ApprovalRequest>, RuntimeError> {
        Ok(self.projection()?.pending_approvals.get(&id).cloned())
    }

    pub fn active_run_id(&self) -> Result<Option<Uuid>, RuntimeError> {
        Ok(self.projection()?.active_run_id)
    }

    pub fn record_deferred_tool(
        &self,
        approval_id: Uuid,
        tool_call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    ) -> Result<(), RuntimeError> {
        self.record(EventPayload::Tool(ToolEvent::Deferred {
            approval_id,
            tool_call_id,
            tool_name,
            arguments,
        }))
    }

    pub fn deferred_tool(
        &self,
        approval_id: Uuid,
    ) -> Result<Option<(String, String, serde_json::Value)>, RuntimeError> {
        Ok(self.projection()?.deferred_tools.get(&approval_id).cloned())
    }

    pub fn cancel(&self) -> Result<RuntimeStatus, RuntimeError> {
        let projection = self.projection()?;
        let Some(run_id) = projection.active_run_id else {
            return self.status();
        };
        self.finish_run(RunEvent::Cancelled { run_id })?;
        self.status()
    }

    pub(crate) fn projection(&self) -> Result<crate::SessionProjection, RuntimeError> {
        reduce(&self.events()?)?.ok_or(RuntimeError::MissingSession(self.session_id))
    }

    fn record(&self, payload: EventPayload) -> Result<(), RuntimeError> {
        self.store.append_next(self.session_id, payload)?;
        Ok(())
    }

    /// Public event recording surface for tool and capability helpers that emit
    /// durable lifecycle events on behalf of a runtime.
    pub fn record_event(&self, payload: EventPayload) -> Result<(), RuntimeError> {
        self.record(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionKind, ActionOrigin, MemoryEventStore, SandboxScope, SqliteEventStore};

    #[test]
    fn write_requires_an_approval_event() {
        let runtime = AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        runtime
            .submit_intent("update workspace")
            .expect("record intent");
        let status = runtime
            .request_action(Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WriteFile,
                summary: "write config".into(),
                target: Some("Cargo.toml".into()),
            })
            .expect("request approval");
        assert_eq!(status, RuntimeStatus::AwaitingApproval);
        assert!(
            runtime
                .events()
                .expect("events")
                .iter()
                .any(|event| matches!(
                    event.payload,
                    EventPayload::Approval(ApprovalEvent::Requested { .. })
                ))
        );
    }

    #[test]
    fn plan_mode_denies_agent_write_via_effect_seam() {
        let runtime = AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        runtime
            .record_event(EventPayload::Session(
                crate::SessionEvent::ExecutionModeChanged {
                    mode: ExecutionMode::Plan,
                },
            ))
            .expect("set plan mode");
        runtime.submit_intent("plan only").expect("record intent");
        let error = runtime
            .request_action(Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WriteFile,
                summary: "write config".into(),
                target: Some("Cargo.toml".into()),
            })
            .expect_err("plan must deny write");
        assert!(matches!(error, RuntimeError::Denied(reason) if reason.contains("PLAN")));
    }

    #[test]
    fn auto_mode_allows_agent_write_without_approval_event() {
        let runtime = AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        runtime
            .record_event(EventPayload::Session(
                crate::SessionEvent::ExecutionModeChanged {
                    mode: ExecutionMode::Auto,
                },
            ))
            .expect("set auto mode");
        runtime.submit_intent("auto edits").expect("record intent");
        let status = runtime
            .request_action(Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WriteFile,
                summary: "write config".into(),
                target: Some("Cargo.toml".into()),
            })
            .expect("auto write allowed");
        assert_eq!(status, RuntimeStatus::Idle);
        assert!(
            !runtime
                .events()
                .expect("events")
                .iter()
                .any(|event| matches!(
                    event.payload,
                    EventPayload::Approval(ApprovalEvent::Requested { .. })
                ))
        );
    }

    #[test]
    fn web_submit_in_ask_mode_requests_approval_not_deny() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut scope = SandboxScope::local_workspace(workspace.path());
        scope.allow_network = true;
        let runtime = AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(scope),
        );
        runtime.submit_intent("submit").expect("intent");
        let decision = runtime
            .action_admission(
                &Action {
                    origin: ActionOrigin::Agent,
                    kind: ActionKind::WebSubmit,
                    summary: "web_submit via agent".into(),
                    target: Some("example.com".into()),
                },
                Some(1),
            )
            .expect("admission");
        assert!(
            matches!(decision, crate::EffectDecision::NeedsApproval { .. }),
            "{decision:?}"
        );
    }

    #[test]
    fn reload_policy_config_applies_without_restart() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut runtime = AgentRuntime::create_with_workspace(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
            workspace.path().to_path_buf(),
        )
        .expect("create");
        runtime
            .submit_intent("update workspace")
            .expect("record intent");

        let write = Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::WriteFile,
            summary: "write".into(),
            target: Some("new-after-reload.txt".into()),
        };
        assert_eq!(
            runtime.request_action(write.clone()).expect("request"),
            RuntimeStatus::AwaitingApproval
        );

        runtime.reload_policy_config(
            crate::PolicyConfig::parse(r#"{"version":1,"overrides":{"write_file":"allow"}}"#)
                .expect("config"),
        );
        assert_eq!(
            runtime.request_action(write).expect("allowed write"),
            RuntimeStatus::Idle
        );
    }

    #[test]
    fn create_with_workspace_preserves_policy_overrides() {
        let workspace = tempfile::tempdir().expect("workspace");
        let config =
            crate::PolicyConfig::parse(r#"{"version":1,"overrides":{"write_file":"allow"}}"#)
                .expect("config");
        let runtime = AgentRuntime::create_with_workspace(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::with_config(SandboxScope::local_workspace(workspace.path()), config),
            workspace.path().to_path_buf(),
        )
        .expect("create");

        let status = runtime
            .request_action(Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WriteFile,
                summary: "write".into(),
                target: Some("preserved.txt".into()),
            })
            .expect("request");
        assert_eq!(status, RuntimeStatus::Idle);
        assert_eq!(
            runtime
                .policy()
                .config()
                .override_for(ActionKind::WriteFile),
            Some(crate::PolicyConfigDecision::Allow)
        );
    }

    #[test]
    fn session_workspace_is_canonical_and_survives_attach() {
        let root = tempfile::tempdir().expect("temp root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).expect("create workspace");
        let store = Arc::new(MemoryEventStore::default());
        let runtime = AgentRuntime::create_with_workspace(
            store.clone(),
            PolicyEngine::new(SandboxScope::local_workspace(root.path())),
            workspace.clone(),
        )
        .expect("create runtime");
        let session_id = runtime.session_id();
        let canonical_workspace = workspace.canonicalize().expect("canonical workspace");
        assert_eq!(
            runtime.workspace_root().expect("workspace root"),
            canonical_workspace
        );

        let attached = AgentRuntime::attach(
            store,
            PolicyEngine::new(SandboxScope::local_workspace(root.path())),
            session_id,
        )
        .expect("attach runtime");
        assert_eq!(
            attached.workspace_root().expect("attached workspace root"),
            canonical_workspace
        );
    }

    #[test]
    fn approval_request_without_a_user_intent_is_rejected() {
        let runtime = AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );

        assert!(matches!(
            runtime.request_action(Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WriteFile,
                summary: "update config".into(),
                target: Some("Cargo.toml".into()),
            }),
            Err(RuntimeError::MissingIntentRevision)
        ));
    }

    #[test]
    fn attach_recovers_pending_approval_and_next_sequence() {
        let test_root =
            std::env::temp_dir().join(format!("impetus-runtime-recovery-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&test_root).expect("create isolated test directory");
        let database = test_root.join("events.sqlite3");
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let session_id;
        let approval_id;
        {
            let runtime = AgentRuntime::create(
                SqliteEventStore::open(&database).expect("open store"),
                policy.clone(),
            )
            .expect("create runtime");
            session_id = runtime.session_id();
            runtime
                .submit_intent("explain repository")
                .expect("record intent");
            runtime
                .request_action(Action {
                    origin: ActionOrigin::Agent,
                    kind: ActionKind::WriteFile,
                    summary: "edit config".into(),
                    target: Some("Cargo.toml".into()),
                })
                .expect("request approval");
            approval_id = runtime
                .projection()
                .expect("projection")
                .pending_approvals
                .keys()
                .next()
                .copied()
                .expect("pending approval");
        }
        let recovered = AgentRuntime::attach(
            SqliteEventStore::open(&database).expect("reopen store"),
            policy,
            session_id,
        )
        .expect("attach runtime");
        assert!(
            recovered
                .store
                .list_sessions()
                .expect("list sessions")
                .iter()
                .any(|session| session.id == session_id)
        );
        assert_eq!(
            recovered.status().expect("recovered status"),
            RuntimeStatus::AwaitingApproval
        );
        recovered
            .resolve_approval(ApprovalResolution::user(
                recovered
                    .projection()
                    .expect("projection")
                    .pending_approvals
                    .get(&approval_id)
                    .expect("pending approval"),
                true,
            ))
            .expect("resolve recovered approval");
        let events = recovered.events().expect("recovered events");
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        std::fs::remove_dir_all(test_root).expect("remove isolated test directory");
    }

    #[test]
    fn reattach_recovers_the_exact_deferred_tool_arguments() {
        let root = tempfile::tempdir().expect("runtime root");
        let database = root.path().join("events.sqlite3");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(&workspace));
        let (session_id, approval_id);
        {
            let runtime = AgentRuntime::create_with_workspace(
                SqliteEventStore::open(&database).expect("store"),
                policy.clone(),
                workspace.clone(),
            )
            .expect("runtime");
            session_id = runtime.session_id();
            runtime
                .submit_intent("write an exact file")
                .expect("intent");
            runtime
                .request_action(crate::Action {
                    origin: ActionOrigin::Agent,
                    kind: ActionKind::WriteFile,
                    summary: "write_file via agent".into(),
                    target: Some("result.txt".into()),
                })
                .expect("approval");
            approval_id = runtime
                .events()
                .expect("events")
                .into_iter()
                .find_map(|event| match event.payload {
                    EventPayload::Approval(ApprovalEvent::Requested { request }) => {
                        Some(request.id)
                    }
                    _ => None,
                })
                .expect("approval id");
            runtime
                .record_deferred_tool(
                    approval_id,
                    "call-7".into(),
                    "write_file".into(),
                    serde_json::json!({"path": "result.txt", "content": "exact value"}),
                )
                .expect("deferred tool");
        }

        let recovered = AgentRuntime::attach(
            SqliteEventStore::open(&database).expect("reopened store"),
            policy,
            session_id,
        )
        .expect("reattach");
        assert_eq!(
            recovered.deferred_tool(approval_id).expect("deferred tool"),
            Some((
                "call-7".into(),
                "write_file".into(),
                serde_json::json!({"path": "result.txt", "content": "exact value"}),
            ))
        );
    }

    #[test]
    fn attached_runtime_recovers_run_status() {
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let runtime = AgentRuntime::create(store.clone(), policy.clone()).expect("create runtime");
        let session_id = runtime.session_id();
        let run_id = runtime.start_run().expect("start run");
        let recovered = AgentRuntime::attach(store, policy, session_id).expect("attach runtime");
        assert_eq!(
            recovered.status().expect("running status"),
            RuntimeStatus::Running
        );
        recovered
            .finish_run(RunEvent::Completed { run_id })
            .expect("finish run");
        assert_eq!(
            recovered.status().expect("completed status"),
            RuntimeStatus::Completed
        );
    }

    #[test]
    fn changed_intent_rejects_stale_approval_durably() {
        let runtime = AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        runtime
            .submit_intent("change config")
            .expect("first intent");
        runtime
            .request_action(Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WriteFile,
                summary: "update config".into(),
                target: Some("Cargo.toml".into()),
            })
            .expect("request approval");
        let request = runtime
            .projection()
            .expect("projection")
            .pending_approvals
            .into_values()
            .next()
            .expect("pending approval");

        runtime
            .submit_intent("change readme instead")
            .expect("new intent");
        assert!(matches!(
            runtime.resolve_approval(ApprovalResolution::user(&request, true)),
            Err(RuntimeError::StaleApproval(id)) if id == request.id
        ));
        assert!(
            runtime
                .projection()
                .expect("projection")
                .pending_approvals
                .is_empty()
        );
        assert!(matches!(
            runtime.events().expect("events").last().map(|event| &event.payload),
            Some(EventPayload::Approval(ApprovalEvent::Resolved { request }))
                if request.state == ApprovalState::Rejected
        ));
    }

    #[test]
    fn agent_cannot_resolve_its_own_approval() {
        let runtime = AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        runtime
            .submit_intent("change config")
            .expect("record intent");
        runtime
            .request_action(Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WriteFile,
                summary: "update config".into(),
                target: Some("Cargo.toml".into()),
            })
            .expect("request approval");
        let request = runtime
            .projection()
            .expect("projection")
            .pending_approvals
            .into_values()
            .next()
            .expect("pending approval");
        let mut decision = ApprovalResolution::user(&request, true);
        decision.resolver = ApprovalResolver::Agent;

        assert!(matches!(
            runtime.resolve_approval(decision),
            Err(RuntimeError::ApprovalResolverNotUser(id)) if id == request.id
        ));
        assert!(
            runtime
                .projection()
                .expect("projection")
                .pending_approvals
                .contains_key(&request.id)
        );
    }

    #[test]
    fn altered_fingerprint_cannot_resolve_pending_approval() {
        let runtime = AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        runtime
            .submit_intent("change config")
            .expect("record intent");
        runtime
            .request_action(Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WriteFile,
                summary: "update config".into(),
                target: Some("Cargo.toml".into()),
            })
            .expect("request approval");
        let request = runtime
            .projection()
            .expect("projection")
            .pending_approvals
            .into_values()
            .next()
            .expect("pending approval");
        let mut decision = ApprovalResolution::user(&request, true);
        decision.action_fingerprint = Action {
            target: Some("README.md".into()),
            ..request.action.clone()
        }
        .fingerprint();

        assert!(matches!(
            runtime.resolve_approval(decision),
            Err(RuntimeError::StaleApproval(id)) if id == request.id
        ));
        assert!(
            runtime
                .projection()
                .expect("projection")
                .pending_approvals
                .is_empty()
        );
    }

    #[test]
    fn approval_fingerprint_and_intent_revision_replay_deterministically() {
        let runtime = AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        runtime
            .submit_intent("change config")
            .expect("record intent");
        runtime
            .request_action(Action {
                origin: ActionOrigin::Agent,
                kind: ActionKind::WriteFile,
                summary: "update config".into(),
                target: Some("Cargo.toml".into()),
            })
            .expect("request approval");

        let events = runtime.events().expect("events");
        let first = reduce(&events).expect("first replay").expect("projection");
        let second = reduce(&events).expect("second replay").expect("projection");
        let request = first
            .pending_approvals
            .values()
            .next()
            .expect("pending approval");
        assert_eq!(first, second);
        assert_eq!(request.intent_revision, 2);
        assert_eq!(request.action_fingerprint, request.action.fingerprint());
    }

    #[test]
    fn runtime_fork_creates_independent_session() {
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));

        let source_runtime = AgentRuntime::new(store.clone(), policy.clone());
        let source_id = source_runtime.session_id();

        source_runtime
            .submit_intent("first intent")
            .expect("submit intent 1");
        source_runtime
            .submit_intent("second intent")
            .expect("submit intent 2");
        source_runtime
            .submit_intent("third intent")
            .expect("submit intent 3");

        // Fork up to sequence 2 (Created + first intent)
        let forked_runtime =
            AgentRuntime::fork(store.clone(), policy.clone(), source_id, 2).expect("fork runtime");
        let forked_id = forked_runtime.session_id();

        assert_ne!(source_id, forked_id);

        let forked_events = forked_runtime.events().expect("forked events");
        assert_eq!(forked_events.len(), 2);

        // Forked session can continue independently
        forked_runtime
            .submit_intent("forked intent")
            .expect("submit to fork");
        let forked_events_after = forked_runtime.events().expect("forked events after");
        assert_eq!(forked_events_after.len(), 3);

        // Source session unchanged
        let source_events = source_runtime.events().expect("source events");
        assert_eq!(source_events.len(), 4);
    }

    #[test]
    fn runtime_restore_checkpoint_creates_new_branch() {
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));

        let source = AgentRuntime::new(store.clone(), policy.clone());
        let source_id = source.session_id();
        source.submit_intent("one").expect("intent 1");
        source.submit_intent("two").expect("intent 2");

        let checkpoint = store
            .create_checkpoint(source_id, "mid".into(), 2)
            .expect("checkpoint");
        source.submit_intent("three").expect("intent 3");

        let restored = AgentRuntime::restore_checkpoint(store.clone(), policy, checkpoint.id)
            .expect("restore");
        assert_ne!(restored.session_id(), source_id);
        assert_eq!(restored.events().expect("restored").len(), 2);
        assert_eq!(source.events().expect("source").len(), 4);
    }

    #[test]
    fn large_agent_chunk_spills_to_artifact_and_keeps_bounded_preview() {
        let artifact_root = tempfile::tempdir().expect("artifact root");
        let store =
            crate::DurableArtifactStore::open(artifact_root.path()).expect("open artifact store");
        let runtime = AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        let run_id = runtime.start_run().expect("start run");
        let megabyte = "x".repeat(MAX_AGENT_CHUNK_EVENT_BYTES + 64 * 1024);
        assert!(
            runtime
                .record_agent_chunk_with_store(run_id, 1, megabyte.clone(), Some(&store))
                .expect("record")
        );

        let events = runtime.events().expect("events");
        let chunk = events
            .iter()
            .find_map(|event| match &event.payload {
                EventPayload::Agent(crate::AgentEvent::Chunk {
                    text,
                    artifact,
                    chunk_id,
                    ..
                }) => Some((text.clone(), artifact.clone(), *chunk_id)),
                _ => None,
            })
            .expect("chunk event");
        assert_eq!(chunk.2, 1);
        assert!(chunk.0.len() <= AGENT_CHUNK_PREVIEW_BYTES + 4);
        assert!(chunk.0.len() < megabyte.len());
        let artifact = chunk.1.expect("artifact ref");
        assert_eq!(artifact.byte_count, megabyte.len());
        let body = store.read(&artifact.id).expect("read artifact");
        assert_eq!(body, megabyte.as_bytes());

        // Duplicate chunk_id is skipped (reconnect/resume).
        assert!(
            !runtime
                .record_agent_chunk_with_store(run_id, 1, "dup", Some(&store))
                .expect("dup")
        );
        assert_eq!(
            runtime
                .events()
                .expect("events")
                .iter()
                .filter(|event| matches!(
                    event.payload,
                    EventPayload::Agent(crate::AgentEvent::Chunk { .. })
                ))
                .count(),
            1
        );
    }

    #[test]
    fn large_final_with_chunks_stays_bounded() {
        let runtime = AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        let run_id = runtime.start_run().expect("start run");
        runtime.record_agent_chunk(run_id, 1, "hi").expect("chunk");
        let megabyte = "y".repeat(MAX_AGENT_CHUNK_EVENT_BYTES + 1024);
        runtime
            .record_agent_final(run_id, megabyte.clone())
            .expect("final");
        let final_text = runtime
            .events()
            .expect("events")
            .iter()
            .find_map(|event| match &event.payload {
                EventPayload::Agent(crate::AgentEvent::Final { text, .. }) => Some(text.clone()),
                _ => None,
            })
            .expect("final");
        assert!(final_text.len() <= AGENT_CHUNK_PREVIEW_BYTES + 4);
        assert!(final_text.len() < megabyte.len());
    }
}
