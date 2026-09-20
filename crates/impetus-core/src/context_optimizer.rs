//! Context Optimizer — lazy description loading (Phase 6 MVP).
//!
//! Catalog entries stay name-only until `description(id)` is requested.
//! Bodies load once into an in-memory cache. No AgentLoop wiring yet.

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
}
