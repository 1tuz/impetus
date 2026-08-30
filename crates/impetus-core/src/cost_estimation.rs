//! Cost estimation utilities for token usage and budget tracking.

use crate::model_router::ModelMetadata;

/// Estimate cost for token usage
pub fn estimate_cost(model: &ModelMetadata, input_tokens: u64, output_tokens: u64) -> Option<f64> {
    model.cost_per_mtok.map(|(input_cost, output_cost)| {
        let input_cost_usd = (input_tokens as f64 / 1_000_000.0) * input_cost;
        let output_cost_usd = (output_tokens as f64 / 1_000_000.0) * output_cost;
        input_cost_usd + output_cost_usd
    })
}

/// Format cost as human-readable string
pub fn format_cost(cost_usd: f64) -> String {
    if cost_usd < 0.01 {
        format!("${:.4}", cost_usd)
    } else {
        format!("${:.2}", cost_usd)
    }
}

/// Estimate remaining budget
pub fn estimate_remaining_budget(budget_tokens: Option<u64>, used_tokens: u64) -> Option<u64> {
    budget_tokens.map(|max| max.saturating_sub(used_tokens))
}

/// Check if request would exceed budget
pub fn would_exceed_budget(
    budget_tokens: Option<u64>,
    used_tokens: u64,
    request_tokens: u64,
) -> bool {
    if let Some(max) = budget_tokens {
        used_tokens + request_tokens > max
    } else {
        false
    }
}

/// Budget warning thresholds
#[derive(Debug, Clone, Copy)]
pub enum BudgetWarningLevel {
    /// No warning
    None,
    /// 50-80% used
    Low,
    /// 80-95% used
    Medium,
    /// 95%+ used
    High,
}

impl BudgetWarningLevel {
    pub fn from_usage_percent(percent: u8) -> Self {
        match percent {
            0..=49 => Self::None,
            50..=79 => Self::Low,
            80..=94 => Self::Medium,
            95..=100 => Self::High,
            _ => Self::High,
        }
    }

    pub fn message(&self, percent: u8) -> Option<String> {
        match self {
            Self::None => None,
            Self::Low => Some(format!("Budget {}% used", percent)),
            Self::Medium => Some(format!("Budget {}% used (approaching limit)", percent)),
            Self::High => Some(format!("Budget {}% used (critical)", percent)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_router::{ModelCapabilities, ModelMetadata};

    fn mock_model(cost_per_mtok: Option<(f64, f64)>) -> ModelMetadata {
        ModelMetadata {
            provider_id: "test".to_string(),
            model_id: "test-model".to_string(),
            capabilities: ModelCapabilities {
                tools: true,
                reasoning: false,
                vision: false,
                context_window: 8192,
            },
            cost_per_mtok,
            is_local: false,
            avg_latency_ms: None,
            health: 1.0,
        }
    }

    #[test]
    fn estimate_cost_calculates_correctly() {
        let model = mock_model(Some((10.0, 30.0))); // $10/Mtok input, $30/Mtok output
        let cost = estimate_cost(&model, 1_000_000, 500_000).unwrap();
        assert_eq!(cost, 25.0); // $10 + $15
    }

    #[test]
    fn estimate_cost_returns_none_for_local() {
        let model = mock_model(None);
        assert!(estimate_cost(&model, 1_000_000, 500_000).is_none());
    }

    #[test]
    fn format_cost_handles_ranges() {
        assert_eq!(format_cost(0.0001), "$0.0001");
        assert_eq!(format_cost(0.05), "$0.05");
        assert_eq!(format_cost(1.23), "$1.23");
        assert_eq!(format_cost(123.45), "$123.45");
    }

    #[test]
    fn estimate_remaining_budget_works() {
        assert_eq!(estimate_remaining_budget(Some(1000), 300), Some(700));
        assert_eq!(estimate_remaining_budget(Some(1000), 1200), Some(0));
        assert_eq!(estimate_remaining_budget(None, 500), None);
    }

    #[test]
    fn would_exceed_budget_detects_overrun() {
        assert!(!would_exceed_budget(Some(1000), 500, 400));
        assert!(would_exceed_budget(Some(1000), 500, 600));
        assert!(!would_exceed_budget(None, 5000, 10000));
    }

    #[test]
    fn budget_warning_levels() {
        assert!(matches!(
            BudgetWarningLevel::from_usage_percent(30),
            BudgetWarningLevel::None
        ));
        assert!(matches!(
            BudgetWarningLevel::from_usage_percent(60),
            BudgetWarningLevel::Low
        ));
        assert!(matches!(
            BudgetWarningLevel::from_usage_percent(85),
            BudgetWarningLevel::Medium
        ));
        assert!(matches!(
            BudgetWarningLevel::from_usage_percent(97),
            BudgetWarningLevel::High
        ));
    }

    #[test]
    fn budget_warning_messages() {
        assert_eq!(BudgetWarningLevel::None.message(30), None);
        assert_eq!(
            BudgetWarningLevel::Low.message(60),
            Some("Budget 60% used".to_string())
        );
        assert_eq!(
            BudgetWarningLevel::Medium.message(85),
            Some("Budget 85% used (approaching limit)".to_string())
        );
        assert_eq!(
            BudgetWarningLevel::High.message(97),
            Some("Budget 97% used (critical)".to_string())
        );
    }
}
