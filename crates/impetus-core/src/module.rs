use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Module identity and metadata
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleDescriptor {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: ModuleKind,
    pub provides: Vec<String>,
    pub requires: Vec<String>,
    pub capabilities: Vec<String>,
    pub permissions: ModulePermissions,
}

/// Module type classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleKind {
    AgentLoop,
    Scheduler,
    ToolProvider,
    SearchBackend,
    BrowserProvider,
    CredentialResolver,
    PolicyExtension,
    Custom,
}

/// Module permission requirements
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModulePermissions {
    #[serde(default)]
    pub filesystem: Vec<String>,
    #[serde(default)]
    pub network: Vec<String>,
    #[serde(default)]
    pub process: bool,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub remote: bool,
}

/// Module execution semantics
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionSemantics {
    ReadOnly,
    Idempotent,
    Mutating,
    NonReplayable,
}

/// Module lifecycle state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleState {
    Discovered,
    Probing,
    Ready,
    Starting,
    Running,
    Degraded,
    Stopping,
    Stopped,
    Failed,
    Incompatible,
}

/// Capability probe result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityProbe {
    pub capability: String,
    pub available: bool,
    pub version: Option<String>,
    pub details: Option<HashMap<String, serde_json::Value>>,
}

/// Module health status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleHealth {
    pub module_id: String,
    pub state: ModuleState,
    pub capabilities: Vec<CapabilityProbe>,
    pub last_check: String,
    pub error: Option<String>,
}

/// Compatibility check result
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Compatibility {
    Compatible,
    PartiallyCompatible,
    Incompatible,
    Unknown,
}

/// Module compatibility report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityReport {
    pub module_id: String,
    pub harness_version: String,
    pub module_version: String,
    pub overall: Compatibility,
    pub details: HashMap<String, Compatibility>,
}
