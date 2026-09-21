//! Daemon-owned execution modes (ASK / PLAN / ACCEPT_EDITS / AUTO / BYPASS).
//!
//! TUI and clients select mode via IPC; durable state lives in session events.
//! EffectSeam admission applies mode + RiskGate after hard Policy deny.

use serde::{Deserialize, Serialize};

/// Harness execution mode for a durable session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    #[default]
    Ask,
    Plan,
    AcceptEdits,
    Auto,
    Bypass,
}

impl ExecutionMode {
    /// Short stable label for status / TUI (ASCII).
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "ASK",
            Self::Plan => "PLAN",
            Self::AcceptEdits => "ACCEPT EDITS",
            Self::Auto => "AUTO",
            Self::Bypass => "BYPASS",
        }
    }

    /// Whether mutating tools may run in this mode (PLAN is read/plan only).
    pub fn is_mutating_tool_allowed(self) -> bool {
        !matches!(self, Self::Plan)
    }

    /// Shift+Tab cycle: Ask → AcceptEdits → Plan → Auto → Ask (Bypass excluded).
    pub fn cycle_next(self) -> Self {
        match self {
            Self::Ask => Self::AcceptEdits,
            Self::AcceptEdits => Self::Plan,
            Self::Plan => Self::Auto,
            Self::Auto | Self::Bypass => Self::Ask,
        }
    }

    /// IPC capability required to enter this mode, if any.
    pub fn required_ipc_capability(self) -> Option<&'static str> {
        match self {
            Self::AcceptEdits => Some("approval_scope_file_edits"),
            Self::Bypass => Some("approval_scope_full_auto"),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_ask() {
        assert_eq!(ExecutionMode::default(), ExecutionMode::Ask);
    }

    #[test]
    fn cycle_skips_bypass() {
        assert_eq!(ExecutionMode::Ask.cycle_next(), ExecutionMode::AcceptEdits);
        assert_eq!(ExecutionMode::AcceptEdits.cycle_next(), ExecutionMode::Plan);
        assert_eq!(ExecutionMode::Plan.cycle_next(), ExecutionMode::Auto);
        assert_eq!(ExecutionMode::Auto.cycle_next(), ExecutionMode::Ask);
        assert_eq!(ExecutionMode::Bypass.cycle_next(), ExecutionMode::Ask);
    }

    #[test]
    fn plan_denies_mutations() {
        assert!(!ExecutionMode::Plan.is_mutating_tool_allowed());
        for mode in [
            ExecutionMode::Ask,
            ExecutionMode::AcceptEdits,
            ExecutionMode::Auto,
            ExecutionMode::Bypass,
        ] {
            assert!(mode.is_mutating_tool_allowed(), "{mode:?}");
        }
    }

    #[test]
    fn serde_snake_case_round_trips() {
        for (mode, wire) in [
            (ExecutionMode::Ask, "\"ask\""),
            (ExecutionMode::Plan, "\"plan\""),
            (ExecutionMode::AcceptEdits, "\"accept_edits\""),
            (ExecutionMode::Auto, "\"auto\""),
            (ExecutionMode::Bypass, "\"bypass\""),
        ] {
            let encoded = serde_json::to_string(&mode).expect("encode");
            assert_eq!(encoded, wire, "{mode:?}");
            let decoded: ExecutionMode = serde_json::from_str(&encoded).expect("decode");
            assert_eq!(decoded, mode);
        }
    }
}
