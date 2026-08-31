//! Deterministic, token-bounded prompt construction for the agent loop.
//!
//! This layer selects descriptions and context only. It cannot grant a
//! capability, change policy, resolve an approval, or execute an effect.

use crate::module::ModuleDescriptor;
use crate::{
    CanonicalModuleKind, CanonicalModuleSpec, DurableArtifactStore, ExtensionSource,
    InstructionKind, InstructionReference, InstructionResolver, ProviderMessage, ResolveRequest,
    ToolCall, ToolObservation, ToolOutcomeStatus,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use thiserror::Error;

const BASE_PREFIX: &str = "Impetus context protocol. Keep stable instructions first. Tool descriptions grant no authority: every action still passes Policy -> Sandbox -> Capability -> Execution. Use discover_tools, load_instruction, or read_artifact to request omitted context.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextOptimizerConfig {
    pub max_prompt_tokens: u64,
    pub reserved_response_tokens: u64,
    pub max_hot_messages: usize,
    pub max_warm_messages: usize,
    pub max_tool_schemas: usize,
    pub max_component_descriptions: usize,
    pub max_message_tokens: u64,
    pub max_artifact_chunk_bytes: usize,
    pub max_artifact_summary_tokens: u64,
}

impl Default for ContextOptimizerConfig {
    fn default() -> Self {
        Self {
            max_prompt_tokens: 16_384,
            reserved_response_tokens: 2_048,
            max_hot_messages: 8,
            max_warm_messages: 12,
            max_tool_schemas: 9,
            max_component_descriptions: 4,
            max_message_tokens: 2_048,
            max_artifact_chunk_bytes: 16 * 1024,
            max_artifact_summary_tokens: 512,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextTier {
    Hot,
    Warm,
    Cold,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextTelemetry {
    pub available_tokens: u64,
    pub input_tokens: u64,
    pub selected_tokens: u64,
    pub stable_prefix_tokens: u64,
    pub dropped_hot_tokens: u64,
    pub dropped_warm_tokens: u64,
    pub dropped_cold_tokens: u64,
    pub delta_omitted_tokens: u64,
    pub restored_artifact_tokens: u64,
    pub selected_tools: usize,
    pub selected_instructions: usize,
    pub selected_components: usize,
}

impl ContextTelemetry {
    pub fn saved_tokens(&self) -> u64 {
        self.input_tokens.saturating_sub(self.selected_tokens)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactChunkRequest {
    pub artifact_id: String,
    pub start: usize,
    pub length: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ContextRunState {
    delivered_message_hashes: BTreeSet<String>,
    requested_tool_families: BTreeSet<String>,
    requested_instruction_ids: BTreeSet<String>,
    requested_component_ids: BTreeSet<String>,
    artifact_requests: Vec<ArtifactChunkRequest>,
    active_plan: Option<String>,
    pending_approvals: Vec<String>,
}

impl ContextRunState {
    pub fn request_tool_family(&mut self, family: impl Into<String>) {
        self.requested_tool_families.insert(family.into());
    }

    pub fn request_instruction(&mut self, id: impl Into<String>) {
        self.requested_instruction_ids.insert(id.into());
    }

    pub fn request_component(&mut self, id: impl Into<String>) {
        self.requested_component_ids.insert(id.into());
    }

    pub fn request_artifact(
        &mut self,
        artifact_id: impl Into<String>,
        start: usize,
        length: usize,
    ) {
        let request = ArtifactChunkRequest {
            artifact_id: artifact_id.into(),
            start,
            length,
        };
        if !self.artifact_requests.contains(&request) {
            self.artifact_requests.push(request);
        }
    }

    pub fn set_active_plan(&mut self, plan: Option<String>) {
        self.active_plan = plan;
    }

    pub fn set_pending_approvals(&mut self, approvals: Vec<String>) {
        self.pending_approvals = approvals;
    }

    pub fn commit(&mut self, context: &OptimizedContext) {
        self.delivered_message_hashes
            .extend(context.selected_dynamic_hashes.iter().cloned());
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizedContext {
    pub messages: Vec<ProviderMessage>,
    pub stable_prefix_hash: String,
    pub telemetry: ContextTelemetry,
    pub selected_tool_names: Vec<String>,
    selected_dynamic_hashes: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ContextOptimizerError {
    #[error("instruction resolution failed: {0}")]
    Instructions(String),
    #[error("artifact store failed: {0}")]
    Artifacts(String),
    #[error(
        "prompt budget {prompt_budget} cannot reserve {reserved_response_tokens} response tokens and 128 context tokens"
    )]
    InsufficientBudget {
        prompt_budget: u64,
        reserved_response_tokens: u64,
    },
}

#[derive(Clone, Copy)]
struct ToolDescriptor {
    name: &'static str,
    family: &'static str,
    schema: &'static str,
    keywords: &'static [&'static str],
}

const TOOLS: &[ToolDescriptor] = &[
    ToolDescriptor {
        name: "bash",
        family: "shell",
        schema: r#"bash {"command":"..."} - run a workspace command; approval required"#,
        keywords: &["build", "cargo", "check", "command", "run", "shell", "test"],
    },
    ToolDescriptor {
        name: "discover_tools",
        family: "context",
        schema: r#"discover_tools {"family":"filesystem|mutation|shell|web"} - load a bounded tool family"#,
        keywords: &[],
    },
    ToolDescriptor {
        name: "edit_file",
        family: "mutation",
        schema: r#"edit_file {"path":"...","content":"..."} - replace file content; approval required"#,
        keywords: &[
            "change",
            "edit",
            "fix",
            "implement",
            "patch",
            "refactor",
            "update",
        ],
    },
    ToolDescriptor {
        name: "list_files",
        family: "filesystem",
        schema: r#"list_files {"path":"..."} - list bounded workspace entries"#,
        keywords: &["directory", "file", "repo", "repository", "workspace"],
    },
    ToolDescriptor {
        name: "load_instruction",
        family: "context",
        schema: r#"load_instruction {"id":"..."} - load a scoped instruction by advertised ID"#,
        keywords: &[],
    },
    ToolDescriptor {
        name: "load_component",
        family: "context",
        schema: r#"load_component {"id":"..."} - load a registered module or MCP description; never activates it"#,
        keywords: &[],
    },
    ToolDescriptor {
        name: "read_artifact",
        family: "context",
        schema: r#"read_artifact {"artifact_id":"...","start":0,"length":4096} - load one bounded referenced chunk"#,
        keywords: &["artifact", "full", "log", "output"],
    },
    ToolDescriptor {
        name: "read_file",
        family: "filesystem",
        schema: r#"read_file {"path":"..."} - read a bounded workspace file"#,
        keywords: &["code", "file", "inspect", "read", "source"],
    },
    ToolDescriptor {
        name: "search",
        family: "filesystem",
        schema: r#"search {"path":"...","pattern":"..."} - search bounded workspace content"#,
        keywords: &["find", "repo", "search", "symbol"],
    },
    ToolDescriptor {
        name: "search_context",
        family: "context",
        schema: r#"search_context {"query":"..."} - search omitted durable history and return bounded redacted matches"#,
        keywords: &[],
    },
    ToolDescriptor {
        name: "web_fetch",
        family: "web",
        schema: r#"web_fetch {"url":"https://..."} - fetch bounded public web content through policy"#,
        keywords: &[
            "cite", "http", "internet", "latest", "release", "url", "web",
        ],
    },
    ToolDescriptor {
        name: "web_search",
        family: "web",
        schema: r#"web_search {"query":"..."} - search public web through policy"#,
        keywords: &[
            "cite", "current", "internet", "latest", "research", "search", "web",
        ],
    },
    ToolDescriptor {
        name: "write_file",
        family: "mutation",
        schema: r#"write_file {"path":"...","content":"..."} - write a workspace file; approval required"#,
        keywords: &["add", "create", "implement", "update", "write"],
    },
];

#[derive(Clone)]
struct Candidate {
    message: ProviderMessage,
    tier: ContextTier,
    order: usize,
    relevance: usize,
    tokens: u64,
    original_tokens: u64,
    hash: String,
    always_include: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ComponentKind {
    Module,
    Mcp,
}

#[derive(Clone, Debug)]
struct ComponentDescription {
    id: String,
    name: String,
    version: String,
    kind: ComponentKind,
    capabilities: Vec<String>,
}

impl ComponentDescription {
    fn searchable(&self) -> String {
        format!("{} {} {}", self.id, self.name, self.capabilities.join(" "))
    }

    fn kind_name(&self) -> &'static str {
        match self.kind {
            ComponentKind::Module => "module",
            ComponentKind::Mcp => "mcp",
        }
    }

    fn catalog_entry(&self) -> String {
        format!(
            "- {} ({}, {}, version {})",
            self.id,
            self.kind_name(),
            self.name,
            self.version
        )
    }

    fn prompt_description(&self) -> String {
        format!(
            "[component id={} kind={}; description only, not activated]\nname: {}\nversion: {}\ncapabilities: {}",
            self.id,
            self.kind_name(),
            self.name,
            self.version,
            self.capabilities.join(", ")
        )
    }
}

pub struct ContextOptimizer {
    config: ContextOptimizerConfig,
    instructions: InstructionResolver,
    artifact_store: DurableArtifactStore,
    components: BTreeMap<String, ComponentDescription>,
}

impl ContextOptimizer {
    pub fn new(workspace: impl Into<PathBuf>) -> Result<Self, ContextOptimizerError> {
        Self::with_config(
            workspace,
            crate::default_artifact_root(),
            ContextOptimizerConfig::default(),
        )
    }

    pub fn with_config(
        workspace: impl Into<PathBuf>,
        artifact_root: impl Into<PathBuf>,
        config: ContextOptimizerConfig,
    ) -> Result<Self, ContextOptimizerError> {
        let workspace = workspace.into();
        let artifact_store = DurableArtifactStore::open(artifact_root)
            .map_err(|error| ContextOptimizerError::Artifacts(error.to_string()))?;
        Ok(Self {
            config,
            instructions: InstructionResolver::new(workspace),
            artifact_store,
            components: BTreeMap::new(),
        })
    }

    pub fn register_module(&mut self, descriptor: ModuleDescriptor) {
        self.components.insert(
            descriptor.id.clone(),
            ComponentDescription {
                id: descriptor.id,
                name: descriptor.name,
                version: descriptor.version,
                kind: ComponentKind::Module,
                capabilities: descriptor.capabilities,
            },
        );
    }

    pub fn register_extension(&mut self, descriptor: CanonicalModuleSpec) {
        let kind = if descriptor.source == ExtensionSource::Mcp
            || descriptor.kind == CanonicalModuleKind::McpServer
        {
            ComponentKind::Mcp
        } else {
            ComponentKind::Module
        };
        self.components.insert(
            descriptor.id.clone(),
            ComponentDescription {
                id: descriptor.id,
                name: descriptor.name,
                version: descriptor.version,
                kind,
                capabilities: descriptor.capabilities,
            },
        );
    }

    pub fn optimize(
        &mut self,
        messages: &[ProviderMessage],
        task: &str,
        state: &ContextRunState,
        prompt_budget: Option<u64>,
    ) -> Result<OptimizedContext, ContextOptimizerError> {
        let total_prompt_budget = prompt_budget
            .unwrap_or(self.config.max_prompt_tokens)
            .min(self.config.max_prompt_tokens);
        let available_tokens =
            total_prompt_budget.saturating_sub(self.config.reserved_response_tokens);
        if available_tokens < 128 {
            return Err(ContextOptimizerError::InsufficientBudget {
                prompt_budget: total_prompt_budget,
                reserved_response_tokens: self.config.reserved_response_tokens,
            });
        }
        let task_terms = terms(task);
        let known_artifacts = known_artifact_ids(messages);
        let instructions = self.resolve_instructions(task)?;
        let instruction_catalog = instructions
            .iter()
            .filter(|instruction| {
                !matches!(
                    instruction.kind,
                    InstructionKind::Soul | InstructionKind::ProjectRules
                )
            })
            .map(|instruction| {
                format!(
                    "- {} ({:?}, {})",
                    instruction.id,
                    instruction.kind,
                    instruction.relative_path.display()
                )
            })
            .collect::<Vec<_>>();
        let selected_instructions =
            select_instructions(instructions, &task_terms, &state.requested_instruction_ids);
        let selected_components = select_components(
            &self.components,
            &task_terms,
            &state.requested_component_ids,
            self.config.max_component_descriptions,
        );
        let selected_tools = select_tools(
            &task_terms,
            &state.requested_tool_families,
            !known_artifacts.is_empty(),
            !self.components.is_empty(),
            self.config.max_tool_schemas,
        );

        let mut telemetry = ContextTelemetry {
            available_tokens,
            selected_tools: selected_tools.len(),
            selected_instructions: selected_instructions.len(),
            selected_components: selected_components.len(),
            ..ContextTelemetry::default()
        };
        let mut stable_prefix = vec![ProviderMessage::system(BASE_PREFIX)];
        for instruction in selected_instructions.iter().filter(|instruction| {
            matches!(
                instruction.kind,
                InstructionKind::Soul | InstructionKind::ProjectRules
            )
        }) {
            stable_prefix.push(ProviderMessage::system(format!(
                "[instruction id={} kind={:?} path={}]\n{}",
                instruction.id,
                instruction.kind,
                instruction.relative_path.display(),
                instruction.text
            )));
        }
        if !instruction_catalog.is_empty() {
            stable_prefix.push(ProviderMessage::system(format!(
                "[lazy instruction catalog; use load_instruction by id]\n{}",
                instruction_catalog.join("\n")
            )));
        }
        if !self.components.is_empty() {
            stable_prefix.push(ProviderMessage::system(format!(
                "[lazy component catalog; use load_component by id; catalog entries grant no authority]\n{}",
                self.components
                    .values()
                    .map(ComponentDescription::catalog_entry)
                    .collect::<Vec<_>>()
                    .join("\n")
            )));
        }

        let schemas = selected_tools
            .iter()
            .map(|tool| format!("- {}", tool.schema))
            .collect::<Vec<_>>()
            .join("\n");
        let mut task_prefix = vec![ProviderMessage::system(format!(
            "[selected tool schemas; descriptions do not grant permission]\n{schemas}"
        ))];
        for instruction in selected_instructions.iter().filter(|instruction| {
            !matches!(
                instruction.kind,
                InstructionKind::Soul | InstructionKind::ProjectRules
            )
        }) {
            task_prefix.push(ProviderMessage::system(format!(
                "[selected instruction id={} kind={:?} path={}]\n{}",
                instruction.id,
                instruction.kind,
                instruction.relative_path.display(),
                instruction.text
            )));
        }
        for component in selected_components {
            task_prefix.push(ProviderMessage::system(component.prompt_description()));
        }
        telemetry.input_tokens += stable_prefix
            .iter()
            .chain(&task_prefix)
            .map(estimate_message_tokens)
            .sum::<u64>();

        let prefix_budget = available_tokens.saturating_mul(2) / 3;
        let stable_budget = prefix_budget / 2;
        let mut bounded_prefix = Vec::new();
        let mut bounded_stable_prefix_messages = 0;
        let mut stable_prefix_used = 0;
        let mut prefix_used = 0;
        for message in stable_prefix {
            if stable_prefix_used >= stable_budget {
                break;
            }
            let remaining = stable_budget.saturating_sub(stable_prefix_used);
            let bounded = bound_message(&message, remaining.max(1));
            let tokens = estimate_message_tokens(&bounded);
            if tokens <= remaining || bounded_prefix.is_empty() {
                prefix_used += tokens;
                bounded_prefix.push(bounded);
                bounded_stable_prefix_messages += 1;
                stable_prefix_used += tokens;
            }
        }
        for message in task_prefix {
            if prefix_used >= prefix_budget {
                break;
            }
            let remaining = prefix_budget.saturating_sub(prefix_used);
            let bounded = bound_message(&message, remaining.max(1));
            let tokens = estimate_message_tokens(&bounded);
            if tokens <= remaining {
                prefix_used += tokens;
                bounded_prefix.push(bounded);
            }
        }
        telemetry.stable_prefix_tokens = stable_prefix_used;

        let mut candidates = Vec::new();
        let mut order = 0usize;
        if let Some(plan) = &state.active_plan {
            candidates.push(candidate(
                ProviderMessage::system(format!("[HOT active plan]\n{plan}")),
                ContextTier::Hot,
                order,
                usize::MAX,
                true,
                self.config.max_message_tokens,
            ));
            order += 1;
        }
        for approval in &state.pending_approvals {
            candidates.push(candidate(
                ProviderMessage::system(format!("[HOT pending approval]\n{approval}")),
                ContextTier::Hot,
                order,
                usize::MAX,
                true,
                self.config.max_message_tokens,
            ));
            order += 1;
        }
        for request in &state.artifact_requests {
            if !known_artifacts.contains(&request.artifact_id) {
                continue;
            }
            let metadata = self
                .artifact_store
                .metadata(&request.artifact_id)
                .map_err(|error| ContextOptimizerError::Artifacts(error.to_string()))?
                .ok_or_else(|| {
                    ContextOptimizerError::Artifacts(format!(
                        "referenced artifact {} is unavailable",
                        request.artifact_id
                    ))
                })?;
            if request.start >= metadata.byte_count {
                return Err(ContextOptimizerError::Artifacts(format!(
                    "artifact start {} is outside {} bytes",
                    request.start, metadata.byte_count
                )));
            }
            let length = request
                .length
                .min(self.config.max_artifact_chunk_bytes)
                .min(metadata.byte_count - request.start);
            if length == 0 {
                continue;
            }
            let bytes = self
                .artifact_store
                .read_range(&request.artifact_id, request.start, length)
                .map_err(|error| ContextOptimizerError::Artifacts(error.to_string()))?;
            let raw = String::from_utf8_lossy(&bytes);
            let reduced = reduce_text(&raw, self.config.max_artifact_summary_tokens);
            telemetry.restored_artifact_tokens += estimate_text_tokens(&reduced);
            candidates.push(candidate(
                ProviderMessage::system(format!(
                    "[HOT artifact chunk id={} start={} bytes={}]\n{}",
                    request.artifact_id,
                    request.start,
                    bytes.len(),
                    reduced
                )),
                ContextTier::Hot,
                order,
                usize::MAX,
                true,
                self.config.max_artifact_summary_tokens + 32,
            ));
            order += 1;
        }

        let latest_user = messages
            .iter()
            .rposition(|message| message.role() == "user");
        let hot_start = messages.len().saturating_sub(self.config.max_hot_messages);
        let warm_start = hot_start.saturating_sub(self.config.max_warm_messages);
        let mut seen = BTreeSet::new();
        for (index, message) in messages.iter().enumerate() {
            let hash = message_hash(message);
            if !seen.insert(hash.clone()) {
                let tokens = estimate_message_tokens(message);
                telemetry.input_tokens += tokens;
                telemetry.delta_omitted_tokens += tokens;
                continue;
            }
            let is_current_task = latest_user == Some(index);
            if state.delivered_message_hashes.contains(&hash) && !is_current_task {
                let tokens = estimate_message_tokens(message);
                telemetry.input_tokens += tokens;
                telemetry.delta_omitted_tokens += tokens;
                continue;
            }
            let tier = if is_current_task
                || index >= hot_start
                || message.content().contains("approval_required")
                || message.content().contains("pending approval")
            {
                ContextTier::Hot
            } else if index >= warm_start {
                ContextTier::Warm
            } else {
                ContextTier::Cold
            };
            let relevance = relevance(message.content(), &task_terms);
            let tier_limit = match tier {
                ContextTier::Hot => self.config.max_message_tokens,
                ContextTier::Warm => self.config.max_message_tokens.min(384),
                ContextTier::Cold => self.config.max_message_tokens.min(192),
            };
            candidates.push(candidate_with_hash(
                message.clone(),
                tier,
                order + index,
                relevance,
                is_current_task,
                tier_limit,
                hash,
            ));
        }

        for candidate in &candidates {
            telemetry.input_tokens += candidate.original_tokens;
            add_dropped(
                &mut telemetry,
                candidate.tier,
                candidate.original_tokens.saturating_sub(candidate.tokens),
            );
        }
        let mut ranked = (0..candidates.len()).collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            let left = &candidates[*left];
            let right = &candidates[*right];
            tier_rank(left.tier)
                .cmp(&tier_rank(right.tier))
                .then_with(|| right.always_include.cmp(&left.always_include))
                .then_with(|| right.relevance.cmp(&left.relevance))
                .then_with(|| right.order.cmp(&left.order))
                .then_with(|| left.hash.cmp(&right.hash))
        });

        let mut remaining = available_tokens.saturating_sub(prefix_used);
        let mut selected = BTreeSet::new();
        for index in ranked {
            let candidate = &candidates[index];
            if candidate.tier == ContextTier::Cold
                && candidate.relevance == 0
                && !candidate.always_include
            {
                add_dropped(&mut telemetry, candidate.tier, candidate.tokens);
                continue;
            }
            if candidate.tokens <= remaining {
                remaining -= candidate.tokens;
                selected.insert(index);
                continue;
            }
            if candidate.tier == ContextTier::Hot && remaining >= 16 {
                let bounded = bound_message(&candidate.message, remaining);
                let used = estimate_message_tokens(&bounded);
                if used <= remaining {
                    add_dropped(
                        &mut telemetry,
                        candidate.tier,
                        candidate.tokens.saturating_sub(used),
                    );
                    candidates[index].message = bounded;
                    candidates[index].tokens = used;
                    remaining -= used;
                    selected.insert(index);
                    continue;
                }
            }
            add_dropped(&mut telemetry, candidate.tier, candidate.tokens);
        }

        let mut selected_candidates = selected.into_iter().collect::<Vec<_>>();
        selected_candidates.sort_by_key(|index| candidates[*index].order);
        let mut selected_dynamic_hashes = Vec::new();
        let mut optimized_messages = bounded_prefix;
        for index in selected_candidates {
            selected_dynamic_hashes.push(candidates[index].hash.clone());
            optimized_messages.push(candidates[index].message.clone());
        }
        telemetry.selected_tokens = optimized_messages
            .iter()
            .map(estimate_message_tokens)
            .sum::<u64>();
        let stable_prefix_hash = prefix_hash(
            &optimized_messages[..bounded_stable_prefix_messages.min(optimized_messages.len())],
        );

        Ok(OptimizedContext {
            messages: optimized_messages,
            stable_prefix_hash,
            telemetry,
            selected_tool_names: selected_tools
                .iter()
                .map(|tool| tool.name.to_owned())
                .collect(),
            selected_dynamic_hashes,
        })
    }

    pub fn handle_control_call(
        &mut self,
        call: &ToolCall,
        messages: &[ProviderMessage],
        task: &str,
        state: &mut ContextRunState,
    ) -> Option<ToolObservation> {
        match call.name.as_str() {
            "discover_tools" => {
                let family = call
                    .arguments
                    .get("family")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let tools = TOOLS
                    .iter()
                    .filter(|tool| tool.family == family)
                    .map(|tool| tool.name)
                    .collect::<Vec<_>>();
                if tools.is_empty() || family == "context" {
                    return Some(control_observation(
                        call,
                        ToolOutcomeStatus::Error,
                        String::new(),
                        Some(format!("unknown or reserved tool family `{family}`")),
                    ));
                }
                state.request_tool_family(family);
                Some(control_observation(
                    call,
                    ToolOutcomeStatus::Success,
                    serde_json::json!({
                        "loaded_tool_family": family,
                        "tools": tools,
                        "authority_changed": false,
                    })
                    .to_string(),
                    None,
                ))
            }
            "load_instruction" => {
                let id = call
                    .arguments
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let available = match self.resolve_instructions(task) {
                    Ok(instructions) => instructions,
                    Err(error) => {
                        return Some(control_observation(
                            call,
                            ToolOutcomeStatus::Error,
                            String::new(),
                            Some(error.to_string()),
                        ));
                    }
                };
                if !available.iter().any(|instruction| instruction.id == id) {
                    return Some(control_observation(
                        call,
                        ToolOutcomeStatus::Error,
                        String::new(),
                        Some(format!("instruction `{id}` is unavailable in this scope")),
                    ));
                }
                state.request_instruction(id);
                Some(control_observation(
                    call,
                    ToolOutcomeStatus::Success,
                    serde_json::json!({
                        "loaded_instruction": id,
                        "authority_changed": false,
                    })
                    .to_string(),
                    None,
                ))
            }
            "load_component" => {
                let id = call
                    .arguments
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if !self.components.contains_key(id) {
                    return Some(control_observation(
                        call,
                        ToolOutcomeStatus::Error,
                        String::new(),
                        Some(format!("component `{id}` is unavailable in this scope")),
                    ));
                }
                state.request_component(id);
                Some(control_observation(
                    call,
                    ToolOutcomeStatus::Success,
                    serde_json::json!({
                        "loaded_component": id,
                        "activated": false,
                        "authority_changed": false,
                    })
                    .to_string(),
                    None,
                ))
            }
            "read_artifact" => {
                let id = call
                    .arguments
                    .get("artifact_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if !known_artifact_ids(messages).contains(id) {
                    return Some(control_observation(
                        call,
                        ToolOutcomeStatus::Denied,
                        String::new(),
                        Some("artifact is not referenced by this session context".into()),
                    ));
                }
                let metadata = match self.artifact_store.metadata(id) {
                    Ok(Some(metadata)) => metadata,
                    Ok(None) => {
                        return Some(control_observation(
                            call,
                            ToolOutcomeStatus::Error,
                            String::new(),
                            Some("referenced artifact is unavailable".into()),
                        ));
                    }
                    Err(error) => {
                        return Some(control_observation(
                            call,
                            ToolOutcomeStatus::Error,
                            String::new(),
                            Some(format!("artifact metadata lookup failed: {error}")),
                        ));
                    }
                };
                let start = call
                    .arguments
                    .get("start")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(0);
                if start >= metadata.byte_count {
                    return Some(control_observation(
                        call,
                        ToolOutcomeStatus::Error,
                        String::new(),
                        Some(format!(
                            "artifact start {start} is outside {} bytes",
                            metadata.byte_count
                        )),
                    ));
                }
                let requested_length = call
                    .arguments
                    .get("length")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(self.config.max_artifact_chunk_bytes);
                let length = requested_length
                    .min(self.config.max_artifact_chunk_bytes)
                    .min(metadata.byte_count - start);
                if length == 0 {
                    return Some(control_observation(
                        call,
                        ToolOutcomeStatus::Error,
                        String::new(),
                        Some("artifact length must be greater than zero".into()),
                    ));
                }
                state.request_artifact(id, start, length);
                Some(control_observation(
                    call,
                    ToolOutcomeStatus::Success,
                    serde_json::json!({
                        "artifact_id": id,
                        "start": start,
                        "bounded_length": length,
                        "total_bytes": metadata.byte_count,
                    })
                    .to_string(),
                    None,
                ))
            }
            "search_context" => {
                let query = call
                    .arguments
                    .get("query")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let query_terms = terms(query);
                if query_terms.is_empty() {
                    return Some(control_observation(
                        call,
                        ToolOutcomeStatus::Error,
                        String::new(),
                        Some("context search requires a non-empty query".into()),
                    ));
                }
                let mut matches = messages
                    .iter()
                    .rev()
                    .filter(|message| relevance(message.content(), &query_terms) > 0)
                    .take(5)
                    .map(|message| {
                        format!(
                            "[{}] {}",
                            message.role(),
                            reduce_text(&crate::tools::redact_text(message.content()), 128)
                        )
                    })
                    .collect::<Vec<_>>();
                matches.reverse();
                Some(control_observation(
                    call,
                    ToolOutcomeStatus::Success,
                    if matches.is_empty() {
                        "no matching durable context".into()
                    } else {
                        matches.join("\n")
                    },
                    None,
                ))
            }
            _ => None,
        }
    }

    fn resolve_instructions(
        &mut self,
        task: &str,
    ) -> Result<Vec<InstructionReference>, ContextOptimizerError> {
        self.instructions
            .resolve(&resolve_request(task))
            .map(|resolved| resolved.references)
            .map_err(|error| ContextOptimizerError::Instructions(error.to_string()))
    }
}

fn resolve_request(task: &str) -> ResolveRequest {
    let task_path = task
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric() && !"._-/".contains(ch)))
        .find(|token| token.contains('/') || token.contains('.'))
        .filter(|token| !token.is_empty())
        .map(PathBuf::from);
    ResolveRequest {
        task_path,
        ecosystems: terms(task),
    }
}

fn select_instructions(
    mut instructions: Vec<InstructionReference>,
    task_terms: &BTreeSet<String>,
    requested: &BTreeSet<String>,
) -> Vec<InstructionReference> {
    instructions.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
            .then_with(|| left.id.cmp(&right.id))
    });
    instructions
        .into_iter()
        .filter(|instruction| {
            matches!(
                instruction.kind,
                InstructionKind::Soul | InstructionKind::ProjectRules
            ) || requested.contains(&instruction.id)
                || instruction_relevance(instruction, task_terms) > 0
        })
        .collect()
}

fn instruction_relevance(
    instruction: &InstructionReference,
    task_terms: &BTreeSet<String>,
) -> usize {
    let searchable = format!(
        "{} {} {}",
        instruction.id,
        instruction.relative_path.display(),
        instruction.text
    );
    relevance(&searchable, task_terms)
}

fn select_tools(
    task_terms: &BTreeSet<String>,
    requested_families: &BTreeSet<String>,
    has_artifacts: bool,
    has_components: bool,
    limit: usize,
) -> Vec<ToolDescriptor> {
    let mut ranked = TOOLS
        .iter()
        .filter_map(|tool| {
            let mandatory = matches!(
                tool.name,
                "discover_tools" | "load_instruction" | "search_context"
            ) || (tool.name == "read_artifact" && has_artifacts)
                || (tool.name == "load_component" && has_components)
                || tool.family == "filesystem";
            let requested = requested_families.contains(tool.family);
            let score = tool
                .keywords
                .iter()
                .filter(|keyword| task_terms.contains(**keyword))
                .count();
            (mandatory || requested || score > 0).then_some((tool, mandatory, requested, score))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| right.3.cmp(&left.3))
            .then_with(|| left.0.name.cmp(right.0.name))
    });
    let mut selected = ranked
        .into_iter()
        .take(limit.max(2))
        .map(|(tool, ..)| *tool)
        .collect::<Vec<_>>();
    selected.sort_by_key(|tool| tool.name);
    selected
}

fn select_components<'a>(
    components: &'a BTreeMap<String, ComponentDescription>,
    task_terms: &BTreeSet<String>,
    requested: &BTreeSet<String>,
    limit: usize,
) -> Vec<&'a ComponentDescription> {
    let mut ranked = components
        .values()
        .filter_map(|component| {
            let explicitly_requested = requested.contains(&component.id);
            let score = relevance(&component.searchable(), task_terms);
            (explicitly_requested || score > 0).then_some((component, explicitly_requested, score))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.0.id.cmp(&right.0.id))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|(component, ..)| component)
        .collect()
}

fn candidate(
    message: ProviderMessage,
    tier: ContextTier,
    order: usize,
    relevance: usize,
    always_include: bool,
    max_tokens: u64,
) -> Candidate {
    let hash = message_hash(&message);
    candidate_with_hash(
        message,
        tier,
        order,
        relevance,
        always_include,
        max_tokens,
        hash,
    )
}

fn candidate_with_hash(
    message: ProviderMessage,
    tier: ContextTier,
    order: usize,
    relevance: usize,
    always_include: bool,
    max_tokens: u64,
    hash: String,
) -> Candidate {
    let original_tokens = estimate_message_tokens(&message);
    let message = bound_message(&message, max_tokens);
    let tokens = estimate_message_tokens(&message);
    Candidate {
        message,
        tier,
        order,
        relevance,
        tokens,
        original_tokens,
        hash,
        always_include,
    }
}

fn control_observation(
    call: &ToolCall,
    outcome: ToolOutcomeStatus,
    preview: String,
    error: Option<String>,
) -> ToolObservation {
    ToolObservation {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        arguments_summary: match call.name.as_str() {
            "discover_tools" => call
                .arguments
                .get("family")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            "load_instruction" => call
                .arguments
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            "load_component" => call
                .arguments
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            "read_artifact" => call
                .arguments
                .get("artifact_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            "search_context" => crate::tools::redact_text(
                call.arguments
                    .get("query")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
            ),
            _ => String::new(),
        },
        outcome,
        preview,
        artifact: None,
        error,
    }
}

fn add_dropped(telemetry: &mut ContextTelemetry, tier: ContextTier, tokens: u64) {
    match tier {
        ContextTier::Hot => telemetry.dropped_hot_tokens += tokens,
        ContextTier::Warm => telemetry.dropped_warm_tokens += tokens,
        ContextTier::Cold => telemetry.dropped_cold_tokens += tokens,
    }
}

fn tier_rank(tier: ContextTier) -> u8 {
    match tier {
        ContextTier::Hot => 0,
        ContextTier::Warm => 1,
        ContextTier::Cold => 2,
    }
}

fn terms(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|character: char| {
            !character.is_alphanumeric() && character != '_' && character != '-'
        })
        .filter(|term| term.len() >= 3)
        .map(str::to_owned)
        .collect()
}

fn relevance(text: &str, task_terms: &BTreeSet<String>) -> usize {
    let content_terms = terms(text);
    task_terms.intersection(&content_terms).count()
}

fn estimate_text_tokens(text: &str) -> u64 {
    text.len().div_ceil(4).max(1) as u64
}

fn estimate_message_tokens(message: &ProviderMessage) -> u64 {
    estimate_text_tokens(message.content()) + 1
}

fn bound_message(message: &ProviderMessage, max_tokens: u64) -> ProviderMessage {
    let content = reduce_text(message.content(), max_tokens.saturating_sub(1).max(1));
    match message.role() {
        "system" => ProviderMessage::system(content),
        "assistant" => ProviderMessage::assistant(content),
        "tool" => ProviderMessage::tool(content),
        _ => ProviderMessage::user(content),
    }
}

fn reduce_text(text: &str, max_tokens: u64) -> String {
    let max_bytes = usize::try_from(max_tokens.saturating_mul(4)).unwrap_or(usize::MAX);
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    if max_bytes < 32 {
        return text.chars().take(max_bytes).collect();
    }
    let marker = "\n...[deterministically reduced]...\n";
    let content_budget = max_bytes.saturating_sub(marker.len());
    let head_budget = content_budget.saturating_mul(3) / 4;
    let tail_budget = content_budget.saturating_sub(head_budget);
    let head = utf8_prefix(text, head_budget);
    let tail = utf8_suffix(text, tail_budget);
    format!("{head}{marker}{tail}")
}

fn utf8_prefix(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn utf8_suffix(text: &str, max_bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(max_bytes);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn message_hash(message: &ProviderMessage) -> String {
    let mut digest = Sha256::new();
    digest.update(message.role().as_bytes());
    digest.update([0]);
    digest.update(message.content().as_bytes());
    format!("{:x}", digest.finalize())
}

fn prefix_hash(messages: &[ProviderMessage]) -> String {
    let mut digest = Sha256::new();
    for message in messages {
        digest.update(message.role().as_bytes());
        digest.update([0]);
        digest.update(message.content().as_bytes());
        digest.update([0xff]);
    }
    format!("{:x}", digest.finalize())
}

fn known_artifact_ids(messages: &[ProviderMessage]) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for message in messages {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(message.content()) {
            collect_artifact_ids(&value, &mut ids);
        }
    }
    ids
}

fn is_artifact_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn collect_artifact_ids(value: &serde_json::Value, ids: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(artifact) = object.get("artifact") {
                match artifact {
                    serde_json::Value::Object(reference) => {
                        if let Some(id) = reference.get("id").and_then(serde_json::Value::as_str) {
                            if is_artifact_id(id) {
                                ids.insert(id.to_owned());
                            }
                        }
                    }
                    serde_json::Value::String(id) => {
                        if is_artifact_id(id) {
                            ids.insert(id.clone());
                        }
                    }
                    _ => {}
                }
            }
            for nested in object.values() {
                collect_artifact_ids(nested, ids);
            }
        }
        serde_json::Value::Array(values) => {
            for nested in values {
                collect_artifact_ids(nested, ids);
            }
        }
        _ => {}
    }
}

pub fn context_notice(telemetry: &ContextTelemetry) -> String {
    format!(
        "context_optimizer {}",
        serde_json::to_string(telemetry).unwrap_or_else(|_| "{}".into())
    )
}

pub fn summarize_pending_approvals(events: &[crate::Event]) -> (Option<String>, Vec<String>) {
    let mut plan = None;
    let mut approvals = BTreeMap::new();
    for event in events {
        match &event.payload {
            crate::EventPayload::Plan(plan_event) => plan = Some(plan_event.summary.clone()),
            crate::EventPayload::Approval(crate::ApprovalEvent::Requested { request }) => {
                approvals.insert(
                    request.id,
                    format!("{}: {}", request.id, request.action.summary),
                );
            }
            crate::EventPayload::Approval(crate::ApprovalEvent::Resolved { request }) => {
                approvals.remove(&request.id);
            }
            _ => {}
        }
    }
    (plan, approvals.into_values().collect())
}
