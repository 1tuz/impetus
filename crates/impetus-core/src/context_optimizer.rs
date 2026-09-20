//! Context Optimizer — lazy descriptions + HOT/WARM/COLD tiers (Phase 6).
//!
//! Catalog entries stay name-only until `description(id)` is requested.
//! Bodies load once into an in-memory cache. `assemble` picks tiered items
//! within a token budget. No AgentLoop wiring yet.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
