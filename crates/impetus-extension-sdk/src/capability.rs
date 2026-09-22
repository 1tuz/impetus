//! Closed set of capability kinds an extension may expose to the host.

use serde::{Deserialize, Serialize};

/// Capability kind token declared on an extension package.
///
/// Closed set for v1 — unknown tokens fail serde deserialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionCapabilityKind {
    Tool,
    Command,
    AgentHook,
    ContextProvider,
    WorkflowComponent,
    McpIntegration,
    LspIntegration,
    BrowserIntegration,
    MemoryProvider,
    /// Instruction pack (skills / declarative prompts).
    SkillProvider,
}

impl ExtensionCapabilityKind {
    /// Snake_case wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Command => "command",
            Self::AgentHook => "agent_hook",
            Self::ContextProvider => "context_provider",
            Self::WorkflowComponent => "workflow_component",
            Self::McpIntegration => "mcp_integration",
            Self::LspIntegration => "lsp_integration",
            Self::BrowserIntegration => "browser_integration",
            Self::MemoryProvider => "memory_provider",
            Self::SkillProvider => "skill_provider",
        }
    }
}

impl std::fmt::Display for ExtensionCapabilityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_capability_fails_serde() {
        let err = serde_json::from_str::<ExtensionCapabilityKind>("\"unknown_cap\"");
        assert!(err.is_err());
    }

    #[test]
    fn skill_provider_roundtrip() {
        let json = serde_json::to_string(&ExtensionCapabilityKind::SkillProvider).unwrap();
        assert_eq!(json, "\"skill_provider\"");
        let back: ExtensionCapabilityKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ExtensionCapabilityKind::SkillProvider);
    }
}
