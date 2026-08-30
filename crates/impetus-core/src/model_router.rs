//! Model Router: intelligent model selection based on capabilities, health, cost, and policies.
//!
//! Routes requests to appropriate models considering:
//! - Capability requirements (tool use, reasoning, vision)
//! - Model health and availability
//! - Cost and budget constraints
//! - Latency requirements
//! - Privacy/local-first preferences
//! - Cache efficiency

use crate::budget::BudgetConfig;
use serde::{Deserialize, Serialize};

/// Model selection policy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RouterPolicy {
    /// Prefer local models, fallback to cloud
    LocalFirst,
    /// Prefer free/cheaper models
    FreeFirst,
    /// Balance cost, latency, and quality
    #[default]
    Balanced,
    /// Prefer highest quality models regardless of cost
    QualityFirst,
    /// Minimize latency
    LowLatency,
}

/// Model capability requirements
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRequirements {
    /// Requires tool/function calling
    #[serde(default)]
    pub tools: bool,

    /// Requires extended reasoning (o1/o3 style)
    #[serde(default)]
    pub reasoning: bool,

    /// Requires vision/image understanding
    #[serde(default)]
    pub vision: bool,

    /// Minimum context window (tokens)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_context: Option<u64>,
}

/// Model metadata for routing decisions
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub provider_id: String,
    pub model_id: String,

    /// Model capabilities
    pub capabilities: ModelCapabilities,

    /// Cost per million tokens (input, output)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_per_mtok: Option<(f64, f64)>,

    /// Is this a local model?
    #[serde(default)]
    pub is_local: bool,

    /// Average latency (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_latency_ms: Option<u64>,

    /// Health status (0.0 = down, 1.0 = healthy)
    #[serde(default = "default_health")]
    pub health: f64,
}

fn default_health() -> f64 {
    1.0
}

/// Model capabilities
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub tools: bool,

    #[serde(default)]
    pub reasoning: bool,

    #[serde(default)]
    pub vision: bool,

    pub context_window: u64,
}

impl ModelCapabilities {
    pub fn satisfies(&self, requirements: &CapabilityRequirements) -> bool {
        if requirements.tools && !self.tools {
            return false;
        }
        if requirements.reasoning && !self.reasoning {
            return false;
        }
        if requirements.vision && !self.vision {
            return false;
        }
        if let Some(min_context) = requirements.min_context
            && self.context_window < min_context
        {
            return false;
        }
        true
    }
}

/// Model router configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelRouterConfig {
    /// Routing policy
    #[serde(default)]
    pub policy: RouterPolicy,

    /// Available models with metadata
    #[serde(default)]
    pub models: Vec<ModelMetadata>,

    /// Escalation chain for retries
    #[serde(default)]
    pub escalation_chain: Vec<String>,
}

/// Model router for intelligent model selection
pub struct ModelRouter {
    config: ModelRouterConfig,
}

impl ModelRouter {
    pub fn new(config: ModelRouterConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &ModelRouterConfig {
        &self.config
    }

    /// Select best model for given requirements and budget
    pub fn select_model(
        &self,
        requirements: &CapabilityRequirements,
        budget: &BudgetConfig,
    ) -> Option<ModelSelection> {
        // Filter models that satisfy capability requirements
        let mut candidates: Vec<&ModelMetadata> = self
            .config
            .models
            .iter()
            .filter(|m| m.capabilities.satisfies(requirements))
            .filter(|m| m.health > 0.5) // Only healthy models
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Score and sort by policy
        candidates.sort_by(|a, b| {
            let score_a = self.score_model(a, budget);
            let score_b = self.score_model(b, budget);
            score_b
                .partial_cmp(&score_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        candidates.first().map(|m| ModelSelection {
            provider_id: m.provider_id.clone(),
            model_id: m.model_id.clone(),
            reasoning: format!("Selected by {:?} policy", self.config.policy),
        })
    }

    /// Score model based on policy and budget
    fn score_model(&self, model: &ModelMetadata, budget: &BudgetConfig) -> f64 {
        let mut score = model.health; // Base: health

        match self.config.policy {
            RouterPolicy::LocalFirst => {
                if model.is_local {
                    score += 10.0;
                }
            }
            RouterPolicy::FreeFirst => {
                if let Some((input_cost, output_cost)) = model.cost_per_mtok {
                    let avg_cost = (input_cost + output_cost) / 2.0;
                    score += (100.0 - avg_cost).max(0.0) / 10.0;
                } else {
                    score += 5.0; // Unknown cost = moderate score
                }
            }
            RouterPolicy::Balanced => {
                // Balance cost, latency, and capabilities
                if model.is_local {
                    score += 2.0;
                }
                if let Some(latency) = model.avg_latency_ms {
                    score += (5000.0 - latency as f64).max(0.0) / 1000.0;
                }
                if let Some((input_cost, output_cost)) = model.cost_per_mtok {
                    let avg_cost = (input_cost + output_cost) / 2.0;
                    score += (50.0 - avg_cost).max(0.0) / 10.0;
                }
            }
            RouterPolicy::QualityFirst => {
                // Prefer models with more capabilities
                if model.capabilities.reasoning {
                    score += 5.0;
                }
                if model.capabilities.vision {
                    score += 2.0;
                }
                score += (model.capabilities.context_window as f64 / 100000.0).min(5.0);
            }
            RouterPolicy::LowLatency => {
                if let Some(latency) = model.avg_latency_ms {
                    score += (10000.0 - latency as f64).max(0.0) / 1000.0;
                } else {
                    score += 2.0; // Unknown latency = moderate penalty
                }
                if model.is_local {
                    score += 5.0; // Local usually faster
                }
            }
        }

        // Budget constraints
        if let Some(_max_tokens) = budget.max_tokens
            && let Some(context_limit) = budget.context_limit
            && model.capabilities.context_window < context_limit
        {
            score *= 0.5; // Penalty for insufficient context
        }

        score
    }

    /// Get escalation model after failure
    pub fn escalate(&self, current_model: &str) -> Option<ModelSelection> {
        let current_idx = self
            .config
            .escalation_chain
            .iter()
            .position(|m| m == current_model)?;

        let next_model_id = self.config.escalation_chain.get(current_idx + 1)?;
        let next_model = self
            .config
            .models
            .iter()
            .find(|m| &m.model_id == next_model_id)?;

        Some(ModelSelection {
            provider_id: next_model.provider_id.clone(),
            model_id: next_model.model_id.clone(),
            reasoning: format!("Escalated from {}", current_model),
        })
    }
}

/// Model selection result
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub provider_id: String,
    pub model_id: String,
    pub reasoning: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_local_model() -> ModelMetadata {
        ModelMetadata {
            provider_id: "ollama".to_string(),
            model_id: "llama3:8b".to_string(),
            capabilities: ModelCapabilities {
                tools: true,
                reasoning: false,
                vision: false,
                context_window: 8192,
            },
            cost_per_mtok: Some((0.0, 0.0)),
            is_local: true,
            avg_latency_ms: Some(500),
            health: 1.0,
        }
    }

    fn mock_cloud_model() -> ModelMetadata {
        ModelMetadata {
            provider_id: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            capabilities: ModelCapabilities {
                tools: true,
                reasoning: false,
                vision: false,
                context_window: 128000,
            },
            cost_per_mtok: Some((30.0, 60.0)),
            is_local: false,
            avg_latency_ms: Some(2000),
            health: 1.0,
        }
    }

    #[test]
    fn local_first_policy_prefers_local() {
        let config = ModelRouterConfig {
            policy: RouterPolicy::LocalFirst,
            models: vec![mock_local_model(), mock_cloud_model()],
            escalation_chain: vec![],
        };
        let router = ModelRouter::new(config);

        let requirements = CapabilityRequirements {
            tools: true,
            ..Default::default()
        };

        let selection = router
            .select_model(&requirements, &BudgetConfig::default())
            .unwrap();
        assert_eq!(selection.model_id, "llama3:8b");
    }

    #[test]
    fn quality_first_prefers_larger_context() {
        let config = ModelRouterConfig {
            policy: RouterPolicy::QualityFirst,
            models: vec![mock_local_model(), mock_cloud_model()],
            escalation_chain: vec![],
        };
        let router = ModelRouter::new(config);

        let requirements = CapabilityRequirements {
            tools: true,
            ..Default::default()
        };

        let selection = router
            .select_model(&requirements, &BudgetConfig::default())
            .unwrap();
        assert_eq!(selection.model_id, "gpt-4");
    }

    #[test]
    fn filters_by_capability_requirements() {
        let config = ModelRouterConfig {
            policy: RouterPolicy::Balanced,
            models: vec![mock_local_model()],
            escalation_chain: vec![],
        };
        let router = ModelRouter::new(config);

        let requirements = CapabilityRequirements {
            reasoning: true, // llama3 doesn't have reasoning
            ..Default::default()
        };

        let selection = router.select_model(&requirements, &BudgetConfig::default());
        assert!(selection.is_none());
    }

    #[test]
    fn escalation_chain_works() {
        let config = ModelRouterConfig {
            policy: RouterPolicy::Balanced,
            models: vec![mock_local_model(), mock_cloud_model()],
            escalation_chain: vec!["llama3:8b".to_string(), "gpt-4".to_string()],
        };
        let router = ModelRouter::new(config);

        let escalated = router.escalate("llama3:8b").unwrap();
        assert_eq!(escalated.model_id, "gpt-4");
    }

    #[test]
    fn capability_satisfaction() {
        let caps = ModelCapabilities {
            tools: true,
            reasoning: false,
            vision: false,
            context_window: 8192,
        };

        let req1 = CapabilityRequirements {
            tools: true,
            ..Default::default()
        };
        assert!(caps.satisfies(&req1));

        let req2 = CapabilityRequirements {
            reasoning: true,
            ..Default::default()
        };
        assert!(!caps.satisfies(&req2));

        let req3 = CapabilityRequirements {
            min_context: Some(10000),
            ..Default::default()
        };
        assert!(!caps.satisfies(&req3));
    }
}
