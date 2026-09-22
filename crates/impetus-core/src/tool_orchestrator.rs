//! Tool Orchestrator: structured tool lifecycle and effect normalization.
//!
//! The orchestrator sits between the agent loop and individual tool implementations,
//! providing:
//! - Normalized effect representation
//! - Policy admission for each effect
//! - Durable observations
//! - Tool execution coordination

use crate::{
    Action, ActionKind, ActionOrigin, AgentRuntime, DurableArtifactStore, EffectSeam,
    HookPrefilter, PolicyEngine, ReadOnlyTool, ReadOnlyTools, RuntimeError, ToolArgError,
    ToolEvent, ToolEventOutcome, ToolOutcome, validate_tool_arguments,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("tool `{0}` not found")]
    ToolNotFound(String),
    #[error(transparent)]
    InvalidArguments(#[from] ToolArgError),
    #[error("tool `{tool}` failed: {reason}")]
    ToolFailed { tool: String, reason: String },
    #[error("tool execution denied: {0}")]
    Denied(String),
    #[error("tool execution requires approval")]
    ApprovalRequired,
}

/// Structured tool invocation request from the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRequest {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Normalized observation returned to the model after tool execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolObservation {
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments_summary: String,
    pub outcome: ToolOutcomeStatus,
    pub preview: String,
    pub artifact: Option<crate::DurableArtifactRef>,
    pub error: Option<String>,
}

pub type ToolOutcomeStatus = ToolEventOutcome;

/// Tool Orchestrator coordinates tool execution through the safety boundary.
#[allow(dead_code)] // Fields used in future iterations
pub struct ToolOrchestrator {
    policy: PolicyEngine,
    workspace_root: PathBuf,
    artifact_root: PathBuf,
    web_research: Option<Arc<dyn crate::web_research::WebResearchService>>,
    mcp_live: Option<Arc<crate::mcp_live::McpLiveBridge>>,
    coding_tools: Arc<dyn crate::CodingToolsService>,
    allowed_tools: Option<Vec<String>>,
    hook_prefilter: HookPrefilter,
}

impl ToolOrchestrator {
    fn session_effect_seam(runtime: &AgentRuntime) -> Result<EffectSeam, OrchestratorError> {
        runtime.effect_seam().map_err(OrchestratorError::Runtime)
    }

    pub fn new(policy: PolicyEngine, workspace_root: PathBuf) -> Self {
        Self::with_artifact_root(policy, workspace_root, crate::default_artifact_root())
    }

    pub fn with_artifact_root(
        policy: PolicyEngine,
        workspace_root: PathBuf,
        artifact_root: PathBuf,
    ) -> Self {
        Self {
            policy,
            workspace_root,
            artifact_root,
            web_research: None,
            mcp_live: None,
            coding_tools: Arc::new(crate::OptionalCodingToolsService::absent()),
            allowed_tools: None,
            hook_prefilter: HookPrefilter::default(),
        }
    }

    /// Build a restricted orchestrator for Explore subagents.
    ///
    /// `allowed_tools` uses Explore/role labels (`list`/`read`/`search`/`write`/`web`).
    /// Call-time checks also accept mapped schema names (`list_files`/`read_file`/…).
    /// Empty allowlist means everything denied.
    pub fn for_explore(
        policy: PolicyEngine,
        workspace_root: PathBuf,
        allowed_tools: &[String],
    ) -> Self {
        Self::with_artifact_root(policy, workspace_root, crate::default_artifact_root())
            .with_allowed_tools(allowed_tools.to_vec())
    }

    pub fn with_allowed_tools(mut self, allowed: Vec<String>) -> Self {
        self.allowed_tools = Some(allowed);
        self
    }

    /// Attach the daemon-owned semantic web service. Provider details stay outside the agent loop.
    pub fn with_web_research(
        mut self,
        service: Arc<dyn crate::web_research::WebResearchService>,
    ) -> Self {
        self.web_research = Some(service);
        self
    }

    /// Attach a live MCP tool catalog for this session (discover/list + call path).
    pub fn with_mcp_live(mut self, bridge: Arc<crate::mcp_live::McpLiveBridge>) -> Self {
        self.mcp_live = Some(bridge);
        self
    }

    /// Attach optional coding-tools seam (definition/refs/…). Absent by default.
    pub fn with_coding_tools(mut self, service: Arc<dyn crate::CodingToolsService>) -> Self {
        self.coding_tools = service;
        self
    }

    /// Attach daemon-owned hook prefilter catalog for process spawn paths.
    pub fn with_hook_prefilter(mut self, prefilter: HookPrefilter) -> Self {
        self.hook_prefilter = prefilter;
        self
    }

    /// Process a batch of tool calls from the model.
    ///
    /// Parallelizes read-only and idempotent tools while preserving
    /// sequential execution for mutating operations.
    ///
    /// For each tool:
    /// 1. Validate arguments against the builtin JSON Schema
    /// 2. Normalize into an Action
    /// 3. Request policy decision
    /// 4. If allowed, execute and capture observation
    /// 5. If denied or needs approval, record that outcome
    ///
    /// Returns observations for all tool calls (success or error).
    pub async fn process_tool_calls(
        &self,
        _run_id: Uuid,
        tool_calls: Vec<crate::ToolCall>,
        runtime: &Arc<AgentRuntime>,
    ) -> Result<Vec<ToolObservation>, OrchestratorError> {
        // Partition tool calls into parallelizable and sequential
        let (parallel_calls, sequential_calls): (Vec<_>, Vec<_>) =
            tool_calls.into_iter().partition(|tool_call| {
                if let Some(bridge) = &self.mcp_live
                    && let Some(entry) = bridge.get(&tool_call.name)
                {
                    return matches!(
                        entry.semantics,
                        crate::module::ExecutionSemantics::ReadOnly
                            | crate::module::ExecutionSemantics::Idempotent
                    );
                }
                // Try to normalize and check if parallelizable
                self.normalize_tool_call(tool_call)
                    .ok()
                    .map(|action| action.kind.can_parallelize())
                    .unwrap_or(false)
            });

        let mut observations = Vec::with_capacity(parallel_calls.len() + sequential_calls.len());

        // Execute parallel tools concurrently
        if !parallel_calls.is_empty() {
            let mut handles = Vec::new();

            for tool_call in parallel_calls {
                let runtime = runtime.clone();
                let orchestrator_policy = self.policy.clone();
                let orchestrator_workspace = self.workspace_root.clone();
                let orchestrator_artifact = self.artifact_root.clone();
                let web_research = self.web_research.clone();
                let mcp_live = self.mcp_live.clone();
                let coding_tools = self.coding_tools.clone();
                let allowed_tools = self.allowed_tools.clone();

                let handle = tokio::spawn(async move {
                    let mut orchestrator = ToolOrchestrator::with_artifact_root(
                        orchestrator_policy,
                        orchestrator_workspace,
                        orchestrator_artifact,
                    )
                    .with_optional_web_research(web_research)
                    .with_optional_mcp_live(mcp_live)
                    .with_coding_tools(coding_tools);
                    if let Some(allowed) = allowed_tools {
                        orchestrator = orchestrator.with_allowed_tools(allowed);
                    }
                    orchestrator.process_single_tool(tool_call, &runtime).await
                });
                handles.push(handle);
            }

            // Collect results in order
            for handle in handles {
                if let Ok(observation) = handle.await {
                    observations.push(observation);
                }
            }
        }

        // Execute sequential tools one by one
        for tool_call in sequential_calls {
            let observation = self.process_single_tool(tool_call, runtime).await;
            observations.push(observation);
        }

        Ok(observations)
    }

    fn with_optional_web_research(
        mut self,
        service: Option<Arc<dyn crate::web_research::WebResearchService>>,
    ) -> Self {
        self.web_research = service;
        self
    }

    fn with_optional_mcp_live(
        mut self,
        bridge: Option<Arc<crate::mcp_live::McpLiveBridge>>,
    ) -> Self {
        self.mcp_live = bridge;
        self
    }

    async fn process_single_tool(
        &self,
        mut tool_call: crate::ToolCall,
        runtime: &Arc<AgentRuntime>,
    ) -> ToolObservation {
        // Step 0: Child allowlist — Explore/role labels and mapped schema names.
        if let Some(allowed) = &self.allowed_tools {
            if !child_tool_allowed(allowed, &tool_call.name) {
                return Self::record_observation(
                    runtime,
                    tool_call.clone(),
                    summarize_arguments(&tool_call.arguments),
                    ToolOutcomeStatus::Denied,
                    String::new(),
                    None,
                    Some(format!("tool `{}` not in tool allowlist", tool_call.name)),
                );
            }

            // Map role/Explore labels to internal tool names
            tool_call.name = match tool_call.name.as_str() {
                "list" => "list_files".to_string(),
                "read" => "read_file".to_string(),
                "search" => "search".to_string(),
                "write" => "write_file".to_string(),
                "web" => "web_search".to_string(),
                name => name.to_string(),
            };
        }

        if self
            .mcp_live
            .as_ref()
            .is_some_and(|bridge| bridge.contains(&tool_call.name))
        {
            return self.execute_mcp_tool(tool_call, runtime).await;
        }

        let arguments_summary = summarize_arguments(&tool_call.arguments);
        // Step 1: Normalize tool call into Action
        let action = match self.normalize_tool_call(&tool_call) {
            Ok(action) => action,
            Err(e) => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Error,
                    String::new(),
                    None,
                    Some(e.to_string()),
                );
            }
        };

        if matches!(
            action.kind,
            ActionKind::WriteFile
                | ActionKind::SpawnProcess
                | ActionKind::WebDownload
                | ActionKind::WebBrowser
                | ActionKind::WebSubmit
                | ActionKind::WebUpload
        ) {
            if contains_sensitive_value(&tool_call.arguments) {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Denied,
                    String::new(),
                    None,
                    Some("mutating tool arguments contain sensitive material".into()),
                );
            }
            let outbound_web = matches!(
                action.kind,
                ActionKind::WebDownload
                    | ActionKind::WebBrowser
                    | ActionKind::WebSubmit
                    | ActionKind::WebUpload
            );
            let outcome = match runtime.request_action_with_capability_version(action, Some(1)) {
                Ok(crate::RuntimeStatus::Idle) if outbound_web => {
                    // Session granted outbound web, but no executor is wired yet.
                    return Self::record_observation(
                        runtime,
                        tool_call,
                        arguments_summary,
                        ToolOutcomeStatus::Error,
                        String::new(),
                        None,
                        Some("outbound web tool is not available in this build".into()),
                    );
                }
                Ok(_) => {
                    if let Some(approval_id) = runtime.events().ok().and_then(|events| {
                        events.iter().rev().find_map(|event| match &event.payload {
                            crate::EventPayload::Approval(crate::ApprovalEvent::Requested {
                                request,
                            }) => Some(request.id),
                            _ => None,
                        })
                    }) {
                        let _ = runtime.record_deferred_tool(
                            approval_id,
                            tool_call.id.clone(),
                            tool_call.name.clone(),
                            tool_call.arguments.clone(),
                        );
                    }
                    ToolOutcomeStatus::ApprovalRequired
                }
                Err(RuntimeError::Denied(_)) => ToolOutcomeStatus::Denied,
                Err(_) => ToolOutcomeStatus::Error,
            };
            return Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                outcome,
                String::new(),
                None,
                Some("awaiting user approval".into()),
            );
        }

        if matches!(tool_call.name.as_str(), "web_search" | "web_fetch") {
            let status = match runtime.request_action_with_capability_version(action, Some(1)) {
                Ok(status) => status,
                Err(RuntimeError::Denied(reason)) => {
                    return Self::record_observation(
                        runtime,
                        tool_call,
                        arguments_summary,
                        ToolOutcomeStatus::Denied,
                        String::new(),
                        None,
                        Some(reason),
                    );
                }
                Err(error) => {
                    return Self::record_observation(
                        runtime,
                        tool_call,
                        arguments_summary,
                        ToolOutcomeStatus::Error,
                        String::new(),
                        None,
                        Some(error.to_string()),
                    );
                }
            };
            if matches!(status, crate::RuntimeStatus::AwaitingApproval) {
                if let Some(approval_id) = runtime.events().ok().and_then(|events| {
                    events.iter().rev().find_map(|event| match &event.payload {
                        crate::EventPayload::Approval(crate::ApprovalEvent::Requested {
                            request,
                        }) => Some(request.id),
                        _ => None,
                    })
                }) {
                    let _ = runtime.record_deferred_tool(
                        approval_id,
                        tool_call.id.clone(),
                        tool_call.name.clone(),
                        tool_call.arguments.clone(),
                    );
                }
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::ApprovalRequired,
                    String::new(),
                    None,
                    Some("awaiting user approval".into()),
                );
            }

            return self
                .execute_web_tool(runtime, tool_call, arguments_summary)
                .await;
        }

        if tool_call.name == "goto_definition" {
            return self
                .execute_goto_definition(runtime, tool_call, arguments_summary)
                .await;
        }

        if tool_call.name == "search" {
            let pattern = tool_call
                .arguments
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let target = tool_call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".")
                .to_owned();
            let _ = runtime.record_event(crate::EventPayload::Tool(ToolEvent::SearchStarted {
                tool_call_id: tool_call.id.clone(),
                pattern,
                target,
            }));
        }

        match self.execute_read_only(runtime, &tool_call) {
            Ok(ToolOutcome::Allowed { result }) => {
                Self::emit_read_search_activity(runtime, &tool_call, &result.preview);
                Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Success,
                    result.preview,
                    result.artifact,
                    None,
                )
            }
            Ok(ToolOutcome::Denied { reason, .. }) => Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                ToolOutcomeStatus::Denied,
                String::new(),
                None,
                Some(reason),
            ),
            Err(error) => Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                ToolOutcomeStatus::Error,
                String::new(),
                None,
                Some(error.to_string()),
            ),
        }
    }

    /// Typed FileRead / SearchResult on durable session log (not tool-name scrape).
    fn emit_read_search_activity(
        runtime: &AgentRuntime,
        tool_call: &crate::ToolCall,
        preview: &str,
    ) {
        let preview = crate::bound_activity_preview(preview);
        match tool_call.name.as_str() {
            "read_file" => {
                let path = tool_call
                    .arguments
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(".")
                    .to_owned();
                let bytes = preview.len() as u64;
                let _ = runtime.record_event(crate::EventPayload::Tool(ToolEvent::FileRead {
                    tool_call_id: tool_call.id.clone(),
                    path,
                    bytes,
                    preview,
                }));
            }
            "search" => {
                let match_count = preview
                    .lines()
                    .filter(|line| !line.trim().is_empty() && !line.starts_with("..."))
                    .count() as u32;
                let _ = runtime.record_event(crate::EventPayload::Tool(ToolEvent::SearchResult {
                    tool_call_id: tool_call.id.clone(),
                    match_count,
                    preview,
                }));
            }
            _ => {}
        }
    }

    async fn execute_mcp_tool(
        &self,
        tool_call: crate::ToolCall,
        runtime: &Arc<AgentRuntime>,
    ) -> ToolObservation {
        let arguments_summary = summarize_arguments(&tool_call.arguments);
        let Some(bridge) = &self.mcp_live else {
            return Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                ToolOutcomeStatus::Error,
                String::new(),
                None,
                Some("MCP live bridge is unavailable".into()),
            );
        };

        let entry = match bridge.validate_arguments(&tool_call.name, &tool_call.arguments) {
            Ok(entry) => entry.clone(),
            Err(error) => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Error,
                    String::new(),
                    None,
                    Some(error.to_string()),
                );
            }
        };

        let mutating = matches!(
            entry.semantics,
            crate::module::ExecutionSemantics::Mutating
                | crate::module::ExecutionSemantics::NonReplayable
        );
        if mutating && contains_sensitive_value(&tool_call.arguments) {
            return Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                ToolOutcomeStatus::Denied,
                String::new(),
                None,
                Some("mutating MCP tool arguments contain sensitive material".into()),
            );
        }

        // Read-only → workspace_read (Allow). Mutating → process_spawn (NeedsApproval).
        // Do not auto-trust readOnlyHint beyond semantics_from_annotations (missing → Mutating).
        let effect = if mutating {
            crate::NormalizedEffect::process_spawn(
                ActionOrigin::Agent,
                format!("{} via mcp", entry.catalog_name),
                self.workspace_root.display().to_string(),
            )
        } else {
            crate::NormalizedEffect::workspace_read(
                ActionOrigin::Agent,
                format!("{} via mcp", entry.catalog_name),
                self.workspace_root.display().to_string(),
            )
        };
        let seam = match Self::session_effect_seam(runtime) {
            Ok(seam) => seam,
            Err(error) => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Error,
                    String::new(),
                    None,
                    Some(error.to_string()),
                );
            }
        };
        match seam.decide(&effect) {
            crate::EffectDecision::Allow => {}
            crate::EffectDecision::NeedsApproval { reason } => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::ApprovalRequired,
                    String::new(),
                    None,
                    Some(reason),
                );
            }
            crate::EffectDecision::Deny { reason } => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Denied,
                    String::new(),
                    None,
                    Some(reason),
                );
            }
        }

        match bridge
            .call(&tool_call.name, tool_call.arguments.clone())
            .await
        {
            crate::mcp_live::McpLiveCallResult::Ok { preview } => Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                ToolOutcomeStatus::Success,
                preview,
                None,
                None,
            ),
            crate::mcp_live::McpLiveCallResult::Failed { message } => Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                ToolOutcomeStatus::Error,
                String::new(),
                None,
                Some(message),
            ),
            crate::mcp_live::McpLiveCallResult::Unknown { message } => Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                ToolOutcomeStatus::Error,
                String::new(),
                None,
                Some(message),
            ),
        }
    }

    fn normalize_tool_call(
        &self,
        tool_call: &crate::ToolCall,
    ) -> Result<Action, OrchestratorError> {
        if crate::canonical_tool_name(&tool_call.name).is_none() {
            return Err(OrchestratorError::ToolNotFound(tool_call.name.clone()));
        }
        // Schema gate first: malformed args never reach policy / sandbox / exec.
        validate_tool_arguments(&tool_call.name, &tool_call.arguments)?;

        // Map tool names to ActionKind
        let kind = match tool_call.name.as_str() {
            "list_files" | "read_file" | "search" | "goto_definition" => ActionKind::ReadFile,
            "web_search" => ActionKind::WebSearch,
            "web_fetch" => ActionKind::WebFetch,
            "web_download" => ActionKind::WebDownload,
            "web_browser" => ActionKind::WebBrowser,
            "web_submit" => ActionKind::WebSubmit,
            "web_upload" => ActionKind::WebUpload,
            "write_file" | "edit_file" => ActionKind::WriteFile,
            "bash" | "shell" | "exec" => ActionKind::SpawnProcess,
            name => {
                return Err(OrchestratorError::ToolNotFound(name.to_string()));
            }
        };

        let target = tool_call
            .arguments
            .get("path")
            .or_else(|| tool_call.arguments.get("command"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let target = if kind == ActionKind::SpawnProcess {
            Some(self.workspace_root.display().to_string())
        } else if tool_call.name == "web_search" {
            Some("web-search:auto".into())
        } else if matches!(
            tool_call.name.as_str(),
            "web_fetch" | "web_download" | "web_browser" | "web_submit" | "web_upload"
        ) {
            tool_call
                .arguments
                .get("url")
                .and_then(serde_json::Value::as_str)
                .and_then(|url| reqwest::Url::parse(url).ok())
                .and_then(|url| url.host_str().map(str::to_owned))
        } else {
            target
        };

        Ok(Action {
            origin: ActionOrigin::Agent,
            kind,
            summary: format!("{} via agent", tool_call.name),
            target,
        })
    }

    fn execute_read_only(
        &self,
        runtime: &AgentRuntime,
        tool_call: &crate::ToolCall,
    ) -> Result<ToolOutcome, OrchestratorError> {
        let target = tool_call
            .arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(".");
        let tool = match tool_call.name.as_str() {
            "list_files" => ReadOnlyTool::List {
                target: target.into(),
            },
            "read_file" => ReadOnlyTool::Read {
                target: target.into(),
            },
            "search" => ReadOnlyTool::Search {
                target: target.into(),
                pattern: tool_call
                    .arguments
                    .get("pattern")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .into(),
            },
            name => return Err(OrchestratorError::ToolNotFound(name.into())),
        };
        let artifacts = DurableArtifactStore::open(&self.artifact_root).map_err(|error| {
            OrchestratorError::ToolFailed {
                tool: tool_call.name.clone(),
                reason: error.to_string(),
            }
        })?;
        let seam = Self::session_effect_seam(runtime)?;
        ReadOnlyTools::new(&self.workspace_root)
            .run_with_seam(tool, ActionOrigin::Agent, &artifacts, &seam)
            .map_err(|error| OrchestratorError::ToolFailed {
                tool: tool_call.name.clone(),
                reason: error.to_string(),
            })
    }

    async fn execute_goto_definition(
        &self,
        runtime: &Arc<AgentRuntime>,
        tool_call: crate::ToolCall,
        arguments_summary: String,
    ) -> ToolObservation {
        let path = tool_call
            .arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(".")
            .to_string();
        let line = tool_call
            .arguments
            .get("line")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32;
        let character = tool_call
            .arguments
            .get("character")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32;

        let effect = crate::NormalizedEffect::workspace_read(
            ActionOrigin::Agent,
            "goto_definition via agent",
            path.clone(),
        );
        let seam = match Self::session_effect_seam(runtime) {
            Ok(seam) => seam,
            Err(error) => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Error,
                    String::new(),
                    None,
                    Some(error.to_string()),
                );
            }
        };
        match seam.decide(&effect) {
            crate::EffectDecision::Allow => {}
            crate::EffectDecision::NeedsApproval { reason } => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::ApprovalRequired,
                    String::new(),
                    None,
                    Some(reason),
                );
            }
            crate::EffectDecision::Deny { reason } => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Denied,
                    String::new(),
                    None,
                    Some(reason),
                );
            }
        }

        let query = crate::PositionQuery::new(path, line, character);
        match self.coding_tools.definition(&query).await {
            Ok(locations) => {
                let preview = if locations.is_empty() {
                    "goto_definition: no locations".into()
                } else {
                    locations
                        .iter()
                        .map(|loc| {
                            format!(
                                "{}:{}:{}-{}:{}",
                                loc.path.display(),
                                loc.range.start.line,
                                loc.range.start.character,
                                loc.range.end.line,
                                loc.range.end.character
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Success,
                    preview,
                    None,
                    None,
                )
            }
            Err(error) => Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                ToolOutcomeStatus::Error,
                String::new(),
                None,
                Some(error.to_string()),
            ),
        }
    }

    async fn execute_web_tool(
        &self,
        runtime: &Arc<AgentRuntime>,
        tool_call: crate::ToolCall,
        arguments_summary: String,
    ) -> ToolObservation {
        let action = match self.normalize_tool_call(&tool_call) {
            Ok(action) => action,
            Err(error) => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Error,
                    String::new(),
                    None,
                    Some(error.to_string()),
                );
            }
        };
        let effect = crate::NormalizedEffect::network_connect(
            ActionOrigin::Agent,
            action.summary,
            action.target.unwrap_or_else(|| "web:invalid-target".into()),
        );
        let seam = match runtime.effect_seam() {
            Ok(seam) => seam,
            Err(error) => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Error,
                    String::new(),
                    None,
                    Some(error.to_string()),
                );
            }
        };
        match seam.decide(&effect) {
            crate::EffectDecision::Allow => {}
            crate::EffectDecision::NeedsApproval { reason } => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::ApprovalRequired,
                    String::new(),
                    None,
                    Some(reason),
                );
            }
            crate::EffectDecision::Deny { reason } => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Denied,
                    String::new(),
                    None,
                    Some(reason),
                );
            }
        }
        let Some(service) = &self.web_research else {
            return Self::record_observation(
                runtime,
                tool_call,
                arguments_summary,
                ToolOutcomeStatus::Error,
                String::new(),
                None,
                Some("web research service is unavailable".into()),
            );
        };
        let artifacts = match DurableArtifactStore::open(&self.artifact_root) {
            Ok(artifacts) => artifacts,
            Err(error) => {
                return Self::record_observation(
                    runtime,
                    tool_call,
                    arguments_summary,
                    ToolOutcomeStatus::Error,
                    String::new(),
                    None,
                    Some(format!("cannot open artifact store: {error}")),
                );
            }
        };
        let observation =
            crate::web_research::execute_web_tool(service.as_ref(), &tool_call, &artifacts)
                .await
                .expect("web tool names are checked before dispatch");
        Self::record_observation(
            runtime,
            tool_call,
            observation.arguments_summary,
            observation.outcome,
            observation.preview,
            observation.artifact,
            observation.error,
        )
    }

    fn record_observation(
        runtime: &Arc<AgentRuntime>,
        tool_call: crate::ToolCall,
        arguments_summary: String,
        outcome: ToolOutcomeStatus,
        preview: String,
        artifact: Option<crate::DurableArtifactRef>,
        error: Option<String>,
    ) -> ToolObservation {
        let _ = runtime.record_tool_started(&tool_call.name, Some(&tool_call.id));
        if !preview.is_empty() {
            let _ = runtime.record_tool_output(&tool_call.id, &tool_call.name, &preview);
        }
        let _ = runtime.record_event(crate::EventPayload::Tool(ToolEvent::Observed {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            arguments_summary: arguments_summary.clone(),
            outcome: outcome.clone(),
            preview: preview.clone(),
            artifact: artifact.clone(),
            error: error.clone(),
        }));
        let finish_summary = error
            .as_deref()
            .filter(|e| !e.is_empty())
            .map(|e| e.to_owned())
            .unwrap_or_else(|| format!("{outcome:?}"));
        let _ = runtime.record_tool_finished(&tool_call.name, &finish_summary, Some(&tool_call.id));
        ToolObservation {
            tool_call_id: tool_call.id,
            tool_name: tool_call.name,
            arguments_summary,
            outcome,
            preview,
            artifact,
            error,
        }
    }

    pub fn record_approval_rejection(
        runtime: &Arc<AgentRuntime>,
        deferred: (String, String, serde_json::Value),
    ) -> ToolObservation {
        let (tool_call_id, tool_name, arguments) = deferred;
        Self::record_observation(
            runtime,
            crate::ToolCall {
                id: tool_call_id,
                name: tool_name,
                arguments: arguments.clone(),
            },
            summarize_arguments(&arguments),
            ToolOutcomeStatus::Denied,
            String::new(),
            None,
            Some("user rejected approval".into()),
        )
    }

    pub fn execute_approved_write(
        runtime: &Arc<AgentRuntime>,
        request: crate::ApprovalRequest,
        resolution: crate::ApprovalResolution,
        deferred: (String, String, serde_json::Value),
    ) -> Result<ToolObservation, OrchestratorError> {
        let (tool_call_id, tool_name, arguments) = deferred;
        if !matches!(tool_name.as_str(), "write_file" | "edit_file") {
            return Err(OrchestratorError::ToolNotFound(tool_name));
        }
        validate_tool_arguments(&tool_name, &arguments)?;
        let path = arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| OrchestratorError::ToolFailed {
                tool: tool_name.clone(),
                reason: "write tool requires path".into(),
            })?;
        let content = arguments
            .get("content")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| OrchestratorError::ToolFailed {
                tool: tool_name.clone(),
                reason: "write tool requires content".into(),
            })?;
        let effect = crate::NormalizedEffect::workspace_write(
            ActionOrigin::Agent,
            format!("{tool_name} via agent"),
            path,
        );
        if effect.action != request.action {
            return Err(OrchestratorError::Denied(
                "deferred action no longer matches approval".into(),
            ));
        }
        let workspace = runtime.workspace_root()?;
        let seam = Self::session_effect_seam(runtime)?;
        let execution = seam
            .execute_after_approval(
                crate::DeferredEffect::from_durable(effect, request.clone()),
                resolution,
                request.intent_revision,
                || {
                    crate::tools::write_file_in_scope(
                        &workspace,
                        PathBuf::from(path).as_path(),
                        content,
                    )
                },
            )
            .map_err(|error| OrchestratorError::ToolFailed {
                tool: tool_name.clone(),
                reason: error.to_string(),
            })?;
        let (outcome, preview, error) = match execution {
            crate::EffectExecution::Executed(()) => {
                (ToolOutcomeStatus::Success, "file written".into(), None)
            }
            crate::EffectExecution::Denied { reason } => {
                (ToolOutcomeStatus::Denied, String::new(), Some(reason))
            }
            crate::EffectExecution::NeedsApproval { reason } => (
                ToolOutcomeStatus::ApprovalRequired,
                String::new(),
                Some(reason),
            ),
        };
        Ok(Self::record_observation(
            runtime,
            crate::ToolCall {
                id: tool_call_id,
                name: tool_name,
                arguments: arguments.clone(),
            },
            summarize_arguments(&arguments),
            outcome,
            preview,
            None,
            error,
        ))
    }

    pub fn execute_approved_bash(
        runtime: &Arc<AgentRuntime>,
        request: crate::ApprovalRequest,
        resolution: crate::ApprovalResolution,
        deferred: (String, String, serde_json::Value),
    ) -> Result<ToolObservation, OrchestratorError> {
        Self::execute_approved_bash_with_artifacts(
            runtime,
            request,
            resolution,
            deferred,
            &crate::default_artifact_root(),
            &HookPrefilter::default(),
        )
    }

    /// Same as [`Self::execute_approved_bash`], but writes large stdout/stderr
    /// bodies into the given artifact root (tests and harness share this path).
    pub fn execute_approved_bash_with_artifacts(
        runtime: &Arc<AgentRuntime>,
        request: crate::ApprovalRequest,
        resolution: crate::ApprovalResolution,
        deferred: (String, String, serde_json::Value),
        artifact_root: &std::path::Path,
        hook_prefilter: &HookPrefilter,
    ) -> Result<ToolObservation, OrchestratorError> {
        let (tool_call_id, tool_name, arguments) = deferred;
        if !matches!(tool_name.as_str(), "bash" | "shell" | "exec") {
            return Err(OrchestratorError::ToolNotFound(tool_name));
        }
        validate_tool_arguments(&tool_name, &arguments)?;
        let command = arguments
            .get("command")
            .and_then(serde_json::Value::as_str)
            .filter(|command| !command.trim().is_empty())
            .ok_or_else(|| OrchestratorError::ToolFailed {
                tool: tool_name.clone(),
                reason: "shell tool requires command".into(),
            })?;
        let workspace = runtime.workspace_root()?;
        let effect = crate::NormalizedEffect::process_spawn(
            ActionOrigin::Agent,
            format!("{tool_name} via agent"),
            workspace.display().to_string(),
        );
        if effect.action != request.action {
            return Err(OrchestratorError::Denied(
                "deferred action no longer matches approval".into(),
            ));
        }
        let artifacts = DurableArtifactStore::open(artifact_root).map_err(|error| {
            OrchestratorError::ToolFailed {
                tool: tool_name.clone(),
                reason: error.to_string(),
            }
        })?;
        let process = crate::ProcessExecutionRequest::new(
            "/bin/sh",
            vec!["-lc".into(), command.into()],
            ActionOrigin::Agent,
            request.intent_revision,
        )
        .with_working_dir(workspace.clone())
        .with_workspace_root(workspace.clone())
        .with_allow_network(runtime.policy().scope().allow_network)
        .with_hook_prefilter(hook_prefilter.clone());
        let _ = runtime.record_event(crate::EventPayload::Command(crate::CommandEvent::Started {
            tool_call_id: tool_call_id.clone(),
            command: command.to_owned(),
        }));
        let seam = Self::session_effect_seam(runtime)?;
        let execution = seam
            .execute_after_approval_with_admission(
                crate::DeferredEffect::from_durable(effect, request.clone()),
                resolution,
                request.intent_revision,
                |admission| {
                    std::thread::scope(|scope| {
                        scope
                            .spawn(|| {
                                tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                    .map_err(crate::ProcessExecutionError::Io)?
                                    .block_on(process.execute(admission, &artifacts))
                            })
                            .join()
                            .map_err(|_| {
                                crate::ProcessExecutionError::ExecutionFailed(
                                    "process worker panicked".into(),
                                )
                            })
                    })?
                },
            )
            .map_err(|error| OrchestratorError::ToolFailed {
                tool: tool_name.clone(),
                reason: error.to_string(),
            })?;
        let (outcome, preview, artifact, error) = match execution {
            crate::EffectExecution::Executed(output) => {
                if let Some(decision) = &output.sandbox_decision {
                    let _ = runtime.record_event(crate::EventPayload::Sandbox(
                        crate::SandboxEvent::from_decision(decision),
                    ));
                }
                let preview = crate::tools::redact_text(&format!(
                    "exit_code={:?}\nstdout:\n{}\nstderr:\n{}",
                    output.exit_code, output.stdout, output.stderr
                ));
                let bounded = crate::bound_activity_preview(&preview);
                let _ = runtime.record_event(crate::EventPayload::Command(
                    crate::CommandEvent::Output {
                        tool_call_id: tool_call_id.clone(),
                        preview: bounded.clone(),
                    },
                ));
                let _ = runtime.record_event(crate::EventPayload::Command(
                    crate::CommandEvent::Finished {
                        tool_call_id: tool_call_id.clone(),
                        exit_code: output.exit_code,
                        summary: Some(format!("exit {:?}", output.exit_code)),
                    },
                ));
                (ToolOutcomeStatus::Success, preview, output.artifact, None)
            }
            crate::EffectExecution::Denied { reason } => {
                let _ = runtime.record_event(crate::EventPayload::Command(
                    crate::CommandEvent::Finished {
                        tool_call_id: tool_call_id.clone(),
                        exit_code: None,
                        summary: Some(reason.clone()),
                    },
                ));
                (ToolOutcomeStatus::Denied, String::new(), None, Some(reason))
            }
            crate::EffectExecution::NeedsApproval { reason } => {
                let _ = runtime.record_event(crate::EventPayload::Command(
                    crate::CommandEvent::Finished {
                        tool_call_id: tool_call_id.clone(),
                        exit_code: None,
                        summary: Some(reason.clone()),
                    },
                ));
                (
                    ToolOutcomeStatus::ApprovalRequired,
                    String::new(),
                    None,
                    Some(reason),
                )
            }
        };
        Ok(Self::record_observation(
            runtime,
            crate::ToolCall {
                id: tool_call_id,
                name: tool_name,
                arguments: arguments.clone(),
            },
            summarize_arguments(&arguments),
            outcome,
            preview,
            artifact,
            error,
        ))
    }
}

fn child_canonical_tool_name(name: &str) -> Option<&'static str> {
    match name.trim() {
        "list" | "list_files" => Some("list_files"),
        "read" | "read_file" => Some("read_file"),
        "search" => Some("search"),
        "write" | "write_file" | "edit_file" => Some("write_file"),
        "web_search" => Some("web_search"),
        "web_fetch" => Some("web_fetch"),
        _ => None,
    }
}

/// Allow Explore/role labels and mapped provider/schema names interchangeably.
///
/// Role label `web` matches read-oriented web tools (`web_search`, `web_fetch`).
fn child_tool_allowed(allowed: &[String], called: &str) -> bool {
    let Some(called_canonical) = child_canonical_tool_name(called) else {
        return false;
    };
    allowed.iter().any(|entry| match entry.trim() {
        "web" => matches!(called_canonical, "web_search" | "web_fetch"),
        other => child_canonical_tool_name(other).is_some_and(|name| name == called_canonical),
    })
}

fn summarize_arguments(arguments: &serde_json::Value) -> String {
    crate::tools::redact_text(&serde_json::to_string(arguments).unwrap_or_default())
        .chars()
        .take(1024)
        .collect()
}

fn contains_sensitive_value(arguments: &serde_json::Value) -> bool {
    let raw = serde_json::to_string(arguments).unwrap_or_default();
    crate::tools::redact_text(&raw) != raw
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryEventStore, SandboxScope};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingWebService(AtomicUsize);

    #[async_trait]
    impl crate::web_research::WebSearchService for CountingWebService {
        async fn search(
            &self,
            _request: crate::web_research::SearchRequest,
        ) -> Result<crate::web_research::SearchResponse, crate::web_research::WebError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            unreachable!("network-disabled policy must stop web search before execution")
        }
    }

    #[async_trait]
    impl crate::web_research::WebFetchService for CountingWebService {
        async fn fetch(
            &self,
            _request: crate::web_research::FetchRequest,
        ) -> Result<crate::web_research::FetchedPage, crate::web_research::WebError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            unreachable!("network-disabled policy must stop web fetch before execution")
        }
    }

    #[tokio::test]
    async fn web_search_is_denied_before_service_execution_when_network_is_disabled() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            policy.clone(),
        ));
        let service = Arc::new(CountingWebService(AtomicUsize::new(0)));
        let orchestrator = ToolOrchestrator::new(policy, workspace.path().to_path_buf())
            .with_web_research(service.clone());

        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "web-call".into(),
                    name: "web_search".into(),
                    arguments: serde_json::json!({"query": "private research"}),
                }],
                &runtime,
            )
            .await
            .expect("tool orchestration");

        assert_eq!(observations[0].outcome, ToolOutcomeStatus::Denied);
        assert_eq!(service.0.load(Ordering::SeqCst), 0);
        assert!(runtime.events().expect("events").iter().any(|event| {
            matches!(
                &event.payload,
                crate::EventPayload::Tool(crate::ToolEvent::Observed {
                    tool_name,
                    outcome: ToolOutcomeStatus::Denied,
                    ..
                }) if tool_name == "web_search"
            )
        }));
    }

    #[test]
    fn normalize_read_file_tool() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let orchestrator = ToolOrchestrator::new(policy, PathBuf::from("."));

        let tool_call = crate::ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "test.txt"}),
        };

        let action = orchestrator.normalize_tool_call(&tool_call).unwrap();
        assert_eq!(action.kind, ActionKind::ReadFile);
        assert_eq!(action.target, Some("test.txt".to_string()));
    }

    #[test]
    fn normalize_web_tools_use_fine_grained_action_kinds() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let orchestrator = ToolOrchestrator::new(policy, PathBuf::from("."));

        let search = orchestrator
            .normalize_tool_call(&crate::ToolCall {
                id: "s".into(),
                name: "web_search".into(),
                arguments: serde_json::json!({"query": "rust"}),
            })
            .unwrap();
        assert_eq!(search.kind, ActionKind::WebSearch);
        assert_eq!(search.target.as_deref(), Some("web-search:auto"));

        let fetch = orchestrator
            .normalize_tool_call(&crate::ToolCall {
                id: "f".into(),
                name: "web_fetch".into(),
                arguments: serde_json::json!({"url": "https://example.com/page"}),
            })
            .unwrap();
        assert_eq!(fetch.kind, ActionKind::WebFetch);
        assert_eq!(fetch.target.as_deref(), Some("example.com"));

        let submit = orchestrator
            .normalize_tool_call(&crate::ToolCall {
                id: "u".into(),
                name: "web_submit".into(),
                arguments: serde_json::json!({"url": "https://example.com/form"}),
            })
            .unwrap();
        assert_eq!(submit.kind, ActionKind::WebSubmit);
    }

    #[tokio::test]
    async fn web_submit_requires_approval_when_network_allowed_without_outbound() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let mut scope = SandboxScope::local_workspace(workspace.path());
        scope.allow_network = true;
        let policy = PolicyEngine::new(scope);
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            policy.clone(),
        ));
        runtime.submit_intent("submit form").expect("intent");
        let orchestrator = ToolOrchestrator::new(policy, workspace.path().to_path_buf());

        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "submit-1".into(),
                    name: "web_submit".into(),
                    arguments: serde_json::json!({"url": "https://example.com/form"}),
                }],
                &runtime,
            )
            .await
            .expect("tool orchestration");

        assert_eq!(observations[0].outcome, ToolOutcomeStatus::ApprovalRequired);
        assert!(runtime.events().expect("events").iter().any(|event| {
            matches!(
                &event.payload,
                crate::EventPayload::Approval(crate::ApprovalEvent::Requested { request })
                    if request.action.kind == ActionKind::WebSubmit
                        && !request.reason.contains("sk-")
            )
        }));
    }

    #[tokio::test]
    async fn web_fetch_private_lan_denied_without_private_network_grant() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let mut scope = SandboxScope::local_workspace(workspace.path());
        scope.allow_network = true;
        let policy = PolicyEngine::new(scope);
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            policy.clone(),
        ));
        runtime.submit_intent("fetch lan").expect("intent");
        let orchestrator = ToolOrchestrator::new(policy, workspace.path().to_path_buf());

        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "fetch-lan".into(),
                    name: "web_fetch".into(),
                    arguments: serde_json::json!({"url": "http://10.0.0.5/status"}),
                }],
                &runtime,
            )
            .await
            .expect("tool orchestration");

        assert_eq!(observations[0].outcome, ToolOutcomeStatus::Denied);
        let err = observations[0].error.as_deref().unwrap_or_default();
        assert!(err.contains("private/LAN"), "{err}");
        assert!(!err.contains("sk-"));
    }

    #[test]
    fn private_lan_fetch_normalizes_and_allows_when_private_network_granted() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let policy = PolicyEngine::new(
            SandboxScope::local_workspace(workspace.path())
                .with_network(true)
                .with_private_network(true),
        );
        let orchestrator = ToolOrchestrator::new(policy.clone(), workspace.path().to_path_buf());
        let action = orchestrator
            .normalize_tool_call(&crate::ToolCall {
                id: "f".into(),
                name: "web_fetch".into(),
                arguments: serde_json::json!({"url": "http://10.0.0.5/status"}),
            })
            .expect("normalize");
        assert_eq!(action.kind, ActionKind::WebFetch);
        assert_eq!(action.target.as_deref(), Some("10.0.0.5"));
        assert_eq!(policy.evaluate(&action), crate::PolicyDecision::Allow);
        assert!(policy.egress_policy().allow_private_network);
    }

    #[test]
    fn normalize_unknown_tool_fails() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let orchestrator = ToolOrchestrator::new(policy, PathBuf::from("."));

        let tool_call = crate::ToolCall {
            id: "call_1".to_string(),
            name: "unknown_tool".to_string(),
            arguments: serde_json::json!({}),
        };

        let result = orchestrator.normalize_tool_call(&tool_call);
        assert!(matches!(result, Err(OrchestratorError::ToolNotFound(_))));
    }

    #[tokio::test]
    async fn process_tool_calls_returns_observations() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        std::fs::write(
            workspace.path().join("evidence.txt"),
            "secret=hidden\nproof",
        )
        .expect("write fixture");
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
        let orchestrator = ToolOrchestrator::new(policy, workspace.path().to_path_buf());

        let tool_calls = vec![crate::ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "evidence.txt"}),
        }];

        let observations = orchestrator
            .process_tool_calls(Uuid::new_v4(), tool_calls, &runtime)
            .await
            .unwrap();

        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].outcome, ToolOutcomeStatus::Success);
        assert!(observations[0].preview.contains("[REDACTED]"));
        assert!(runtime.events().unwrap().iter().any(|event| {
            matches!(
                &event.payload,
                crate::EventPayload::Tool(crate::ToolEvent::Observed {
                    tool_call_id,
                    outcome: ToolOutcomeStatus::Success,
                    ..
                }) if tool_call_id == "call_1"
            )
        }));
        assert!(
            runtime.events().unwrap().iter().any(|event| {
                matches!(
                    &event.payload,
                    crate::EventPayload::Tool(crate::ToolEvent::FileRead {
                        tool_call_id,
                        path,
                        ..
                    }) if tool_call_id == "call_1" && path == "evidence.txt"
                )
            }),
            "read_file must emit typed FileRead on durable session log"
        );
    }

    #[tokio::test]
    async fn write_is_durable_deferred_until_approval() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
        runtime.submit_intent("write a note").expect("intent");
        let orchestrator = ToolOrchestrator::new(policy, workspace.path().to_path_buf());
        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "write-1".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path": "note.txt", "content": "approved"}),
                }],
                &runtime,
            )
            .await
            .expect("tool call");
        assert_eq!(observations[0].outcome, ToolOutcomeStatus::ApprovalRequired);
        assert!(!workspace.path().join("note.txt").exists());
        assert!(runtime.events().unwrap().iter().any(|event| {
            matches!(
                &event.payload,
                crate::EventPayload::Tool(crate::ToolEvent::Deferred {
                    tool_call_id,
                    tool_name,
                    ..
                }) if tool_call_id == "write-1" && tool_name == "write_file"
            )
        }));
        let request = runtime
            .events()
            .unwrap()
            .iter()
            .find_map(|event| match &event.payload {
                crate::EventPayload::Approval(crate::ApprovalEvent::Requested { request }) => {
                    Some(request.clone())
                }
                _ => None,
            })
            .expect("approval request");
        let deferred = runtime
            .deferred_tool(request.id)
            .expect("deferred lookup")
            .expect("deferred tool");
        let resolution = crate::ApprovalResolution::user(&request, true);
        runtime
            .resolve_approval(resolution.clone())
            .expect("resolve approval");
        let observation =
            ToolOrchestrator::execute_approved_write(&runtime, request, resolution, deferred)
                .expect("execute approved write");
        assert_eq!(observation.outcome, ToolOutcomeStatus::Success);
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("note.txt")).expect("written file"),
            "approved"
        );
    }

    #[tokio::test]
    async fn bash_executes_only_after_exact_user_approval() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        ));
        runtime.submit_intent("inspect workspace").expect("intent");
        let orchestrator = ToolOrchestrator::new(runtime.policy(), workspace.path().to_path_buf());
        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "bash-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "printf verified"}),
                }],
                &runtime,
            )
            .await
            .expect("request bash");
        assert_eq!(observations[0].outcome, ToolOutcomeStatus::ApprovalRequired);
        let request = runtime
            .events()
            .unwrap()
            .iter()
            .find_map(|event| match &event.payload {
                crate::EventPayload::Approval(crate::ApprovalEvent::Requested { request }) => {
                    Some(request.clone())
                }
                _ => None,
            })
            .expect("approval request");
        let deferred = runtime
            .deferred_tool(request.id)
            .expect("deferred lookup")
            .expect("deferred bash");
        let resolution = crate::ApprovalResolution::user(&request, true);
        runtime
            .resolve_approval(resolution.clone())
            .expect("approval");
        let observation =
            ToolOrchestrator::execute_approved_bash(&runtime, request, resolution, deferred)
                .expect("approved shell execution");
        assert_eq!(observation.outcome, ToolOutcomeStatus::Success);
        assert!(observation.preview.contains("verified"));
    }

    #[tokio::test]
    async fn large_bash_stdout_artifact_survives_store_reopen() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let artifact_root = tempfile::tempdir().expect("artifact root");
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        ));
        runtime.submit_intent("inspect workspace").expect("intent");
        let orchestrator = ToolOrchestrator::with_artifact_root(
            runtime.policy(),
            workspace.path().to_path_buf(),
            artifact_root.path().to_path_buf(),
        );
        let payload = "y".repeat(crate::MAX_PROCESS_PREVIEW_BYTES + 2048);
        let byte_count = payload.len();
        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "bash-large".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({
                        "command": format!("yes y | tr -d '\\n' | head -c {byte_count}")
                    }),
                }],
                &runtime,
            )
            .await
            .expect("request bash");
        assert_eq!(observations[0].outcome, ToolOutcomeStatus::ApprovalRequired);
        let request = runtime
            .events()
            .unwrap()
            .iter()
            .find_map(|event| match &event.payload {
                crate::EventPayload::Approval(crate::ApprovalEvent::Requested { request }) => {
                    Some(request.clone())
                }
                _ => None,
            })
            .expect("approval request");
        let deferred = runtime
            .deferred_tool(request.id)
            .expect("deferred lookup")
            .expect("deferred bash");
        let resolution = crate::ApprovalResolution::user(&request, true);
        runtime
            .resolve_approval(resolution.clone())
            .expect("approval");
        let observation = ToolOrchestrator::execute_approved_bash_with_artifacts(
            &runtime,
            request,
            resolution,
            deferred,
            artifact_root.path(),
            &crate::HookPrefilter::default(),
        )
        .expect("approved large shell");
        assert_eq!(observation.outcome, ToolOutcomeStatus::Success);
        let artifact = observation.artifact.expect("large bash artifact");
        assert!(
            observation.preview.len() < payload.len(),
            "durable event must keep bounded preview"
        );
        let reopened = DurableArtifactStore::open(artifact_root.path()).expect("reopen");
        let full = String::from_utf8(reopened.read(&artifact.id).expect("read")).expect("utf8");
        assert!(full.contains(&payload));
        assert!(
            runtime.events().expect("events").iter().any(|event| {
                matches!(
                    &event.payload,
                    crate::EventPayload::Tool(crate::ToolEvent::Observed {
                        artifact: Some(stored),
                        ..
                    }) if stored.id == artifact.id
                )
            }),
            "durable tool event must retain ArtifactRef"
        );
    }

    #[tokio::test]
    async fn large_read_artifact_survives_store_reopen() {
        let workspace = tempfile::tempdir().expect("workspace");
        let artifact_root = tempfile::tempdir().expect("artifact root");
        let content = "durable evidence\n".repeat(2_000);
        std::fs::write(workspace.path().join("large.txt"), &content).expect("fixture");
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
        ));
        let orchestrator = ToolOrchestrator::with_artifact_root(
            runtime.policy(),
            workspace.path().to_path_buf(),
            artifact_root.path().to_path_buf(),
        );

        let observation = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "read-large".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "large.txt"}),
                }],
                &runtime,
            )
            .await
            .expect("read tool")
            .pop()
            .expect("observation");
        let artifact = observation.artifact.expect("large output artifact");
        let reopened = DurableArtifactStore::open(artifact_root.path()).expect("reopen store");
        assert_eq!(
            reopened.read(&artifact.id).expect("artifact bytes"),
            content.as_bytes()
        );
    }

    #[test]
    fn normalize_rejects_missing_required_args() {
        let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
        let orchestrator = ToolOrchestrator::new(policy, PathBuf::from("."));
        let result = orchestrator.normalize_tool_call(&crate::ToolCall {
            id: "bad".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        });
        assert!(matches!(
            result,
            Err(OrchestratorError::InvalidArguments(_))
        ));
    }

    #[tokio::test]
    async fn invalid_args_never_reach_policy_or_sandbox() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let marker = workspace.path().join("must-not-exist.txt");
        let store = Arc::new(MemoryEventStore::default());
        // Policy would allow writes after approval; schema must stop us first.
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
        runtime.submit_intent("write").expect("intent");
        let orchestrator = ToolOrchestrator::new(policy, workspace.path().to_path_buf());

        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "bad-write".into(),
                    name: "write_file".into(),
                    // Missing required `content` — must not defer/approve/exec.
                    arguments: serde_json::json!({"path": "must-not-exist.txt"}),
                }],
                &runtime,
            )
            .await
            .expect("batch ok");

        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].outcome, ToolOutcomeStatus::Error);
        let error = observations[0].error.as_deref().unwrap_or_default();
        assert!(
            error.contains("missing required property `content`"),
            "error={error}"
        );
        assert!(!marker.exists(), "executor must not run");
        assert!(
            !runtime.events().unwrap().iter().any(|event| {
                matches!(
                    &event.payload,
                    crate::EventPayload::Approval(_)
                        | crate::EventPayload::Tool(crate::ToolEvent::Deferred { .. })
                )
            }),
            "invalid args must not create approval/deferred tool"
        );
    }

    #[tokio::test]
    async fn valid_args_still_reach_policy_allow_path() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        std::fs::write(workspace.path().join("ok.txt"), "hello").expect("fixture");
        let store = Arc::new(MemoryEventStore::default());
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));
        let orchestrator = ToolOrchestrator::new(policy, workspace.path().to_path_buf());

        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "ok-read".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "ok.txt"}),
                }],
                &runtime,
            )
            .await
            .expect("batch ok");

        assert_eq!(observations[0].outcome, ToolOutcomeStatus::Success);
        assert!(observations[0].preview.contains("hello"));
    }

    struct ScriptedMcpCaller {
        result: crate::mcp_live::McpLiveCallResult,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl crate::mcp_live::McpLiveCaller for ScriptedMcpCaller {
        async fn call_tool(
            &self,
            _tool: &str,
            _arguments: serde_json::Value,
        ) -> crate::mcp_live::McpLiveCallResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn live_mcp_readonly_echo_goes_through_policy_sandbox_execution() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            policy.clone(),
        ));
        let tool = crate::mcp_adapter::McpTool {
            name: "echo".into(),
            description: "Echo".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"]
            }),
            annotations: Some(serde_json::json!({"readOnlyHint": true})),
        };
        let caller = Arc::new(ScriptedMcpCaller {
            result: crate::mcp_live::McpLiveCallResult::Ok {
                preview: "echo:live".into(),
            },
            calls: AtomicUsize::new(0),
        });
        let bridge = Arc::new(crate::mcp_live::McpLiveBridge::from_tools(
            "mock",
            vec![tool],
            caller.clone(),
        ));
        let orchestrator =
            ToolOrchestrator::new(policy, workspace.path().to_path_buf()).with_mcp_live(bridge);

        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "mcp-1".into(),
                    name: "mcp:mock:echo".into(),
                    arguments: serde_json::json!({"text": "live"}),
                }],
                &runtime,
            )
            .await
            .expect("mcp batch");

        assert_eq!(observations[0].outcome, ToolOutcomeStatus::Success);
        assert_eq!(observations[0].preview, "echo:live");
        assert_eq!(caller.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn live_mcp_mutating_requires_approval_before_call() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            policy.clone(),
        ));
        let tool = crate::mcp_adapter::McpTool {
            name: "sum".into(),
            description: "Sum".into(),
            input_schema: serde_json::json!({"type": "object"}),
            annotations: None,
        };
        let caller = Arc::new(ScriptedMcpCaller {
            result: crate::mcp_live::McpLiveCallResult::Ok {
                preview: "3".into(),
            },
            calls: AtomicUsize::new(0),
        });
        let bridge = Arc::new(crate::mcp_live::McpLiveBridge::from_tools(
            "mock",
            vec![tool],
            caller.clone(),
        ));
        let orchestrator =
            ToolOrchestrator::new(policy, workspace.path().to_path_buf()).with_mcp_live(bridge);

        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "mcp-mut".into(),
                    name: "mcp:mock:sum".into(),
                    arguments: serde_json::json!({"a": 1, "b": 2}),
                }],
                &runtime,
            )
            .await
            .expect("mcp batch");

        assert_eq!(observations[0].outcome, ToolOutcomeStatus::ApprovalRequired);
        assert_eq!(caller.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn live_mcp_end_to_end_discover_and_call_via_mock_server() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            policy.clone(),
        ));
        let (adapter, server_task) = crate::mcp_adapter::tests::duplex_adapter().await;
        let bridge = Arc::new(
            crate::mcp_live::McpLiveBridge::discover("mock", adapter)
                .await
                .expect("discover"),
        );
        assert!(bridge.contains("mcp:mock:echo"));
        let orchestrator =
            ToolOrchestrator::new(policy, workspace.path().to_path_buf()).with_mcp_live(bridge);

        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "mcp-e2e".into(),
                    name: "mcp:mock:echo".into(),
                    arguments: serde_json::json!({"text": "e2e"}),
                }],
                &runtime,
            )
            .await
            .expect("mcp e2e");

        assert_eq!(observations[0].outcome, ToolOutcomeStatus::Success);
        assert_eq!(observations[0].preview, "echo:e2e");
        server_task.abort();
    }

    #[tokio::test]
    async fn goto_definition_uses_mock_coding_tools_provider() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let src = workspace.path().join("src");
        std::fs::create_dir_all(&src).expect("src dir");
        std::fs::write(src.join("lib.rs"), "fn run() {}\n").expect("fixture");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            policy.clone(),
        ));
        let query = crate::PositionQuery::new("src/lib.rs", 42, 5);
        let location =
            crate::SourceLocation::new("src/lib.rs", crate::SourceRange::new(10, 0, 10, 3));
        let mock =
            Arc::new(crate::MockCodingToolsProvider::new().with_definition(query, vec![location]));
        let service = Arc::new(crate::OptionalCodingToolsService::with_provider(mock));
        let orchestrator = ToolOrchestrator::new(policy, workspace.path().to_path_buf())
            .with_coding_tools(service);

        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "def-1".into(),
                    name: "goto_definition".into(),
                    arguments: serde_json::json!({
                        "path": "src/lib.rs",
                        "line": 42,
                        "character": 5
                    }),
                }],
                &runtime,
            )
            .await
            .expect("definition batch");

        assert_eq!(observations[0].outcome, ToolOutcomeStatus::Success);
        assert!(observations[0].preview.contains("src/lib.rs:10:0-10:3"));
        assert!(observations[0].error.is_none());
    }

    #[tokio::test]
    async fn goto_definition_absent_provider_fail_closed() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let src = workspace.path().join("src");
        std::fs::create_dir_all(&src).expect("src dir");
        std::fs::write(src.join("lib.rs"), "fn run() {}\n").expect("fixture");
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            policy.clone(),
        ));
        let orchestrator = ToolOrchestrator::new(policy, workspace.path().to_path_buf());

        let observations = orchestrator
            .process_tool_calls(
                Uuid::new_v4(),
                vec![crate::ToolCall {
                    id: "def-absent".into(),
                    name: "goto_definition".into(),
                    arguments: serde_json::json!({
                        "path": "src/lib.rs",
                        "line": 0,
                        "character": 0
                    }),
                }],
                &runtime,
            )
            .await
            .expect("definition batch");

        assert_eq!(observations[0].outcome, ToolOutcomeStatus::Error);
        let err = observations[0].error.as_deref().expect("error");
        assert!(err.contains(crate::ABSENT_CODING_TOOLS_REASON));
    }
}
