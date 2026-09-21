//! Context Optimizer — lazy descriptions + HOT/WARM/COLD tiers (Phase 6).
//!
//! Catalog entries stay name-only until `description(id)` is requested.
//! Bodies load once into an in-memory cache. `assemble` picks tiered items
//! within a token budget. [`ContextService`] wires this into the prompt path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::instructions::{InstructionKind, ResolvedInstructions};
use crate::profile::ServiceBinding;
use crate::provider::ProviderMessage;

/// Default token budget for instruction/tool system context in the prompt path.
pub const DEFAULT_CONTEXT_BUDGET_TOKENS: usize = 4_000;

/// Optional tool/module name stub for the context catalog (COLD by default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStub {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

impl ToolStub {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: None,
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// Builtin read-only tool name stubs (COLD refs unless budget allows).
pub fn default_tool_stubs() -> Vec<ToolStub> {
    vec![
        ToolStub::new("tool:list", "List").with_description("List workspace directory entries"),
        ToolStub::new("tool:read", "Read").with_description("Read a workspace file"),
        ToolStub::new("tool:search", "Search").with_description("Search workspace content"),
        ToolStub::new("tool:bash", "Bash").with_description("Run a shell command (policy-gated)"),
    ]
}

fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

fn tier_for_instruction(kind: InstructionKind) -> ContextTier {
    match kind {
        InstructionKind::Soul | InstructionKind::ProjectRules => ContextTier::Hot,
        InstructionKind::Convention | InstructionKind::Guide | InstructionKind::Skill => {
            ContextTier::Warm
        }
    }
}

/// Catalog entry without a loaded description body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextCatalogEntry {
    pub id: String,
    pub name: String,
}

/// Loads description bodies on first access.
pub trait DescriptionSource: Send + Sync {
    fn load_description(&self, id: &str) -> Option<String>;
}

/// In-memory source used by tests and simple registrations.
#[derive(Debug, Default, Clone)]
pub struct MemoryDescriptionSource {
    bodies: HashMap<String, String>,
}

impl MemoryDescriptionSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, id: impl Into<String>, body: impl Into<String>) {
        self.bodies.insert(id.into(), body.into());
    }
}

impl DescriptionSource for MemoryDescriptionSource {
    fn load_description(&self, id: &str) -> Option<String> {
        self.bodies.get(id).cloned()
    }
}

/// Prompt context temperature tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContextTier {
    /// Always included when present (task, recent turns, approvals).
    Hot,
    /// Included while budget remains (summaries).
    Warm,
    /// Id/ref only — never full body in the assembled prompt.
    Cold,
}

/// Kind of context payload for assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextPayload {
    /// Inline text for HOT/WARM.
    Text(String),
    /// Reference/id for COLD (or overflow demotion).
    Ref(String),
}

/// One assemblable context unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextItem {
    pub tier: ContextTier,
    pub id: String,
    pub kind: String,
    pub token_est: usize,
    pub payload: ContextPayload,
}

/// Builtin lazy context optimizer (`profile` binding `context: lazy`).
pub struct BuiltinContextOptimizer {
    catalog: Vec<ContextCatalogEntry>,
    source: Arc<dyn DescriptionSource>,
    cache: Mutex<HashMap<String, String>>,
    load_count: Mutex<HashMap<String, usize>>,
}

impl BuiltinContextOptimizer {
    pub fn new(catalog: Vec<ContextCatalogEntry>, source: Arc<dyn DescriptionSource>) -> Self {
        Self {
            catalog,
            source,
            cache: Mutex::new(HashMap::new()),
            load_count: Mutex::new(HashMap::new()),
        }
    }

    /// Name-only catalog (no description bodies).
    pub fn catalog(&self) -> &[ContextCatalogEntry] {
        &self.catalog
    }

    /// Load description on first access; subsequent calls hit the cache.
    pub fn description(&self, id: &str) -> Option<String> {
        {
            let cache = self.cache.lock().expect("context cache lock");
            if let Some(body) = cache.get(id) {
                return Some(body.clone());
            }
        }

        let body = self.source.load_description(id)?;
        {
            let mut cache = self.cache.lock().expect("context cache lock");
            cache.insert(id.to_string(), body.clone());
        }
        {
            let mut counts = self.load_count.lock().expect("load count lock");
            *counts.entry(id.to_string()).or_insert(0) += 1;
        }
        Some(body)
    }

    /// How many times the underlying source was hit for `id` (tests / metrics stub).
    pub fn load_count(&self, id: &str) -> usize {
        self.load_count
            .lock()
            .expect("load count lock")
            .get(id)
            .copied()
            .unwrap_or(0)
    }

    /// Token estimate without caching the body (does not increment `load_count`).
    pub fn peek_token_est(&self, id: &str) -> Option<usize> {
        {
            let cache = self.cache.lock().expect("context cache lock");
            if let Some(body) = cache.get(id) {
                return Some(estimate_tokens(body));
            }
        }
        self.source
            .load_description(id)
            .map(|body| estimate_tokens(&body))
    }

    /// Assemble items within `budget_tokens`.
    ///
    /// Order: HOT (all, even if over budget) → WARM while space → COLD as
    /// `Ref` only (token cost 1 each, dropped first when budget is tight).
    /// Deterministic: stable sort by (tier, id).
    pub fn assemble(items: &[ContextItem], budget_tokens: usize) -> Vec<ContextItem> {
        let mut hot: Vec<ContextItem> = Vec::new();
        let mut warm: Vec<ContextItem> = Vec::new();
        let mut cold: Vec<ContextItem> = Vec::new();

        for item in items {
            match item.tier {
                ContextTier::Hot => hot.push(item.clone()),
                ContextTier::Warm => warm.push(item.clone()),
                ContextTier::Cold => {
                    // COLD always becomes a cheap ref in the assembled set.
                    cold.push(ContextItem {
                        tier: ContextTier::Cold,
                        id: item.id.clone(),
                        kind: item.kind.clone(),
                        token_est: 1,
                        payload: ContextPayload::Ref(item.id.clone()),
                    });
                }
            }
        }

        hot.sort_by(|a, b| a.id.cmp(&b.id));
        warm.sort_by(|a, b| a.id.cmp(&b.id));
        cold.sort_by(|a, b| a.id.cmp(&b.id));

        let mut used: usize = hot.iter().map(|i| i.token_est).sum();
        let mut out = hot;

        for item in warm {
            if used.saturating_add(item.token_est) > budget_tokens {
                continue;
            }
            used = used.saturating_add(item.token_est);
            out.push(item);
        }

        for item in cold {
            if used.saturating_add(item.token_est) > budget_tokens {
                continue;
            }
            used = used.saturating_add(item.token_est);
            out.push(item);
        }

        out
    }
}

/// Prompt-path contract over a context optimizer.
///
/// Implementations select instruction/tool catalog entries under a token budget.
/// They do **not** evaluate policy or grant capabilities — tool execution still
/// goes through `Policy → Sandbox → Capability → Execution`.
pub trait ContextService: Send + Sync {
    /// Assemble system messages for the provider prompt (deterministic order).
    fn select_system_messages(&self, budget_tokens: usize) -> Vec<ProviderMessage>;

    /// How many times the underlying description source was hit for `id`.
    fn load_count(&self, id: &str) -> usize;

    /// Name-only catalog snapshot.
    fn catalog(&self) -> &[ContextCatalogEntry];
}

/// Thin wrapper: build catalog from instructions + optional tool stubs, assemble
/// under budget via [`BuiltinContextOptimizer`].
pub struct BuiltinContextService {
    optimizer: BuiltinContextOptimizer,
    tiers: HashMap<String, ContextTier>,
    kinds: HashMap<String, String>,
}

impl BuiltinContextService {
    /// Build from resolved workspace instructions and optional tool stubs.
    ///
    /// Instruction bodies are registered in the lazy source but only loaded into
    /// the assemble set when their tier requires a body (HOT/WARM). COLD stubs
    /// stay as refs unless budget includes them (still without loading bodies).
    pub fn from_instructions(
        instructions: &ResolvedInstructions,
        tools: impl IntoIterator<Item = ToolStub>,
    ) -> Self {
        let mut catalog = Vec::new();
        let mut source = MemoryDescriptionSource::new();
        let mut tiers = HashMap::new();
        let mut kinds = HashMap::new();

        for reference in &instructions.references {
            catalog.push(ContextCatalogEntry {
                id: reference.id.clone(),
                name: reference.relative_path.display().to_string(),
            });
            source.insert(reference.id.clone(), reference.text.clone());
            tiers.insert(reference.id.clone(), tier_for_instruction(reference.kind));
            kinds.insert(reference.id.clone(), format!("{:?}", reference.kind));
        }

        for tool in tools {
            catalog.push(ContextCatalogEntry {
                id: tool.id.clone(),
                name: tool.name.clone(),
            });
            if let Some(description) = tool.description {
                source.insert(tool.id.clone(), description);
            }
            tiers.insert(tool.id.clone(), ContextTier::Cold);
            kinds.insert(tool.id.clone(), "tool".to_string());
        }

        Self {
            optimizer: BuiltinContextOptimizer::new(catalog, Arc::new(source)),
            tiers,
            kinds,
        }
    }

    /// Materialize tiered items for assembly.
    ///
    /// HOT/WARM use `peek_token_est` (no load_count) so budget selection can run
    /// before bodies enter the description cache. COLD stays Ref/cost 1.
    fn build_items_for_budget(&self) -> Vec<ContextItem> {
        let mut items = Vec::with_capacity(self.optimizer.catalog().len());
        for entry in self.optimizer.catalog() {
            let tier = self
                .tiers
                .get(&entry.id)
                .copied()
                .unwrap_or(ContextTier::Cold);
            let kind = self
                .kinds
                .get(&entry.id)
                .cloned()
                .unwrap_or_else(|| "context".to_string());
            match tier {
                ContextTier::Hot | ContextTier::Warm => {
                    let Some(token_est) = self.optimizer.peek_token_est(&entry.id) else {
                        continue;
                    };
                    // Placeholder text; real body loaded only for selected ids.
                    items.push(ContextItem {
                        tier,
                        id: entry.id.clone(),
                        kind,
                        token_est,
                        payload: ContextPayload::Text(String::new()),
                    });
                }
                ContextTier::Cold => {
                    items.push(ContextItem {
                        tier,
                        id: entry.id.clone(),
                        kind,
                        token_est: 1,
                        payload: ContextPayload::Ref(entry.id.clone()),
                    });
                }
            }
        }
        items
    }
}

impl ContextService for BuiltinContextService {
    fn select_system_messages(&self, budget_tokens: usize) -> Vec<ProviderMessage> {
        let items = self.build_items_for_budget();
        let assembled = BuiltinContextOptimizer::assemble(&items, budget_tokens);
        assembled
            .into_iter()
            .filter_map(|item| match item.tier {
                ContextTier::Cold => Some(ProviderMessage::system(format!(
                    "[context-ref:{}]",
                    item.id
                ))),
                ContextTier::Hot | ContextTier::Warm => self
                    .optimizer
                    .description(&item.id)
                    .map(ProviderMessage::system),
            })
            .collect()
    }

    fn load_count(&self, id: &str) -> usize {
        self.optimizer.load_count(id)
    }

    fn catalog(&self) -> &[ContextCatalogEntry] {
        self.optimizer.catalog()
    }
}

/// Select system messages according to profile `context` binding.
///
/// - `Builtin { variant: "lazy" }` → token-budgeted [`BuiltinContextService`]
/// - `Disabled` → no instruction/tool system messages
/// - other builtins → eager full instruction dump (no budget trim)
pub fn system_messages_for_binding(
    binding: &ServiceBinding,
    instructions: &ResolvedInstructions,
    tools: &[ToolStub],
    budget_tokens: usize,
) -> Vec<ProviderMessage> {
    match binding {
        ServiceBinding::Builtin { variant } if variant == "lazy" => {
            let service =
                BuiltinContextService::from_instructions(instructions, tools.iter().cloned());
            service.select_system_messages(budget_tokens)
        }
        ServiceBinding::Disabled => Vec::new(),
        _ => instructions
            .references
            .iter()
            .map(|reference| ProviderMessage::system(reference.text.clone()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::{
        InstructionKind, InstructionReference, InstructionScope, InstructionTokenEstimate,
        ResolvedInstructions,
    };
    use std::path::PathBuf;

    fn sample() -> BuiltinContextOptimizer {
        let catalog = vec![
            ContextCatalogEntry {
                id: "bash".into(),
                name: "Bash".into(),
            },
            ContextCatalogEntry {
                id: "read".into(),
                name: "Read".into(),
            },
        ];
        let mut source = MemoryDescriptionSource::new();
        source.insert("bash", "Run shell commands");
        source.insert("read", "Read files");
        BuiltinContextOptimizer::new(catalog, Arc::new(source))
    }

    fn item(tier: ContextTier, id: &str, tokens: usize) -> ContextItem {
        ContextItem {
            tier,
            id: id.into(),
            kind: "test".into(),
            token_est: tokens,
            payload: match tier {
                ContextTier::Cold => ContextPayload::Ref(id.into()),
                _ => ContextPayload::Text(format!("body-{id}")),
            },
        }
    }

    #[test]
    fn catalog_is_name_only() {
        let opt = sample();
        assert_eq!(opt.catalog().len(), 2);
        assert_eq!(opt.catalog()[0].id, "bash");
    }

    #[test]
    fn description_loads_once_then_cache_hit() {
        let opt = sample();
        assert_eq!(opt.load_count("bash"), 0);
        assert_eq!(
            opt.description("bash").as_deref(),
            Some("Run shell commands")
        );
        assert_eq!(opt.load_count("bash"), 1);
        assert_eq!(
            opt.description("bash").as_deref(),
            Some("Run shell commands")
        );
        assert_eq!(opt.load_count("bash"), 1);
    }

    #[test]
    fn unknown_id_returns_none_without_cache() {
        let opt = sample();
        assert!(opt.description("nope").is_none());
        assert_eq!(opt.load_count("nope"), 0);
    }

    #[test]
    fn assemble_keeps_all_hot_even_over_budget() {
        let items = vec![
            item(ContextTier::Hot, "a", 80),
            item(ContextTier::Hot, "b", 80),
        ];
        let out = BuiltinContextOptimizer::assemble(&items, 50);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|i| i.tier == ContextTier::Hot));
    }

    #[test]
    fn assemble_drops_cold_then_warm_on_overflow() {
        let items = vec![
            item(ContextTier::Hot, "h", 40),
            item(ContextTier::Warm, "w1", 30),
            item(ContextTier::Warm, "w2", 30),
            item(ContextTier::Cold, "c1", 100),
            item(ContextTier::Cold, "c2", 100),
        ];
        // Budget 70: HOT(40) + one WARM(30) = 70; no room for second WARM or COLD refs.
        let out = BuiltinContextOptimizer::assemble(&items, 70);
        let ids: Vec<_> = out.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["h", "w1"]);
        assert!(out.iter().all(|i| i.tier != ContextTier::Cold));
    }

    #[test]
    fn assemble_includes_cold_refs_when_budget_allows() {
        let items = vec![
            item(ContextTier::Hot, "h", 10),
            item(ContextTier::Cold, "c-b", 50),
            item(ContextTier::Cold, "c-a", 50),
        ];
        let out = BuiltinContextOptimizer::assemble(&items, 20);
        let ids: Vec<_> = out.iter().map(|i| i.id.as_str()).collect();
        // COLD sorted by id; each costs 1 as Ref.
        assert_eq!(ids, vec!["h", "c-a", "c-b"]);
        assert!(matches!(out[1].payload, ContextPayload::Ref(_)));
    }

    #[test]
    fn assemble_is_deterministic() {
        let items = vec![
            item(ContextTier::Warm, "z", 5),
            item(ContextTier::Warm, "a", 5),
            item(ContextTier::Hot, "m", 5),
        ];
        let a = BuiltinContextOptimizer::assemble(&items, 100);
        let b = BuiltinContextOptimizer::assemble(&items, 100);
        assert_eq!(a, b);
        assert_eq!(
            a.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec!["m", "a", "z"]
        );
    }

    fn sample_instructions() -> ResolvedInstructions {
        ResolvedInstructions {
            references: vec![
                InstructionReference {
                    id: "project-rules".into(),
                    kind: InstructionKind::ProjectRules,
                    scope: InstructionScope::Workspace,
                    relative_path: PathBuf::from("AGENTS.md"),
                    content_hash: "h1".into(),
                    text: "HOT rules body that must stay".into(),
                },
                InstructionReference {
                    id: "conv-z".into(),
                    kind: InstructionKind::Convention,
                    scope: InstructionScope::Workspace,
                    relative_path: PathBuf::from(".impetus/conventions/z.md"),
                    content_hash: "h2".into(),
                    // ~30 tokens at len/4
                    text: "W".repeat(120),
                },
                InstructionReference {
                    id: "conv-a".into(),
                    kind: InstructionKind::Convention,
                    scope: InstructionScope::Workspace,
                    relative_path: PathBuf::from(".impetus/conventions/a.md"),
                    content_hash: "h3".into(),
                    text: "W".repeat(120),
                },
            ],
            estimated_tokens: InstructionTokenEstimate::default(),
        }
    }

    #[test]
    fn service_deterministic_order_and_overflow_drops_cold_then_warm() {
        let tools = vec![
            ToolStub::new("tool:bash", "Bash").with_description("shell"),
            ToolStub::new("tool:read", "Read").with_description("read"),
        ];
        let service = BuiltinContextService::from_instructions(&sample_instructions(), tools);
        // HOT (~8) + one WARM(30) = ~38; second WARM dropped; no room for COLD refs at 38.
        let messages = service.select_system_messages(38);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content(), "HOT rules body that must stay");
        // WARM sorted by id: conv-a before conv-z
        assert_eq!(messages[1].content(), "W".repeat(120));
        assert!(
            !messages
                .iter()
                .any(|m| m.content().starts_with("[context-ref:"))
        );

        let again = service.select_system_messages(38);
        assert_eq!(messages, again);
    }

    #[test]
    fn service_lazy_load_skips_cold_bodies() {
        let tools = vec![ToolStub::new("tool:bash", "Bash").with_description("Run shell commands")];
        let service = BuiltinContextService::from_instructions(&sample_instructions(), tools);
        // Tight budget: HOT only — WARM dropped after peek, COLD never description()-loaded.
        let _ = service.select_system_messages(10);
        assert_eq!(service.load_count("project-rules"), 1);
        assert_eq!(service.load_count("conv-a"), 0);
        assert_eq!(service.load_count("conv-z"), 0);
        assert_eq!(service.load_count("tool:bash"), 0);
    }

    #[test]
    fn lazy_binding_uses_optimizer_disabled_skips() {
        let instructions = sample_instructions();
        let tools = default_tool_stubs();
        let lazy = ServiceBinding::Builtin {
            variant: "lazy".into(),
        };
        let msgs = system_messages_for_binding(&lazy, &instructions, &tools, 40);
        assert!(!msgs.is_empty());
        assert_eq!(msgs[0].content(), "HOT rules body that must stay");

        let disabled =
            system_messages_for_binding(&ServiceBinding::Disabled, &instructions, &tools, 40);
        assert!(disabled.is_empty());
    }

    #[test]
    fn context_service_does_not_grant_policy_authority() {
        // Optimizer only produces messages — no Action / PolicyDecision surface.
        let service =
            BuiltinContextService::from_instructions(&sample_instructions(), default_tool_stubs());
        let messages = service.select_system_messages(100);
        assert!(messages.iter().all(|m| m.role() == "system"));
        // Catalog includes tools as name stubs only; no allow/deny decision.
        assert!(service.catalog().iter().any(|e| e.id.starts_with("tool:")));
    }
}
