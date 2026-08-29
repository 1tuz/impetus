use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Canonical module specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalModuleSpec {
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: ExtensionSource,
    pub kind: CanonicalModuleKind,
    pub capabilities: Vec<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Extension source system
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionSource {
    Native,
    AgentSkills,
    Mcp,
    AgentPlugins,
    ClaudeCode,
    Codex,
    Cursor,
    DeepSeekHarness,
    Custom(String),
}

/// Canonical module kind (normalized across systems)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalModuleKind {
    Skill,
    Tool,
    Command,
    Instruction,
    Profile,
    McpServer,
    Plugin,
    Extension,
}

/// Canonical skill definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: Vec<Instruction>,
    pub tools: Vec<ToolProvider>,
    pub triggers: Vec<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Canonical instruction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instruction {
    pub content: String,
    pub context: InstructionContext,
    pub priority: InstructionPriority,
}

/// Instruction context
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionContext {
    Global,
    Project,
    Session,
    Task,
}

/// Instruction priority
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionPriority {
    Critical,
    High,
    Normal,
    Low,
}

/// Canonical agent profile
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    pub model: Option<String>,
    pub instructions: Vec<Instruction>,
    pub tools: Vec<String>,
    pub capabilities: Vec<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Canonical command definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub handler: CommandHandler,
    pub arguments: Vec<CommandArgument>,
}

/// Command handler specification
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandHandler {
    Native {
        function: String,
    },
    External {
        executable: String,
        args: Vec<String>,
    },
    Mcp {
        server: String,
        method: String,
    },
}

/// Command argument
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandArgument {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub arg_type: String,
    pub default: Option<serde_json::Value>,
}

/// Canonical MCP module specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpModule {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub transport: McpTransport,
    pub capabilities: McpCapabilities,
}

/// MCP transport type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    Stdio,
    Http,
    Sse,
}

/// MCP capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpCapabilities {
    pub tools: bool,
    pub resources: bool,
    pub prompts: bool,
    pub sampling: bool,
}

/// Canonical tool provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolProvider {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
    pub handler: ToolHandler,
}

/// Tool handler specification
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolHandler {
    Native { function: String },
    External { executable: String },
    Mcp { server: String, tool: String },
}

/// Import capability status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ImportCapability {
    /// Fully supported, no loss of functionality
    Supported,
    /// Partially supported, some features missing
    Partial,
    /// Not supported yet, planned
    Unsupported,
    /// Incompatible, cannot be imported
    Incompatible,
}

/// Import compatibility matrix
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityMatrix {
    pub source: ExtensionSource,
    pub capabilities: HashMap<String, ImportCapability>,
    pub notes: Vec<String>,
}

/// Extension import result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub source: ExtensionSource,
    pub capability: ImportCapability,
    pub canonical: Option<CanonicalModuleSpec>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

impl CompatibilityMatrix {
    /// Get compatibility matrix for Agent Skills
    pub fn agent_skills() -> Self {
        let mut capabilities = HashMap::new();
        capabilities.insert("instructions".to_string(), ImportCapability::Unsupported);
        capabilities.insert("tools".to_string(), ImportCapability::Unsupported);
        capabilities.insert("triggers".to_string(), ImportCapability::Unsupported);
        capabilities.insert("profiles".to_string(), ImportCapability::Unsupported);

        Self {
            source: ExtensionSource::AgentSkills,
            capabilities,
            notes: vec!["Requires upstream spec audit before implementation".to_string()],
        }
    }

    /// Get compatibility matrix for MCP
    pub fn mcp() -> Self {
        let mut capabilities = HashMap::new();
        capabilities.insert("stdio".to_string(), ImportCapability::Unsupported);
        capabilities.insert("http".to_string(), ImportCapability::Unsupported);
        capabilities.insert("tools".to_string(), ImportCapability::Unsupported);
        capabilities.insert("resources".to_string(), ImportCapability::Unsupported);
        capabilities.insert("prompts".to_string(), ImportCapability::Unsupported);

        Self {
            source: ExtensionSource::Mcp,
            capabilities,
            notes: vec!["MCP adapter planned".to_string()],
        }
    }

    /// Get compatibility matrix for Agent Plugins
    pub fn agent_plugins() -> Self {
        let mut capabilities = HashMap::new();
        capabilities.insert("skills".to_string(), ImportCapability::Unsupported);
        capabilities.insert("mcp_servers".to_string(), ImportCapability::Unsupported);

        Self {
            source: ExtensionSource::AgentPlugins,
            capabilities,
            notes: vec!["Agent Plugins adapter planned".to_string()],
        }
    }

    /// Get compatibility matrix for Claude Code
    pub fn claude_code() -> Self {
        let mut capabilities = HashMap::new();
        capabilities.insert("extensions".to_string(), ImportCapability::Unsupported);
        capabilities.insert("plugins".to_string(), ImportCapability::Unsupported);

        Self {
            source: ExtensionSource::ClaudeCode,
            capabilities,
            notes: vec!["Claude Code adapter planned".to_string()],
        }
    }

    /// Get compatibility matrix for Codex
    pub fn codex() -> Self {
        let mut capabilities = HashMap::new();
        capabilities.insert("extensions".to_string(), ImportCapability::Unsupported);
        capabilities.insert("plugins".to_string(), ImportCapability::Unsupported);
        capabilities.insert("skills".to_string(), ImportCapability::Unsupported);

        Self {
            source: ExtensionSource::Codex,
            capabilities,
            notes: vec!["Codex adapter planned".to_string()],
        }
    }

    /// Get compatibility matrix for Cursor
    pub fn cursor() -> Self {
        let mut capabilities = HashMap::new();
        capabilities.insert("plugins".to_string(), ImportCapability::Unsupported);
        capabilities.insert("rules".to_string(), ImportCapability::Unsupported);
        capabilities.insert("skills".to_string(), ImportCapability::Unsupported);
        capabilities.insert("agents".to_string(), ImportCapability::Unsupported);
        capabilities.insert("commands".to_string(), ImportCapability::Unsupported);

        Self {
            source: ExtensionSource::Cursor,
            capabilities,
            notes: vec!["Cursor adapter planned".to_string()],
        }
    }

    /// Get compatibility matrix for DeepSeek Harness
    pub fn deepseek_harness() -> Self {
        let mut capabilities = HashMap::new();
        capabilities.insert("process_adapter".to_string(), ImportCapability::Unsupported);

        Self {
            source: ExtensionSource::DeepSeekHarness,
            capabilities,
            notes: vec!["Process adapter planned, no TS in daemon".to_string()],
        }
    }

    /// Get all compatibility matrices
    pub fn all() -> Vec<Self> {
        vec![
            Self::agent_skills(),
            Self::mcp(),
            Self::agent_plugins(),
            Self::claude_code(),
            Self::codex(),
            Self::cursor(),
            Self::deepseek_harness(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_module_spec_serialization() {
        let spec = CanonicalModuleSpec {
            id: "test-module".to_string(),
            name: "Test Module".to_string(),
            version: "1.0.0".to_string(),
            source: ExtensionSource::Native,
            kind: CanonicalModuleKind::Skill,
            capabilities: vec!["test".to_string()],
            metadata: HashMap::new(),
        };

        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: CanonicalModuleSpec = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, "test-module");
        assert_eq!(deserialized.source, ExtensionSource::Native);
    }

    #[test]
    fn compatibility_matrices_available() {
        let matrices = CompatibilityMatrix::all();
        assert_eq!(matrices.len(), 7);

        let mcp = CompatibilityMatrix::mcp();
        assert_eq!(mcp.source, ExtensionSource::Mcp);
        assert!(!mcp.capabilities.is_empty());
    }

    #[test]
    fn instruction_context_ordering() {
        // Higher context should override lower
        assert!(InstructionContext::Task as u8 > InstructionContext::Session as u8);
    }

    #[test]
    fn import_capability_levels() {
        assert_ne!(ImportCapability::Supported, ImportCapability::Partial);
        assert_ne!(
            ImportCapability::Unsupported,
            ImportCapability::Incompatible
        );
    }

    #[test]
    fn canonical_skill_structure() {
        let skill = CanonicalSkill {
            id: "test-skill".to_string(),
            name: "Test Skill".to_string(),
            description: "A test skill".to_string(),
            instructions: vec![],
            tools: vec![],
            triggers: vec!["test_trigger".to_string()],
            metadata: HashMap::new(),
        };

        assert_eq!(skill.id, "test-skill");
        assert_eq!(skill.triggers.len(), 1);
    }

    #[test]
    fn mcp_capabilities_default() {
        let caps = McpCapabilities::default();
        assert!(!caps.tools);
        assert!(!caps.resources);
        assert!(!caps.prompts);
        assert!(!caps.sampling);
    }
}
