//! Per-agent budget и compaction model.
//!
//! Референс: OpenClaude per-agent step budget, separate compaction model.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Конфигурация budget для session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetConfig {
    /// Максимальное количество turns (None = unlimited).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,

    /// Максимальное количество tokens (None = unlimited).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,

    /// Максимальное wall-clock время (в секундах для JSON).
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "humantime_serde",
        default
    )]
    pub max_wall_time: Option<Duration>,

    /// Reasoning effort level.
    #[serde(default)]
    pub reasoning_effort: ReasoningEffort,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_turns: None,
            max_tokens: None,
            max_wall_time: None,
            reasoning_effort: ReasoningEffort::Medium,
        }
    }
}

/// Reasoning effort level (референс: Claude Code auto-mode, o1/o3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    #[default]
    Medium,
    High,
}

/// Runtime budget state для session.
#[derive(Debug, Clone)]
pub struct BudgetState {
    pub turns_used: u32,
    pub tokens_used: u64,
    pub started_at: Instant,
    pub compaction_count: u32,
}

impl BudgetState {
    pub fn new() -> Self {
        Self {
            turns_used: 0,
            tokens_used: 0,
            started_at: Instant::now(),
            compaction_count: 0,
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    pub fn context_used_percent(&self, limit: u64) -> u8 {
        if limit == 0 {
            return 0;
        }
        ((self.tokens_used as f64 / limit as f64) * 100.0).min(100.0) as u8
    }
}

impl Default for BudgetState {
    fn default() -> Self {
        Self::new()
    }
}

/// Budget enforcement errors.
#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    #[error("Turn limit exceeded: {used}/{limit}")]
    TurnLimitExceeded { limit: u32, used: u32 },

    #[error("Token limit exceeded: {used}/{limit} (requested {requested})")]
    TokenLimitExceeded {
        limit: u64,
        used: u64,
        requested: u64,
    },

    #[error("Wall time exceeded: {elapsed:?}/{limit:?}")]
    WallTimeExceeded { limit: Duration, elapsed: Duration },
}

/// Budget checker для session.
pub struct BudgetChecker {
    config: BudgetConfig,
    state: BudgetState,
}

impl BudgetChecker {
    pub fn new(config: BudgetConfig) -> Self {
        Self {
            config,
            state: BudgetState::new(),
        }
    }

    pub fn state(&self) -> &BudgetState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut BudgetState {
        &mut self.state
    }

    pub fn check_turn(&self) -> Result<(), BudgetError> {
        if let Some(max) = self.config.max_turns
            && self.state.turns_used >= max
        {
            return Err(BudgetError::TurnLimitExceeded {
                limit: max,
                used: self.state.turns_used,
            });
        }
        Ok(())
    }

    pub fn check_tokens(&self, request_tokens: u64) -> Result<(), BudgetError> {
        if let Some(max) = self.config.max_tokens {
            let projected = self.state.tokens_used + request_tokens;
            if projected > max {
                return Err(BudgetError::TokenLimitExceeded {
                    limit: max,
                    used: self.state.tokens_used,
                    requested: request_tokens,
                });
            }
        }
        Ok(())
    }

    pub fn check_wall_time(&self) -> Result<(), BudgetError> {
        if let Some(max) = self.config.max_wall_time {
            let elapsed = self.state.elapsed();
            if elapsed > max {
                return Err(BudgetError::WallTimeExceeded {
                    limit: max,
                    elapsed,
                });
            }
        }
        Ok(())
    }

    pub fn check_all(&self, request_tokens: u64) -> Result<(), BudgetError> {
        self.check_turn()?;
        self.check_tokens(request_tokens)?;
        self.check_wall_time()?;
        Ok(())
    }

    pub fn record_turn(&mut self, tokens_used: u64) {
        self.state.turns_used += 1;
        self.state.tokens_used += tokens_used;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_config_defaults() {
        let config = BudgetConfig::default();
        assert_eq!(config.max_turns, None);
        assert_eq!(config.max_tokens, None);
        assert_eq!(config.max_wall_time, None);
        assert_eq!(config.reasoning_effort, ReasoningEffort::Medium);
    }

    #[test]
    fn budget_checker_respects_turn_limit() {
        let config = BudgetConfig {
            max_turns: Some(3),
            ..Default::default()
        };
        let mut checker = BudgetChecker::new(config);

        checker.check_turn().unwrap();
        checker.record_turn(100);

        checker.check_turn().unwrap();
        checker.record_turn(100);

        checker.check_turn().unwrap();
        checker.record_turn(100);

        assert!(matches!(
            checker.check_turn(),
            Err(BudgetError::TurnLimitExceeded { .. })
        ));
    }

    #[test]
    fn budget_checker_respects_token_limit() {
        let config = BudgetConfig {
            max_tokens: Some(1000),
            ..Default::default()
        };
        let mut checker = BudgetChecker::new(config);

        checker.check_tokens(500).unwrap();
        checker.record_turn(500);

        checker.check_tokens(400).unwrap();
        checker.record_turn(400);

        assert!(matches!(
            checker.check_tokens(200),
            Err(BudgetError::TokenLimitExceeded { .. })
        ));
    }

    #[test]
    fn budget_checker_respects_wall_time_limit() {
        let config = BudgetConfig {
            max_wall_time: Some(Duration::from_millis(10)),
            ..Default::default()
        };
        let checker = BudgetChecker::new(config);

        std::thread::sleep(Duration::from_millis(20));

        assert!(matches!(
            checker.check_wall_time(),
            Err(BudgetError::WallTimeExceeded { .. })
        ));
    }

    #[test]
    fn budget_state_context_percent() {
        let mut state = BudgetState::new();
        state.tokens_used = 8000;

        assert_eq!(state.context_used_percent(10000), 80);
        assert_eq!(state.context_used_percent(0), 0);

        state.tokens_used = 15000;
        assert_eq!(state.context_used_percent(10000), 100);
    }

    #[test]
    fn budget_config_json_roundtrip() {
        let config = BudgetConfig {
            max_turns: Some(10),
            max_tokens: Some(100000),
            max_wall_time: Some(Duration::from_secs(300)),
            reasoning_effort: ReasoningEffort::High,
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: BudgetConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.max_turns, config.max_turns);
        assert_eq!(parsed.max_tokens, config.max_tokens);
        assert_eq!(parsed.reasoning_effort, config.reasoning_effort);
    }
}
